use limpid_core::transport::{Handoff, Origin, Scheme, socket_path};
use limpid_core::{render, settings::Settings};
use std::{
    error::Error,
    io::Read,
    os::unix::net::UnixStream,
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

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut out = None;
    let mut target = None;
    let mut turn_spec = None;
    let mut slot = None;
    let mut origin = Origin::User;
    let mut scheme = Scheme::Dark;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--out") => out = Some(args.next().ok_or("--out requires a file path")?),
            Some("--slot") => {
                slot = Some(
                    args.next()
                        .ok_or("--slot requires an ID")?
                        .to_string_lossy()
                        .into_owned(),
                )
            }
            Some("--hook") => origin = Origin::Hook,
            Some("--dark") => scheme = Scheme::Dark,
            Some("--light") => scheme = Scheme::Light,
            Some("--help" | "-h") => {
                println!(
                    "Usage: limpid [--out FILE] [--slot ID] [--hook] [--dark|--light] <file.md | page.html | ->"
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
    }
    let (socket, mut host) = if out.is_none() {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required")?;
        let socket = socket_path(Path::new(&runtime), scheme);
        let host = launch_host(&socket, scheme)?;
        (Some(socket), host)
    } else {
        (None, None)
    };
    let target_is_html = target != "-"
        && Path::new(&target)
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("html");
    let mut source_path = None;
    let mut source_hash = None;
    let html = if target_is_html && out.is_none() {
        None
    } else if target == "-" {
        let mut source = String::new();
        std::io::stdin().read_to_string(&mut source)?;
        let settings = Settings::load_default()?;
        Some(render::page(&render::render_with_questions(
            &source,
            render::RenderMode::File,
            &settings.questions.headings,
        )))
    } else {
        let path = Path::new(&target);
        if target_is_html {
            Some(std::fs::read_to_string(path)?)
        } else {
            let source = std::fs::read_to_string(path)?;
            source_hash = Some(limpid_core::replies::source_hash(source.as_bytes()));
            source_path = Some(path.canonicalize()?);
            let settings = Settings::load_default()?;
            Some(render::page(&render::render_with_questions(
                &source,
                render::RenderMode::File,
                &settings.questions.headings,
            )))
        }
    };
    let path = if let Some(out) = out {
        let path = PathBuf::from(out);
        std::fs::write(&path, html.ok_or("no rendered page")?)?;
        path
    } else if target_is_html {
        PathBuf::from(target)
    } else {
        let cache = cache_dir()?;
        std::fs::create_dir_all(&cache)?;
        let path = cache.join(format!(
            "page-{}-{}.html",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let html = html.ok_or("no rendered page")?;
        std::fs::write(&path, render::externalize_assets(&html, &cache)?)?;
        path
    };
    if let Some(socket) = socket {
        wait_host(&socket, &mut host)?;
        limpid_core::transport::send(
            &socket,
            &Handoff {
                path: path.canonicalize()?,
                slot,
                origin,
                theme: scheme,
                pane: std::env::var("WEZTERM_PANE").ok(),
                source: source_path,
                source_hash,
            },
        )?;
    }
    Ok(())
}

fn cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(base).join("limpid"));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".cache/limpid"))
}

fn launch_host(
    socket: &Path,
    scheme: Scheme,
) -> Result<Option<std::process::Child>, Box<dyn Error>> {
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let sibling = std::env::current_exe()?.with_file_name("limpid-host");
    let executable = if sibling.exists() {
        sibling
    } else {
        PathBuf::from("limpid-host")
    };
    let child = Command::new(executable)
        .arg(if scheme == Scheme::Dark {
            "--dark"
        } else {
            "--light"
        })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(Some(child))
}

fn wait_host(socket: &Path, child: &mut Option<std::process::Child>) -> Result<(), Box<dyn Error>> {
    for _ in 0..200 {
        if UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        if let Some(child) = child
            && let Some(status) = child.try_wait()?
        {
            return Err(format!("limpid-host exited before opening its socket: {status}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("limpid-host did not open its socket within two seconds".into())
}
