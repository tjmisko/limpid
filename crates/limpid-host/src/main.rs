use gtk4::{gio, glib, prelude::*};
use limpid_core::transport::{Handoff, Scheme, serve, socket_path};
use std::{
    cell::RefCell,
    error::Error,
    fs,
    io::Write,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use webkit6::prelude::*;

struct WindowState {
    window: gtk4::ApplicationWindow,
    view: webkit6::WebView,
    request: Rc<RefCell<Handoff>>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("limpid-host: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut scheme = Scheme::Dark;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--light" => scheme = Scheme::Light,
            "--dark" => scheme = Scheme::Dark,
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    if std::env::var("LIMPID_JIT").as_deref() != Ok("1") {
        // This must be set before JavaScriptCore starts.
        unsafe {
            std::env::set_var("JSC_useJIT", "0");
        }
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
    let socket = socket_path(Path::new(&runtime), scheme);
    let listener = bind_socket(&socket)?;
    let (sender, receiver) = mpsc::channel::<Handoff>();
    std::thread::spawn(move || serve(listener, sender));
    let receiver = Rc::new(RefCell::new(receiver));

    let app = gtk4::Application::builder()
        .application_id("io.quasioptimal.limpid")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        let receiver = receiver.clone();
        let context = webkit6::WebContext::new();
        let windows = Rc::new(RefCell::new(Vec::<WindowState>::new()));
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
    });
    app.run();
    let _ = fs::remove_file(socket);
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
    let uri = format!(
        "{}?theme={}",
        gio::File::for_path(&path).uri(),
        request.theme.as_str()
    );
    if let Some(slot) = &request.slot {
        if let Some(existing) = windows
            .borrow()
            .iter()
            .find(|state| state.request.borrow().slot.as_ref() == Some(slot))
        {
            existing.view.load_uri(&uri);
            *existing.request.borrow_mut() = request.clone();
            if request.origin == limpid_core::transport::Origin::User {
                existing.window.present();
            }
            return Ok(());
        }
    }
    let manager = webkit6::UserContentManager::new();
    for name in ["reply", "comment", "mermaid-svg", "review", "send"] {
        manager.register_script_message_handler(name, None);
    }
    let active_request = Rc::new(RefCell::new(request.clone()));
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    let view = webkit6::WebView::builder()
        .web_context(context)
        .user_content_manager(&manager)
        .settings(&settings)
        .build();
    let restore_request = active_request.clone();
    view.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        let request = restore_request.borrow();
        if request.source.is_some() {
            return;
        }
        let mut path = request.path.clone();
        path.set_extension("replies.json");
        let Ok(replies) = limpid_core::replies::AgentReplies::load(&path) else {
            return;
        };
        let Ok(json) = serde_json::to_string(&replies.replies) else {
            return;
        };
        view.evaluate_javascript(
            &format!("window.limpidRestoreReplies({json})"),
            None,
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
    });
    let reply_request = active_request.clone();
    let reply_view = view.clone();
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
            Ok(true) => reply_view.reload(),
            Ok(false) => {}
            Err(error) => eprintln!("limpid-host: could not save reply: {error}"),
        }
    });
    let comment_request = active_request.clone();
    let comment_view = view.clone();
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
        match save_comment(&mut comment_request.borrow_mut(), range, turn, quote, text) {
            Ok(true) => comment_view.reload(),
            Ok(false) => {}
            Err(error) => eprintln!("limpid-host: could not save comment: {error}"),
        }
    });
    let review_request = active_request.clone();
    let review_view = view.clone();
    manager.connect_script_message_received(Some("review"), move |_, _| {
        let title = review_view
            .title()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Document".into());
        match compose_review(&review_request.borrow(), &title) {
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
            Err(error) => eprintln!("limpid-host: could not compose reply: {error}"),
        }
    });
    let send_request = active_request.clone();
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
            eprintln!("limpid-host: could not send reply: {error}");
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
    });
    if request.origin == limpid_core::transport::Origin::Hook {
        window.set_visible(true);
    } else {
        window.present();
    }
    Ok(())
}

