//! Stage 3: `limpid ssh` against an isolated sshd on a loopback port. The test
//! sshd runs as the current user with its own keys and configuration; it never
//! touches a real host. The local viewer is a stand-in socket, so no window
//! opens, and the launcher is a copy of the binary with no `limpid-view`
//! beside it.

use limpid_core::transport::{Handoff, Scheme, view_socket_path};
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const LIMPID: &str = env!("CARGO_BIN_EXE_limpid");
const TIMEOUT: Duration = Duration::from_secs(40);

/// Spawn, retrying while another test thread's fork still holds a freshly
/// copied executable open for writing (ETXTBSY).
fn spawn_retry(command: &mut Command) -> Child {
    for _ in 0..100 {
        match command.spawn() {
            Ok(child) => return child,
            Err(error) if error.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("{error}"),
        }
    }
    panic!("executable stayed busy");
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lsb-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of the launcher with no `limpid-view` beside it, so nothing here
/// can start a real viewer.
fn isolated_binary(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let copy = bin.join("limpid");
    // Another test thread forking while the copy is open for writing makes
    // the first exec fail with ETXTBSY; the window is short, so retry.
    let mut tries = 0;
    loop {
        match fs::copy(LIMPID, &copy) {
            Ok(_) => break,
            Err(error) if error.raw_os_error() == Some(26) && tries < 100 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("{error}"),
        }
    }
    copy
}

/// Stands in for the native viewer's socket and records every handoff.
struct FakeViewer {
    received: Arc<Mutex<Vec<Handoff>>>,
    stop: Arc<AtomicBool>,
}

impl FakeViewer {
    fn start(runtime: &Path) -> Self {
        let socket = view_socket_path(runtime, Scheme::Dark);
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, flag) = (received.clone(), stop.clone());
        std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let _ = stream.read_to_end(&mut bytes);
                        if !bytes.is_empty() {
                            log.lock()
                                .unwrap()
                                .push(serde_json::from_slice(&bytes).unwrap());
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self { received, stop }
    }

    fn handoffs(&self) -> Vec<Handoff> {
        self.received.lock().unwrap().clone()
    }
}

impl Drop for FakeViewer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// An sshd that serves this user on loopback with throwaway keys.
struct Sshd {
    dir: PathBuf,
    port: u16,
    child: Child,
}

impl Sshd {
    /// `None` when OpenSSH is not installed; the test then has nothing to run.
    fn start(name: &str, extra_config: &str) -> Option<Self> {
        let sshd = Path::new("/usr/sbin/sshd");
        if !sshd.exists() || Command::new("ssh-keygen").arg("-?").output().is_err() {
            eprintln!("skipping: OpenSSH server or ssh-keygen is not installed");
            return None;
        }
        let dir = scratch(name);
        for key in ["host", "user"] {
            let status = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(dir.join(key))
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::copy(dir.join("user.pub"), dir.join("authorized_keys")).unwrap();
        fs::set_permissions(
            dir.join("authorized_keys"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(
            dir.join("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host\n\
                 AuthorizedKeysFile {d}/authorized_keys\nPidFile none\nUsePAM no\n\
                 PasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                 StrictModes no\nLogLevel ERROR\n{extra_config}\n",
                d = dir.display()
            ),
        )
        .unwrap();
        let child = Command::new(sshd)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(dir.join("sshd.log")).unwrap())
            .spawn()
            .unwrap();
        let mut sshd = Self { dir, port, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if let Some(status) = sshd.child.try_wait().unwrap() {
                panic!(
                    "test sshd exited early ({status}): {}",
                    fs::read_to_string(sshd.dir.join("sshd.log")).unwrap_or_default()
                );
            }
            assert!(Instant::now() < deadline, "test sshd did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(sshd)
    }

    /// Arguments after `limpid ssh` that reach this sshd.
    fn client_args(&self) -> Vec<String> {
        let identity = self.dir.join("user");
        [
            "-F",
            "/dev/null",
            "-p",
            &self.port.to_string(),
            "-i",
            identity.to_str().unwrap(),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "127.0.0.1",
        ]
        .map(String::from)
        .to_vec()
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    route: Option<String>,
}

/// Run `limpid ssh` with `args`, feed `script` to the remote login shell and
/// wait for it to finish. Panics if it hangs.
fn login(runtime: &Path, launcher: &Path, args: &[String], script: &str) -> Outcome {
    let mut child = spawn_retry(
        Command::new(launcher)
            .arg("ssh")
            .args(args)
            .env("XDG_RUNTIME_DIR", runtime)
            .env("XDG_CACHE_HOME", runtime)
            .env("XDG_CONFIG_HOME", runtime)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    // The route is the name of the one socket the launcher binds.
    let route = wait_for_route(runtime, &mut child);
    let mut stdin = child.stdin.take().unwrap();
    let script = script.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(script.as_bytes());
    });
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let stdout = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("limpid ssh hung; stdout {:?}", stdout.join());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Outcome {
        code: status.code(),
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
        route,
    }
}

/// Routes some `login` already took, so two logins sharing a runtime
/// directory each pick their own socket.
static CLAIMED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn claim(route: &str) -> bool {
    let mut claimed = CLAIMED.lock().unwrap();
    if claimed.iter().any(|known| known == route) {
        return false;
    }
    claimed.push(route.to_string());
    true
}

fn wait_for_route(runtime: &Path, child: &mut Child) -> Option<String> {
    let directory = runtime.join("limpid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(route) = name
                    .strip_prefix("b-")
                    .and_then(|rest| rest.strip_suffix(".sock"))
                    && claim(route)
                {
                    return Some(route.to_string());
                }
            }
        }
        if child.try_wait().unwrap().is_some() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Pids of processes that carry this login's route in their environment.
fn processes_for(route: &str) -> Vec<u32> {
    let needle = format!("LIMPID_BRIDGE_ROUTE={route}");
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| {
            fs::read(format!("/proc/{pid}/environ"))
                .map(|bytes| {
                    bytes
                        .split(|byte| *byte == 0)
                        .any(|entry| entry == needle.as_bytes())
                })
                .unwrap_or(false)
        })
        .collect()
}

fn assert_nothing_left_behind(runtime: &Path, route: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes_for(route).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        processes_for(route),
        Vec::<u32>::new(),
        "relays left behind"
    );
    assert!(
        !runtime.join(format!("limpid/b-{route}.sock")).exists(),
        "local bridge socket left behind"
    );
    assert!(
        !runtime.join(format!("limpid/s-{route}.sock")).exists(),
        "local save socket left behind"
    );
    assert!(
        !Path::new(&format!("/tmp/limpid-{route}.sock")).exists(),
        "remote socket left behind"
    );
}

fn remote_page(dir: &Path, name: &str, markdown: &str, session: &str) -> PathBuf {
    let pages = dir.join("remote-pages");
    fs::create_dir_all(&pages).unwrap();
    let html = pages.join(format!("{name}.html"));
    fs::write(&html, "<html></html>").unwrap();
    fs::write(pages.join(format!("{name}.md")), markdown).unwrap();
    fs::write(
        pages.join(format!("{name}.meta.json")),
        format!(r#"{{"agent":"claude","session_id":"{session}"}}"#),
    )
    .unwrap();
    html
}

#[test]
fn should_open_the_page_locally_and_log_out_cleanly_when_remote_limpid_runs_with_a_page_open() {
    let Some(sshd) = Sshd::start("open", "") else {
        return;
    };
    let dir = scratch("open-run");
    let viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let markdown = "# Remote plan\n\nnaïve 日本語\n\n## Questions\n\n1. Which?\n   - A\n";
    let html = remote_page(&dir, "claude-1", markdown, "sess-ssh");
    let script = format!(
        "start=$(date +%s)\n{LIMPID} --source-backed {page}\necho OPEN_STATUS=$?\n\
         echo ELAPSED=$(( $(date +%s) - start ))\nexit\n",
        page = html.display()
    );
    let started = Instant::now();
    let outcome = login(&dir, &launcher, &sshd.client_args(), &script);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("OPEN_STATUS=0"),
        "{}",
        outcome.stdout
    );
    let elapsed: u64 = outcome
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("ELAPSED="))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(elapsed <= 3, "/read-style open took {elapsed}s");
    assert!(started.elapsed() < TIMEOUT, "logout hung with a page open");

    let handoffs = viewer.handoffs();
    assert_eq!(handoffs.len(), 1, "{handoffs:?}");
    let handoff = &handoffs[0];
    assert_eq!(fs::read_to_string(&handoff.path).unwrap(), markdown);
    assert!(
        handoff.path.starts_with(&dir),
        "the page lives in the local cache, not at the remote path"
    );
    assert!(!handoff.path.starts_with(dir.join("remote-pages")));
    assert_eq!(handoff.source, None);
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(handoff.path.with_extension("meta.json")).unwrap())
            .unwrap();
    assert_eq!(meta["session_id"], "sess-ssh");
    assert_eq!(meta["remote"]["session"], "sess-ssh");
    assert_eq!(meta["remote"]["preview"], true);
    assert_eq!(
        meta["remote"]["remote_path"],
        html.canonicalize().unwrap().to_str().unwrap()
    );
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_open_pages_from_two_independent_logins_when_they_share_one_viewer() {
    let Some(sshd) = Sshd::start("two", "") else {
        return;
    };
    let dir = scratch("two-run");
    let viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let first = remote_page(&dir, "a", "# From first\n", "sess-a");
    let second = remote_page(&dir, "b", "# From second\n", "sess-b");
    let args = sshd.client_args();
    let run = |page: PathBuf| {
        let (dir, launcher, args) = (dir.clone(), launcher.clone(), args.clone());
        std::thread::spawn(move || {
            let script = format!(
                "{LIMPID} --source-backed {}\necho OPEN_STATUS=$?\nsleep 1\nexit\n",
                page.display()
            );
            login(&dir, &launcher, &args, &script)
        })
    };
    let (one, two) = (run(first), run(second));
    let (one, two) = (one.join().unwrap(), two.join().unwrap());
    for outcome in [&one, &two] {
        assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
        assert!(
            outcome.stdout.contains("OPEN_STATUS=0"),
            "{}",
            outcome.stdout
        );
    }
    assert_ne!(one.route, two.route, "each login has its own route");
    let mut titles: Vec<String> = viewer
        .handoffs()
        .iter()
        .map(|handoff| fs::read_to_string(&handoff.path).unwrap())
        .collect();
    titles.sort();
    assert_eq!(titles, ["# From first\n", "# From second\n"]);
    assert_nothing_left_behind(&dir, &one.route.unwrap());
    assert_nothing_left_behind(&dir, &two.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_fail_with_a_hint_and_leave_nothing_behind_when_the_server_refuses_forwarding() {
    let Some(sshd) = Sshd::start("noforward", "AllowStreamLocalForwarding no") else {
        return;
    };
    let dir = scratch("noforward-run");
    let _viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let outcome = login(
        &dir,
        &launcher,
        &sshd.client_args(),
        "echo SHOULD_NOT_RUN\nexit\n",
    );
    assert_eq!(outcome.code, Some(255), "{}", outcome.stderr);
    assert!(
        !outcome.stdout.contains("SHOULD_NOT_RUN"),
        "a login without its forward must not start"
    );
    assert!(
        outcome.stderr.contains("AllowStreamLocalForwarding"),
        "{}",
        outcome.stderr
    );
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_report_the_remote_failure_and_log_out_when_the_remote_has_no_limpid() {
    let Some(sshd) = Sshd::start("nolimpid", "") else {
        return;
    };
    let dir = scratch("nolimpid-run");
    let _viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let script = "limpid-definitely-not-installed /nowhere\necho STATUS=$?\nexit\n";
    let outcome = login(&dir, &launcher, &sshd.client_args(), script);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert!(outcome.stdout.contains("STATUS=127"), "{}", outcome.stdout);
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_fail_the_remote_open_and_stay_logged_in_when_no_local_viewer_is_installed() {
    let Some(sshd) = Sshd::start("noviewer", "") else {
        return;
    };
    let dir = scratch("noviewer-run");
    // No FakeViewer: nothing listens and no limpid-view exists on PATH or
    // beside the launcher, so the local launch fails.
    let launcher = isolated_binary(&dir);
    let html = remote_page(&dir, "p", "# Page\n", "sess-x");
    let script = format!(
        "{LIMPID} --source-backed {}\necho OPEN_STATUS=$?\nexit\n",
        html.display()
    );
    let outcome = login(&dir, &launcher, &sshd.client_args(), &script);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("OPEN_STATUS=1"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("limpid-view is not installed")
            || outcome.stderr.contains("limpid-view is not installed"),
        "{} / {}",
        outcome.stdout,
        outcome.stderr
    );
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_report_missing_forwarding_when_the_relay_cannot_reach_its_socket() {
    let Some(sshd) = Sshd::start("norelay", "") else {
        return;
    };
    let dir = scratch("norelay-run");
    let _viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let html = remote_page(&dir, "p", "# Page\n", "sess-x");
    // A login whose route points at a socket nobody listens on.
    let script = format!(
        "LIMPID_BRIDGE_SOCKET=/tmp/limpid-no-such-{pid}.sock {LIMPID} {}\necho OPEN_STATUS=$?\nexit\n",
        html.display(),
        pid = std::process::id()
    );
    let outcome = login(&dir, &launcher, &sshd.client_args(), &script);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("OPEN_STATUS=1"),
        "{}",
        outcome.stdout
    );
    let all = format!("{}{}", outcome.stdout, outcome.stderr);
    assert!(all.contains("forwarding"), "{all}");
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

// ---- no sshd: a fake ssh records what the launcher asks for ----------------

fn fake_ssh(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-ssh");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn launch_with(dir: &Path, ssh: &Path, args: &[&str]) -> std::process::Output {
    spawn_retry(
        Command::new(isolated_binary(dir))
            .arg("ssh")
            .args(args)
            .env("LIMPID_SSH", ssh)
            .env("XDG_RUNTIME_DIR", dir)
            .env("XDG_CACHE_HOME", dir)
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .wait_with_output()
    .unwrap()
}

#[test]
fn should_pass_the_forward_and_options_to_ssh_when_the_launcher_runs() {
    let dir = scratch("fake-args");
    let record = dir.join("args.txt");
    let ssh = fake_ssh(
        &dir,
        &format!(
            "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\nls {}/limpid > {}/live.txt\nexit 0",
            record.display(),
            dir.display(),
            dir.display()
        ),
    );
    let output = launch_with(&dir, &ssh, &["-p", "2222", "--light", "me@box"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args: Vec<String> = fs::read_to_string(&record)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert!(
        args.contains(&"ExitOnForwardFailure=yes".to_string()),
        "{args:?}"
    );
    assert!(args.contains(&"ControlPath=none".to_string()));
    let forward = args.iter().position(|a| a == "-R").unwrap();
    let (remote, local) = args[forward + 1].split_once(':').unwrap();
    assert!(remote.starts_with("/tmp/limpid-") && remote.ends_with(".sock"));
    assert!(local.starts_with(dir.join("limpid").to_str().unwrap()));
    assert_eq!(args[args.len() - 2], "me@box");
    assert!(
        args.last()
            .unwrap()
            .starts_with("env LIMPID_BRIDGE_SOCKET=")
    );
    assert!(
        fs::read_to_string(dir.join("live.txt"))
            .unwrap()
            .contains("b-"),
        "the hub socket exists while ssh runs"
    );
    assert!(
        fs::read_dir(dir.join("limpid")).unwrap().next().is_none(),
        "the hub socket is removed when ssh exits"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_propagate_the_exit_status_when_ssh_fails() {
    let dir = scratch("fake-status");
    let ssh = fake_ssh(&dir, "exit 7");
    let output = launch_with(&dir, &ssh, &["box"]);
    assert_eq!(output.status.code(), Some(7));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_name_the_missing_client_when_ssh_is_not_installed() {
    let dir = scratch("fake-missing");
    let output = launch_with(&dir, &dir.join("no-such-ssh"), &["box"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") && stderr.contains("LIMPID_SSH"),
        "{stderr}"
    );
    assert!(fs::read_dir(dir.join("limpid")).unwrap().next().is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_set_the_route_and_remove_the_socket_when_the_remote_command_runs_in_sh() {
    use limpid_core::bridge_ssh::remote_command;
    let dir = scratch("quoting");
    let socket = dir.join("it's a sock");
    fs::write(&socket, "").unwrap();
    let shell = dir.join("login-shell");
    fs::write(
        &shell,
        "#!/bin/sh\nprintf '%s|%s' \"$LIMPID_BRIDGE_ROUTE\" \"$LIMPID_BRIDGE_SOCKET\"\nexit 3\n",
    )
    .unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    let route = "r'o $(touch pwned) \"x\"";
    let command = remote_command(socket.to_str().unwrap(), route);
    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .env("SHELL", &shell)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "the shell's status carries through"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{route}|{}", socket.display())
    );
    assert!(!socket.exists(), "the login's socket is removed on exit");
    assert!(!Path::new("pwned").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_save_feedback_to_the_remote_page_when_the_local_copy_is_pushed_over_ssh() {
    let Some(sshd) = Sshd::start("save", "") else {
        return;
    };
    let dir = scratch("save-run");
    let viewer = FakeViewer::start(&dir);
    let launcher = isolated_binary(&dir);
    let markdown = "# Remote plan\n\nnaïve 日本語\n\n## Questions\n\n1. Which?\n   - A\n";
    let html = remote_page(&dir, "claude-save", markdown, "sess-save");
    let flag = dir.join("pushed");
    let script = format!(
        "{LIMPID} --source-backed {page}\necho OPEN_STATUS=$?\n\
         while [ ! -f {flag} ]; do sleep 0.1; done\nexit\n",
        page = html.display(),
        flag = flag.display()
    );
    let args = sshd.client_args();
    let session = {
        let (dir, launcher) = (dir.clone(), launcher.clone());
        std::thread::spawn(move || login(&dir, &launcher, &args, &script))
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while viewer.handoffs().is_empty() {
        assert!(Instant::now() < deadline, "the page never opened locally");
        std::thread::sleep(Duration::from_millis(20));
    }
    let handoff = viewer.handoffs().remove(0);
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(handoff.path.with_extension("meta.json")).unwrap())
            .unwrap();
    let origin = &meta["remote"];
    // What the viewer's :w does: save the local copy, then ask to push it.
    let mut replies = limpid_core::replies::AgentReplies::default();
    replies
        .replies
        .insert("q1".into(), "A, naïve 日本語".into());
    replies
        .save(&limpid_core::replies::sidecar_path(
            &handoff.path.with_extension("html"),
        ))
        .unwrap();
    let request = limpid_core::bridge_push::PushRequest {
        instance: origin["instance"].as_str().unwrap().into(),
        page_id: origin["page_id"].as_str().unwrap().into(),
        submit: false,
    };
    let reply = limpid_core::bridge_push::request_push(
        Path::new(origin["save_sock"].as_str().unwrap()),
        &request,
        Duration::from_secs(10),
    );
    assert_eq!(
        reply.state,
        limpid_core::bridge_push::PushState::RemoteSaved,
        "{reply:?}"
    );
    let remote = limpid_core::bridge_save::read_feedback(&html).unwrap();
    assert_eq!(remote, replies, "the remote page holds the feedback");
    assert_eq!(
        limpid_core::bridge_save::read_revision(&html)
            .unwrap()
            .revision,
        1
    );
    fs::write(&flag, "").unwrap();
    let outcome = session.join().unwrap();
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("OPEN_STATUS=0"),
        "{}",
        outcome.stdout
    );
    assert_nothing_left_behind(&dir, &outcome.route.unwrap());
    fs::remove_dir_all(dir).unwrap();
}

// ---- stage 6: concurrency, failures, recovery and measurement --------------

use limpid_core::{
    bridge_push::{PushReply, PushRequest, PushState, request_push},
    replies::{AgentReplies, sidecar_path},
};

/// One desktop: its own runtime and cache directories, viewer and launcher.
struct Client {
    dir: PathBuf,
    viewer: FakeViewer,
    launcher: PathBuf,
}

impl Client {
    fn new(name: &str) -> Self {
        let dir = scratch(name);
        let viewer = FakeViewer::start(&dir);
        let launcher = isolated_binary(&dir);
        Self {
            dir,
            viewer,
            launcher,
        }
    }

    /// A login on its own thread: `script` runs in the remote shell.
    fn login(&self, sshd: &Sshd, script: String) -> std::thread::JoinHandle<Outcome> {
        let (dir, launcher, args) = (self.dir.clone(), self.launcher.clone(), sshd.client_args());
        std::thread::spawn(move || login(&dir, &launcher, &args, &script))
    }

    fn wait_for_handoffs(&self, count: usize) -> Vec<Handoff> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let handoffs = self.viewer.handoffs();
            if handoffs.len() >= count {
                return handoffs;
            }
            assert!(
                Instant::now() < deadline,
                "only {} of {count} pages opened locally",
                handoffs.len()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The cache directory `limpid recover` reads on this client.
    fn cache(&self) -> PathBuf {
        self.dir.join("limpid/remote")
    }

    fn recover(&self, args: &[&str]) -> std::process::Output {
        spawn_retry(
            Command::new(&self.launcher)
                .arg("recover")
                .args(args)
                .env("XDG_CACHE_HOME", &self.dir)
                .env("PATH", "/usr/bin:/bin")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .wait_with_output()
        .unwrap()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn origin_of(handoff: &Handoff) -> serde_json::Value {
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(handoff.path.with_extension("meta.json")).unwrap())
            .unwrap();
    meta["remote"].clone()
}

fn feedback(pairs: &[(&str, &str)]) -> AgentReplies {
    let mut replies = AgentReplies::default();
    for (field, text) in pairs {
        replies.replies.insert((*field).into(), (*text).into());
    }
    replies
}

/// What the viewer's `:w` (or `:wq`) does: save the local copy, then ask the
/// login's launcher to push it.
fn viewer_saves(handoff: &Handoff, replies: &AgentReplies, submit: bool) -> (PushReply, Duration) {
    replies
        .save(&sidecar_path(&handoff.path.with_extension("html")))
        .unwrap();
    let origin = origin_of(handoff);
    let request = PushRequest {
        instance: origin["instance"].as_str().unwrap().into(),
        page_id: origin["page_id"].as_str().unwrap().into(),
        submit,
    };
    let started = Instant::now();
    let reply = request_push(
        Path::new(origin["save_sock"].as_str().unwrap()),
        &request,
        Duration::from_secs(15),
    );
    (reply, started.elapsed())
}

fn open_script(pages: &[&Path], flag: &Path) -> String {
    let mut script = String::new();
    for page in pages {
        script.push_str(&format!(
            "{LIMPID} --source-backed {}\necho OPEN_STATUS=$?\n",
            page.display()
        ));
    }
    script.push_str(&format!(
        "while [ ! -f {} ]; do sleep 0.1; done\nexit\n",
        flag.display()
    ));
    script
}

#[test]
fn should_keep_two_clients_apart_and_refuse_the_stale_one_when_both_open_the_same_remote_page() {
    let Some(sshd) = Sshd::start("clients", "") else {
        return;
    };
    let (one, two) = (Client::new("client-one"), Client::new("client-two"));
    let html = remote_page(
        &one.dir,
        "shared",
        "# Shared\n\n## Questions\n\n1. Which?\n",
        "sess-shared",
    );
    let (flag_one, flag_two) = (one.dir.join("done"), two.dir.join("done"));
    let first = one.login(&sshd, open_script(&[&html], &flag_one));
    let second = two.login(&sshd, open_script(&[&html], &flag_two));
    let (handoff_one, handoff_two) = (
        one.wait_for_handoffs(1).remove(0),
        two.wait_for_handoffs(1).remove(0),
    );
    assert_ne!(
        origin_of(&handoff_one)["route"],
        origin_of(&handoff_two)["route"],
        "each client has its own login"
    );
    assert_ne!(handoff_one.path, handoff_two.path, "and its own cache");

    let (reply, _) = viewer_saves(&handoff_one, &feedback(&[("q1", "from one")]), false);
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    let (reply, _) = viewer_saves(&handoff_two, &feedback(&[("q1", "from two")]), false);
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(
        reply.describe().contains("stale revision"),
        "{}",
        reply.describe()
    );
    assert_eq!(
        limpid_core::bridge_save::read_feedback(&html).unwrap(),
        feedback(&[("q1", "from one")]),
        "the second client did not overwrite the first"
    );
    assert_eq!(
        AgentReplies::load(&sidecar_path(&handoff_two.path.with_extension("html"))).unwrap(),
        feedback(&[("q1", "from two")]),
        "the second client keeps its local copy"
    );
    let listing = String::from_utf8(two.recover(&[]).stdout).unwrap();
    assert!(listing.contains("unsent feedback"), "{listing}");
    assert!(
        String::from_utf8(one.recover(&[]).stdout)
            .unwrap()
            .contains("No unsent")
    );

    fs::write(&flag_one, "").unwrap();
    fs::write(&flag_two, "").unwrap();
    let (first, second) = (first.join().unwrap(), second.join().unwrap());
    for outcome in [&first, &second] {
        assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    }
    assert_nothing_left_behind(&one.dir, &first.route.unwrap());
    assert_nothing_left_behind(&two.dir, &second.route.unwrap());
}

#[test]
fn should_save_each_page_to_its_own_remote_file_when_one_login_opens_several_pages() {
    let Some(sshd) = Sshd::start("several", "") else {
        return;
    };
    let client = Client::new("several-pages");
    let pages: Vec<PathBuf> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            remote_page(
                &client.dir,
                name,
                &format!("# Page {name}\n"),
                &format!("sess-{name}"),
            )
        })
        .collect();
    let flag = client.dir.join("done");
    let refs: Vec<&Path> = pages.iter().map(PathBuf::as_path).collect();
    let session = client.login(&sshd, open_script(&refs, &flag));
    let handoffs = client.wait_for_handoffs(3);
    let routes: std::collections::HashSet<String> = handoffs
        .iter()
        .map(|handoff| origin_of(handoff)["route"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(routes.len(), 1, "one login, one route");
    for handoff in &handoffs {
        let title = fs::read_to_string(&handoff.path).unwrap();
        let name = title.trim().rsplit(' ').next().unwrap().to_string();
        let index = ["a", "b", "c"].iter().position(|n| *n == name).unwrap();
        let (reply, took) = viewer_saves(
            handoff,
            &feedback(&[("q1", &format!("note {name}"))]),
            false,
        );
        assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
        assert!(took < Duration::from_secs(5), "save took {took:?}");
        assert_eq!(
            limpid_core::bridge_save::read_feedback(&pages[index]).unwrap(),
            feedback(&[("q1", &format!("note {name}"))])
        );
    }
    fs::write(&flag, "").unwrap();
    let outcome = session.join().unwrap();
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(
        outcome.stdout.matches("OPEN_STATUS=0").count(),
        3,
        "{}",
        outcome.stdout
    );
    assert_nothing_left_behind(&client.dir, &outcome.route.unwrap());
}

/// Pids of `ssh` clients that carry this login's forward.
fn ssh_clients_for(route: &str) -> Vec<u32> {
    let needle = format!("/tmp/limpid-{route}.sock:");
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| {
            fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|comm| comm.trim() == "ssh")
                .unwrap_or(false)
                && fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains(&needle))
                    .unwrap_or(false)
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Ending {
    /// The remote shell exits.
    ShellExit,
    /// The network goes: the ssh client is killed without a goodbye.
    ConnectionLost,
}

/// Open a page, save feedback locally and remotely, edit it, end the login
/// the given way, and check that the edit survives only locally and is
/// recoverable.
fn unsent_feedback_survives(name: &str, ending: Ending) {
    let Some(sshd) = Sshd::start(name, "") else {
        return;
    };
    let client = Client::new(&format!("{name}-client"));
    let html = remote_page(
        &client.dir,
        "p",
        "# Plan\n\n## Questions\n\n1. Which?\n",
        "sess-end",
    );
    let flag = client.dir.join("done");
    let session = client.login(&sshd, open_script(&[&html], &flag));
    let handoff = client.wait_for_handoffs(1).remove(0);
    let route = origin_of(&handoff)["route"].as_str().unwrap().to_string();
    let (reply, _) = viewer_saves(&handoff, &feedback(&[("q1", "first")]), false);
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    feedback(&[("q1", "second, naïve 日本語")])
        .save(&sidecar_path(&handoff.path.with_extension("html")))
        .unwrap();

    match ending {
        Ending::ShellExit => fs::write(&flag, "").unwrap(),
        Ending::ConnectionLost => {
            let pids = ssh_clients_for(&route);
            assert_eq!(pids.len(), 1, "ssh clients for {route}: {pids:?}");
            let status = Command::new("kill")
                .args(["-9", &pids[0].to_string()])
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
    let started = Instant::now();
    let outcome = session.join().unwrap();
    assert!(
        started.elapsed() < TIMEOUT,
        "the launcher hung after the login ended"
    );
    if matches!(ending, Ending::ConnectionLost) {
        assert_ne!(
            outcome.code,
            Some(0),
            "a lost connection is not a clean exit"
        );
        // Let the abandoned remote shell finish so nothing is left running.
        fs::write(&flag, "").unwrap();
    }

    // The launcher is gone: the push cannot happen, and says why.
    let (reply, took) = viewer_saves(&handoff, &feedback(&[("q1", "second, naïve 日本語")]), true);
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(!reply.submitted());
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(
        reply
            .describe()
            .contains("login that opened this page is gone"),
        "{}",
        reply.describe()
    );
    assert_eq!(
        limpid_core::bridge_save::read_feedback(&html).unwrap(),
        feedback(&[("q1", "first")]),
        "the remote holds the last acknowledged copy"
    );

    let listing = client.recover(&[]);
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("unsent feedback"), "{listing}");
    let stem = handoff
        .path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(listing.contains(&stem), "{listing}");
    let text = client.recover(&["--text", &stem]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("q1: second, naïve 日本語"), "{text}");
    assert_eq!(client.cache(), handoff.path.parent().unwrap());

    assert_nothing_left_behind(&client.dir, &route);
}

#[test]
fn should_keep_unsent_feedback_recoverable_when_the_shell_exits() {
    unsent_feedback_survives("shellexit", Ending::ShellExit);
}

#[test]
fn should_keep_unsent_feedback_recoverable_when_the_connection_is_lost() {
    unsent_feedback_survives("netloss", Ending::ConnectionLost);
}

#[test]
fn should_list_nothing_and_succeed_when_there_is_no_unsent_feedback() {
    let client = Client::new("recover-empty");
    let output = client.recover(&[]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "No unsent feedback.\n"
    );
    let output = client.recover(&["--text", "remote-missing"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no unsent feedback"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = client.recover(&["--bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage: limpid recover"));
}

// ---- measurements (run by hand: scripts/measure-ssh-bridge) ----------------

fn percentile(samples: &[Duration], fraction: f64) -> Duration {
    samples[((samples.len() - 1) as f64 * fraction).round() as usize]
}

fn report(name: &str, samples: &mut [Duration]) {
    samples.sort();
    println!(
        "MEASURE {name}: n={} min={:?} p50={:?} max={:?}",
        samples.len(),
        samples[0],
        percentile(samples, 0.5),
        samples[samples.len() - 1]
    );
}

/// Warm opening (viewer already running) and feedback acknowledgment, over
/// the isolated sshd with a stand-in viewer and agent listener. Cold viewer
/// startup needs a real viewer and display; the script measures that.
#[test]
#[ignore = "measurement; run through scripts/measure-ssh-bridge"]
fn measure_warm_open_and_feedback_acknowledgment_latency() {
    let sshd = Sshd::start("measure", "").expect("measurement needs OpenSSH");
    let client = Client::new("measure-run");
    const ROUNDS: usize = 12;
    let listener_path = client.dir.join("agent.sock");
    let listener = UnixListener::bind(&listener_path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut line);
            let _ = (&stream).write_all(b"{\"ok\":true}\n");
        }
    });
    let pages: Vec<PathBuf> = (0..ROUNDS)
        .map(|index| {
            let html = remote_page(
                &client.dir,
                &format!("m{index}"),
                &format!("# Measure {index}\n\n## Questions\n\n1. Which?\n"),
                &format!("sess-m{index}"),
            );
            let meta = html.with_extension("meta.json");
            fs::write(
                &meta,
                format!(
                    r#"{{"agent":"claude","session_id":"sess-m{index}","title":"m","feedback_sock":"{}"}}"#,
                    listener_path.display()
                ),
            )
            .unwrap();
            html
        })
        .collect();
    let flag = client.dir.join("done");
    let mut script = String::new();
    for page in &pages {
        script.push_str(&format!(
            "t0=$(date +%s%N)\n{LIMPID} --source-backed {}\nt1=$(date +%s%N)\n\
             echo OPEN_MS=$(( (t1 - t0) / 1000000 ))\n",
            page.display()
        ));
    }
    script.push_str(&format!(
        "while [ ! -f {} ]; do sleep 0.1; done\nexit\n",
        flag.display()
    ));
    let session = client.login(&sshd, script);
    let handoffs = client.wait_for_handoffs(ROUNDS);
    let (mut saves, mut submits) = (Vec::new(), Vec::new());
    for (index, handoff) in handoffs.iter().enumerate() {
        let (reply, took) = viewer_saves(
            handoff,
            &feedback(&[("q1", &format!("save {index}"))]),
            false,
        );
        assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
        saves.push(took);
        let (reply, took) = viewer_saves(
            handoff,
            &feedback(&[("q1", &format!("send {index}"))]),
            true,
        );
        assert!(reply.submitted(), "{reply:?}");
        submits.push(took);
    }
    fs::write(&flag, "").unwrap();
    let outcome = session.join().unwrap();
    let mut opens: Vec<Duration> = outcome
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("OPEN_MS="))
        .map(|ms| Duration::from_millis(ms.trim().parse().unwrap()))
        .collect();
    assert_eq!(opens.len(), ROUNDS, "{}", outcome.stdout);
    report(
        "warm_open (remote /read-style command, stand-in viewer)",
        &mut opens,
    );
    report("save_ack (viewer :w to remote saved)", &mut saves);
    report("submit_ack (viewer :wq to agent accepted)", &mut submits);
}
