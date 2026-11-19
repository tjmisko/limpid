//! Sending side of the bridge on the remote host: the route a login supplies,
//! the page `limpid` builds from an agent page's files, and the detached relay
//! that keeps the page's connection open after the opening command returns.
//!
//! The relay is a separate process. `limpid <page>` writes the page to its
//! standard input and reads one status line back, `ok` once the viewer
//! accepted the page or `error: ...` otherwise, and then returns. The relay
//! keeps the connection until it ends or the login's forwarded socket path is
//! removed, which the login wrapper does when its shell exits.

use crate::{
    bridge::{Body, BridgeError, Delivery, ErrorCode, OpenPage, Relay},
    bridge_save::{apply_save, page_id_of, read_feedback, read_revision},
    bridge_submit::{LISTENER_WAIT, submit_feedback},
};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// The login's forwarded socket on this host.
pub const SOCKET_ENV: &str = "LIMPID_BRIDGE_SOCKET";
/// The login's route; announced in the handshake and bound to every page.
pub const ROUTE_ENV: &str = "LIMPID_BRIDGE_ROUTE";

/// How long the relay waits for the viewer to accept a page. The local
/// launcher may start a cold viewer, which takes longer than a plain request.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// How often a relay checks that its login still exists.
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// How often a relay looks for its login's sessions; a /proc scan, so rarer.
const LOGIN_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How long the opening command waits for the relay's status line.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEnv {
    pub socket: PathBuf,
    pub route: String,
}

/// The route in the environment. Neither variable set means no route. A
/// socket alone gets a fresh route; a route alone names no socket and is
/// reported, never ignored.
pub fn route_from_env(
    socket: Option<OsString>,
    route: Option<OsString>,
) -> Result<Option<RouteEnv>, String> {
    let socket = socket.filter(|value| !value.is_empty());
    let route = route.filter(|value| !value.is_empty());
    match (socket, route) {
        (None, None) => Ok(None),
        (Some(socket), Some(route)) => Ok(Some(RouteEnv {
            socket: PathBuf::from(socket),
            route: route
                .into_string()
                .map_err(|_| format!("{ROUTE_ENV} is not valid UTF-8"))?,
        })),
        (Some(socket), None) => Ok(Some(RouteEnv {
            socket: PathBuf::from(socket),
            route: new_route()?,
        })),
        (None, Some(_)) => Err(format!(
            "{ROUTE_ENV} is set but {SOCKET_ENV} is not; unset it or name the socket too"
        )),
    }
}

/// Where an SSH login forwards the bridge for `user` when nothing in the
/// environment says otherwise. The client's `~/.ssh/config` names the same
/// path: `RemoteForward /tmp/limpid-%r.sock ...`.
pub fn default_socket_for(user: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/limpid-{user}.sock"))
}

/// The route for this process: the environment's, or else the forwarded
/// socket at `default` when one is there. Found by path, not inherited, so a
/// shell that outlived its login (a tmux pane) reaches the current login.
///
/// A file at `default` that is not a private socket owned by `uid` is an
/// error, never used: anyone can create a file in /tmp, and a socket someone
/// else planted would receive this user's pages.
pub fn discover_route(
    socket: Option<OsString>,
    route: Option<OsString>,
    default: &Path,
    uid: u32,
) -> Result<Option<RouteEnv>, String> {
    if let Some(found) = route_from_env(socket, route)? {
        return Ok(Some(found));
    }
    let metadata = match std::fs::symlink_metadata(default) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", default.display())),
    };
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "{} exists but is not a socket; remove it so SSH logins can forward the bridge there",
            default.display()
        ));
    }
    if metadata.uid() != uid {
        return Err(format!(
            "{} belongs to uid {}, not to you; refusing to send pages to it",
            default.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is accessible to other users (mode {:o}); refusing to send pages to it",
            default.display(),
            metadata.mode() & 0o777
        ));
    }
    Ok(Some(RouteEnv {
        socket: default.to_path_buf(),
        route: new_route()?,
    }))
}