fn save_reply(
    request: &mut Handoff,
    field: &str,
    text: &str,
    source_range: &str,
) -> Result<bool, Box<dyn Error>> {
    if !field.starts_with('q') || !field[1..].bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid question id".into());
    }
    if let (Some(source), Some(hash)) = (&request.source, &request.source_hash) {
        let end = source_range
            .split_once('-')
            .ok_or("invalid source range")?
            .1
            .parse::<usize>()?;
        let revision = limpid_core::replies::FileRevision::open_checked(source, hash)?;
        revision.insert_after(
            end,
            limpid_core::replies::FileAnnotation::Reply { field, text },
        )?;
        let updated = fs::read_to_string(source)?;
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
        request.source_hash = Some(limpid_core::replies::source_hash(updated.as_bytes()));
        return Ok(true);
    } else {
        let mut path = request.path.clone();
        path.set_extension("replies.json");
        let mut replies = limpid_core::replies::AgentReplies::load(&path)?;
        replies.replies.insert(field.to_owned(), text.to_owned());
        replies.save(&path)?;
    }
    Ok(false)
}

fn save_comment(
    request: &mut Handoff,
    range: &str,
    turn: &str,
    quote: &str,
    text: &str,
) -> Result<bool, Box<dyn Error>> {
    let (_, end) = range.split_once('-').ok_or("invalid source range")?;
    let end = end.parse::<usize>()?;
    if let (Some(source), Some(hash)) = (&request.source, &request.source_hash) {
        let revision = limpid_core::replies::FileRevision::open_checked(source, hash)?;
        revision.insert_after(end, limpid_core::replies::FileAnnotation::Comment { text })?;
        let updated = fs::read_to_string(source)?;
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
        request.source_hash = Some(limpid_core::replies::source_hash(updated.as_bytes()));
        return Ok(true);
    }
    let mut path = request.path.clone();
    path.set_extension("replies.json");
    let mut replies = limpid_core::replies::AgentReplies::load(&path)?;
    let key = if turn.is_empty() {
        range.to_owned()
    } else {
        format!("{turn}:{range}")
    };
    replies.comments.insert(
        key,
        limpid_core::replies::Comment {
            text: text.to_owned(),
            quote: quote.to_owned(),
            start_line: 0,
            end_line: 0,
        },
    );
    replies.save(&path)?;
    Ok(false)
}

fn page_meta(request: &Handoff) -> Result<serde_json::Value, Box<dyn Error>> {
    let mut path = request.path.clone();
    path.set_extension("meta.json");
    match fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Value::Null),
        Err(error) => Err(error.into()),
    }
}

fn compose_review(request: &Handoff, title: &str) -> Result<String, Box<dyn Error>> {
    let meta = page_meta(request)?;
    let agent = meta
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("file");
    let spec = meta
        .get("turn_spec")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("0");
    let context = if agent == "file" {
        "file".to_owned()
    } else {
        format!("turn {}", if spec.is_empty() { "0" } else { spec })
    };
    let replies = if let Some(source) = &request.source {
        limpid_core::replies::from_file_annotations(&fs::read_to_string(source)?)
    } else {
        let mut path = request.path.clone();
        path.set_extension("replies.json");
        limpid_core::replies::AgentReplies::load(&path)?
    };
    Ok(limpid_core::replies::compose_message(
        title, agent, &context, &replies,
    ))
}

fn deliver(request: &Handoff, message: &str) -> Result<(), Box<dyn Error>> {
    let settings = limpid_core::settings::Settings::load_default()?;
    let meta = page_meta(request)?;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&settings.reply.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env(
            "LIMPID_AGENT",
            meta.get("agent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("file"),
        );
    if let Some(session) = meta.get("session_id").and_then(serde_json::Value::as_str) {
        command.env("LIMPID_SESSION", session);
    }
    if let Some(source) = request
        .source
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| {
            meta.get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    {
        command.env("LIMPID_SOURCE", source);
    }
    if let Some(pane) = &request.pane {
        command.env("LIMPID_PANE", pane);
    }
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or("reply command has no stdin")?
        .write_all(message.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "reply command exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}
