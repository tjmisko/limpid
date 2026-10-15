use limpid_core::{AGENT_INSTRUCTIONS, render};
use serde::Serialize;
use serde_json::Value;
use std::{
    error::Error,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("limpid-core: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("instructions") if args.len() == 1 => println!("{AGENT_INSTRUCTIONS}"),
        Some("render") if args.len() == 2 => {
            let path = Path::new(&args[1]);
            let source = std::fs::read_to_string(path)?;
            let settings = limpid_core::settings::Settings::load_default()?;
            let rendered = render::render_with_questions(
                &source,
                render::RenderMode::File,
                &settings.questions.headings,
            );
            let html = render::page(&rendered);
            let cache = cache_dir()?;
            std::fs::create_dir_all(&cache)?;
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("page");
            let destination = cache.join(format!("{name}-{}.html", std::process::id()));
            std::fs::write(&destination, render::externalize_assets(&html, &cache)?)?;
            println!("{}", destination.display());
        }
        Some("send") if args.len() == 1 => send()?,
        Some("codex") if args.len() <= 2 => {
            codex(args.get(1).and_then(|arg| arg.to_str()).unwrap_or(""))?
        }
        Some("claude") if args.len() <= 2 => stdin_agent(
            "claude",
            args.get(1).and_then(|arg| arg.to_str()).unwrap_or(""),
        )?,
        Some("pi") if args.len() <= 2 => {
            stdin_agent("pi", args.get(1).and_then(|arg| arg.to_str()).unwrap_or(""))?
        }
        Some("grammars") => grammars(&args[1..])?,
        _ => {
            return Err(
                "usage: limpid-core render FILE.md | claude [spec] | codex [spec] | pi [spec] | grammars list|sync | instructions | send".into(),
            );
        }
    }
    Ok(())
}

fn grammars(args: &[std::ffi::OsString]) -> Result<(), Box<dyn Error>> {
    match args.first().and_then(|arg| arg.to_str()) {
        Some("list") if args.len() == 1 => print!(
            "{}",
            limpid_core::grammars::list(&limpid_core::grammars::grammar_dir()?)?
        ),
        Some("sync") if args.len() == 1 => {
            let settings = limpid_core::settings::Settings::load_default()?;
            limpid_core::grammars::sync(
                &settings.highlight.grammars,
                &limpid_core::grammars::grammar_dir()?,
            )?;
        }
        Some("generate") if args.len() == 3 => {
            let source = std::fs::read_to_string(&args[1])?;
            let catalog = limpid_core::grammars::generate(&source)?;
            std::fs::write(&args[2], toml::to_string_pretty(&catalog)?)?;
        }
        _ => {
            return Err(
                "usage: limpid-core grammars list | sync | generate HELIX_LANGUAGES_TOML OUTPUT"
                    .into(),
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PageMeta {
    agent: &'static str,
    session_id: Option<String>,
    turn_spec: String,
    source: Option<PathBuf>,
}

fn codex(spec: &str) -> Result<(), Box<dyn Error>> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?);
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("sessions");
    let thread = std::env::var("CODEX_THREAD_ID")
        .ok()
        .or_else(|| std::env::var("CODEX_SESSION_ID").ok());
    let source =
        limpid_core::codex::find_rollout(&root, thread.as_deref(), &std::env::current_dir()?)
            .ok_or("no Codex rollout found for this session or directory")?;
    let turns = limpid_core::codex::parse_rollout(&std::fs::read_to_string(&source)?);
    write_agent_page(
        "codex",
        &turns.turns,
        turns.model.as_deref(),
        spec,
        thread,
        Some(source),
    )
}

fn stdin_agent(agent: &'static str, spec: &str) -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let value: Value = serde_json::from_str(&input)?;
    let model = value.get("model").and_then(Value::as_str);
    let session = value
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let turns = match agent {
        "pi" => limpid_core::pi::parse_branch(
            value
                .get("branch")
                .and_then(Value::as_array)
                .ok_or("pi input requires a branch array")?,
        ),
        "claude" => {
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .ok_or("claude input requires a messages array")?;
            let messages = messages
                .iter()
                .filter_map(|message| {
                    let role = match message.get("role")?.as_str()? {
                        "user" => limpid_core::claude::Role::User,
                        "assistant" => limpid_core::claude::Role::Assistant,
                        _ => return None,
                    };
                    Some(limpid_core::claude::Message {
                        role,
                        text: message.get("text")?.as_str()?,
                    })
                })
                .collect::<Vec<_>>();
            limpid_core::claude::collect_turns(&messages)
        }
        _ => return Err("unsupported agent".into()),
    };
    write_agent_page(agent, &turns, model, spec, session, source)
}

fn write_agent_page(
    agent: &'static str,
    turns: &[limpid_core::codex::Turn],
    model: Option<&str>,
    spec: &str,
    session_id: Option<String>,
    source: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let settings = limpid_core::settings::Settings::load_default()?;
    let page = limpid_core::agent_page::build(turns, spec, model, &settings.questions.headings)?;
    let cache = cache_dir()?;
    std::fs::create_dir_all(&cache)?;
    let filename = format!(
        "{agent}-{}-{}.html",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let destination = cache.join(filename);
    std::fs::write(
        &destination,
        render::externalize_assets(&page.html, &cache)?,
    )?;
    let mut meta_path = destination.clone();
    meta_path.set_extension("meta.json");
    std::fs::write(
        meta_path,
        serde_json::to_vec_pretty(&PageMeta {
            agent,
            session_id,
            turn_spec: spec.to_owned(),
            source,
        })?,
    )?;
    println!("{}", destination.display());
    prune_agent_pages(&cache)?;
    Ok(())
}

fn prune_agent_pages(cache: &Path) -> Result<(), Box<dyn Error>> {
    let mut pages = std::fs::read_dir(cache)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|extension| extension.to_str()) == Some("html")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        ["codex-", "claude-", "pi-"]
                            .iter()
                            .any(|prefix| name.starts_with(prefix))
                    })
        })
        .collect::<Vec<_>>();
    pages.sort_by_key(|path| {
        std::cmp::Reverse(
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok(),
        )
    });
    for page in pages.into_iter().skip(50) {
        std::fs::remove_file(&page)?;
        for extension in ["meta.json", "replies.json"] {
            let mut associated = page.clone();
            associated.set_extension(extension);
            if associated.exists() {
                std::fs::remove_file(associated)?;
            }
        }
    }
    Ok(())
}

fn send() -> Result<(), Box<dyn Error>> {
    let mut message = String::new();
    std::io::stdin().read_to_string(&mut message)?;
    let message = message.trim_end_matches(['\r', '\n']);
    if message.is_empty() {
        return Err("reply message is empty".into());
    }
    let pane = std::env::var("LIMPID_PANE")
        .ok()
        .filter(|pane| !pane.is_empty());
    let mut command = if let Some(pane) = pane {
        let mut command = Command::new("wezterm");
        command.args(["cli", "send-text", "--pane-id", &pane]);
        command
    } else {
        Command::new("wl-copy")
    };
    let mut child = command.stdin(Stdio::piped()).spawn()?;
    child
        .stdin
        .take()
        .ok_or("reply command has no stdin")?
        .write_all(message.as_bytes())?;
    if !child.wait()?.success() {
        return Err("reply command failed".into());
    }
    Ok(())
}

fn cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(base).join("limpid"));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".cache/limpid"))
}
