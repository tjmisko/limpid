//! Versioned bridge protocol for carrying agent pages between machines.
//!
//! A *relay* (the page sender, typically remote) connects to a *hub* (the
//! viewer side, local) over a Unix socket. Frames are a 4-byte big-endian
//! length followed by a JSON body. The relay sends `Open` and `Close`; the
//! hub sends `Save` and `Submit` back over the connection that opened the
//! page. Remote paths are metadata only; neither side opens the other's files.
//!
//! A page opened with [`Delivery::Remote`] only ever answers over its own
//! connection. When that connection is gone the operation fails; it never
//! falls back to local clipboard or terminal delivery.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    IncompatibleVersion,
    HandshakeRequired,
    Malformed,
    RouteMismatch,
    DuplicateInstance,
    UnknownInstance,
    PageMismatch,
    Rejected,
    DeliveryUnavailable,
    FrameTooLarge,
    /// A submission may or may not have reached the agent.
    SubmitUncertain,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Responses go back over the opening connection and nowhere else.
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenPage {
    /// Login route; must equal the route announced in the handshake.
    pub route: String,
    /// Agent session the page belongs to.
    pub session: String,
    /// Immutable identity of the page content accepted at open time.
    pub page_id: String,
    /// Identifies this particular open of the page.
    pub instance: String,
    /// Path on the sending machine; metadata only.
    #[serde(default)]
    pub remote_path: Option<String>,
    pub markdown: String,
    #[serde(default)]
    pub annotations: serde_json::Value,
    /// The sender's annotation revision at open time; a save names the next one.
    #[serde(default)]
    pub revision: u64,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Hello {
        version: u32,
        route: String,
    },
    HelloAck {
        version: u32,
    },
    Open(OpenPage),
    Save {
        instance: String,
        page_id: String,
        revision: u64,
        annotations: serde_json::Value,
    },
    Submit {
        instance: String,
        page_id: String,
        revision: u64,
    },
    Close {
        instance: String,
    },
    Ack {
        reply_to: u64,
        #[serde(default)]
        revision: Option<u64>,
    },
    Error {
        #[serde(default)]
        reply_to: Option<u64>,
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    pub id: u64,
    pub body: Body,
}

#[derive(Debug)]
pub enum BridgeError {
    Io(io::Error),
    FrameTooLarge(usize),
    Malformed(String),
    Disconnected,
    Timeout,
    Remote { code: ErrorCode, message: String },
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "bridge io error: {e}"),
            Self::FrameTooLarge(n) => {
                write!(f, "frame of {n} bytes exceeds {MAX_FRAME_BYTES} limit")
            }
            Self::Malformed(m) => write!(f, "malformed frame: {m}"),
            Self::Disconnected => write!(f, "bridge connection closed"),
            Self::Timeout => write!(f, "bridge request timed out"),
            Self::Remote { code, message } => write!(f, "bridge error {code:?}: {message}"),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<io::Error> for BridgeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, BridgeError> {
    let body = serde_json::to_vec(frame).map_err(|e| BridgeError::Malformed(e.to_string()))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(BridgeError::FrameTooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// One decoded unit: a frame, or a body that was well framed but unusable.
#[derive(Debug)]
pub enum Decoded {
    Frame(Frame),
    Invalid(String),
}

/// Incremental decoder tolerant of arbitrary fragmentation.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Next complete item, or `Ok(None)` when more bytes are needed. A length
    /// beyond the limit is fatal: the stream cannot be resynchronised.
    pub fn next_item(&mut self) -> Result<Option<Decoded>, BridgeError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(BridgeError::FrameTooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let body: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
        Ok(Some(match serde_json::from_slice::<Frame>(&body) {
            Ok(frame) => Decoded::Frame(frame),
            Err(e) => Decoded::Invalid(e.to_string()),
        }))
    }
}

/// Read until one item is available. `Ok(None)` means clean EOF between frames.
fn read_item(stream: &mut UnixStream, dec: &mut Decoder) -> Result<Option<Decoded>, BridgeError> {
    loop {
        if let Some(item) = dec.next_item()? {
            return Ok(Some(item));
        }
        let mut chunk = [0u8; 8192];
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(BridgeError::Timeout);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            return if dec.buf.is_empty() {
                Ok(None)
            } else {
                Err(BridgeError::Disconnected)
            };
        }
        dec.push(&chunk[..n]);
    }
}

/// Shared write side plus request/ack correlation for one connection.
struct Peer {
    writer: Mutex<UnixStream>,
    pending: Mutex<HashMap<u64, Sender<Body>>>,
    next_id: AtomicU64,
    open: AtomicBool,
}

impl Peer {
    fn new(stream: UnixStream) -> Arc<Self> {
        Arc::new(Self {
            writer: Mutex::new(stream),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            open: AtomicBool::new(true),
        })
    }

    fn send(&self, frame: &Frame) -> Result<(), BridgeError> {
        if !self.open.load(Ordering::SeqCst) {
            return Err(BridgeError::Disconnected);
        }
        let bytes = encode_frame(frame)?;
        let mut w = self.writer.lock().unwrap();
        w.write_all(&bytes).map_err(|_| BridgeError::Disconnected)
    }

    fn reply(&self, body: Body) -> Result<(), BridgeError> {
        self.send(&Frame { id: 0, body })
    }

    fn request(&self, body: Body, timeout: Duration) -> Result<Option<u64>, BridgeError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.send(&Frame { id, body }) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        let result = rx.recv_timeout(timeout);
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Body::Ack { revision, .. }) => Ok(revision),
            Ok(Body::Error { code, message, .. }) => Err(BridgeError::Remote { code, message }),
            Ok(_) => Err(BridgeError::Malformed("unexpected reply".into())),
            Err(RecvTimeoutError::Timeout) => Err(BridgeError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(BridgeError::Disconnected),
        }
    }

    /// Route an Ack or Error to its waiter. Returns false if it was not a reply.
    fn deliver_reply(&self, body: &Body) -> bool {
        let reply_to = match body {
            Body::Ack { reply_to, .. } => Some(*reply_to),
            Body::Error { reply_to, .. } => *reply_to,
            _ => return false,
        };
        if let Some(id) = reply_to
            && let Some(tx) = self.pending.lock().unwrap().get(&id)
        {
            let _ = tx.send(body.clone());
        }
        true
    }

    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
        // Dropping the senders wakes every waiter with Disconnected.
        self.pending.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------------
