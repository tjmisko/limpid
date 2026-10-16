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
            "/tests/fixtures/codex/simple.jsonl"
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
fn pi_page_path_is_stable_and_session_specific() {
    let temp = std::env::temp_dir().join(format!("limpid-identity-cli-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let run = |session: &str, extra: bool| -> PathBuf {
        let mut branch = vec![
            serde_json::json!({"type": "message", "message": {"role": "user", "content": "pick"}}),
            serde_json::json!({"type": "message", "message": {"role": "assistant", "content": "## Questions\n\n1. Which?\n   - A"}}),
        ];
        if extra {
            branch.push(serde_json::json!({"type": "message", "message": {"role": "user", "content": "again"}}));
            branch.push(serde_json::json!({"type": "message", "message": {"role": "assistant", "content": "## Questions\n\n1. Which?\n   - B"}}));
        }
        let input = serde_json::json!({"model": "test", "session_id": session, "branch": branch});
        let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
            .args(["pi", "0"])
            .env("XDG_CACHE_HOME", &temp)
            .env("XDG_CONFIG_HOME", &temp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&input).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
    };
    let first = run("s1", false);
    assert_eq!(first, run("s1", false));
    assert_ne!(first, run("s2", false));
    assert_ne!(first, run("s1", true));
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
