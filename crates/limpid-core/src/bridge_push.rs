//! The viewer's half of saving: push locally saved feedback to the sender.
//!
//! The viewer saves feedback to the local cache first, exactly as for any
//! page, and then asks the launcher's save socket to push it. The launcher
//! sends what the cache holds, so the local copy is the source of truth and
//! survives every failure here. Each page's pushes are serialised, and an
//! unchanged page is not sent again. Feedback is "saved remotely" exactly when
//! the sender acknowledged the digest of the local copy; the acknowledgment is
//! recorded before the viewer is told.
//!
//! The service works over one JSON line per connection in each direction, and
//! never runs on the viewer's event loop: the viewer asks from a worker.

use crate::{
    bridge::{BridgeError, ErrorCode, Hub},
    bridge_import::{Importer, SyncState, replies_digest},
    replies::AgentReplies,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// How long the viewer waits for the launcher's answer: the launcher's own
/// request timeout plus headroom.
pub const PUSH_WAIT: Duration = Duration::from_secs(15);
const MAX_LINE: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushRequest {
    pub instance: String,
    pub page_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushState {
    /// The sender acknowledged this feedback.
    RemoteSaved,
    /// The local copy is safe; the sender does not hold it.
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushReply {
    pub state: PushState,
    #[serde(default)]
    pub revision: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

impl PushReply {
    fn local_only(error: impl Into<String>) -> Self {
        Self {
            state: PushState::LocalOnly,
            revision: None,
            error: Some(error.into()),
        }
    }

    /// The line the viewer shows.
    pub fn describe(&self) -> String {
        match (self.state, &self.error) {
            (PushState::RemoteSaved, _) => match self.revision {
                Some(revision) => format!("Saved locally and on the remote (revision {revision})"),
                None => "Saved locally and on the remote".into(),
            },
            (PushState::LocalOnly, Some(error)) => {
                format!("Saved locally only, not on the remote: {error}")
            }
            (PushState::LocalOnly, None) => "Saved locally only, not on the remote".into(),
        }
    }
}

/// Describe a bridge failure for the viewer, without claiming more than is known.
fn describe_failure(error: &BridgeError) -> String {
    match error {
        BridgeError::Remote { code, message } => match code {
            ErrorCode::DeliveryUnavailable => {
                format!("the remote connection is gone ({message}); saved copy kept")
            }
            _ => message.clone(),
        },
        BridgeError::Timeout => {
            "the remote did not confirm in time; it may or may not have saved; local copy kept"
                .into()
        }
        BridgeError::Disconnected => {
            "the remote connection closed before confirming; local copy kept".into()
        }
        other => other.to_string(),
    }
}

pub struct PushService {
    path: PathBuf,
    core: Arc<PushCore>,
    stop: Arc<AtomicBool>,
}

struct PushCore {
    importer: Arc<Importer>,
    hub: Arc<Hub>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl PushCore {
    fn push(&self, request: &PushRequest) -> PushReply {
        let Some((imported, page_id)) = self.importer.imported(&request.instance) else {
            return PushReply::local_only("this page is not open here; saved copy kept");
        };
        if page_id != request.page_id {
            return PushReply::local_only("the request names a different page than was opened");
        }
        let lock = self
            .locks
            .lock()
            .unwrap()
            .entry(request.instance.clone())
            .or_default()
            .clone();
        let _serial = lock.lock().unwrap();
        let replies = match AgentReplies::load(&imported.sidecar) {
            Ok(replies) => replies,
            Err(error) => return PushReply::local_only(format!("local copy unreadable: {error}")),
        };
        let digest = replies_digest(&replies);
        let state = match SyncState::load(&imported) {
            Ok(state) => state,
            Err(error) => return PushReply::local_only(error),
        };
        if state.synced_digest.as_deref() == Some(digest.as_str()) {
            return PushReply {
                state: PushState::RemoteSaved,
                revision: Some(state.revision),
                error: None,
            };
        }
        let annotations = match serde_json::to_value(&replies) {
            Ok(value) => value,
            Err(error) => return PushReply::local_only(error.to_string()),
        };
        // Recorded before the network call: whatever happens next, the file
        // says this digest is not known to be on the sender.
        let pending = SyncState {
            revision: state.revision,
            synced_digest: None,
        };
        if let Err(error) = pending.store(&imported) {
            return PushReply::local_only(error);
        }
        let revision = state.revision + 1;
        match self
            .hub
            .save(&request.instance, &page_id, revision, annotations)
        {
            Ok(acknowledged) => {
                let revision = acknowledged.unwrap_or(revision);
                let saved = SyncState {
                    revision,
                    synced_digest: Some(digest),
                };
                if let Err(error) = saved.store(&imported) {
                    return PushReply::local_only(error);
                }
                PushReply {
                    state: PushState::RemoteSaved,
                    revision: Some(revision),
                    error: None,
                }
            }
            Err(error) => PushReply::local_only(describe_failure(&error)),
        }
    }
}

impl PushService {
    /// Bind a private (0600) socket at `path`.
    pub fn bind(path: &Path, importer: Arc<Importer>, hub: Arc<Hub>) -> std::io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let core = Arc::new(PushCore {
            importer,
            hub,
            locks: Mutex::new(HashMap::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (accept_core, accept_stop) = (core.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let core = accept_core.clone();
                thread::spawn(move || serve_one(&core, stream));
            }
        });
        Ok(Self {
            path: path.to_path_buf(),
            core,
            stop,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Push one page's feedback now; the socket handler calls the same.
    pub fn push(&self, request: &PushRequest) -> PushReply {
        self.core.push(request)
    }

    /// Stop accepting and remove the socket this service created.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for PushService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve_one(core: &PushCore, stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = Vec::new();
    let read = BufReader::new(&stream)
        .take(MAX_LINE)
        .read_until(b'\n', &mut line);
    let reply = match (read, serde_json::from_slice::<PushRequest>(&line)) {
        (Ok(_), Ok(request)) => core.push(&request),
        _ => PushReply::local_only("malformed save request"),
    };
    let mut writer = &stream;
    if let Ok(bytes) = serde_json::to_vec(&reply) {
        let _ = writer.write_all(&bytes);
        let _ = writer.write_all(b"\n");
    }
}

/// Ask a launcher's save socket to push one page. Any failure to ask or to
/// understand the answer is reported as local-only: the viewer has already
/// saved the local copy and must not claim the remote has it.
pub fn request_push(socket: &Path, request: &PushRequest, wait: Duration) -> PushReply {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) => {
            return PushReply::local_only(format!(
                "the login that opened this page is gone ({error}); saved copy kept"
            ));
        }
    };
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let mut writer = &stream;
    let sent = serde_json::to_vec(request)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            writer
                .write_all(&bytes)
                .and_then(|()| writer.write_all(b"\n"))
                .map_err(|error| error.to_string())
        });
    if let Err(error) = sent {
        return PushReply::local_only(format!("could not ask the launcher: {error}"));
    }
    let mut line = Vec::new();
    match BufReader::new(&stream)
        .take(MAX_LINE)
        .read_until(b'\n', &mut line)
    {
        Ok(_) if line.last() == Some(&b'\n') => serde_json::from_slice(&line)
            .unwrap_or_else(|_| PushReply::local_only("the launcher's answer was malformed")),
        _ => PushReply::local_only(
            "the launcher did not answer; the remote may or may not have saved; local copy kept",
        ),
    }
}
