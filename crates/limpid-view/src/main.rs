use limpid_core::{
    settings::Settings,
    transport::{Handoff, Scheme},
};
use limpid_view::{
    cli::{self, Args, Snapshot},
    feedback::{Feedback, host::EditorHost},
    nav::CloseKind,
    page::{Closer, DocPage, SharedFonts, load::Waker, mode_for},
    serve::{self, Request},
    snapshot::{render, write_png},
    text::Fonts,
    watch::WatchedPage,
    window::{App, Page, PageFactory, UserEvent, WindowConfig},
};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use winit::event_loop::{EventLoop, EventLoopProxy};

fn main() {
    if let Err(error) = run() {
        eprintln!("limpid-view: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args =
        cli::parse(std::env::args().skip(1)).map_err(|error| format!("{error}\n{}", cli::USAGE))?;
    let settings = Settings::load_default().map_err(|error| error.to_string())?;
    let fonts: SharedFonts = Rc::new(RefCell::new(Fonts::new()));
    if let Some(snapshot) = &args.snapshot {
        return run_snapshot(&args, snapshot, &settings, &fonts);
    }
    run_windows(args, settings, fonts)
}

/// Set to anything to paint the focus band and status bar into snapshots.
/// Snapshots are of the document by default, so they compare with other
/// renderers.
const SNAPSHOT_CHROME_VARIABLE: &str = "LIMPID_SNAPSHOT_CHROME";

fn run_snapshot(
    args: &Args,
    snapshot: &Snapshot,
    settings: &Settings,
    fonts: &SharedFonts,
) -> Result<(), String> {
    let target = args
        .target
        .as_deref()
        .ok_or("--snapshot needs a TARGET file")?;
    let chrome = std::env::var_os(SNAPSHOT_CHROME_VARIABLE).is_some();
    let mut page = DocPage::open(target, settings, args.scheme, fonts.clone())?.with_chrome(chrome);
    let image = render(&mut page, snapshot.width, snapshot.height, snapshot.scale)?;
    write_png(&snapshot.out, &image)
}

/// A file named on the command line is its own source: replies and comments
/// are written into it.
fn handoff_for(target: &Path, scheme: Scheme) -> Handoff {
    Handoff {
        path: target.to_path_buf(),
        theme: scheme,
        pane: None,
        source: Some(target.to_path_buf()),
        source_hash: None,
        source_backed: false,
    }
}

/// Lets one page ask the event loop to close windows. Closing this window
/// sets a flag the loop reads after the page handles an event; the process
/// ends when its last window closes.
struct WindowCloser {
    closing: Cell<bool>,
    proxy: EventLoopProxy<UserEvent>,
}

impl WindowCloser {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            closing: Cell::new(false),
            proxy,
        }
    }
}

impl Closer for WindowCloser {
    fn close(&self, kind: CloseKind) -> bool {
        let event = match kind {
            CloseKind::Window => {
                self.closing.set(true);
                UserEvent::Wake
            }
            CloseKind::All => UserEvent::CloseAll,
        };
        self.proxy.send_event(event).is_ok()
    }

    fn closing(&self) -> bool {
        self.closing.get()
    }
}

fn run_windows(args: Args, settings: Settings, fonts: SharedFonts) -> Result<(), String> {
    // Tests set a window class so a compositor rule can keep their windows off
    // the live desktop; the same variable switches on the test-only close request.
    let test_class = std::env::var("LIMPID_WINDOW_CLASS")
        .ok()
        .filter(|class| !class.is_empty());
    let config = WindowConfig {
        class: test_class
            .clone()
            .unwrap_or_else(|| WindowConfig::default().class),
        ..WindowConfig::default()
    };
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|error| error.to_string())?;
    let proxy = event_loop.create_proxy();
    // One nvim for the whole process, lent to one window's box at a time.
    let editor_host = EditorHost::shared_nvim();
    let factory_proxy = proxy.clone();
    let factory: PageFactory = Box::new(move |request: &Handoff| {
        let mode = mode_for(request);
        // The worker parses; it wakes the loop when the document is ready.
        let wake_proxy = factory_proxy.clone();
        let waker: Waker = Arc::new(move || {
            let _ = wake_proxy.send_event(UserEvent::Wake);
        });
        let page = DocPage::open_async(
            &request.path,
            &settings,
            request.theme,
            fonts.clone(),
            mode,
            Some(waker),
        )?;
        let feedback =
            Feedback::new(request.clone(), page.title(), &settings).with_host(editor_host.clone());
        let page = page
            .with_feedback(feedback)
            .with_closer(Box::new(WindowCloser::new(factory_proxy.clone())));
        let watched = WatchedPage::new(page, request.path.clone(), &settings).with_mode(mode);
        Ok(Box::new(watched) as Box<dyn Page>)
    });

    let mut socket = None;
    if args.separate {
        let request = match &args.target {
            Some(target) => handoff_for(target, args.scheme),
            None => serde_json::from_reader(std::io::stdin()).map_err(|error| error.to_string())?,
        };
        proxy
            .send_event(UserEvent::Open(request))
            .map_err(|error| error.to_string())?;
    } else {
        let path = socket_path(&args)?;
        let listener = serve::bind_socket(&path)?;
        let server_proxy = proxy.clone();
        let allow_close = test_class.is_some();
        std::thread::spawn(move || {
            serve::serve(listener, allow_close, |request| {
                let event = match request {
                    Request::Open(handoff) => UserEvent::Open(handoff),
                    Request::Close => UserEvent::CloseAll,
                };
                server_proxy.send_event(event).is_ok()
            });
        });
        socket = Some(path);
        if let Some(target) = &args.target {
            proxy
                .send_event(UserEvent::Open(handoff_for(target, args.scheme)))
                .map_err(|error| error.to_string())?;
        }
    }

    let mut app = App::new(factory, config, args.separate);
    let result = event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string());
    if let Some(socket) = socket {
        let _ = std::fs::remove_file(socket);
    }
    result
}

/// `--socket PATH`, else `limpid-view-<scheme>.sock` in the runtime directory.
fn socket_path(args: &Args) -> Result<PathBuf, String> {
    if let Some(socket) = &args.socket {
        return Ok(socket.clone());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or("XDG_RUNTIME_DIR is required (or pass --socket)")?;
    Ok(default_socket(Path::new(&runtime), args.scheme))
}

fn default_socket(runtime: &Path, scheme: Scheme) -> PathBuf {
    limpid_core::transport::view_socket_path(runtime, scheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_make_a_file_its_own_source_when_building_a_handoff() {
        let handoff = handoff_for(Path::new("/tmp/notes.md"), Scheme::Dark);
        assert_eq!(handoff.path, PathBuf::from("/tmp/notes.md"));
        assert_eq!(handoff.source.as_deref(), Some(Path::new("/tmp/notes.md")));
        assert_eq!(handoff.source_hash, None, "Feedback fills the hash in");
    }

    #[test]
    fn should_name_the_socket_after_the_scheme_when_no_path_is_given() {
        assert_eq!(
            default_socket(Path::new("/run/user/1"), Scheme::Light),
            PathBuf::from("/run/user/1/limpid-view-light.sock")
        );
    }
}
