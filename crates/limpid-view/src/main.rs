use limpid_core::{
    settings::Settings,
    transport::{Handoff, Scheme},
};
use limpid_view::{
    cli::{self, Args, Snapshot},
    feedback::Feedback,
    nav::CloseKind,
    page::{Closer, DocPage, SharedFonts},
    serve::{self, Request},
    snapshot::{render, write_png},
    text::Fonts,
    window::{App, Page, PageFactory, UserEvent, WindowConfig},
};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
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

/// Lets one page ask the event loop to close windows. The loop can only end
/// every window at once, so closing just this one works only while it is the
/// last; `live` counts the pages that still exist.
struct WindowCloser {
    live: Rc<Cell<usize>>,
    proxy: EventLoopProxy<UserEvent>,
}

impl WindowCloser {
    fn new(live: Rc<Cell<usize>>, proxy: EventLoopProxy<UserEvent>) -> Self {
        live.set(live.get() + 1);
        Self { live, proxy }
    }
}

impl Drop for WindowCloser {
    fn drop(&mut self) {
        self.live.set(self.live.get().saturating_sub(1));
    }
}

impl Closer for WindowCloser {
    fn close(&self, kind: CloseKind) -> bool {
        if kind == CloseKind::Window && self.live.get() > 1 {
            return false;
        }
        self.proxy.send_event(UserEvent::CloseAll).is_ok()
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
    let live_pages = Rc::new(Cell::new(0));
    let factory_proxy = proxy.clone();
    let factory: PageFactory = Box::new(move |request: &Handoff| {
        let page = DocPage::open(&request.path, &settings, request.theme, fonts.clone())?;
        let feedback = Feedback::new(request.clone(), page.title(), &settings);
        let page = page
            .with_feedback(feedback)
            .with_closer(Box::new(WindowCloser::new(
                live_pages.clone(),
                factory_proxy.clone(),
            )));
        Ok(Box::new(page) as Box<dyn Page>)
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
    runtime.join(format!("limpid-view-{}.sock", scheme.as_str()))
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
