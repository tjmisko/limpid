use limpid_core::{AGENT_INSTRUCTIONS, render};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    error::Error,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const KEEP_AGENT_PAGES: usize = 500;

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
        Some("feedback") => feedback(&args[1..])?,
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
        Some("math") => math(&args[1..])?,
        _ => {
            return Err(
                "usage: limpid-core render FILE.md | math FILE.md [--json] | claude [spec] | codex [spec] | pi [spec] | feedback [--json] [--session ID [--spec S]] [PAGE] | grammars list|sync | instructions | send".into(),
            );
        }
    }
    Ok(())
}

fn math(args: &[std::ffi::OsString]) -> Result<(), Box<dyn Error>> {
    let mut json = false;
    let mut file = None;
    for arg in args {
        match arg.to_str() {
            Some("--json") => json = true,
            Some(option) if option.starts_with("--") => {
                return Err(format!("unknown option: {option}").into());
            }
            _ if file.is_some() => return Err("only one file is allowed".into()),
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let file = file.ok_or("usage: limpid-core math FILE.md [--json]")?;
    let source = std::fs::read_to_string(&file)?;
    let settings = load_math_settings()?;
    settings.validate()?;
    let formulas = limpid_core::math::collect(&source);
    let report = limpid_core::math::render_missing(&formulas, &settings);
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!(
            "{} formulas: {} rendered, {} cached, {} failed",
            formulas.len(),
            report.rendered,
            report.cached,
            report.failed.len()
        );
        for (hash, reason) in &report.failed {
            eprintln!("{hash}: {reason}");
        }
    }
    if report.failed.is_empty() {
        return Ok(());
    }
    std::process::exit(2);
}

fn load_math_settings() -> Result<limpid_core::math::MathSettings, Box<dyn Error>> {
    let dir = limpid_core::settings::Settings::config_dir()
        .ok_or("HOME or XDG_CONFIG_HOME is required")?;
    match std::fs::read_to_string(dir.join("config.toml")) {
        Ok(source) => Ok(limpid_core::math::MathSettings::from_config_source(
            &source,
        )?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(Box::new(error)),
    }
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

#[derive(Serialize, Deserialize)]
struct PageMeta {
    agent: String,
    session_id: Option<String>,
    turn_spec: String,
    source: Option<PathBuf>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    feedback_sock: Option<String>,
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
        None,
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
        .or_else(|| value.get("transcript_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let feedback_sock = value
        .get("feedback_sock")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let turns = match agent {
        "pi" => limpid_core::pi::parse_branch(
            value
                .get("branch")
                .and_then(Value::as_array)
                .ok_or("pi input requires a branch array")?,
        ),
        "claude" => {
            if let Some(response) = value.get("last_assistant_message").and_then(Value::as_str) {
                vec![limpid_core::codex::Turn {
                    prompt: String::new(),
                    response: response.to_owned(),
                }]
            } else {
                let messages = value
                    .get("messages")
                    .and_then(Value::as_array)
                    .ok_or("claude input requires a messages array or last_assistant_message")?;
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
        }
        _ => return Err("unsupported agent".into()),
    };
    write_agent_page(agent, &turns, model, spec, session, source, feedback_sock)
}

fn write_agent_page(
    agent: &'static str,
    turns: &[limpid_core::codex::Turn],
    model: Option<&str>,
    spec: &str,
    session_id: Option<String>,
    source: Option<PathBuf>,
    feedback_sock: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let settings = limpid_core::settings::Settings::load_default()?;
    let page = limpid_core::agent_page::build(turns, spec, model, &settings.questions.headings)?;
    let cache = cache_dir()?;
    std::fs::create_dir_all(&cache)?;
    let filename = page_filename(agent, session_id.as_deref(), spec, &page.content_hash);
    let destination = cache.join(filename);
    std::fs::write(
        &destination,
        render::externalize_assets(&page.html, &cache)?,
    )?;
    // The native viewer opens this; its replies sidecar is keyed by the same
    // page stem as the HTML page's.
    let mut markdown_path = destination.clone();
    markdown_path.set_extension("md");
    std::fs::write(&markdown_path, &page.markdown)?;
    let mut meta_path = destination.clone();
    meta_path.set_extension("meta.json");
    std::fs::write(
        meta_path,
        serde_json::to_vec_pretty(&PageMeta {
            agent: agent.to_owned(),
            session_id,
            turn_spec: spec.to_owned(),
            source,
            title: page.title.clone(),
            feedback_sock,
        })?,
    )?;
    println!("{}", destination.display());
    prune_agent_pages(&cache, &destination)?;
    Ok(())
}

fn page_filename(agent: &str, session: Option<&str>, spec: &str, content_hash: &str) -> String {
    let joined = format!("{agent}\0{}\0{spec}\0{content_hash}", session.unwrap_or(""));
    let digest = blake3::hash(joined.as_bytes()).to_hex().to_string();
    format!("{agent}-{}.html", &digest[..16])
}

fn prune_agent_pages(cache: &Path, preserve: &Path) -> Result<(), Box<dyn Error>> {
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
    let (protected, unprotected): (Vec<_>, Vec<_>) =
        pages.into_iter().partition(|page| page_has_feedback(page));
    let keep = KEEP_AGENT_PAGES.saturating_sub(protected.len());
    for page in unprotected.into_iter().skip(keep) {
        if page == preserve {
            continue;
        }
        std::fs::remove_file(&page)?;
        for extension in ["meta.json", "md", "replies.json"] {
            let mut associated = page.clone();
            associated.set_extension(extension);
            if associated.exists() {
                std::fs::remove_file(associated)?;
            }
        }
    }
    Ok(())
}

/// Annotations of an agent page: those embedded in the HTML plus those the
/// native viewer saved to the `<page>.replies.json` sidecar. The sidecar is
/// newer, so its entries win.
fn agent_page_annotations(
    page: &Path,
) -> Result<limpid_core::replies::AgentReplies, Box<dyn Error>> {
    let mut merged = limpid_core::replies::read_annotations_file(page)?;
    let sidecar =
        limpid_core::replies::AgentReplies::load(&limpid_core::replies::sidecar_path(page))?;
    merged.replies.extend(sidecar.replies);
    merged.comments.extend(sidecar.comments);
    Ok(merged)
}

fn page_has_feedback(page: &Path) -> bool {
    match agent_page_annotations(page) {
        Ok(replies) => !replies.replies.is_empty() || !replies.comments.is_empty(),
        Err(_) => false,
    }
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

#[derive(Debug, PartialEq)]
struct FeedbackArgs {
    json: bool,
    session: Option<String>,
    spec: Option<String>,
    page: Option<PathBuf>,
}

fn parse_feedback_args(args: &[std::ffi::OsString]) -> Result<FeedbackArgs, String> {
    let mut parsed = FeedbackArgs {
        json: false,
        session: None,
        spec: None,
        page: None,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.to_str() {
            Some("--json") => parsed.json = true,
            Some("--session") => {
                let value = rest.next().ok_or("--session requires an id")?;
                parsed.session = Some(value.to_string_lossy().into_owned());
            }
            Some("--spec") => {
                let value = rest.next().ok_or("--spec requires a value")?;
                parsed.spec = Some(value.to_string_lossy().into_owned());
            }
            Some(value) if value.starts_with("--") => {
                return Err(format!("unknown option: {value}"));
            }
            _ => {
                if parsed.page.is_some() {
                    return Err("only one page is allowed".into());
                }
                parsed.page = Some(PathBuf::from(arg));
            }
        }
    }
    if parsed.session.is_none() && parsed.page.is_none() {
        return Err("feedback requires a page or --session".into());
    }
    if parsed.session.is_some() && parsed.page.is_some() {
        return Err("a page cannot be combined with --session".into());
    }
    Ok(parsed)
}

fn feedback(args: &[std::ffi::OsString]) -> Result<(), Box<dyn Error>> {
    let parsed = parse_feedback_args(args)?;
    let page = if let Some(session) = &parsed.session {
        resolve_session_page(session, parsed.spec.as_deref())?
    } else {
        parsed.page.ok_or("feedback requires a page or --session")?
    };
    let is_agent_markdown = page.extension().and_then(|extension| extension.to_str()) == Some("md")
        && page.with_extension("meta.json").exists();
    let page = if is_agent_markdown {
        page.with_extension("html")
    } else {
        page
    };
    let (title, agent, context, replies) =
        if page.extension().and_then(|extension| extension.to_str()) == Some("md") {
            let source = std::fs::read_to_string(&page)?;
            let title = page
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                title,
                "file".to_owned(),
                "file".to_owned(),
                limpid_core::replies::from_file_annotations(&source),
            )
        } else {
            let mut meta_path = page.clone();
            meta_path.set_extension("meta.json");
            let meta: PageMeta = serde_json::from_slice(&std::fs::read(&meta_path)?)?;
            let spec = if meta.turn_spec.is_empty() {
                "0".to_owned()
            } else {
                meta.turn_spec.clone()
            };
            let title = if meta.title.is_empty() {
                page.file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                meta.title.clone()
            };
            let context = if meta.agent == "file" {
                "file".to_owned()
            } else {
                format!("turn {spec}")
            };
            (
                title,
                meta.agent.clone(),
                context,
                agent_page_annotations(&page)?,
            )
        };
    if parsed.json {
        println!("{}", serde_json::to_string(&replies)?);
    } else if replies.replies.is_empty() && replies.comments.is_empty() {
        println!("No feedback for {}.", page.display());
    } else {
        println!(
            "{}",
            limpid_core::replies::compose_message(&title, &agent, &context, &replies)
        );
    }
    Ok(())
}

fn page_for_meta(meta: &Path) -> PathBuf {
    let name = meta
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".meta.json").unwrap_or(&name);
    meta.with_file_name(format!("{stem}.html"))
}

fn resolve_session_page(session: &str, spec: Option<&str>) -> Result<PathBuf, Box<dyn Error>> {
    let cache = cache_dir()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&cache)?.flatten() {
        let meta_path = entry.path();
        let Some(name) = meta_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".meta.json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<PageMeta>(&bytes) else {
            continue;
        };
        if meta.session_id.as_deref() != Some(session) {
            continue;
        }
        if let Some(spec) = spec
            && meta.turn_spec != spec
        {
            continue;
        }
        let modified = std::fs::metadata(&meta_path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
            newest = Some((modified, page_for_meta(&meta_path)));
        }
    }
    newest
        .map(|(_, page)| page)
        .ok_or_else(|| format!("no page found for session {session}").into())
}

fn cache_dir() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(base).join("limpid"));
    }
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".cache/limpid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_filename_is_deterministic_and_separates_sessions() {
        let name = page_filename("pi", Some("s1"), "0", "abc");
        assert_eq!(name, page_filename("pi", Some("s1"), "0", "abc"));
        assert!(name.starts_with("pi-"));
        assert!(name.ends_with(".html"));
        assert_eq!(name.len(), "pi-".len() + 16 + ".html".len());
        assert_ne!(name, page_filename("pi", Some("s2"), "0", "abc"));
        assert_ne!(name, page_filename("pi", Some("s1"), "1", "abc"));
        assert_ne!(name, page_filename("pi", Some("s1"), "0", "def"));
        assert_ne!(name, page_filename("claude", Some("s1"), "0", "abc"));
    }

    #[test]
    fn should_remove_the_markdown_sidecar_when_pruning_an_old_page() {
        use std::fs::{File, FileTimes};
        let cache = std::env::temp_dir().join(format!("limpid-prune-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for index in 0..(KEEP_AGENT_PAGES as u64 + 1) {
            let page = cache.join(format!("pi-{index:03}.html"));
            File::create(&page).unwrap();
            File::create(page.with_extension("md")).unwrap();
            File::open(&page)
                .unwrap()
                .set_times(
                    FileTimes::new().set_modified(base + std::time::Duration::from_secs(index)),
                )
                .unwrap();
        }
        let newest = cache.join(format!("pi-{:03}.html", KEEP_AGENT_PAGES));
        prune_agent_pages(&cache, &newest).unwrap();
        assert!(!cache.join("pi-000.html").exists());
        assert!(
            !cache.join("pi-000.md").exists(),
            "the sidecar goes with its page"
        );
        assert!(
            cache
                .join(format!("pi-{:03}.md", KEEP_AGENT_PAGES))
                .exists()
        );
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn pruning_keeps_pages_with_feedback() {
        use std::fs::{File, FileTimes};
        let cache = std::env::temp_dir().join(format!("limpid-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for index in 0..(KEEP_AGENT_PAGES as u64 + 5) {
            let page = cache.join(format!("pi-{index:02}.html"));
            File::create(&page).unwrap();
            File::open(&page)
                .unwrap()
                .set_times(
                    FileTimes::new().set_modified(base + std::time::Duration::from_secs(index)),
                )
                .unwrap();
        }
        let protected = cache.join("pi-00.html");
        std::fs::write(
            &protected,
            r#"<html><body><script type="application/json" id="limpid-annotations">{"replies":{"q1":"keep"},"comments":{}}</script></body></html>"#,
        )
        .unwrap();
        let corrupt = cache.join("pi-02.html");
        std::fs::write(&corrupt, b"<html>not json</html>").unwrap();
        File::open(&protected)
            .unwrap()
            .set_times(FileTimes::new().set_modified(base))
            .unwrap();
        File::open(&corrupt)
            .unwrap()
            .set_times(FileTimes::new().set_modified(base + std::time::Duration::from_secs(2)))
            .unwrap();

        let newest = cache.join(format!("pi-{:02}.html", KEEP_AGENT_PAGES + 4));
        prune_agent_pages(&cache, &newest).unwrap();

        let pages = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("html"))
            .count();
        assert_eq!(pages, KEEP_AGENT_PAGES);
        assert!(cache.join("pi-00.html").exists());
        assert!(protected.exists());
        assert!(!cache.join("pi-01.html").exists());
        assert!(!cache.join("pi-02.html").exists());
        assert!(cache.join("pi-06.html").exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }

    fn write_agent_page_set(cache: &Path, name: &str, sidecar: Option<&str>) -> PathBuf {
        let page = cache.join(format!("{name}.html"));
        std::fs::write(&page, "<html><body></body></html>").unwrap();
        std::fs::write(page.with_extension("md"), "# Q\n").unwrap();
        std::fs::write(
            page.with_extension("meta.json"),
            r#"{"agent":"claude","turn_spec":"2","title":"Pick a store"}"#,
        )
        .unwrap();
        if let Some(sidecar) = sidecar {
            std::fs::write(page.with_extension("replies.json"), sidecar).unwrap();
        }
        page
    }

    #[test]
    fn should_read_view_saved_replies_when_the_page_has_a_sidecar() {
        let cache = std::env::temp_dir().join(format!("limpid-side-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let page =
            write_agent_page_set(&cache, "claude-abc", Some(r#"{"replies":{"q1":"SQLite"}}"#));
        let from_html = agent_page_annotations(&page).unwrap();
        assert_eq!(
            from_html.replies.get("q1").map(String::as_str),
            Some("SQLite")
        );
        assert!(page_has_feedback(&page));
        let args = vec![page.with_extension("md").into_os_string(), "--json".into()];
        feedback(&args).unwrap();
        let args = vec![page.clone().into_os_string(), "--json".into()];
        feedback(&args).unwrap();
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn should_prefer_the_sidecar_when_html_and_sidecar_answer_the_same_question() {
        let cache = std::env::temp_dir().join(format!("limpid-side2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let page = write_agent_page_set(&cache, "claude-def", Some(r#"{"replies":{"q1":"new"}}"#));
        std::fs::write(
            &page,
            r#"<html><body><script type="application/json" id="limpid-annotations">{"replies":{"q1":"old","q2":"kept"},"comments":{}}</script></body></html>"#,
        )
        .unwrap();
        let merged = agent_page_annotations(&page).unwrap();
        assert_eq!(merged.replies["q1"], "new");
        assert_eq!(merged.replies["q2"], "kept");
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn should_keep_and_clean_sidecars_when_pruning_agent_pages() {
        use std::fs::{File, FileTimes};
        let cache = std::env::temp_dir().join(format!("limpid-side3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let answered =
            write_agent_page_set(&cache, "claude-000", Some(r#"{"replies":{"q1":"keep"}}"#));
        let blank = write_agent_page_set(&cache, "claude-001", Some(r#"{}"#));
        File::open(&answered)
            .unwrap()
            .set_times(FileTimes::new().set_modified(base))
            .unwrap();
        File::open(&blank)
            .unwrap()
            .set_times(FileTimes::new().set_modified(base + std::time::Duration::from_secs(1)))
            .unwrap();
        let mut newest = blank.clone();
        for index in 0..KEEP_AGENT_PAGES {
            newest = write_agent_page_set(&cache, &format!("claude-1{index:02}"), None);
            File::open(&newest)
                .unwrap()
                .set_times(
                    FileTimes::new()
                        .set_modified(base + std::time::Duration::from_secs(10 + index as u64)),
                )
                .unwrap();
        }
        prune_agent_pages(&cache, &newest).unwrap();
        assert!(answered.exists());
        assert!(answered.with_extension("replies.json").exists());
        assert!(!blank.exists());
        assert!(
            !blank.with_extension("replies.json").exists(),
            "no orphan sidecar"
        );
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn pruning_keeps_the_new_page_when_feedback_fills_the_cache() {
        use std::fs::File;
        let cache = std::env::temp_dir().join(format!("limpid-prune-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        for index in 0..KEEP_AGENT_PAGES as u64 {
            let page = cache.join(format!("pi-{index:02}.html"));
            std::fs::write(
                &page,
                r#"<html><body><script type="application/json" id="limpid-annotations">{"replies":{"q1":"keep"},"comments":{}}</script></body></html>"#,
            )
            .unwrap();
        }
        let fresh = cache.join("pi-fresh.html");
        File::create(&fresh).unwrap();
        prune_agent_pages(&cache, &fresh).unwrap();
        assert!(fresh.exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn feedback_arguments_are_parsed_and_validated() {
        use std::ffi::OsString;
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            parse_feedback_args(&args(&["a.html"])).unwrap(),
            FeedbackArgs {
                json: false,
                session: None,
                spec: None,
                page: Some(PathBuf::from("a.html")),
            }
        );
        assert_eq!(
            parse_feedback_args(&args(&["--json", "--session", "s", "--spec", "0"])).unwrap(),
            FeedbackArgs {
                json: true,
                session: Some("s".into()),
                spec: Some("0".into()),
                page: None,
            }
        );
        for bad in [
            vec![],
            vec!["--session", "s", "b.html"],
            vec!["a.html", "b.html"],
            vec!["--session"],
            vec!["--nope"],
        ] {
            assert!(parse_feedback_args(&args(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn page_meta_reads_old_files_without_new_fields() {
        let old: PageMeta = serde_json::from_str(
            r#"{"agent":"pi","session_id":"s","turn_spec":"0","source":"/tmp/a.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(old.agent, "pi");
        assert!(old.title.is_empty());
        assert!(old.feedback_sock.is_none());
        let new: PageMeta = serde_json::from_str(
            r#"{"agent":"pi","session_id":null,"turn_spec":"0","source":null,"title":"T","feedback_sock":"/run/s.sock"}"#,
        )
        .unwrap();
        assert_eq!(new.title, "T");
        assert_eq!(new.feedback_sock.as_deref(), Some("/run/s.sock"));
    }
}
