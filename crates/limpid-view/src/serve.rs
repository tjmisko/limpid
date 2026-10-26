//! The handoff socket. It speaks `limpid_core::transport::Handoff` lines, plus
//! `{"close":true}` for tests, which is accepted only when the process was
//! started with `LIMPID_WINDOW_CLASS` set.

use limpid_core::transport::Handoff;
use std::{
    io::{BufRead, BufReader, Read},
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    time::Duration,
};

const MAX_LINE: u64 = 1 << 20;
const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open(Handoff),
    Close,
}

/// Parse one request line. `allow_close` gates the test-only close request.
pub fn parse_request(line: &[u8], allow_close: bool) -> Option<Request> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let object = value.as_object()?;
    if object.contains_key("close") {
        let is_plain_close = object.len() == 1 && object["close"] == serde_json::Value::Bool(true);
        return (allow_close && is_plain_close).then_some(Request::Close);
    }
    serde_json::from_value(value).ok().map(Request::Open)
}

/// Read and parse one request from a connection. Rejects other users, slow
/// writers, unterminated lines and overlong lines.
pub fn receive_request(stream: UnixStream, allow_close: bool) -> Option<Request> {
    if !same_user(&stream) {
        return None;
    }
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(MAX_LINE)
        .read_until(b'\n', &mut bytes)
        .ok()?;
    if bytes.last() != Some(&b'\n') {
        return None;
    }
    parse_request(&bytes, allow_close)
}

/// Accept connections until `deliver` returns false (the event loop is gone).
pub fn serve(listener: UnixListener, allow_close: bool, deliver: impl Fn(Request) -> bool) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        let Some(request) = receive_request(stream, allow_close) else {
            continue;
        };
        if !deliver(request) {
            break;
        }
    }
}

/// Bind the socket, replacing a stale file and refusing a live one.
pub fn bind_socket(path: &Path) -> Result<UnixListener, String> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("a viewer is already running at {}", path.display()));
        }
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    let listener =
        UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    Ok(listener)
}

fn same_user(stream: &UnixStream) -> bool {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `size` are valid for the call and `size`
    // holds the buffer length, as SO_PEERCRED requires.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut size,
        )
    };
    status == 0
        && size as usize == std::mem::size_of::<libc::ucred>()
        // SAFETY: geteuid has no preconditions.
        && credentials.uid == unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc;

    #[test]
    fn should_parse_a_handoff_when_the_line_is_valid() {
        let request = parse_request(br#"{"path":"/tmp/a.md","theme":"light"}"#, false).unwrap();
        let Request::Open(handoff) = request else {
            panic!("expected an open request");
        };
        assert_eq!(handoff.path, Path::new("/tmp/a.md"));
    }

    #[test]
    fn should_reject_a_close_request_when_it_is_not_allowed() {
        assert_eq!(parse_request(br#"{"close":true}"#, false), None);
        assert_eq!(
            parse_request(br#"{"close":true}"#, true),
            Some(Request::Close)
        );
    }

    #[test]
    fn should_reject_malformed_requests_when_parsing() {
        for payload in [
            &br#"{"close":false}"#[..],
            br#"{"close":true,"path":"/x"}"#,
            br#"{"close":"yes"}"#,
            br#"{"path":"/tmp/a.md"}"#,
            br#"{"path":"/tmp/a.md","theme":"sepia"}"#,
            br#"{"path":"/tmp/a.md","theme":"dark","extra":1}"#,
            b"[1,2]",
            b"null",
            b"garbage",
            b"",
        ] {
            assert_eq!(
                parse_request(payload, true),
                None,
                "{:?}",
                String::from_utf8_lossy(payload)
            );
        }
    }

    #[test]
    fn should_require_a_terminated_line_when_receiving() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(br#"{"path":"/tmp/a.md","theme":"dark"}"#)
            .unwrap();
        drop(client);
        assert_eq!(receive_request(server, true), None);

        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(b"{\"path\":\"/tmp/a.md\",\"theme\":\"dark\"}\n")
            .unwrap();
        assert!(matches!(
            receive_request(server, true),
            Some(Request::Open(_))
        ));
    }

    #[test]
    fn should_reject_overlong_lines_when_receiving() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let payload = format!(
            "{{\"path\":\"/{}\",\"theme\":\"dark\"}}\n",
            "a".repeat(1 << 20)
        );
        std::thread::spawn(move || {
            let _ = client.write_all(payload.as_bytes());
        });
        assert_eq!(receive_request(server, true), None);
    }

    #[test]
    fn should_skip_bad_connections_and_deliver_the_next_valid_one_when_serving() {
        let dir = std::env::temp_dir().join(format!("limpid-view-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("view.sock");
        let listener = bind_socket(&socket).unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            serve(listener, true, move |request| sender.send(request).is_ok());
        });

        let mut bad = UnixStream::connect(&socket).unwrap();
        bad.write_all(b"not json\n").unwrap();
        drop(bad);
        let mut close = UnixStream::connect(&socket).unwrap();
        close.write_all(b"{\"close\":true}\n").unwrap();
        drop(close);

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            Request::Close
        );
        drop(receiver);
        // The next delivery fails and ends the loop.
        let mut last = UnixStream::connect(&socket).unwrap();
        last.write_all(b"{\"close\":true}\n").unwrap();
        drop(last);
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_replace_a_stale_socket_and_refuse_a_live_one_when_binding() {
        let dir = std::env::temp_dir().join(format!("limpid-view-bind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("view.sock");
        let first = bind_socket(&socket).unwrap();
        assert!(
            bind_socket(&socket)
                .unwrap_err()
                .contains("already running")
        );
        drop(first);
        // The file remains but nothing listens: stale.
        assert!(socket.exists());
        let second = bind_socket(&socket).unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(second);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
