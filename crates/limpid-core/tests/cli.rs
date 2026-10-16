use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
fn pi_command_writes_page_and_metadata() {
    let temp = std::env::temp_dir().join(format!("limpid-pi-cli-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .args(["pi", "0"])
        .env("XDG_CACHE_HOME", &temp)
        .env("XDG_CONFIG_HOME", &temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "model": "test",
        "session_id": "pi-1",
        "branch": [
            {"type": "message", "message": {"role": "user", "content": "pick"}},
            {"type": "message", "message": {"role": "assistant", "content": "## Questions\n\n1. Which?\n   - A"}}
        ]
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("class=\"question-field\""));
    let mut meta = path.clone();
    meta.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta).unwrap()).unwrap();
    assert_eq!(meta["agent"], "pi");
    assert_eq!(meta["session_id"], "pi-1");
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn claude_read_uses_latest_completed_turn_and_ignores_read_invocation() {
    let temp = std::env::temp_dir().join(format!("limpid-claude-read-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .args(["claude", "0"])
        .env("XDG_CACHE_HOME", &temp)
        .env("XDG_CONFIG_HOME", &temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "model": "opus",
        "session_id": "claude-1",
        "messages": [
            {"role": "user", "text": "first"},
            {"role": "assistant", "text": "first answer"},
            {"role": "user", "text": "second"},
            {"role": "assistant", "text": "working"},
            {"role": "user", "text": "", "toolResults": [{"text": "tool output"}]},
            {"role": "assistant", "text": "final answer"},
            {"role": "user", "text": "<command-name>/read</command-name><command-args>0</command-args>"}
        ]
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let html = fs::read_to_string(&page).unwrap();
    assert!(html.contains("final answer"));
    assert!(!html.contains("first answer"));
    assert!(!html.contains("<command-name>/read"));
    let mut meta_path = page;
    meta_path.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta_path).unwrap()).unwrap();
    assert_eq!(meta["agent"], "claude");
    assert_eq!(meta["session_id"], "claude-1");
    assert_eq!(meta["turn_spec"], "0");
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn codex_command_selects_fixture_turn_and_records_source() {
    let temp = std::env::temp_dir().join(format!("limpid-codex-cli-{}", std::process::id()));
    let sessions = temp.join("sessions/2026/10/15");
    fs::create_dir_all(&sessions).unwrap();
    let source = sessions.join("rollout-2026-10-15T10-00-00-x.jsonl");
    fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test/fixtures/codex/simple.jsonl"
        ),
        &source,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .args(["codex", "0"])
        .env("CODEX_HOME", &temp)
        .env("CODEX_THREAD_ID", "x")
        .env("XDG_CACHE_HOME", &temp)
        .env("XDG_CONFIG_HOME", &temp)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("second answer"));
    assert!(!html.contains("first answer"));
    let mut meta = path.clone();
    meta.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta).unwrap()).unwrap();
    assert_eq!(meta["agent"], "codex");
    assert_eq!(meta["turn_spec"], "0");
    assert_eq!(meta["source"], source.to_string_lossy().as_ref());
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn invalid_utf8_file_is_rejected_before_rendering() {
    let temp = std::env::temp_dir().join(format!("limpid-utf8-cli-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let source = temp.join("invalid.md");
    fs::write(&source, b"# Title\n\xff").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .arg("render")
        .arg(&source)
        .env("XDG_CACHE_HOME", &temp)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("UTF-8"));
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn render_command_writes_cached_page_with_shared_assets() {
    let temp = std::env::temp_dir().join(format!("limpid-render-cli-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let source = temp.join("notes.md");
    fs::write(
        &source,
        "# Notes\n\n## Questions\n\n1. Which?\n   - SQLite\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .arg("render")
        .arg(&source)
        .env("XDG_CACHE_HOME", &temp)
        .env("XDG_CONFIG_HOME", &temp)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let html = fs::read_to_string(page).unwrap();
    assert!(html.contains("class=\"question-field\""));
    assert!(html.contains("rel=\"stylesheet\" href=\"assets/"));
    assert!(temp.join("limpid/assets").is_dir());
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn pi_autoopen_only_writes_a_page_for_enabled_questions() {
    let temp = std::env::temp_dir().join(format!("limpid-autoopen-cli-{}", std::process::id()));
    let config = temp.join("limpid");
    fs::create_dir_all(&config).unwrap();
    let input = |response: &str| {
        serde_json::json!({
            "session_id": "pi-autoopen",
            "branch": [
                {"type": "message", "message": {"role": "user", "content": "pick"}},
                {"type": "message", "message": {"role": "assistant", "content": response}}
            ]
        })
        .to_string()
    };
    let run = |body: String| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
            .args(["autoopen", "pi"])
            .env("XDG_CACHE_HOME", temp.join("cache"))
            .env("XDG_CONFIG_HOME", &temp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(body.as_bytes()) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    };
    let response = "## Questions\n\n1. Which?\n   - SQLite";
    let disabled = run(input(response));
    assert!(disabled.status.success());
    assert!(disabled.stdout.is_empty());
    assert!(!temp.join("cache/limpid").exists());

    fs::write(config.join("config.toml"), "[autoopen]\npi = true\n").unwrap();
    let no_questions = run(input("## Questions\n\nNothing to decide."));
    assert!(no_questions.status.success());
    assert!(no_questions.stdout.is_empty());
    let unrelated_question = run(input("What should I do next?"));
    assert!(unrelated_question.status.success());
    assert!(unrelated_question.stdout.is_empty());

    let opened = run(input(response));
    assert!(opened.status.success());
    let path = PathBuf::from(String::from_utf8(opened.stdout).unwrap().trim());
    assert!(path.exists());
    let mut meta = path.clone();
    meta.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta).unwrap()).unwrap();
    assert_eq!(meta["session_id"], "pi-autoopen");
    assert_eq!(meta["turn_spec"], "0");
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn codex_autoopen_writes_only_enabled_question_turns() {
    let temp = std::env::temp_dir().join(format!("limpid-codex-autoopen-{}", std::process::id()));
    let sessions = temp.join("sessions/2026/10/15");
    let config = temp.join("config/limpid");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&config).unwrap();
    let source = sessions.join("rollout-2026-10-15T10-00-00-x.jsonl");
    let fixture = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/codex/simple.jsonl"
    ))
    .unwrap()
    .replace(
        "second answer",
        "## Questions\\n\\n1. Which database?\\n   - SQLite",
    );
    fs::write(&source, fixture).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_limpid-core"))
            .args(["autoopen", "codex"])
            .env("CODEX_HOME", &temp)
            .env("CODEX_THREAD_ID", "x")
            .env("XDG_CACHE_HOME", temp.join("cache"))
            .env("XDG_CONFIG_HOME", temp.join("config"))
            .output()
            .unwrap()
    };
    let disabled = run();
    assert!(disabled.status.success());
    assert!(disabled.stdout.is_empty());

    fs::write(config.join("config.toml"), "[autoopen]\ncodex = true\n").unwrap();
    let opened = run();
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let page = PathBuf::from(String::from_utf8(opened.stdout).unwrap().trim());
    assert!(
        fs::read_to_string(&page)
            .unwrap()
            .contains("class=\"question-field\"")
    );
    let mut meta_path = page;
    meta_path.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta_path).unwrap()).unwrap();
    assert_eq!(meta["agent"], "codex");
    assert_eq!(meta["session_id"], "x");
    assert_eq!(meta["source"], source.to_string_lossy().as_ref());

    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn claude_stop_payload_opens_only_answer_fields() {
    let temp = std::env::temp_dir().join(format!("limpid-claude-stop-{}", std::process::id()));
    let config = temp.join("limpid");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "[autoopen]\nclaude = true\n").unwrap();
    let run = |message: &str| {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "claude-stop-1",
            "transcript_path": "/tmp/claude-stop-1.jsonl",
            "last_assistant_message": message
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
            .args(["autoopen", "claude"])
            .env("XDG_CONFIG_HOME", &temp)
            .env("XDG_CACHE_HOME", temp.join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let no_questions = run("## Questions\n\nI have none.");
    assert!(no_questions.status.success());
    assert!(no_questions.stdout.is_empty());
    let opened = run("## Questions\n\n1. Continue?\n   - Yes");
    assert!(opened.status.success());
    let page = PathBuf::from(String::from_utf8(opened.stdout).unwrap().trim());
    assert!(fs::read_to_string(&page).unwrap().contains("Continue?"));
    let mut meta = page.clone();
    meta.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta).unwrap()).unwrap();
    assert_eq!(meta["session_id"], "claude-stop-1");
    assert_eq!(meta["source"], "/tmp/claude-stop-1.jsonl");
    fs::remove_dir_all(temp).unwrap();
}
