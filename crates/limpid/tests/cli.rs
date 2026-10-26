use limpid_core::transport::{Handoff, Scheme, socket_path, view_socket_path};
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
    listen_on(socket_path(runtime, scheme))
}

fn receive_view_handoff(runtime: &Path, scheme: Scheme) -> JoinHandle<Option<Handoff>> {
    listen_on(view_socket_path(runtime, scheme))
}

fn listen_on(socket: std::path::PathBuf) -> JoinHandle<Option<Handoff>> {
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
fn markdown_open_hands_the_file_to_the_view() {
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
    let receiver = receive_view_handoff(&directory, Scheme::Light);

    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--light")
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
    assert_eq!(request.theme, Scheme::Light);
    assert_eq!(request.pane.as_deref(), Some("4242"));
    assert_eq!(request.source, Some(source.canonicalize().unwrap()));
    assert!(request.source_hash.is_some());
    assert!(request.source_backed);
    assert_eq!(request.path, source.canonicalize().unwrap());
    assert_eq!(
        request.source_hash.as_deref(),
        Some(limpid_core::replies::source_hash(&fs::read(&source).unwrap()).as_str())
    );

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn markdown_open_with_the_host_forced_renders_html_and_uses_the_host_socket() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-forced-host-{}-{:?}",
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
    let receiver = receive_handoff(&directory, Scheme::Dark);
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["--viewer", "host"])
        .arg(&source)
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = receiver.join().unwrap().expect("no handoff received");
    assert!(request.path.starts_with(&directory));
    assert!(
        fs::read_to_string(&request.path)
            .unwrap()
            .contains("class=\"question-field\"")
    );
    assert_eq!(request.source, Some(source.canonicalize().unwrap()));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn agent_page_with_a_markdown_sidecar_opens_the_sidecar_in_the_view() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-agent-route-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let page = directory.join("claude-abc.html");
    fs::write(&page, "<!doctype html><p>page</p>").unwrap();
    fs::write(directory.join("claude-abc.md"), "# Answer\n").unwrap();
    fs::write(directory.join("claude-abc.meta.json"), "{}").unwrap();
    let receiver = receive_view_handoff(&directory, Scheme::Dark);
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--source-backed")
        .arg(&page)
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = receiver.join().unwrap().expect("no handoff received");
    assert_eq!(
        request.path,
        directory.join("claude-abc.md").canonicalize().unwrap()
    );
    assert_eq!(request.source, None, "replies go to the page's sidecar");
    assert!(request.source_backed);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn standard_input_opens_as_cached_markdown_in_the_view() {
    let directory = std::env::temp_dir().join(format!(
        "limpid-stdin-route-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let receiver = receive_view_handoff(&directory, Scheme::Dark);
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("-")
        .env("XDG_RUNTIME_DIR", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .env("XDG_CONFIG_HOME", &directory)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"# From a pipe\n")
        .unwrap();
    assert!(child.wait().unwrap().success());
    let request = receiver.join().unwrap().expect("no handoff received");
    assert_eq!(request.path.extension().unwrap(), "md");
    assert_eq!(
        fs::read_to_string(&request.path).unwrap(),
        "# From a pipe\n"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unknown_viewer_names_are_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["--viewer", "gtk", "x.md"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown viewer"));
}

#[test]
fn forcing_the_view_on_plain_html_fails_before_starting_anything() {
    let directory = std::env::temp_dir().join(format!("limpid-view-html-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let page = directory.join("page.html");
    fs::write(&page, "<p>x</p>").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .args(["--viewer", "view"])
        .arg(&page)
        .env("XDG_RUNTIME_DIR", &directory)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no Markdown"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn html_source_backed_is_explicit() {
    for (suffix, flag, expected) in [("default", false, false), ("flagged", true, true)] {
        let directory = std::env::temp_dir().join(format!(
            "limpid-html-{suffix}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let page = directory.join("page.html");
        fs::write(
            &page,
            "<!doctype html><html><body><p>Text</p></body></html>",
        )
        .unwrap();
        let receiver = receive_handoff(&directory, Scheme::Dark);
        let mut command = Command::new(env!("CARGO_BIN_EXE_limpid"));
        if flag {
            command.arg("--source-backed");
        }
        let output = command
            .arg("--dark")
            .arg(&page)
            .env("XDG_RUNTIME_DIR", &directory)
            .env("XDG_CACHE_HOME", &directory)
            .env("XDG_CONFIG_HOME", &directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request = receiver.join().unwrap().expect("no handoff received");
        assert_eq!(request.source_backed, expected);
        fs::remove_dir_all(directory).unwrap();
    }
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
fn missing_markdown_fails_fast() {
    let directory = std::env::temp_dir().join(format!("limpid-missing-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--out")
        .arg(directory.join("out.html"))
        .arg(directory.join("nope.md"))
        .env("XDG_CONFIG_HOME", &directory)
        .env("XDG_CACHE_HOME", &directory)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.to_lowercase().contains("no such file"), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn missing_target_and_help_report_usage() {
    let missing = Command::new(env!("CARGO_BIN_EXE_limpid")).output().unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("usage"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let help = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: limpid"));
}

fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

/// `limpid-view` is built by the same workspace build; it sits beside
/// `limpid`, which is where the command looks for it.
fn sibling_view() -> Option<std::path::PathBuf> {
    let view = Path::new(env!("CARGO_BIN_EXE_limpid")).with_file_name("limpid-view");
    view.exists().then_some(view)
}

#[test]
fn opening_markdown_starts_the_view_and_its_socket_when_a_display_exists() {
    if !display_available() || sibling_view().is_none() {
        return;
    }
    let runtime = std::env::temp_dir().join(format!("limpid-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&runtime);
    fs::create_dir_all(&runtime).unwrap();
    if let (Some(display), Some(real)) = (
        std::env::var_os("WAYLAND_DISPLAY"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        let _ = std::os::unix::fs::symlink(Path::new(&real).join(&display), runtime.join(display));
    }
    let source = runtime.join("small.md");
    fs::write(&source, "# Small\n\nA paragraph.\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limpid"))
        .arg(&source)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_CACHE_HOME", &runtime)
        .env("XDG_CONFIG_HOME", &runtime)
        .env("LIMPID_WINDOW_CLASS", "limpid-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let socket = view_socket_path(&runtime, Scheme::Dark);
    assert!(socket.exists(), "the view should be listening");
    assert!(
        !socket_path(&runtime, Scheme::Dark).exists(),
        "the host must not have been started"
    );
    // Test-only close request: accepted because the class is set.
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    stream.write_all(b"{\"close\":true}\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !socket.exists(),
        "the view should exit and remove its socket"
    );
    let _ = fs::remove_dir_all(runtime);
}
