use gtk4::{gio, glib, prelude::*};
use limpid_core::replies::{
    compose_pending, compose_review, deliver, deliver_or_wake, has_feedback, save_comment,
    save_pending, save_reply,
};
use limpid_core::transport::{Handoff, Scheme, serve, socket_path};
use std::{
    cell::RefCell,
    error::Error,
    fs,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use webkit6::prelude::*;

mod keys;
mod mermaid;

// Ctrl-Up/Down: jump to the previous/next section. Sections are the targets of
// the page's table of contents (in-page links inside <nav>) when it has one,
// else its h1-h3 headings. DIRECTION is replaced with 1 or -1.
const SECTION_JUMP: &str = r##"
(() => {
  const SLACK = 8;
  const fromToc = [...document.querySelectorAll('nav a[href^="#"]')]
    .map((a) => document.getElementById(decodeURIComponent(a.hash.slice(1))))
    .filter(Boolean);
  const targets = fromToc.length ? fromToc : [...document.querySelectorAll('h1, h2, h3')];
  const tops = [...new Set(targets
    .filter((el) => el.getClientRects().length)
    .map((el) => {
      const margin = parseFloat(getComputedStyle(el).scrollMarginTop) || 0;
      return Math.round(el.getBoundingClientRect().top + window.scrollY - margin);
    }))].sort((a, b) => a - b);
  const y = window.scrollY;
  const target = DIRECTION > 0
    ? tops.find((top) => top > y + SLACK)
    : tops.reverse().find((top) => top < y - SLACK);
  const top = target !== undefined ? target : DIRECTION < 0 ? 0 : y;
  window.scrollTo({ top, behavior: 'instant' });
})();
"##;

struct WindowState {
    window: gtk4::ApplicationWindow,
    view: webkit6::WebView,
    request: Rc<RefCell<Handoff>>,
    restore_anchor: Rc<RefCell<Option<String>>>,
    file_stamp: Rc<RefCell<Option<(std::time::SystemTime, u64)>>>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("limpid-host: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("--render-mermaid") {
        return mermaid::run_cli(&arguments[1..]);
    }
    let mut scheme = Scheme::Dark;
    let mut separate = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--light" => scheme = Scheme::Light,
            "--dark" => scheme = Scheme::Dark,
            "--separate" => separate = true,
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    if std::env::var("LIMPID_JIT").as_deref() != Ok("1") {
        // This must be set before JavaScriptCore starts.
        // SAFETY: no other threads exist yet, so the environment is not read
        // concurrently.
        unsafe {
            std::env::set_var("JSC_useJIT", "0");
        }
    }
    if std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_none() {
        // The DMA-BUF renderer flashes its uninitialized buffer (magenta) before
        // the first paint.
        // SAFETY: no other threads exist yet, so the environment is not read
        // concurrently.
        unsafe {
            std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
        }
    }
    let (sender, receiver) = mpsc::channel::<Handoff>();
    let socket = if separate {
        let request = serde_json::from_reader(std::io::stdin())?;
        sender.send(request)?;
        None
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
        let socket = socket_path(Path::new(&runtime), scheme);
        let listener = bind_socket(&socket)?;
        std::thread::spawn(move || serve(listener, sender));
        Some(socket)
    };
    let receiver = Rc::new(RefCell::new(receiver));

    // Tests and benchmarks set a window class so a compositor rule can keep
    // their windows off the live desktop. GTK derives the Wayland app id from
    // the program name.
    if let Ok(class) = std::env::var("LIMPID_WINDOW_CLASS") {
        glib::set_prgname(Some(&class));
    }
    let app = gtk4::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        if let Some(settings) = gtk4::Settings::default() {
            settings.set_gtk_interface_color_scheme(match scheme {
                Scheme::Dark => gtk4::InterfaceColorScheme::Dark,
                Scheme::Light => gtk4::InterfaceColorScheme::Light,
            });
        }
        let receiver = receiver.clone();
        let context = webkit6::WebContext::new();
        let windows = Rc::new(RefCell::new(Vec::<WindowState>::new()));
        let watched_windows = windows.clone();
        let startup_hold = Rc::new(RefCell::new(Some(app.hold())));
        let app = app.clone();
        glib::timeout_add_local(Duration::from_millis(10), move || {
            while let Ok(request) = receiver.borrow().try_recv() {
                if let Err(error) = open_request(&app, &context, &windows, request) {
                    eprintln!("limpid-host: {error}");
                } else {
                    startup_hold.borrow_mut().take();
                }
            }
            glib::ControlFlow::Continue
        });
        glib::timeout_add_local(Duration::from_millis(300), move || {
            for state in watched_windows.borrow().iter() {
                if let Err(error) = refresh_file(state) {
                    eprintln!("limpid-host: could not refresh file: {error}");
                }
            }
            glib::ControlFlow::Continue
        });
    });
    app.run_with_args(&["limpid-host"]);
    if let Some(socket) = socket {
        let _ = fs::remove_file(socket);
    }
    Ok(())
}

