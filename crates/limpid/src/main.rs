use limpid_core::transport::{Handoff, Scheme, socket_path, view_socket_path};
use limpid_core::{render, settings::Settings};
use std::{
    error::Error,
    ffi::OsStr,
    io::{Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("limpid: {error}");
        std::process::exit(1);
    }
}

/// Which program shows a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Viewer {
    /// The native viewer: Markdown, standard input and agent pages.
    View,
    /// The WebKit host: HTML pages.
    Host,
}

impl Viewer {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "view" | "native" => Ok(Self::View),
            "host" | "webkit" => Ok(Self::Host),
            other => Err(format!("unknown viewer {other:?}; use view or host")),
        }
    }

    fn program(self) -> &'static str {
        match self {
            Self::View => "limpid-view",
            Self::Host => "limpid-host",
        }
    }

    fn socket(self, runtime: &Path, scheme: Scheme) -> PathBuf {
        match self {
            Self::View => view_socket_path(runtime, scheme),
            Self::Host => socket_path(runtime, scheme),
        }
    }
}

/// What the target names, before any viewer is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Stdin,
    Markdown(PathBuf),
    /// An HTML page; `sidecar` is the Markdown an agent page was rendered
    /// from, when it has one.
    Html {
        path: PathBuf,
        sidecar: Option<PathBuf>,
    },
}

impl Target {
    fn classify(target: &Path) -> Self {
        if target == Path::new("-") {
            return Self::Stdin;
        }
        let is_html = target.extension().and_then(|extension| extension.to_str()) == Some("html");
        if !is_html {
            return Self::Markdown(target.to_path_buf());
        }
        Self::Html {
            path: target.to_path_buf(),
            sidecar: agent_sidecar(target),
        }
    }
}

/// The Markdown beside an agent page: `<page>.md` next to `<page>.html` with
/// a `<page>.meta.json`, as `limpid-core <agent>` writes them. An exported
/// page that merely sits beside its source has no meta file and stays HTML.
fn agent_sidecar(page: &Path) -> Option<PathBuf> {
    let markdown = page.with_extension("md");
    let meta = page.with_extension("meta.json");
    (markdown.is_file() && meta.is_file()).then_some(markdown)
}