// Hub (viewer side)
// ---------------------------------------------------------------------

/// Receives pages on the hub side. `open` runs on the connection thread; its
/// result becomes the opening acknowledgment.
pub trait PageSink: Send + Sync + 'static {
    fn open(&self, page: &OpenPage) -> Result<(), String>;
    /// Page closed by the sender or lost with its connection.
    fn close(&self, instance: &str, page_id: &str);
}

struct Binding {
    route: String,
    session: String,
    page_id: String,
    peer: Arc<Peer>,
}

struct HubState {
    bindings: Mutex<HashMap<String, Binding>>,
    sink: Arc<dyn PageSink>,
    stop: AtomicBool,
}

pub struct Hub {
    path: PathBuf,
    state: Arc<HubState>,
    timeout: Duration,
    /// Whether shutdown removes the socket file; not when a service manager
    /// created it and hands it over.
    owns_socket: bool,
}

impl Hub {
    /// Bind a private (0600) socket at `path`. Fails if something already
    /// lives there; stale endpoints are not removed.
    pub fn bind(path: &Path, sink: Arc<dyn PageSink>) -> io::Result<Hub> {
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self::serve(listener, path, sink, true))
    }

    /// Serve a listener created elsewhere, such as a socket systemd passed
    /// in. `path` is where it is bound; shutdown leaves that file in place.
    pub fn from_listener(listener: UnixListener, path: &Path, sink: Arc<dyn PageSink>) -> Hub {
        Self::serve(listener, path, sink, false)
    }

    fn serve(
        listener: UnixListener,
        path: &Path,
        sink: Arc<dyn PageSink>,
        owns_socket: bool,
    ) -> Hub {
        let state = Arc::new(HubState {
            bindings: Mutex::new(HashMap::new()),
            sink,
            stop: AtomicBool::new(false),
        });
        let accept_state = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_state.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let st = accept_state.clone();
                thread::spawn(move || serve_connection(st, stream));
            }
        });
        Hub {
            path: path.to_path_buf(),
            state,
            timeout: REQUEST_TIMEOUT,
            owns_socket,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Session recorded for an open instance, if still bound.
    pub fn session_of(&self, instance: &str) -> Option<String> {
        let b = self.state.bindings.lock().unwrap();
        b.get(instance).map(|b| b.session.clone())
    }

    pub fn route_of(&self, instance: &str) -> Option<String> {
        let b = self.state.bindings.lock().unwrap();
        b.get(instance).map(|b| b.route.clone())
    }

    pub fn open_instances(&self) -> Vec<String> {
        let b = self.state.bindings.lock().unwrap();
        let mut v: Vec<_> = b.keys().cloned().collect();
        v.sort();
        v
    }

    fn peer_for(&self, instance: &str, page_id: &str) -> Result<Arc<Peer>, BridgeError> {
        let b = self.state.bindings.lock().unwrap();
        let Some(binding) = b.get(instance) else {
            return Err(BridgeError::Remote {
                code: ErrorCode::DeliveryUnavailable,
                message: format!("instance {instance} is not open; no local fallback"),
            });
        };
        if binding.page_id != page_id {
            return Err(BridgeError::Remote {
                code: ErrorCode::PageMismatch,
                message: format!("instance {instance} was opened with a different page"),
            });
        }
        Ok(binding.peer.clone())
    }

    /// Save annotations to the page's origin; returns the acknowledged revision.
    pub fn save(
        &self,
        instance: &str,
        page_id: &str,
        revision: u64,
        annotations: serde_json::Value,
    ) -> Result<Option<u64>, BridgeError> {
        let peer = self.peer_for(instance, page_id)?;
        peer.request(
            Body::Save {
                instance: instance.into(),
                page_id: page_id.into(),
                revision,
                annotations,
            },
            self.timeout,
        )
    }

    /// Ask the page's origin to submit feedback to its agent session.
    pub fn submit(
        &self,
        instance: &str,
        page_id: &str,
        revision: u64,
    ) -> Result<Option<u64>, BridgeError> {
        let peer = self.peer_for(instance, page_id)?;
        peer.request(
            Body::Submit {
                instance: instance.into(),
                page_id: page_id.into(),
                revision,
            },
            self.timeout,
        )
    }

    /// Stop accepting and remove the socket file this hub created.
    pub fn shutdown(&self) {
        self.state.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let peers: Vec<_> = {
            let b = self.state.bindings.lock().unwrap();
            b.values().map(|b| b.peer.clone()).collect()
        };
        for p in peers {
            p.close();
        }
        if self.owns_socket {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn error_body(reply_to: Option<u64>, code: ErrorCode, message: impl Into<String>) -> Body {
    Body::Error {
        reply_to,
        code,
        message: message.into(),
    }
}

fn serve_connection(state: Arc<HubState>, mut stream: UnixStream) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let peer = Peer::new(write_half);
    let mut dec = Decoder::default();
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let route = match handshake(&peer, &mut stream, &mut dec) {
        Some(r) => r,
        None => {
            peer.close();
            return;
        }
    };
    let _ = stream.set_read_timeout(None);
    let mut owned: Vec<(String, String)> = Vec::new();
    loop {
        let item = match read_item(&mut stream, &mut dec) {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(BridgeError::FrameTooLarge(n)) => {
                let _ = peer.reply(error_body(
                    None,
                    ErrorCode::FrameTooLarge,
                    format!("frame of {n} bytes exceeds limit"),
                ));
                break;
            }
            Err(_) => break,
        };
        let frame = match item {
            Decoded::Frame(f) => f,
            Decoded::Invalid(m) => {
                let _ = peer.reply(error_body(None, ErrorCode::Malformed, m));
                continue;
            }
        };
        if peer.deliver_reply(&frame.body) {
            continue;
        }
        match frame.body {
            Body::Open(page) => {
                let reply = hub_open(&state, &peer, &route, &page);
                if reply.is_ok() {
                    owned.push((page.instance.clone(), page.page_id.clone()));
                }
                let body = match reply {
                    Ok(()) => Body::Ack {
                        reply_to: frame.id,
                        revision: None,
                    },
                    Err((code, m)) => error_body(Some(frame.id), code, m),
                };
                if peer.reply(body).is_err() {
                    break;
                }
            }
            Body::Close { instance } => {
                let removed = {
                    let mut b = state.bindings.lock().unwrap();
                    match b.get(&instance) {
                        Some(x) if Arc::ptr_eq(&x.peer, &peer) => b.remove(&instance),
                        _ => None,
                    }
                };
                let body = match removed {
                    Some(binding) => {
                        owned.retain(|(i, _)| *i != instance);
                        state.sink.close(&instance, &binding.page_id);
                        Body::Ack {
                            reply_to: frame.id,
                            revision: None,
                        }
                    }
                    None => error_body(
                        Some(frame.id),
                        ErrorCode::UnknownInstance,
                        format!("instance {instance} is not open on this connection"),
                    ),
                };
                if peer.reply(body).is_err() {
                    break;
                }
            }
            _ => {
                let _ = peer.reply(error_body(
                    Some(frame.id),
                    ErrorCode::Malformed,
                    "unexpected message for hub",
                ));
            }
        }
    }
    peer.close();
    // Connection lost: unbind everything it opened.
    for (instance, _) in owned {
        let removed = {
            let mut b = state.bindings.lock().unwrap();
            match b.get(&instance) {
                Some(x) if Arc::ptr_eq(&x.peer, &peer) => b.remove(&instance),
                _ => None,
            }
        };
        if let Some(binding) = removed {
            state.sink.close(&instance, &binding.page_id);
        }
    }
}

fn handshake(peer: &Arc<Peer>, stream: &mut UnixStream, dec: &mut Decoder) -> Option<String> {
    let item = read_item(stream, dec).ok()??;
    let frame = match item {
        Decoded::Frame(f) => f,
        Decoded::Invalid(m) => {
            let _ = peer.reply(error_body(None, ErrorCode::Malformed, m));
            return None;
        }
    };
    match frame.body {
        Body::Hello { version, route } if version == PROTOCOL_VERSION => {
            peer.reply(Body::HelloAck {
                version: PROTOCOL_VERSION,
            })
            .ok()?;
            Some(route)
        }
        Body::Hello { version, .. } => {
            let _ = peer.reply(error_body(
                Some(frame.id),
                ErrorCode::IncompatibleVersion,
                format!("peer speaks version {version}; this side speaks {PROTOCOL_VERSION}"),
            ));
            None
        }
        _ => {
            let _ = peer.reply(error_body(
                Some(frame.id),
                ErrorCode::HandshakeRequired,
                "first message must be hello",
            ));
            None
        }
    }
}

fn hub_open(
    state: &Arc<HubState>,
    peer: &Arc<Peer>,
    route: &str,
    page: &OpenPage,
) -> Result<(), (ErrorCode, String)> {
    if page.route != route {
        return Err((
            ErrorCode::RouteMismatch,
            format!("open for route {} on connection for {route}", page.route),
        ));
    }
    let mut b = state.bindings.lock().unwrap();
    if b.contains_key(&page.instance) {
        return Err((
            ErrorCode::DuplicateInstance,
            format!("instance {} is already open", page.instance),
        ));
    }
    // Bind before consulting the sink so a racing Save cannot miss it, but
    // do not hold the lock across the sink call.
    b.insert(
        page.instance.clone(),
        Binding {
            route: page.route.clone(),
            session: page.session.clone(),
            page_id: page.page_id.clone(),
            peer: peer.clone(),
        },
    );
    drop(b);
    if let Err(m) = state.sink.open(page) {
        state.bindings.lock().unwrap().remove(&page.instance);
        return Err((ErrorCode::Rejected, m));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Relay (sender side)
// ---------------------------------------------------------------------

/// A Save or Submit pushed by the hub, awaiting the relay's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u64,
    pub body: Body,
}

pub struct Relay {
    peer: Arc<Peer>,
    requests: Mutex<Receiver<Request>>,
    timeout: Duration,
    peer_pid: Option<i32>,
}

/// The process that listens on the other end of `stream`, as the kernel
/// recorded it at connect time. Through an SSH forward that is the login's
/// own `sshd` process.
#[cfg(target_os = "linux")]
fn listener_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe a valid ucred.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && credentials.pid > 0).then_some(credentials.pid)
}

#[cfg(not(target_os = "linux"))]
fn listener_pid(_stream: &UnixStream) -> Option<i32> {
    None
}

impl Relay {
    /// Connect to a hub and complete the handshake for `route`.
    pub fn connect(socket: &Path, route: &str) -> Result<Relay, BridgeError> {
        Self::connect_with_version(socket, route, PROTOCOL_VERSION)
    }

    pub fn connect_with_version(
        socket: &Path,
        route: &str,
        version: u32,
    ) -> Result<Relay, BridgeError> {
        let mut stream = UnixStream::connect(socket)?;
        let peer_pid = listener_pid(&stream);
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let peer = Peer::new(stream.try_clone()?);
        peer.send(&Frame {
            id: 0,
            body: Body::Hello {
                version,
                route: route.into(),
            },
        })?;
        let mut dec = Decoder::default();
        match read_item(&mut stream, &mut dec)? {
            Some(Decoded::Frame(Frame {
                body: Body::HelloAck { .. },
                ..
            })) => {}
            Some(Decoded::Frame(Frame {
                body: Body::Error { code, message, .. },
                ..
            })) => return Err(BridgeError::Remote { code, message }),
            Some(_) => return Err(BridgeError::Malformed("bad handshake reply".into())),
            None => return Err(BridgeError::Disconnected),
        }
        stream.set_read_timeout(None)?;
        let (tx, rx) = mpsc::channel();
        let reader_peer = peer.clone();
        thread::spawn(move || {
            let mut stream = stream;
            let mut dec = dec;
            loop {
                match read_item(&mut stream, &mut dec) {
                    Ok(Some(Decoded::Frame(f))) => {
                        if reader_peer.deliver_reply(&f.body) {
                            continue;
                        }
                        if tx
                            .send(Request {
                                id: f.id,
                                body: f.body,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Some(Decoded::Invalid(_))) => continue,
                    _ => break,
                }
            }
            reader_peer.close();
        });
        Ok(Relay {
            peer,
            requests: Mutex::new(rx),
            timeout: REQUEST_TIMEOUT,
            peer_pid,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The process listening on the hub side of this connection, when the
    /// platform reports it.
    pub fn peer_pid(&self) -> Option<i32> {
        self.peer_pid
    }

    /// Send a page; returns once the hub reports real acceptance or failure.
    pub fn open(&self, page: OpenPage) -> Result<(), BridgeError> {
        self.peer
            .request(Body::Open(page), self.timeout)
            .map(|_| ())
    }

    pub fn close_page(&self, instance: &str) -> Result<(), BridgeError> {
        self.peer
            .request(
                Body::Close {
                    instance: instance.into(),
                },
                self.timeout,
            )
            .map(|_| ())
    }

    /// Next Save or Submit from the hub. `Err(Disconnected)` when the
    /// connection ends; `Err(Timeout)` when nothing arrived in time.
    pub fn next_request(&self, timeout: Duration) -> Result<Request, BridgeError> {
        match self.requests.lock().unwrap().recv_timeout(timeout) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => Err(BridgeError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(BridgeError::Disconnected),
        }
    }

    pub fn ack(&self, request: &Request, revision: Option<u64>) -> Result<(), BridgeError> {
        self.peer.reply(Body::Ack {
            reply_to: request.id,
            revision,
        })
    }

    pub fn fail(
        &self,
        request: &Request,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Result<(), BridgeError> {
        self.peer.reply(error_body(Some(request.id), code, message))
    }

    /// Write raw bytes to the connection; for protocol tests.
    pub fn write_raw(&self, bytes: &[u8]) -> Result<(), BridgeError> {
        let mut w = self.peer.writer.lock().unwrap();
        w.write_all(bytes).map_err(|_| BridgeError::Disconnected)
    }

    pub fn is_open(&self) -> bool {
        self.peer.open.load(Ordering::SeqCst)
    }

    pub fn disconnect(&self) {
        self.peer.close();
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.peer.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        opened: Mutex<Vec<(String, String)>>,
    }

    impl PageSink for Recorder {
        fn open(&self, page: &OpenPage) -> Result<(), String> {
            self.opened
                .lock()
                .unwrap()
                .push((page.route.clone(), page.instance.clone()));
            Ok(())
        }
        fn close(&self, _instance: &str, _page_id: &str) {}
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-hub-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn page(route: &str, instance: &str) -> OpenPage {
        OpenPage {
            route: route.into(),
            session: "s".into(),
            page_id: "p".into(),
            instance: instance.into(),
            remote_path: None,
            markdown: "# x".into(),
            annotations: serde_json::Value::Null,
            revision: 0,
            delivery: Delivery::Remote,
        }
    }

    #[test]
    fn should_accept_pages_from_logins_with_different_routes_when_serving_a_handed_over_listener() {
        let dir = scratch("handed");
        let path = dir.join("hub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let sink = Arc::new(Recorder::default());
        let hub = Hub::from_listener(listener, &path, sink.clone());
        let first = Relay::connect(&path, "route-a").unwrap();
        let second = Relay::connect(&path, "route-b").unwrap();
        first.open(page("route-a", "i-1")).unwrap();
        second.open(page("route-b", "i-2")).unwrap();
        assert_eq!(hub.route_of("i-1").as_deref(), Some("route-a"));
        assert_eq!(hub.route_of("i-2").as_deref(), Some("route-b"));
        assert_eq!(sink.opened.lock().unwrap().len(), 2);
        hub.shutdown();
        assert!(
            path.exists(),
            "a handed-over socket belongs to whoever created it"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_refuse_an_open_for_another_route_when_the_hub_serves_many_logins() {
        let dir = scratch("mismatch");
        let path = dir.join("hub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let _hub = Hub::from_listener(listener, &path, Arc::new(Recorder::default()));
        let relay = Relay::connect(&path, "route-a").unwrap();
        match relay.open(page("route-b", "i-1")) {
            Err(BridgeError::Remote {
                code: ErrorCode::RouteMismatch,
                ..
            }) => {}
            other => panic!("expected a route mismatch, got {other:?}"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_remove_its_socket_on_shutdown_when_the_hub_bound_it_itself() {
        let dir = scratch("owned");
        let path = dir.join("hub.sock");
        let hub = Hub::bind(&path, Arc::new(Recorder::default())).unwrap();
        hub.shutdown();
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn should_report_the_listening_process_when_a_relay_connects() {
        let dir = scratch("peer");
        let path = dir.join("hub.sock");
        let _hub = Hub::bind(&path, Arc::new(Recorder::default())).unwrap();
        let relay = Relay::connect(&path, "r").unwrap();
        assert_eq!(relay.peer_pid(), Some(std::process::id() as i32));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
