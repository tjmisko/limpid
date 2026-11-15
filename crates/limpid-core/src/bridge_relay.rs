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
};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
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
/// How long the opening command waits for the relay's status line.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEnv {
    pub socket: PathBuf,
    pub route: String,
}

/// The route in the environment. Neither variable set means no route; one
/// without the other is a broken bootstrap and is reported, never ignored.
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
        (Some(_), None) => Err(format!(
            "{SOCKET_ENV} is set but {ROUTE_ENV} is not; start the login with `limpid ssh`"
        )),
        (None, Some(_)) => Err(format!(
            "{ROUTE_ENV} is set but {SOCKET_ENV} is not; start the login with `limpid ssh`"
        )),
    }
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
/// host and acknowledged with its revision; it never reaches the agent.
/// Submitting is not implemented and is refused, and a page is never answered
/// locally.
pub fn serve(route: &RouteEnv, page: OpenPage, status: &mut dyn Write) -> Result<(), String> {
    let report = |status: &mut dyn Write, line: &str| {
        let _ = writeln!(status, "{line}");
        let _ = status.flush();
    };
    let identity = (
        page.instance.clone(),
        page.page_id.clone(),
        page.remote_path.clone(),
    );
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
    loop {
        // The login's wrapper removes the socket path when its shell exits.
        // ssh will not end the login while a forwarded connection is open, so
        // the relay must let go as soon as the path is gone.
        if !route.socket.exists() {
            return Ok(());
        }
        match relay.next_request(POLL_INTERVAL) {
            Ok(request) => answer(&relay, &request, &identity),
            Err(BridgeError::Timeout) => {}
            Err(_) => return Ok(()),
        }
    }
}

/// Answer one request from the hub for the page this relay holds.
fn answer(
    relay: &Relay,
    request: &crate::bridge::Request,
    (instance, page_id, remote_path): &(String, String, Option<String>),
) {
    let Body::Save {
        instance: saved_instance,
        page_id: saved_page,
        revision,
        annotations,
    } = &request.body
    else {
        let _ = relay.fail(
            request,
            ErrorCode::DeliveryUnavailable,
            "submitting feedback is not available yet; it is saved on this host only",
        );
        return;
    };
    if saved_instance != instance || saved_page != page_id {
        let _ = relay.fail(
            request,
            ErrorCode::PageMismatch,
            "the save is for a different page than this relay holds",
        );
        return;
    }
    let Some(html) = remote_path else {
        let _ = relay.fail(
            request,
            ErrorCode::DeliveryUnavailable,
            "the page has no path on this host to save to",
        );
        return;
    };
    match apply_save(Path::new(html), page_id, instance, *revision, annotations) {
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
        BridgeError::Io(io) => format!(
            "cannot reach the bridge at {}: {io}; the login's socket forwarding is not active \
             (was this shell started with `limpid ssh`, and does the server allow \
             AllowStreamLocalForwarding?)",
            socket.display()
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
    fn should_name_the_missing_variable_when_only_one_is_set() {
        let error = route_from_env(os("/tmp/x.sock"), None).unwrap_err();
        assert!(error.contains(ROUTE_ENV), "{error}");
        let error = route_from_env(None, os("abc")).unwrap_err();
        assert!(error.contains(SOCKET_ENV), "{error}");
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
}
