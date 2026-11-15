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
