use limpid_core::transport::{Handoff, Origin, Scheme, socket_path};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::{Command, Stdio},
    thread::JoinHandle,
    time::{Duration, Instant},
};

fn receive_handoff(runtime: &Path, scheme: Scheme) -> JoinHandle<Option<Handoff>> {
    let socket = socket_path(runtime, scheme);
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).unwrap();
                    if !bytes.is_empty() {
                        return Some(serde_json::from_slice::<Handoff>(&bytes).unwrap());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("socket accept failed: {error}"),
            }
        }
        None
    })
}

#[test]
fn markdown_open_renders_and_hands_off_hook_context() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-front-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("notes.md");
    fs::write(
        &source,
        "# Notes\n\n## Questions\n\n1. Which?\n   - SQLite\n",
    )
    .unwrap();
    let receiver = receive_handoff(&directory, Scheme::Light);

    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["--hook", "--slot", "session-1", "--light"])
        .arg(&source)
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .env("WEZTERM_PANE", "4242")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = receiver.join().unwrap().expect("no handoff received");
    assert_eq!(request.slot.as_deref(), Some("session-1"));
    assert_eq!(request.origin, Origin::Hook);
    assert_eq!(request.theme, Scheme::Light);
    assert_eq!(request.pane.as_deref(), Some("4242"));
    assert_eq!(request.source, Some(source.canonicalize().unwrap()));
    assert!(request.source_hash.is_some());
    assert!(request.path.starts_with(&directory));
    assert!(
        fs::read_to_string(&request.path)
            .unwrap()
            .contains("class=\"question-field\"")
    );

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn export_writes_self_contained_page_without_a_display() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-export-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("notes.md");
    let output_path = directory.join("export.html");
    fs::write(&source, "# Exported page\n\nSome text.\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--out")
        .arg(&output_path)
        .arg(&source)
        .env("XDG_CONFIG_HOME", &directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&output_path).unwrap();
    assert!(html.contains("Exported page"));
    assert!(html.contains("<style>"));
    assert!(html.contains("<script>"));
    assert!(!html.contains("href=\"assets/"));

    let existing = directory.join("existing.html");
    let copied = directory.join("copied.html");
    fs::write(&existing, "<!doctype html><title>Existing</title>").unwrap();
    let copy = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--out")
        .arg(&copied)
        .arg(&existing)
        .env("XDG_CONFIG_HOME", &directory)
        .output()
        .unwrap();
    assert!(copy.status.success());
    assert_eq!(fs::read(&copied).unwrap(), fs::read(&existing).unwrap());

    let piped = directory.join("piped.html");
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--out")
        .arg(&piped)
        .arg("-")
        .env("XDG_CONFIG_HOME", &directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"# Piped page\n")
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    assert!(fs::read_to_string(&piped).unwrap().contains("Piped page"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn codex_hook_opens_question_turn_in_its_session_slot() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-codex-hook-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let sessions = directory.join("codex/sessions/2026/10/15");
    let config = directory.join("config/limpid");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "[autoopen]\ncodex = true\n").unwrap();
    let source = sessions.join("rollout-2026-10-15T10-00-00-x.jsonl");
    let fixture = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/codex/simple.jsonl"
    ))
    .unwrap()
    .replace("second answer", "## Questions\\n\\n1. Which?\\n   - SQLite");
    fs::write(&source, fixture).unwrap();
    let receiver = receive_handoff(&directory, Scheme::Dark);
    let payload = serde_json::json!({
        "type": "agent-turn-complete",
        "thread-id": "x",
        "cwd": directory,
    });
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["hook", "codex", &payload.to_string()])
        .env("CODEX_HOME", directory.join("codex"))
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .env("WEZTERM_PANE", "4242")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = receiver.join().unwrap().expect("no hook handoff received");
    assert_eq!(request.origin, Origin::Hook);
    assert_eq!(request.slot.as_deref(), Some("x"));
    assert_eq!(request.pane.as_deref(), Some("4242"));
    assert!(
        fs::read_to_string(&request.path)
            .unwrap()
            .contains("class=\"question-field\"")
    );
    let mut meta_path = request.path;
    meta_path.set_extension("meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(meta_path).unwrap()).unwrap();
    assert_eq!(meta["agent"], "codex");
    assert_eq!(meta["session_id"], "x");
    assert_eq!(meta["source"], source.to_string_lossy().as_ref());
    fs::remove_dir_all(directory).unwrap();
}
