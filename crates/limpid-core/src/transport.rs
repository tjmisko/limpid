use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Dark,
    Light,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub path: PathBuf,
    pub theme: Scheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(default)]
    pub source_backed: bool,
}

pub fn socket_path(runtime: &Path, scheme: Scheme) -> PathBuf {
    runtime.join(format!("limpid-{}.sock", scheme.as_str()))
}

pub fn send(socket: &Path, handoff: &Handoff) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(socket)?;
    serde_json::to_writer(&mut stream, handoff)?;
    stream.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSignal {
    pub page: PathBuf,
    pub message: String,
}

pub const FEEDBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub fn send_feedback(
    socket: &Path,
    signal: &FeedbackSignal,
) -> Result<bool, Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(FEEDBACK_TIMEOUT))?;
    stream.set_read_timeout(Some(FEEDBACK_TIMEOUT))?;
    let mut writer = &stream;
    serde_json::to_writer(&mut writer, signal)?;
    writer.write_all(b"\n")?;
    let mut bytes = Vec::new();
    BufReader::new(&stream)
        .take(1 << 20)
        .read_until(b'\n', &mut bytes)?;
    if bytes.last() != Some(&b'\n') {
        return Err("feedback listener closed without a reply".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(value.get("ok").and_then(serde_json::Value::as_bool) == Some(true))
}

pub fn receive(stream: UnixStream) -> Option<Handoff> {
    if !same_user(&stream) {
        return None;
    }
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok()?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(1 << 20)
        .read_until(b'\n', &mut bytes)
        .ok()?;
    if bytes.last() != Some(&b'\n') {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub fn serve(listener: UnixListener, sender: mpsc::Sender<Handoff>) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        let Some(request) = receive(stream) else {
            continue;
        };
        if sender.send(request).is_err() {
            break;
        }
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
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
        && credentials.uid == unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_round_trip_and_socket_names() {
        let request = Handoff {
            path: "/tmp/agent.html".into(),
            theme: Scheme::Dark,
            pane: Some("3".into()),
            source: None,
            source_hash: None,
            source_backed: false,
        };
        assert_eq!(
            serde_json::from_slice::<Handoff>(&serde_json::to_vec(&request).unwrap()).unwrap(),
            request
        );
        assert_eq!(
            socket_path(Path::new("/run/user/1000"), Scheme::Light),
            PathBuf::from("/run/user/1000/limpid-light.sock")
        );
    }

    #[test]
    fn receives_one_line_and_rejects_unknown_fields() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let handoff = Handoff {
            path: "/tmp/a.html".into(),
            theme: Scheme::Light,
            pane: None,
            source: None,
            source_hash: None,
            source_backed: false,
        };
        serde_json::to_writer(&mut client, &handoff).unwrap();
        client.write_all(b"\n").unwrap();
        assert_eq!(receive(server), Some(handoff));

        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(b"{\"path\":\"/tmp/a.html\",\"theme\":\"dark\",\"bad\":1}\n")
            .unwrap();
        assert!(receive(server).is_none());
    }

    #[test]
    fn handoff_rejects_malformed_enums_and_missing_required_fields() {
        for payload in [
            r#"{"path":"/tmp/a.html","theme":"sepia"}"#,
            r#"{"path":"/tmp/a.html","theme":"dark","origin":"hook"}"#,
            r#"{"path":"/tmp/a.html"}"#,
            r#"{"theme":"dark"}"#,
            "garbage",
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            client.write_all(payload.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            assert!(receive(server).is_none(), "accepted {payload}");
        }
    }

    #[test]
    fn handoff_limits_payload_size_and_preserves_optional_fields() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(b"{\"path\":\"/tmp/a.html\",\"theme\":\"dark\"}\n")
            .unwrap();
        let request = receive(server).unwrap();
        assert!(request.pane.is_none());

        let (mut client, server) = UnixStream::pair().unwrap();
        let oversized = "x".repeat(1 << 20);
        let payload = format!("{{\"path\":\"{oversized}\",\"theme\":\"dark\"}}\n");
        std::thread::spawn(move || {
            let _ = client.write_all(payload.as_bytes());
        });
        assert!(receive(server).is_none());
    }

    #[test]
    fn rejects_incomplete_and_overlong_lines() {
        for payload in [
            b"{\"path\":\"/tmp/a.html\",\"theme\":\"dark\"}".to_vec(),
            format!(
                "{{\"path\":\"/tmp/{}\",\"theme\":\"dark\"}}\n",
                "a".repeat(1 << 20)
            )
            .into_bytes(),
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                let _ = client.write_all(&payload);
            });
            assert!(receive(server).is_none());
        }
    }

    #[test]
    fn feedback_round_trips_and_reports_ok() {
        let directory = std::env::temp_dir().join(format!(
            "limpid-feedback-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("feedback.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            for reply in [
                b"{\"ok\":true}\n".as_slice(),
                b"{\"ok\":false}\n".as_slice(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let signal: FeedbackSignal = serde_json::from_str(&line).unwrap();
                assert_eq!(signal.page.file_name().unwrap(), "page.html");
                stream.write_all(reply).unwrap();
            }
        });
        let signal = FeedbackSignal {
            page: PathBuf::from("/tmp/page.html"),
            message: "q1: yes".into(),
        };
        assert!(send_feedback(&socket, &signal).unwrap());
        assert!(!send_feedback(&socket, &signal).unwrap());
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn feedback_refused_and_timeout_are_errors() {
        let directory = std::env::temp_dir().join(format!(
            "limpid-feedback-errors-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let signal = FeedbackSignal {
            page: PathBuf::from("/tmp/page.html"),
            message: "q1: yes".into(),
        };
        assert!(send_feedback(&directory.join("missing.sock"), &signal).is_err());

        let socket = directory.join("silent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(FEEDBACK_TIMEOUT + std::time::Duration::from_millis(500));
            drop(stream);
        });
        assert!(send_feedback(&socket, &signal).is_err());
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn server_skips_bad_handoffs_and_delivers_the_next_valid_one() {
        let directory = std::env::temp_dir().join(format!(
            "limpid-transport-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("handoff.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = std::thread::spawn(move || serve(listener, sender));

        let mut invalid = UnixStream::connect(&socket).unwrap();
        invalid.write_all(b"not json\n").unwrap();
        drop(invalid);

        let request = Handoff {
            path: "/tmp/valid.html".into(),
            theme: Scheme::Light,
            pane: Some("7".into()),
            source: None,
            source_hash: None,
            source_backed: false,
        };
        send(&socket, &request).unwrap();
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            request
        );
        drop(receiver);
        send(&socket, &request).unwrap();
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