/// The viewer a target opens in, unless the user chose one. Choosing the
/// native viewer for HTML without a Markdown sidecar is an error: it has
/// nothing to show.
fn choose_viewer(target: &Target, forced: Option<Viewer>) -> Result<Viewer, String> {
    match (target, forced) {
        (Target::Html { sidecar: None, .. }, Some(Viewer::View)) => {
            Err("the native viewer opens Markdown; this HTML page has no Markdown beside it".into())
        }
        (_, Some(viewer)) => Ok(viewer),
        (Target::Html { sidecar: None, .. }, None) => Ok(Viewer::Host),
        _ => Ok(Viewer::View),
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mut args = arguments.into_iter();
    let mut out = None;
    let mut target = None;
    let mut turn_spec = None;
    let mut scheme = Scheme::Dark;
    let mut separate = false;
    let mut source_backed_flag = false;
    let mut forced = match std::env::var("LIMPID_VIEWER") {
        Ok(name) if !name.is_empty() => Some(Viewer::parse(&name)?),
        _ => None,
    };
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--out") => out = Some(args.next().ok_or("--out requires a file path")?),
            Some("--watch") => {}
            Some("--separate") => separate = true,
            Some("--source-backed") => source_backed_flag = true,
            Some("--dark") => scheme = Scheme::Dark,
            Some("--light") => scheme = Scheme::Light,
            Some("--viewer") => {
                let name = args.next().ok_or("--viewer requires view or host")?;
                forced = Some(Viewer::parse(&name.to_string_lossy())?);
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: limpid [--out FILE] [--watch] [--separate] [--source-backed] [--viewer view|host] [--dark|--light] <file.md | page.html | ->"
                );
                return Ok(());
            }
            Some(value) if value.starts_with('-') && value != "-" => {
                return Err(format!("unknown option: {value}").into());
            }
            _ if target.is_none() => target = Some(arg),
            _ if target.as_deref() == Some(std::ffi::OsStr::new("codex"))
                && turn_spec.is_none() =>
            {
                turn_spec = Some(arg)
            }
            _ => return Err("only one target is allowed".into()),
        }
    }
    let mut target = target.ok_or("missing target; use --help for usage")?;
    let mut generated = false;
    if target == "codex" {
        let sibling = std::env::current_exe()?.with_file_name("limpid-core");
        let executable = if sibling.exists() {
            sibling
        } else {
            PathBuf::from("limpid-core")
        };
        let mut command = Command::new(executable);
        command.arg("codex");
        if let Some(spec) = turn_spec {
            command.arg(spec);
        }
        let output = command.output()?;
        if !output.status.success() {
            return Err(format!(
                "limpid-core codex: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        target = String::from_utf8(output.stdout)?.trim().into();
        generated = true;
    }
    let classified = Target::classify(Path::new(&target));
    if let Some(out) = out {
        return export(&classified, Path::new(&out));
    }
    let viewer = choose_viewer(&classified, forced)?;
    let source_backed =
        generated || source_backed_flag || !matches!(classified, Target::Html { .. });
    open(&Opening {
        target: classified,
        viewer,
        scheme,
        separate,
        source_backed,
    })
}

/// `--out`: write the rendered HTML page and open nothing.
fn export(target: &Target, out: &Path) -> Result<(), Box<dyn Error>> {
    let html = match target {
        Target::Html { path, .. } => std::fs::read_to_string(path)?,
        Target::Stdin => {
            let mut source = String::new();
            std::io::stdin().read_to_string(&mut source)?;
            render_markdown(&source)?
        }
        Target::Markdown(path) => render_markdown(&std::fs::read_to_string(path)?)?,
    };
    std::fs::write(out, html)?;
    Ok(())
}

fn render_markdown(source: &str) -> Result<String, Box<dyn Error>> {
    let settings = Settings::load_default()?;
    Ok(render::page(&render::render_with_questions(
        source,
        render::RenderMode::File,
        &settings.questions.headings,
    )))
}

struct Opening {
    target: Target,
    viewer: Viewer,
    scheme: Scheme,
    separate: bool,
    source_backed: bool,
}

/// Hand the page to its viewer, starting the viewer when it is not running.
fn open(opening: &Opening) -> Result<(), Box<dyn Error>> {
    let pane = std::env::var("WEZTERM_PANE").ok();
    let handoff = match opening.viewer {
        Viewer::View => view_handoff(opening, pane)?,
        Viewer::Host => host_handoff(opening, pane)?,
    };
    if opening.separate {
        let mut child = viewer_command(opening.viewer)?
            .arg("--separate")
            .arg(scheme_flag(opening.scheme))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| launch_error(opening.viewer, &error))?;
        let mut stdin = child.stdin.take().ok_or("viewer has no stdin")?;
        serde_json::to_writer(&mut stdin, &handoff)?;
        stdin.write_all(b"\n")?;
        return Ok(());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
    let socket = opening.viewer.socket(Path::new(&runtime), opening.scheme);
    let mut child = launch_viewer(opening.viewer, &socket, opening.scheme)?;
    wait_socket(opening.viewer, &socket, &mut child)?;
    limpid_core::transport::send(&socket, &handoff)?;
    Ok(())
}

/// The native viewer reads the Markdown itself. A file named on the command
/// line is its own source; an agent page opens its sidecar Markdown and keeps
/// its replies beside it; standard input becomes a cached Markdown file.
fn view_handoff(opening: &Opening, pane: Option<String>) -> Result<Handoff, Box<dyn Error>> {
    let (path, source, source_hash) = match &opening.target {
        Target::Markdown(path) => {
            let bytes = std::fs::read(path)?;
            let path = path.canonicalize()?;
            (
                path.clone(),
                Some(path),
                Some(limpid_core::replies::source_hash(&bytes)),
            )
        }
        Target::Html {
            sidecar: Some(markdown),
            ..
        } => (markdown.canonicalize()?, None, None),
        Target::Html { sidecar: None, .. } => {
            return Err("the native viewer opens Markdown, not HTML".into());
        }
        Target::Stdin => {
            let mut source = Vec::new();
            std::io::stdin().read_to_end(&mut source)?;
            let cache = cache_dir()?;
            std::fs::create_dir_all(&cache)?;
            let path = cache.join(format!("stdin-{}.md", unique_suffix()?));
            std::fs::write(&path, source)?;
            (path.canonicalize()?, None, None)
        }
    };
    Ok(Handoff {
        path,
        theme: opening.scheme,
        pane,
        source,
        source_hash,
        source_backed: opening.source_backed,
    })
}

/// The WebKit host shows an HTML page: the one named, or Markdown rendered
/// into the cache.
fn host_handoff(opening: &Opening, pane: Option<String>) -> Result<Handoff, Box<dyn Error>> {
    let (path, source, source_hash) = match &opening.target {
        Target::Html { path, .. } => (path.clone(), None, None),
        Target::Markdown(_) | Target::Stdin => {
            let (bytes, source_path) = match &opening.target {
                Target::Markdown(path) => (std::fs::read(path)?, Some(path.canonicalize()?)),
                _ => {
                    let mut bytes = Vec::new();
                    std::io::stdin().read_to_end(&mut bytes)?;
                    (bytes, None)
                }
            };
            let html = render_markdown(&String::from_utf8_lossy(&bytes))?;
            let cache = cache_dir()?;
            std::fs::create_dir_all(&cache)?;
            let path = cache.join(format!("page-{}.html", unique_suffix()?));
            std::fs::write(&path, render::externalize_assets(&html, &cache)?)?;
            let hash = source_path
                .as_ref()
                .map(|_| limpid_core::replies::source_hash(&bytes));
            (path, source_path, hash)
        }
    };
    Ok(Handoff {
        path: path.canonicalize()?,
        theme: opening.scheme,
        pane,
        source,
        source_hash,
        source_backed: opening.source_backed,
    })
}

fn unique_suffix() -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
}

fn scheme_flag(scheme: Scheme) -> &'static str {
    match scheme {
        Scheme::Dark => "--dark",
        Scheme::Light => "--light",
    }
}

