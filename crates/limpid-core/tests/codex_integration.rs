use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct Harness {
    root: PathBuf,
}
impl Harness {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("limpid-codex-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_limpid-core"),
            root.join("bin/limpid-core"),
        )
        .unwrap();
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/codex/simple.jsonl"
            ),
            root.join("source.jsonl"),
        )
        .unwrap();
        Self { root }
    }
    fn script(&self, name: &str, body: &str) {
        let path = self.root.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(self.root.join("bin/limpid-core"));
        cmd.env("XDG_CACHE_HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.root)
            .env("CODEX_HOME", &self.root)
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("LIMPID_TEST_ARGS", self.root.join("args"));
        cmd
    }
    fn hook(&self, prompt: &str, session: &str) -> Output {
        let event = json!({"hook_event_name":"UserPromptSubmit", "session_id":session,
            "transcript_path":self.root.join("source.jsonl"), "cwd": self.root, "prompt":prompt});
        let mut child = self
            .command()
            .arg("codex-hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn hook_opens_exact_session_and_consumes_read_without_a_model() {
    let h = Harness::new("hook-open");
    h.script("limpid", "printf '%s\\n' \"$@\" > \"$LIMPID_TEST_ARGS\"");
    for (spec, expected) in [
        ("/read", "second answer"),
        ("$read", "second answer"),
        ("limpid-read", "second answer"),
        ("/read 1", "first answer"),
        ("/read 0..2", "first answer"),
    ] {
        let output = h.hook(spec, "x");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["decision"], "block");
        assert!(
            result["reason"].as_str().unwrap().contains("Opened Limpid"),
            "{result}"
        );
        let argv = fs::read_to_string(h.root.join("args")).unwrap();
        let mut argv = argv.lines();
        assert_eq!(argv.next(), Some("--source-backed"));
        let page = PathBuf::from(argv.next().unwrap());
        assert!(
            fs::read_to_string(page.with_extension("md"))
                .unwrap()
                .contains(expected)
        );
        let meta: Value =
            serde_json::from_slice(&fs::read(page.with_extension("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["session_id"], "x");
        assert_eq!(
            meta["source"],
            h.root.join("source.jsonl").to_str().unwrap()
        );
    }
}

#[test]
fn hook_ignores_normal_prompts_and_blocks_invalid_or_unreadable_requests() {
    let h = Harness::new("hook-failure");
    let normal = h.hook("Please read this code", "x");
    assert!(normal.status.success());
    assert!(normal.stdout.is_empty());
    for (prompt, session, reason) in [
        ("/read bad", "x", "Invalid argument"),
        ("/read 0 extra", "x", "usage:"),
        ("/read", "other-thread", "different session"),
    ] {
        let output = h.hook(prompt, session);
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["decision"], "block");
        assert!(
            result["reason"].as_str().unwrap().contains(reason),
            "{result}"
        );
    }
    h.script("limpid", "echo viewer-failed >&2\nexit 1");
    let output = h.hook("/read", "x");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["reason"].as_str().unwrap().contains("viewer-failed"));
    fs::remove_file(h.root.join("source.jsonl")).unwrap();
    let result: Value = serde_json::from_slice(&h.hook("/read", "x").stdout).unwrap();
    assert_eq!(result["decision"], "block");
    assert!(!h.root.join("args").exists());
}

#[test]
fn send_targets_page_session_and_reports_queue_and_resume_failures() {
    let h = Harness::new("send");
    // Shell syntax in feedback must remain literal argv data.
    let message = "q1: yes; $(touch SHOULD_NOT_EXIST)\ncomment: café";
    let run = || {
        let mut child = h
            .command()
            .arg("send")
            .env("LIMPID_AGENT", "codex")
            .env("LIMPID_SESSION", "page-thread")
            .env("LIMPID_PANE", "unrelated-pane")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(message.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    h.script("codex", "printf '%s\\n' \"$@\" > \"$LIMPID_TEST_ARGS\"");
    assert!(run().status.success());
    assert_eq!(
        fs::read_to_string(h.root.join("args")).unwrap(),
        format!("queue\n--thread\npage-thread\n--message\n{message}\n--resume\n")
    );
    h.script("codex", "echo 'warning: could not resume' >&2\nexit 0");
    let output = run();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("do not resend"));
    h.script("codex", "echo 'queue failed' >&2\nexit 1");
    assert!(!run().status.success());
    let mut child = h
        .command()
        .arg("send")
        .env("LIMPID_AGENT", "codex")
        .env_remove("LIMPID_SESSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"answer").unwrap();
    assert!(!child.wait_with_output().unwrap().status.success());
}

#[test]
fn directory_discovery_records_the_actual_session_and_explicit_missing_id_fails() {
    let h = Harness::new("identity");
    let sessions = h.root.join("sessions/2026/01/01");
    fs::create_dir_all(&sessions).unwrap();
    let source = fs::read_to_string(h.root.join("source.jsonl"))
        .unwrap()
        .replace("\"cwd\":\"/work\"", &format!("\"cwd\":{}", json!(h.root)));
    fs::write(sessions.join("rollout-date-x.jsonl"), source).unwrap();
    let output = h
        .command()
        .arg("codex")
        .current_dir(&h.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let meta: Value =
        serde_json::from_slice(&fs::read(page.with_extension("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["session_id"], "x");
    assert!(
        !h.command()
            .args(["codex", "--session", "missing"])
            .current_dir(&h.root)
            .output()
            .unwrap()
            .status
            .success()
    );
}
