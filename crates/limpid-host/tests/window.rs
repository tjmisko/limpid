//! Behavioral window tests. Each test opens a real window briefly and is
//! skipped when no display is available.
//!
//! The host derives both its Wayland connection and its handoff socket from
//! `XDG_RUNTIME_DIR`, so each test uses a private runtime directory and links
//! the real Wayland socket into it.

use std::{
    collections::HashMap,
    fs,
    io::Write,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hello");
const SETTLE: Duration = Duration::from_secs(4);

fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "limpid-window-{}-{name}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn link_wayland(runtime: &Path) {
    if let (Some(display), Some(real)) = (
        std::env::var_os("WAYLAND_DISPLAY"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        let target = Path::new(&real).join(&display);
        let link = runtime.join(&display);
        if target.exists() && !link.exists() {
            let _ = std::os::unix::fs::symlink(&target, &link);
        }
    }
}

struct Host {
    child: Child,
    socket: PathBuf,
    runtime: PathBuf,
}

impl Host {
    fn start(runtime: &Path, scheme: &str) -> Host {
        link_wayland(runtime);
        let child = Command::new(env!("CARGO_BIN_EXE_limpid-host"))
            .arg(scheme)
            .env("XDG_RUNTIME_DIR", runtime)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let socket = runtime.join(format!("limpid-{}.sock", scheme.trim_start_matches("--")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "host did not open its socket");
            thread::sleep(Duration::from_millis(20));
        }
        Host {
            child,
            socket,
            runtime: runtime.to_path_buf(),
        }
    }

    fn open(&self, page: &Path, theme: &str) {
        self.send(&serde_json::json!({"path": page, "theme": theme}).to_string());
    }

    fn open_watched(&self, page: &Path, theme: &str, source: &Path, source_hash: &str) {
        self.send(
            &serde_json::json!({
                "path": page,
                "theme": theme,
                "source": source,
                "source_hash": source_hash,
            })
            .to_string(),
        );
    }

    fn send(&self, payload: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut stream) = UnixStream::connect(&self.socket) {
                stream.write_all(payload.as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "could not connect to the host");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stop(self) -> String {
        let _ = Command::new("kill")
            .arg("-INT")
            .arg(self.child.id().to_string())
            .status();
        let output = self.child.wait_with_output().unwrap();
        let _ = fs::remove_dir_all(&self.runtime);
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

fn web_processes(root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit(')').next() else {
            continue;
        };
        let Some(ppid) = rest
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())
        else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            stack.push(child);
            let comm = fs::read_to_string(format!("/proc/{child}/comm")).unwrap_or_default();
            if comm.trim().starts_with("WebKitWebProces") {
                found.push(child);
            }
        }
    }
    found
}

#[test]
fn local_file_supports_fetch_modules_and_color_scheme() {
    if !display_available() {
        return;
    }
    let page = Path::new(FIXTURE).join("hello.html");
    for (scheme, expected) in [("--light", "light"), ("--dark", "dark")] {
        let runtime = temp_dir(&format!("fetch-{expected}"));
        let host = Host::start(&runtime, scheme);
        host.open(&page, scheme.trim_start_matches("--"));
        thread::sleep(SETTLE);
        let stderr = host.stop();
        assert!(
            stderr.contains(&format!("LIMPID_TEST scheme={expected}")),
            "{scheme}: {stderr}"
        );
        assert!(
            stderr.contains("LIMPID_TEST fetch=ok 42"),
            "{scheme}: {stderr}"
        );
        assert!(
            stderr.contains("LIMPID_TEST module=ok"),
            "{scheme}: {stderr}"
        );
        assert!(stderr.contains("LIMPID_TEST done"), "{scheme}: {stderr}");
    }
}

#[test]
fn second_handoff_shares_one_web_process() {
    if !display_available() {
        return;
    }
    let runtime = temp_dir("shared");
    let first = runtime.join("first.html");
    let second = runtime.join("second.html");
    fs::write(
        &first,
        "<title>First</title><script>console.log('LIMPID_TEST first')</script>",
    )
    .unwrap();
    fs::write(
        &second,
        "<title>Second</title><script>console.log('LIMPID_TEST second')</script>",
    )
    .unwrap();
    let host = Host::start(&runtime, "--light");
    host.open(&first, "light");
    thread::sleep(SETTLE);
    host.open(&second, "light");
    thread::sleep(SETTLE);
    let web = web_processes(host.pid());
    let stderr = host.stop();
    assert_eq!(web.len(), 1, "both windows should share one web process");
    assert!(stderr.contains("LIMPID_TEST first"), "{stderr}");
    assert!(stderr.contains("LIMPID_TEST second"), "{stderr}");
}

#[test]
fn watched_markdown_reloads_after_an_edit() {
    if !display_available() {
        return;
    }
    let runtime = temp_dir("watch");
    let source = runtime.join("notes.md");
    fs::write(&source, "# Title\n\nfirst version\n").unwrap();
    let headings = ["Questions".to_owned()];
    let html = limpid_core::render::page(&limpid_core::render::render_with_questions(
        &fs::read_to_string(&source).unwrap(),
        limpid_core::render::RenderMode::File,
        &headings,
    ));
    let page = runtime.join("watch.html");
    fs::write(
        &page,
        limpid_core::render::externalize_assets(&html, &runtime).unwrap(),
    )
    .unwrap();
    let hash = limpid_core::replies::source_hash(&fs::read(&source).unwrap());

    let host = Host::start(&runtime, "--light");
    host.open_watched(&page, "light", &source, &hash);
    thread::sleep(SETTLE);
    assert!(fs::read_to_string(&page).unwrap().contains("first version"));

    fs::write(&source, "# Title\n\nsecond version\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut updated = false;
    while Instant::now() < deadline {
        if fs::read_to_string(&page)
            .unwrap()
            .contains("second version")
        {
            updated = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    host.stop();
    assert!(updated, "watch did not reload the edited markdown");
}