fn cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(base).join("limpid"));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".cache/limpid"))
}

fn launch_viewer(
    viewer: Viewer,
    socket: &Path,
    scheme: Scheme,
) -> Result<Option<std::process::Child>, Box<dyn Error>> {
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let child = viewer_command(viewer)?
        .arg(scheme_flag(scheme))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| launch_error(viewer, &error))?;
    Ok(Some(child))
}

fn launch_error(viewer: Viewer, error: &std::io::Error) -> String {
    if error.kind() != std::io::ErrorKind::NotFound {
        return format!("could not start {}: {error}", viewer.program());
    }
    match viewer {
        Viewer::View => "limpid-view is not installed; run install.sh".into(),
        Viewer::Host => "limpid-host is not installed; it is built only with \
                         LIMPID_WEBKIT=1 ./install.sh, and Markdown opens in limpid-view without it"
            .into(),
    }
}

fn viewer_command(viewer: Viewer) -> Result<Command, Box<dyn Error>> {
    let sibling = std::env::current_exe()?.with_file_name(viewer.program());
    let executable = if sibling.exists() {
        sibling
    } else {
        PathBuf::from(viewer.program())
    };
    Ok(detached_command(executable))
}

/// The viewer outlives the caller, so it must not share the caller's process
/// group: Ctrl-Z or Ctrl-C aimed at the launching shell or agent would
/// otherwise stop or kill every open window along with it.
fn detached_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.process_group(0);
    command
}