/// [`discover_route`] for this process: its environment, its user's
/// default path. Without a user name there is no default path, and only the
/// environment counts.
pub fn route_for_process() -> Result<Option<RouteEnv>, String> {
    let socket = std::env::var_os(SOCKET_ENV);
    let route = std::env::var_os(ROUTE_ENV);
    // SAFETY: getuid cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    match user_name(uid) {
        Ok(name) => discover_route(socket, route, &default_socket_for(&name), uid),
        Err(_) => route_from_env(socket, route),
    }
}

/// The login name of `uid`, from the password database.
pub fn user_name(uid: u32) -> Result<String, String> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to storage that outlives the call.
    let result = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() {
        return Err(format!("no user name for uid {uid}"));
    }
    // SAFETY: getpwuid_r succeeded, so pw_name is a NUL-terminated string in `buffer`.
    let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
    name.to_str()
        .map(str::to_string)
        .map_err(|_| format!("the user name for uid {uid} is not valid UTF-8"))
}

/// A fresh, unguessable login route: 64 random bits as hex.
pub fn new_route() -> Result<String, String> {
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("could not generate a route: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether the SSH login behind `pid` has ended. `pid` is the process that
/// listens on the forwarded socket; for an SSH forward that is the login's
/// own `sshd` process, whose children are the login's sessions. Once the last
/// session exits the login is over, even though `sshd` stays up while any
/// forwarded connection (this relay's) is open, and so would keep the user's
/// `ssh` client waiting at logout.
///
/// A listener that is not `sshd` is no login to watch: false.
pub fn login_ended(pid: i32, proc_root: &Path) -> bool {
    let process = proc_root.join(pid.to_string());
    let Ok(name) = std::fs::read_to_string(process.join("comm")) else {
        // Gone: whatever it was, it no longer holds the forward.
        return true;
    };
    if !name.trim_end().starts_with("sshd") {
        return false;
    }
    !has_children(pid, proc_root)
}

fn has_children(pid: i32, proc_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return true;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("stat")).ok())
        .any(|stat| parent_of(&stat) == Some(pid))
}

/// The parent pid in a `/proc/<pid>/stat` line. The command name is in
/// parentheses and may itself contain spaces or parentheses, so the fields
/// are read after the last `)`.
fn parent_of(stat: &str) -> Option<i32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Build the page for an agent page's files: `<page>.md`, `<page>.meta.json`
/// and the annotations beside or inside `<page>.html`. The page identity is a
/// hash of the Markdown, so a changed page can never answer an older open.
pub fn page_from_agent_files(html: &Path, route: &str) -> Result<OpenPage, String> {
    let markdown_path = html.with_extension("md");
    let meta_path = html.with_extension("meta.json");
    let markdown = std::fs::read_to_string(&markdown_path)
        .map_err(|error| format!("{}: {error}", markdown_path.display()))?;
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&meta_path).map_err(|error| format!("{}: {error}", meta_path.display()))?,
    )
    .map_err(|error| format!("{}: {error}", meta_path.display()))?;
    let session = meta
        .get("session_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{}: no session_id; the page does not name its agent session",
                meta_path.display()
            )
        })?
        .to_string();
    // The same set `limpid-core feedback` reads: embedded plus sidecar.
    let replies = read_feedback(html).map_err(|error| format!("{}: {error}", html.display()))?;
    let annotations = serde_json::to_value(replies).map_err(|error| error.to_string())?;
    let revision = read_revision(html)?.revision;
    let remote_path = html
        .canonicalize()
        .unwrap_or_else(|_| html.to_path_buf())
        .to_string_lossy()
        .into_owned();
    Ok(OpenPage {
        route: route.to_string(),
        session,
        page_id: page_id_of(markdown.as_bytes()),
        instance: new_instance(),
        remote_path: Some(remote_path),
        markdown,
        annotations,
        revision,
        delivery: Delivery::Remote,
    })
}

fn new_instance() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("i-{}-{nanos}", std::process::id())
}

