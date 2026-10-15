use limpid_core::transport::{Handoff, Origin, Scheme, socket_path};
use limpid_core::{render, settings::Settings};
use std::{
    error::Error,
    io::{Read, Write},
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
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().is_some_and(|arg| arg == "hook") {
        if arguments.len() < 2 || arguments.len() > 3 {
            return Err("hook requires an agent and optional JSON payload".into());
        }
        return hook(
            &arguments[1],
            arguments.get(2).map(|value| value.as_os_str()),
        );
    }
    let mut args = arguments.into_iter();
    let mut out = None;
    let mut target = None;
    let mut turn_spec = None;
    let mut slot = None;
    let mut origin = Origin::User;
    let mut scheme = Scheme::Dark;
    let mut separate = false;
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
            Some("--watch") => {}
            Some("--separate") => separate = true,
            Some("--dark") => scheme = Scheme::Dark,
            Some("--light") => scheme = Scheme::Light,
            Some("--help" | "-h") => {
                println!(
                    "Usage: limpid [--out FILE] [--slot ID] [--hook] [--watch] [--separate] [--dark|--light] <file.md | page.html | ->"
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
    let output_only = out.is_some();
    let (socket, mut host) = if out.is_none() && !separate {
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
    if !output_only && (socket.is_some() || separate) {
        let handoff = Handoff {
            path: path.canonicalize()?,
            slot,
            origin,
            theme: scheme,
            pane: std::env::var("WEZTERM_PANE").ok(),
            source: source_path,
            source_hash,
        };
        if separate {
            let sibling = std::env::current_exe()?.with_file_name("limpid-host");
            let executable = if sibling.exists() {
                sibling
            } else {
                PathBuf::from("limpid-host")
            };
            let mut child = Command::new(executable)
                .arg("--separate")
                .arg(if scheme == Scheme::Dark {
                    "--dark"
                } else {
                    "--light"
                })
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            let mut stdin = child.stdin.take().ok_or("host has no stdin")?;
            serde_json::to_writer(&mut stdin, &handoff)?;
            stdin.write_all(b"\n")?;
        } else if let Some(socket) = socket {
            wait_host(&socket, &mut host)?;
            limpid_core::transport::send(&socket, &handoff)?;
        }
    }
    Ok(())
}

fn hook(
    agent: &std::ffi::OsStr,
    payload_argument: Option<&std::ffi::OsStr>,
) -> Result<(), Box<dyn Error>> {
    if agent != "claude" && agent != "codex" {
        return Err("hook requires claude or codex".into());
    }
    let input = if let Some(value) = payload_argument {
        value
            .to_str()
            .ok_or("hook payload is not UTF-8")?
            .to_owned()
    } else {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    };
    let payload: serde_json::Value = serde_json::from_str(&input)?;
    let session = payload
        .get(if agent == "codex" {
            "thread-id"
        } else {
            "session_id"
        })
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("hook payload has no session ID")?;
    if agent == "codex"
        && payload.get("type").and_then(serde_json::Value::as_str) != Some("agent-turn-complete")
    {
        return Ok(());
    }
    if agent == "claude"
        && payload
            .get("hook_event_name")
            .and_then(serde_json::Value::as_str)
            != Some("Stop")
    {
        return Ok(());
    }
    let sibling = std::env::current_exe()?.with_file_name("limpid-core");
    let executable = if sibling.exists() {
        sibling
    } else {
        PathBuf::from("limpid-core")
    };
    let mut command = Command::new(executable);
    command.args(["autoopen", agent.to_str().ok_or("invalid agent")?]);
    if agent == "codex" {
        command.env("CODEX_THREAD_ID", session);
        if let Some(cwd) = payload.get("cwd").and_then(serde_json::Value::as_str) {
            command.current_dir(cwd);
        }
    }
    let mut child = command
        .stdin(if agent == "claude" {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .spawn()?;
    if agent == "claude" {
        child
            .stdin
            .take()
            .ok_or("core has no stdin")?
            .write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "limpid-core autoopen: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let page = String::from_utf8(output.stdout)?;
    let page = page.trim();
    if page.is_empty() {
        return Ok(());
    }
    let status = Command::new(std::env::current_exe()?)
        .args(["--hook", "--slot", session, page])
        .status()?;
    if !status.success() {
        return Err(format!("limpid could not open hook page: {status}").into());
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