fn wait_socket(
    viewer: Viewer,
    socket: &Path,
    child: &mut Option<std::process::Child>,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..200 {
        if UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        if let Some(child) = child
            && let Some(status) = child.try_wait()?
        {
            return Err(format!(
                "{} exited before opening its socket: {status}",
                viewer.program()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!(
        "{} did not open its socket within two seconds",
        viewer.program()
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_group_of(pid: u32) -> u32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let after_name = stat.rsplit_once(')').unwrap().1;
        after_name
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn should_lead_its_own_process_group_when_spawned_detached() {
        let mut child = detached_command("sleep").arg("5").spawn().unwrap();
        let child_group = process_group_of(child.id());
        let own_group = process_group_of(std::process::id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(child_group, child.id());
        assert_ne!(child_group, own_group);
    }

    fn html(path: &str, sidecar: Option<&str>) -> Target {
        Target::Html {
            path: path.into(),
            sidecar: sidecar.map(Into::into),
        }
    }

    #[test]
    fn should_send_markdown_stdin_and_agent_pages_to_the_view_when_nothing_is_forced() {
        for target in [
            Target::Markdown("notes.md".into()),
            Target::Stdin,
            html("claude-1.html", Some("claude-1.md")),
        ] {
            assert_eq!(choose_viewer(&target, None), Ok(Viewer::View), "{target:?}");
        }
    }

    #[test]
    fn should_send_plain_html_to_the_host_when_nothing_is_forced() {
        assert_eq!(
            choose_viewer(&html("page.html", None), None),
            Ok(Viewer::Host)
        );
    }

    #[test]
    fn should_honour_a_forced_host_for_every_target() {
        for target in [
            Target::Markdown("notes.md".into()),
            Target::Stdin,
            html("claude-1.html", Some("claude-1.md")),
            html("page.html", None),
        ] {
            assert_eq!(
                choose_viewer(&target, Some(Viewer::Host)),
                Ok(Viewer::Host),
                "{target:?}"
            );
        }
    }

    #[test]
    fn should_refuse_a_forced_view_when_html_has_no_markdown_beside_it() {
        let error = choose_viewer(&html("page.html", None), Some(Viewer::View)).unwrap_err();
        assert!(error.contains("no Markdown"), "{error}");
        assert_eq!(
            choose_viewer(&html("a.html", Some("a.md")), Some(Viewer::View)),
            Ok(Viewer::View)
        );
    }

    #[test]
    fn should_parse_viewer_names_and_reject_others() {
        assert_eq!(Viewer::parse("view"), Ok(Viewer::View));
        assert_eq!(Viewer::parse("native"), Ok(Viewer::View));
        assert_eq!(Viewer::parse("host"), Ok(Viewer::Host));
        assert_eq!(Viewer::parse("webkit"), Ok(Viewer::Host));
        assert!(Viewer::parse("").is_err());
        assert!(Viewer::parse("View").is_err());
    }

    #[test]
    fn should_keep_view_and_host_sockets_apart_when_both_run_in_one_runtime_directory() {
        let runtime = Path::new("/run/user/1000");
        assert_eq!(
            Viewer::View.socket(runtime, Scheme::Light),
            PathBuf::from("/run/user/1000/limpid-view-light.sock")
        );
        assert_eq!(
            Viewer::Host.socket(runtime, Scheme::Light),
            PathBuf::from("/run/user/1000/limpid-light.sock")
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-route-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_find_the_sidecar_when_markdown_and_meta_sit_beside_the_page() {
        let dir = scratch("sidecar");
        let page = dir.join("codex-1.html");
        std::fs::write(&page, "<html>").unwrap();
        std::fs::write(dir.join("codex-1.md"), "# Hi").unwrap();
        std::fs::write(dir.join("codex-1.meta.json"), "{}").unwrap();
        assert_eq!(agent_sidecar(&page), Some(dir.join("codex-1.md")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_treat_an_exported_page_as_an_agent_page_when_it_has_no_meta_file() {
        let dir = scratch("export");
        let page = dir.join("notes.html");
        std::fs::write(&page, "<html>").unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes").unwrap();
        assert_eq!(agent_sidecar(&page), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_not_find_a_sidecar_when_only_the_meta_file_exists() {
        let dir = scratch("meta-only");
        let page = dir.join("pi-1.html");
        std::fs::write(&page, "<html>").unwrap();
        std::fs::write(dir.join("pi-1.meta.json"), "{}").unwrap();
        assert_eq!(agent_sidecar(&page), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_classify_targets_by_extension_and_dash() {
        assert_eq!(Target::classify(Path::new("-")), Target::Stdin);
        assert_eq!(
            Target::classify(Path::new("notes.md")),
            Target::Markdown("notes.md".into())
        );
        assert_eq!(
            Target::classify(Path::new("README")),
            Target::Markdown("README".into()),
            "anything that is not HTML is read as Markdown"
        );
        assert_eq!(
            Target::classify(Path::new("/nonexistent/p.html")),
            Target::Html {
                path: "/nonexistent/p.html".into(),
                sidecar: None
            }
        );
    }

    #[test]
    fn should_say_how_to_get_the_host_when_it_is_not_installed() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let message = launch_error(Viewer::Host, &missing);
        assert!(message.contains("LIMPID_WEBKIT=1"), "{message}");
        let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(launch_error(Viewer::View, &other).contains("limpid-view"));
    }
}