/// The relay's life: connect, open, report the outcome on `status`, then hold
/// the connection until it ends. A save from the hub is persisted on this
/// host and acknowledged with its revision; it never reaches the agent. A
/// submit composes the saved feedback here and delivers it to the listener of
/// the page's own agent session, and nowhere else.
pub fn serve(route: &RouteEnv, page: OpenPage, status: &mut dyn Write) -> Result<(), String> {
    let report = |status: &mut dyn Write, line: &str| {
        let _ = writeln!(status, "{line}");
        let _ = status.flush();
    };
    let identity = Identity {
        instance: page.instance.clone(),
        page_id: page.page_id.clone(),
        remote_path: page.remote_path.clone(),
        session: page.session.clone(),
    };
    let relay = match Relay::connect(&route.socket, &route.route) {
        Ok(relay) => relay.with_timeout(OPEN_TIMEOUT),
        Err(error) => {
            let message = connect_message(&route.socket, &error);
            report(status, &format!("error: {message}"));
            return Err(message);
        }
    };
    if let Err(error) = relay.open(page) {
        let message = open_message(&error);
        report(status, &format!("error: {message}"));
        return Err(message);
    }
    report(status, "ok");
    let login = relay.peer_pid();
    let mut since_login_check = Duration::ZERO;
    loop {
        // ssh will not end the login while a forwarded connection is open, so
        // the relay must let go as soon as the login is over: when its socket
        // path is gone (`limpid ssh` removes it) or its sshd has no sessions
        // left (a plain `ssh` login).
        if !route.socket.exists() {
            return Ok(());
        }
        if since_login_check >= LOGIN_CHECK_INTERVAL {
            since_login_check = Duration::ZERO;
            if login.is_some_and(|pid| login_ended(pid, Path::new("/proc"))) {
                return Ok(());
            }
        }
        since_login_check += POLL_INTERVAL;
        match relay.next_request(POLL_INTERVAL) {
            Ok(request) => answer(&relay, &request, &identity),
            Err(BridgeError::Timeout) => {}
            Err(_) => return Ok(()),
        }
    }
}

/// What the relay holds: the page it opened and the session it belongs to.
struct Identity {
    instance: String,
    page_id: String,
    remote_path: Option<String>,
    session: String,
}

/// Answer one request from the hub for the page this relay holds.
fn answer(relay: &Relay, request: &crate::bridge::Request, identity: &Identity) {
    let (instance, page_id, revision) = match &request.body {
        Body::Save {
            instance,
            page_id,
            revision,
            ..
        }
        | Body::Submit {
            instance,
            page_id,
            revision,
        } => (instance, page_id, *revision),
        _ => return,
    };
    if instance != &identity.instance || page_id != &identity.page_id {
        let _ = relay.fail(
            request,
            ErrorCode::PageMismatch,
            "the request is for a different page than this relay holds",
        );
        return;
    }
    let Some(html) = &identity.remote_path else {
        let _ = relay.fail(
            request,
            ErrorCode::DeliveryUnavailable,
            "the page has no path on this host to save to",
        );
        return;
    };
    let result = match &request.body {
        Body::Save { annotations, .. } => {
            apply_save(Path::new(html), page_id, instance, revision, annotations)
        }
        _ => submit_feedback(
            Path::new(html),
            &identity.session,
            page_id,
            revision,
            LISTENER_WAIT,
        ),
    };
    match result {
        Ok(stored) => {
            let _ = relay.ack(request, Some(stored));
        }
        Err((code, message)) => {
            let _ = relay.fail(request, code, message);
        }
    }
}

fn connect_message(socket: &Path, error: &BridgeError) -> String {
    match error {
        BridgeError::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => format!(
            "the bridge socket {} is stale: the SSH login that forwarded it has ended. Log in \
             again; if the forward then fails, set `StreamLocalBindUnlink yes` in this host's \
             sshd_config so a new login can replace the old socket",
            socket.display()
        ),
        BridgeError::Io(io) => format!(
            "cannot reach the bridge at {}: {io}; the login's socket forwarding is not active \
             (does ~/.ssh/config forward it with RemoteForward, does the local \
             `limpid bridge serve` run, and does the server allow AllowStreamLocalForwarding?)",
            socket.display()
        ),
        BridgeError::Remote {
            code: ErrorCode::IncompatibleVersion,
            message,
        } => format!(
            "this host and the machine that ran `limpid ssh` run incompatible limpid versions \
             ({message}); install the same limpid release on both"
        ),
        BridgeError::Remote { code, message } => {
            format!("the local limpid refused the connection ({code:?}): {message}")
        }
        other => format!("bridge handshake failed: {other}"),
    }
}

