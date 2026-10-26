//! Window lifecycle test. It opens a real window briefly (class
//! `limpid-test`, which a compositor rule keeps off the live desktop) and is
//! skipped when no display is available.

use std::{
    fs,
    io::Write,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.md");

fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("limpid-view-window-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Make the real Wayland socket reachable from a private runtime directory.
fn link_wayland(runtime: &Path) {
    let (Some(display), Some(real)) = (
        std::env::var_os("WAYLAND_DISPLAY"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) else {
        return;
    };
    let target = Path::new(&real).join(&display);
    let link = runtime.join(&display);
    if target.exists() && !link.exists() {
        let _ = std::os::unix::fs::symlink(&target, &link);
    }
}

fn wait_for(deadline: Instant, what: &str, mut ready: impl FnMut() -> bool) {
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn send_line(socket: &Path, line: &str) {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
}

fn wait_for_exit(child: &mut Child, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn should_serve_a_handoff_and_exit_when_a_close_request_arrives() {
    if !display_available() {
        return;
    }
    let runtime = temp_dir("lifecycle");
    link_wayland(&runtime);
    let socket = runtime.join("view.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-view"))
        .arg("--socket")
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_CONFIG_HOME", &runtime)
        .env("LIMPID_WINDOW_CLASS", "limpid-test")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    wait_for(
        Instant::now() + Duration::from_secs(5),
        "the socket",
        || socket.exists(),
    );
    send_line(
        &socket,
        &serde_json::json!({"path": FIXTURE, "theme": "dark"}).to_string(),
    );
    thread::sleep(Duration::from_millis(800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the viewer exited before the window was closed"
    );

    send_line(&socket, r#"{"close":true}"#);
    let exited = wait_for_exit(&mut child, Duration::from_secs(5));
    if !exited {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        exited,
        "the viewer did not exit after a close request: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !socket.exists(),
        "the socket file should be removed on exit"
    );
    let _ = fs::remove_dir_all(runtime);
}

#[test]
fn should_ignore_a_close_request_when_no_window_class_is_set() {
    if !display_available() {
        return;
    }
    let runtime = temp_dir("noclose");
    link_wayland(&runtime);
    let socket = runtime.join("view.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-view"))
        .arg("--socket")
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_CONFIG_HOME", &runtime)
        .env_remove("LIMPID_WINDOW_CLASS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(
        Instant::now() + Duration::from_secs(5),
        "the socket",
        || socket.exists(),
    );
    send_line(&socket, r#"{"close":true}"#);
    // No window was ever opened, so only a (wrongly honoured) close request
    // could make the process leave.
    let exited = wait_for_exit(&mut child, Duration::from_millis(600));
    let _ = child.kill();
    let _ = child.wait();
    assert!(!exited, "a close request must be ignored outside tests");
    let _ = fs::remove_dir_all(runtime);
}
