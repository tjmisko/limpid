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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    #[default]
    User,
    Hook,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    pub theme: Scheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
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
            slot: Some("session-1".into()),
            origin: Origin::Hook,
            theme: Scheme::Dark,
            pane: Some("3".into()),
            source: None,
            source_hash: None,
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
            slot: None,
            origin: Origin::User,
            theme: Scheme::Light,
            pane: None,
            source: None,
            source_hash: None,
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
}