fn open_message(error: &BridgeError) -> String {
    match error {
        BridgeError::Remote { message, .. } => {
            format!("the local viewer did not open the page: {message}")
        }
        BridgeError::Timeout => "the local viewer did not answer in time".into(),
        BridgeError::Disconnected => {
            "the bridge connection closed before the page was accepted".into()
        }
        other => format!("{other}"),
    }
}

/// Entry point of the detached relay process: the page arrives as JSON on
/// standard input and status lines go to standard output.
pub fn relay_main(route: &RouteEnv) -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("could not read the page: {error}"))?;
    let page: OpenPage =
        serde_json::from_slice(&input).map_err(|error| format!("malformed page: {error}"))?;
    let mut stdout = std::io::stdout();
    serve(route, page, &mut stdout)
}

/// Start the relay detached from the caller and wait for its verdict. The
/// relay outlives this call; it has its own process group and no inherited
/// output, so the caller's pipes close when the caller exits.
pub fn open_through_relay(
    program: &Path,
    route: &RouteEnv,
    page: &OpenPage,
    timeout: Duration,
) -> Result<(), String> {
    let payload = serde_json::to_vec(page).map_err(|error| error.to_string())?;
    let mut command = Command::new(program);
    command
        .arg("--bridge-relay")
        .env(SOCKET_ENV, &route.socket)
        .env(ROUTE_ENV, &route.route)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the relay {}: {error}", program.display()))?;
    let mut stdin = child.stdin.take().ok_or("relay has no stdin")?;
    stdin
        .write_all(&payload)
        .map_err(|error| format!("relay exited before taking the page: {error}"))?;
    drop(stdin);
    let stdout = child.stdout.take().ok_or("relay has no stdout")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    match receiver.recv_timeout(timeout) {
        Ok(line) => match line.trim() {
            "ok" => Ok(()),
            "" => {
                let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
                Err(format!(
                    "the relay exited before opening the page ({status})"
                ))
            }
            other => {
                let _ = child.wait();
                Err(other.strip_prefix("error: ").unwrap_or(other).to_string())
            }
        },
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("the relay did not answer within {timeout:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn os(value: &str) -> Option<OsString> {
        Some(value.into())
    }

    #[test]
    fn should_report_no_route_when_neither_variable_is_set() {
        assert_eq!(route_from_env(None, None), Ok(None));
        assert_eq!(route_from_env(os(""), os("")), Ok(None));
    }

    #[test]
    fn should_return_the_route_when_both_variables_are_set() {
        let route = route_from_env(os("/tmp/x.sock"), os("abc"))
            .unwrap()
            .unwrap();
        assert_eq!(route.socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!(route.route, "abc");
    }

    #[test]
    fn should_make_a_fresh_route_when_only_the_socket_is_set() {
        let first = route_from_env(os("/tmp/x.sock"), None).unwrap().unwrap();
        let second = route_from_env(os("/tmp/x.sock"), None).unwrap().unwrap();
        assert_eq!(first.socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!(first.route.len(), 16);
        assert_ne!(first.route, second.route, "each process is its own route");
    }

    #[test]
    fn should_name_the_socket_variable_when_only_the_route_is_set() {
        let error = route_from_env(None, os("abc")).unwrap_err();
        assert!(error.contains(SOCKET_ENV), "{error}");
    }

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    fn private_socket(path: &Path) -> std::os::unix::net::UnixListener {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener
    }

    #[test]
    fn should_find_no_route_when_nothing_is_set_and_no_socket_is_forwarded() {
        let dir = scratch("discover-none");
        let found = discover_route(None, None, &dir.join("absent.sock"), uid()).unwrap();
        assert_eq!(found, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_use_the_forwarded_socket_when_it_is_private_and_owned_by_the_user() {
        let dir = scratch("discover-found");
        let path = dir.join("limpid-me.sock");
        let _listener = private_socket(&path);
        let found = discover_route(None, None, &path, uid()).unwrap().unwrap();
        assert_eq!(found.socket, path);
        assert_eq!(found.route.len(), 16);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_prefer_the_environment_when_it_names_a_socket() {
        let dir = scratch("discover-env");
        let path = dir.join("limpid-me.sock");
        let _listener = private_socket(&path);
        let found = discover_route(os("/elsewhere.sock"), os("r"), &path, uid())
            .unwrap()
            .unwrap();
        assert_eq!(found.socket, PathBuf::from("/elsewhere.sock"));
        assert_eq!(found.route, "r");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_still_report_a_lone_route_when_a_socket_is_forwarded() {
        let dir = scratch("discover-lone");
        let path = dir.join("limpid-me.sock");
        let _listener = private_socket(&path);
        assert!(discover_route(None, os("r"), &path, uid()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_the_path_when_it_is_not_a_socket() {
        let dir = scratch("discover-file");
        let path = dir.join("limpid-me.sock");
        std::fs::write(&path, "").unwrap();
        let error = discover_route(None, None, &path, uid()).unwrap_err();
        assert!(error.contains("not a socket"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_the_path_when_it_is_a_symlink_to_a_socket() {
        let dir = scratch("discover-link");
        let real = dir.join("real.sock");
        let _listener = private_socket(&real);
        let path = dir.join("limpid-me.sock");
        std::os::unix::fs::symlink(&real, &path).unwrap();
        let error = discover_route(None, None, &path, uid()).unwrap_err();
        assert!(error.contains("not a socket"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_the_socket_when_another_user_owns_it() {
        let dir = scratch("discover-owner");
        let path = dir.join("limpid-me.sock");
        let _listener = private_socket(&path);
        let error = discover_route(None, None, &path, uid() + 1).unwrap_err();
        assert!(error.contains("not to you"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_the_socket_when_other_users_can_reach_it() {
        let dir = scratch("discover-mode");
        let path = dir.join("limpid-me.sock");
        let _listener = private_socket(&path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let error = discover_route(None, None, &path, uid()).unwrap_err();
        assert!(error.contains("other users"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_name_the_forward_after_the_user_when_no_path_is_configured() {
        assert_eq!(
            default_socket_for("tj"),
            PathBuf::from("/tmp/limpid-tj.sock")
        );
    }

    #[test]
    fn should_look_up_the_current_user_name_when_asked_for_this_uid() {
        let name = user_name(uid()).unwrap();
        assert!(!name.is_empty());
        assert!(!name.contains('/'), "{name}");
    }

    /// A fake /proc: each entry is (pid, comm, parent pid).
    fn fake_proc(name: &str, processes: &[(i32, &str, i32)]) -> PathBuf {
        let root = scratch(name);
        for (pid, comm, parent) in processes {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(
                dir.join("stat"),
                format!("{pid} ({comm}) S {parent} {pid} {pid} 0 -1 4194560"),
            )
            .unwrap();
        }
        std::fs::create_dir_all(root.join("self")).unwrap();
        root
    }

    #[test]
    fn should_keep_the_login_when_its_sshd_still_has_a_session() {
        let root = fake_proc("login-alive", &[(10, "sshd-session", 1), (11, "bash", 10)]);
        assert!(!login_ended(10, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn should_end_the_login_when_its_sshd_has_no_session_left() {
        // The relay itself was reparented away from the shell long ago.
        let root = fake_proc("login-over", &[(10, "sshd-session", 1), (12, "limpid", 1)]);
        assert!(login_ended(10, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn should_end_the_login_when_the_listening_process_is_gone() {
        let root = fake_proc("login-gone", &[(11, "bash", 1)]);
        assert!(login_ended(10, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn should_watch_an_older_sshd_when_its_process_is_named_sshd() {
        let root = fake_proc("login-old", &[(10, "sshd", 1)]);
        assert!(login_ended(10, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn should_never_end_the_login_when_the_listener_is_not_sshd() {
        // A hub in a test process, or a forwarder that is not OpenSSH.
        let root = fake_proc("login-other", &[(10, "limpid", 1)]);
        assert!(!login_ended(10, &root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn should_read_the_parent_when_the_command_name_has_spaces_and_parentheses() {
        assert_eq!(parent_of("77 (a) b (c) S 5 77 77 0"), Some(5));
        assert_eq!(parent_of("77 (sshd-session) S 1 77"), Some(1));
        assert_eq!(parent_of("garbage"), None);
        assert_eq!(parent_of("77 (x) S"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn should_see_a_live_child_when_reading_the_real_proc() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        assert!(has_children(std::process::id() as i32, Path::new("/proc")));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-relay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_build_a_page_from_agent_files_when_they_are_complete() {
        let dir = scratch("build");
        let html = dir.join("claude-1.html");
        std::fs::write(&html, "<html>").unwrap();
        std::fs::write(dir.join("claude-1.md"), "# Plan\n\nnaïve 日本語\n").unwrap();
        std::fs::write(
            dir.join("claude-1.meta.json"),
            r#"{"agent":"claude","session_id":"sess-9"}"#,
        )
        .unwrap();
        let page = page_from_agent_files(&html, "route-1").unwrap();
        assert_eq!(page.route, "route-1");
        assert_eq!(page.session, "sess-9");
        assert_eq!(page.markdown, "# Plan\n\nnaïve 日本語\n");
        assert!(page.page_id.starts_with("md-"));
        assert_eq!(page.delivery, Delivery::Remote);
        assert!(page.remote_path.unwrap().ends_with("claude-1.html"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_give_distinct_instances_and_one_page_id_when_the_same_page_opens_twice() {
        let dir = scratch("twice");
        let html = dir.join("p.html");
        std::fs::write(&html, "<html>").unwrap();
        std::fs::write(dir.join("p.md"), "# P\n").unwrap();
        std::fs::write(dir.join("p.meta.json"), r#"{"session_id":"s"}"#).unwrap();
        let a = page_from_agent_files(&html, "r").unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let b = page_from_agent_files(&html, "r").unwrap();
        assert_eq!(a.page_id, b.page_id);
        assert_ne!(a.instance, b.instance);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_a_page_when_the_meta_names_no_session() {
        let dir = scratch("nosession");
        let html = dir.join("p.html");
        std::fs::write(&html, "<html>").unwrap();
        std::fs::write(dir.join("p.md"), "# P\n").unwrap();
        std::fs::write(dir.join("p.meta.json"), r#"{"agent":"claude"}"#).unwrap();
        let error = page_from_agent_files(&html, "r").unwrap_err();
        assert!(error.contains("session_id"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_a_page_when_the_markdown_or_meta_is_missing_or_malformed() {
        let dir = scratch("missing");
        let html = dir.join("p.html");
        std::fs::write(&html, "<html>").unwrap();
        assert!(page_from_agent_files(&html, "r").is_err());
        std::fs::write(dir.join("p.md"), "# P\n").unwrap();
        assert!(page_from_agent_files(&html, "r").is_err());
        std::fs::write(dir.join("p.meta.json"), "not json").unwrap();
        assert!(page_from_agent_files(&html, "r").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_report_a_connect_error_naming_forwarding_when_the_socket_is_absent() {
        let dir = scratch("noconnect");
        let route = RouteEnv {
            socket: dir.join("absent.sock"),
            route: "r".into(),
        };
        let page = OpenPage {
            route: "r".into(),
            session: "s".into(),
            page_id: "p".into(),
            instance: "i".into(),
            remote_path: None,
            markdown: "# x".into(),
            annotations: serde_json::Value::Null,
            revision: 0,
            delivery: Delivery::Remote,
        };
        let mut status = Vec::new();
        let error = serve(&route, page, &mut status).unwrap_err();
        assert!(error.contains("forwarding"), "{error}");
        assert!(String::from_utf8(status).unwrap().starts_with("error: "));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_name_both_sides_and_the_remedy_when_the_versions_are_incompatible() {
        let error = BridgeError::Remote {
            code: ErrorCode::IncompatibleVersion,
            message: "peer speaks version 2; this side speaks 1".into(),
        };
        let message = connect_message(Path::new("/tmp/x.sock"), &error);
        assert!(
            message.contains("incompatible limpid versions"),
            "{message}"
        );
        assert!(
            message.contains("speaks version 2") && message.contains("speaks 1"),
            "{message}"
        );
        assert!(message.contains("same limpid release"), "{message}");
    }
}