fn bind_socket(path: &Path) -> Result<UnixListener, Box<dyn Error>> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("host already running at {}", path.display()).into());
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn open_request(
    app: &gtk4::Application,
    context: &webkit6::WebContext,
    windows: &Rc<RefCell<Vec<WindowState>>>,
    request: Handoff,
) -> Result<(), Box<dyn Error>> {
    let path = fs::canonicalize(&request.path)?;
    if !path.is_file() || path.extension().and_then(|extension| extension.to_str()) != Some("html")
    {
        return Err("handoff target must be a local HTML file".into());
    }
    let source_backed = if request.source_backed {
        "&source-backed=1"
    } else {
        ""
    };
    let uri = format!(
        "{}?theme={}{source_backed}",
        gio::File::for_path(&path).uri(),
        request.theme.as_str()
    );
    let manager = webkit6::UserContentManager::new();
    for name in [
        "reply",
        "comment",
        "mermaid-svg",
        "review",
        "send",
        "close",
        "save",
        "deliver",
        "close-all",
    ] {
        manager.register_script_message_handler(name, None);
    }
    // Pages limpid renders already carry the client; injected pages get it here.
    if let Ok(editor) = limpid_core::render::client_editor_asset(&cache_dir()) {
        let editor = gio::File::for_path(&editor).uri();
        manager.add_script(&webkit6::UserScript::new(
            &format!("window.limpidAssets = {{ editor: {editor:?} }};"),
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
    }
    manager.add_style_sheet(&webkit6::UserStyleSheet::new(
        limpid_core::render::client_css(),
        webkit6::UserContentInjectedFrames::AllFrames,
        webkit6::UserStyleLevel::User,
        &[],
        &[],
    ));
    manager.add_script(&webkit6::UserScript::new(
        limpid_core::render::client_js(),
        webkit6::UserContentInjectedFrames::AllFrames,
        webkit6::UserScriptInjectionTime::End,
        &[],
        &[],
    ));
    let active_request = Rc::new(RefCell::new(request.clone()));
    let restore_anchor = Rc::new(RefCell::new(None::<String>));
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_allow_file_access_from_file_urls(true);
    settings.set_allow_universal_access_from_file_urls(true);
    settings.set_enable_write_console_messages_to_stdout(true);
    settings.set_enable_developer_extras(true);
    let builder = webkit6::WebView::builder()
        .user_content_manager(&manager)
        .settings(&settings);
    let first_view = windows.borrow().first().map(|state| state.view.clone());
    let view = if let Some(first_view) = &first_view {
        builder.related_view(first_view).build()
    } else {
        builder.web_context(context).build()
    };
    let anchor_on_load = restore_anchor.clone();
    view.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        if let Some(anchor) = anchor_on_load.borrow_mut().take() {
            view.evaluate_javascript(
                &format!("window.limpidRestoreAnchor({anchor})"),
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    });
    let reply_request = active_request.clone();
    let reply_view = view.clone();
    let reply_anchor = restore_anchor.clone();
    manager.connect_script_message_received(Some("reply"), move |_, value| {
        let Some(json) = value.to_json(0) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        let (Some(field), Some(text), Some(source_range)) = (
            message.get("field").and_then(serde_json::Value::as_str),
            message.get("text").and_then(serde_json::Value::as_str),
            message.get("src").and_then(serde_json::Value::as_str),
        ) else {
            return;
        };
        match save_reply(&mut reply_request.borrow_mut(), field, text, source_range) {
            Ok(true) => reload_with_anchor(&reply_view, &reply_anchor),
            Ok(false) => {}
            Err(error) => show_page_error(&reply_view, &format!("Could not save reply: {error}")),
        }
    });
    let comment_request = active_request.clone();
    let comment_view = view.clone();
    let comment_anchor = restore_anchor.clone();
    manager.connect_script_message_received(Some("comment"), move |_, value| {
        let Some(json) = value.to_json(0) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        let (Some(range), Some(text), Some(quote)) = (
            message.get("src").and_then(serde_json::Value::as_str),
            message.get("text").and_then(serde_json::Value::as_str),
            message.get("quote").and_then(serde_json::Value::as_str),
        ) else {
            return;
        };
        let turn = message
            .get("turn")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let line_start = message
            .get("lineStart")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;
        let line_end = message
            .get("lineEnd")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;
        match save_comment(
            &mut comment_request.borrow_mut(),
            range,
            turn,
            quote,
            text,
            line_start,
            line_end,
        ) {
            Ok(true) => reload_with_anchor(&comment_view, &comment_anchor),
            Ok(false) => {}
            Err(error) => {
                show_page_error(&comment_view, &format!("Could not save comment: {error}"))
            }
        }
    });
    let save_request = active_request.clone();
    let save_view = view.clone();
    let save_anchor = restore_anchor.clone();
    manager.connect_script_message_received(Some("save"), move |_, value| {
        let parsed = value_json(value);
        match save_pending(&mut save_request.borrow_mut(), &parsed) {
            Ok(true) => reload_with_anchor(&save_view, &save_anchor),
            Ok(false) => ack_saved(&save_view),
            Err(error) => show_page_error(&save_view, &format!("Could not save: {error}")),
        }
    });
    let deliver_request = active_request.clone();
    let deliver_view = view.clone();
    manager.connect_script_message_received(Some("deliver"), move |_, value| {
        let parsed = value_json(value);
        if let Err(error) = save_pending(&mut deliver_request.borrow_mut(), &parsed) {
            show_page_error(&deliver_view, &format!("Could not save: {error}"));
            return;
        }
        match has_feedback(&deliver_request.borrow()) {
            Ok(true) => {}
            Ok(false) => {
                deliver_view.evaluate_javascript(
                    "window.webkit?.messageHandlers?.close?.postMessage({})",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
                return;
            }
            Err(error) => {
                show_page_error(&deliver_view, &format!("Could not read feedback: {error}"));
                return;
            }
        }
        let title = deliver_view
            .title()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Document".into());
        let message = match compose_review(&deliver_request.borrow(), &title) {
            Ok(message) => message,
            Err(error) => {
                show_page_error(&deliver_view, &format!("Could not compose reply: {error}"));
                return;
            }
        };
        if let Err(error) = deliver_or_wake(&deliver_request.borrow(), &message) {
            show_page_error(&deliver_view, &format!("Could not send reply: {error}"));
        }
        deliver_view.evaluate_javascript(
            "window.webkit?.messageHandlers?.close?.postMessage({})",
            None,
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
    });
    let review_request = active_request.clone();
    let review_view = view.clone();
    manager.connect_script_message_received(Some("review"), move |_, value| {
        let title = review_view
            .title()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Document".into());
        let parsed = value_json(value);
        let composed = if parsed.get("replies").is_some() || parsed.get("comments").is_some() {
            compose_pending(&review_request.borrow(), &title, &parsed)
        } else {
            compose_review(&review_request.borrow(), &title)
        };
        match composed {
            Ok(message) => {
                if let Ok(json) = serde_json::to_string(&message) {
                    review_view.evaluate_javascript(
                        &format!("window.limpidShowReview({json})"),
                        None,
                        None,
                        None::<&gio::Cancellable>,
                        |_| {},
                    );
                }
            }
            Err(error) => {
                show_page_error(&review_view, &format!("Could not compose reply: {error}"))
            }
        }
    });
    let send_request = active_request.clone();
    let send_view = view.clone();
    manager.connect_script_message_received(Some("send"), move |_, value| {
        let Some(json) = value.to_json(0) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        let Some(text) = message.get("text").and_then(serde_json::Value::as_str) else {
            return;
        };
        if let Err(error) = deliver(&send_request.borrow(), text) {
            show_page_error(&send_view, &format!("Could not send reply: {error}"));
        }
    });
    manager.connect_script_message_received(Some("mermaid-svg"), move |_, value| {
        let Some(json) = value.to_json(0) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        let (Some(source), Some(svg), Some(hash)) = (
            message.get("source").and_then(serde_json::Value::as_str),
            message.get("svg").and_then(serde_json::Value::as_str),
            message.get("hash").and_then(serde_json::Value::as_str),
        ) else {
            return;
        };
        if svg.len() > 2_000_000
            || !svg.trim_start().starts_with("<svg")
            || limpid_core::replies::source_hash(source.as_bytes()) != hash
        {
            return;
        }
        if let Err(error) = cache_mermaid(hash, svg) {
            eprintln!("limpid-host: could not cache Mermaid diagram: {error}");
        }
    });
    view.load_uri(&uri);
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("limpid"),
        )
        .default_width(1000)
        .default_height(800)
        .build();
    window.set_child(Some(&view));
    // The page owns most keys; this handles the ones it leaves unhandled
    // (reload, zoom, inspector, quit, history).
    let keyboard = gtk4::EventControllerKey::new();
    let key_window = window.clone();
    let key_view = view.clone();
    let dispatcher = keys::KeyDispatcher::new();
    keyboard.connect_key_pressed(move |_, key, _, state| {
        use gtk4::gdk::ModifierType;
        let action = dispatcher.dispatch(
            key.name().as_deref(),
            state.contains(ModifierType::CONTROL_MASK),
            state.contains(ModifierType::SHIFT_MASK),
            (glib::monotonic_time() / 1000).max(0) as u64,
            false,
            false,
        );
        let handled = match action {
            Some(keys::Action::Quit) => {
                key_window.close();
                true
            }
            Some(keys::Action::Reload) => {
                key_view.reload();
                true
            }
            Some(keys::Action::ReloadBypassCache) => {
                key_view.reload_bypass_cache();
                true
            }
            Some(keys::Action::ZoomIn) => {
                key_view.set_zoom_level((key_view.zoom_level() + 0.1).min(2.0));
                true
            }
            Some(keys::Action::ZoomOut) => {
                key_view.set_zoom_level((key_view.zoom_level() - 0.1).max(0.5));
                true
            }
            Some(keys::Action::ZoomReset) => {
                key_view.set_zoom_level(1.0);
                true
            }
            Some(keys::Action::Inspector) => {
                if let Some(inspector) = key_view.inspector() {
                    inspector.show();
                }
                true
            }
            Some(keys::Action::Back) => {
                key_view.go_back();
                true
            }
            Some(keys::Action::Forward) => {
                key_view.go_forward();
                true
            }
            Some(keys::Action::SectionNext) => {
                jump_section(&key_view, 1);
                true
            }
            Some(keys::Action::SectionPrev) => {
                jump_section(&key_view, -1);
                true
            }
            Some(keys::Action::ToggleContents) => {
                key_view.evaluate_javascript(
                    "document.querySelector('.shell')?.classList.toggle('rail-hidden')",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
                true
            }
            _ => false,
        };
        if handled {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keyboard);
    let close_window = window.downgrade();
    manager.connect_script_message_received(Some("close"), move |_, _| {
        if let Some(window) = close_window.upgrade() {
            window.close();
        }
    });
    let all_windows = windows.clone();
    manager.connect_script_message_received(Some("close-all"), move |_, _| {
        let open = all_windows
            .borrow()
            .iter()
            .map(|state| state.window.clone())
            .collect::<Vec<_>>();
        for window in open {
            window.close();
        }
    });
    let title_window = window.clone();
    view.connect_title_notify(move |view| {
        if let Some(title) = view.title() {
            title_window.set_title(Some(&title));
        }
    });
    let states = windows.clone();
    let app_for_close = app.clone();
    window.connect_destroy(move |closed| {
        states.borrow_mut().retain(|state| state.window != *closed);
        if states.borrow().is_empty() {
            app_for_close.quit();
        }
    });
    windows.borrow_mut().push(WindowState {
        window: window.clone(),
        view,
        request: active_request,
        restore_anchor,
        file_stamp: Rc::new(RefCell::new(None)),
    });
    window.present();
    Ok(())
}

fn refresh_file(state: &WindowState) -> Result<(), Box<dyn Error>> {
    let request = state.request.borrow();
    let (Some(source), Some(previous_hash)) = (&request.source, &request.source_hash) else {
        return Ok(());
    };
    let metadata = fs::metadata(source)?;
    let stamp = (metadata.modified()?, metadata.len());
    if state.file_stamp.borrow().as_ref() == Some(&stamp) {
        return Ok(());
    }
    let updated = fs::read_to_string(source)?;
    let hash = limpid_core::replies::source_hash(updated.as_bytes());
    if &hash == previous_hash {
        *state.file_stamp.borrow_mut() = Some(stamp);
        return Ok(());
    }
    let settings = limpid_core::settings::Settings::load_default()?;
    let rendered = limpid_core::render::render_with_questions(
        &updated,
        limpid_core::render::RenderMode::File,
        &settings.questions.headings,
    );
    let html = limpid_core::render::page(&rendered);
    let cache = request
        .path
        .parent()
        .ok_or("page has no parent directory")?;
    fs::write(
        &request.path,
        limpid_core::render::externalize_assets(&html, cache)?,
    )?;
    drop(request);
    state.request.borrow_mut().source_hash = Some(hash);
    *state.file_stamp.borrow_mut() = Some(stamp);
    reload_with_anchor(&state.view, &state.restore_anchor);
    Ok(())
}

fn jump_section(view: &webkit6::WebView, direction: i32) {
    view.evaluate_javascript(
        &SECTION_JUMP.replace("DIRECTION", &direction.to_string()),
        None,
        None,
        None::<&gio::Cancellable>,
        |_| {},
    );
}

fn reload_with_anchor(view: &webkit6::WebView, anchor: &Rc<RefCell<Option<String>>>) {
    let view_to_reload = view.clone();
    let anchor = anchor.clone();
    view.evaluate_javascript(
        "window.limpidCaptureAnchor()",
        None,
        None,
        None::<&gio::Cancellable>,
        move |result| {
            *anchor.borrow_mut() = result
                .ok()
                .and_then(|value| value.to_json(0))
                .map(|value| value.to_string());
            view_to_reload.reload();
        },
    );
}

fn show_page_error(view: &webkit6::WebView, message: &str) {
    let Ok(json) = serde_json::to_string(message) else {
        return;
    };
    view.evaluate_javascript(
        &format!("window.limpidShowError({json})"),
        None,
        None,
        None::<&gio::Cancellable>,
        |_| {},
    );
}

fn value_json(value: &webkit6::javascriptcore::Value) -> serde_json::Value {
    value
        .to_json(0)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn ack_saved(view: &webkit6::WebView) {
    view.evaluate_javascript(
        "if (typeof window.limpidSaved === 'function') window.limpidSaved()",
        None,
        None,
        None::<&gio::Cancellable>,
        |_| {},
    );
}

fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })
        .unwrap_or_default();
    base.join("limpid")
}

fn cache_mermaid(hash: &str, svg: &str) -> Result<(), Box<dyn Error>> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })
        .ok_or("HOME or XDG_CACHE_HOME is required")?;
    let directory = base.join("limpid/mermaid");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join(format!("{hash}.svg")), svg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_socket_replaces_stale_socket_and_rejects_live_host() {
        let directory = std::env::temp_dir().join(format!(
            "limpid-host-socket-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("handoff.sock");

        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        let live = bind_socket(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(bind_socket(&path).is_err());

        drop(live);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
