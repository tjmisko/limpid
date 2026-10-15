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
