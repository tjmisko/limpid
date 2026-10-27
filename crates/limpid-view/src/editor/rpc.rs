//! Minimal msgpack-rpc client over a child's stdin and stdout.
//!
//! Wire format: requests are `[0, id, method, params]`, responses are
//! `[1, id, error, result]`, notifications are `[2, method, params]`. A
//! reader thread decodes the child's output; responses wake the matching
//! caller and notifications go to a channel the owner drains.

use rmpv::Value;
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

/// How long a blocking request waits for its response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum RpcError {
    /// The child closed its output before answering.
    Closed,
    /// No response within the timeout.
    Timeout,
    /// The peer answered with an error value.
    Remote(String),
    Io(std::io::Error),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Closed => write!(f, "the editor process closed its connection"),
            RpcError::Timeout => write!(f, "the editor process did not answer in time"),
            RpcError::Remote(message) => write!(f, "{message}"),
            RpcError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RpcError {}

/// What the reader thread delivers to the owner.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Vec<Value>,
    },
    /// The child closed its output (it exited or crashed).
    Closed,
}

type Waiter = Sender<Response>;
type Response = Result<Value, Value>;
type Sent = (u64, Option<Receiver<Response>>);
/// The waiting callers by request id; `None` once the reader has ended, so a
/// request issued after the hang-up fails at once instead of waiting for a
/// response nobody can send.
type Pending = Arc<Mutex<Option<HashMap<u64, Waiter>>>>;

pub struct Rpc {
    writer: Box<dyn Write + Send>,
    next_id: u64,
    pending: Pending,
    incoming: Receiver<Incoming>,
}

impl Rpc {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Rpc {
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (sender, incoming) = mpsc::channel();
        let reader_pending = Arc::clone(&pending);
        thread::spawn(move || read_loop(reader, reader_pending, sender));
        Rpc {
            writer: Box::new(writer),
            next_id: 1,
            pending,
            incoming,
        }
    }

    /// Send a request and wait for its result.
    pub fn request(&mut self, method: &str, params: Vec<Value>) -> Result<Value, RpcError> {
        self.request_timeout(method, params, REQUEST_TIMEOUT)
    }

    pub fn request_timeout(
        &mut self,
        method: &str,
        params: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let (id, receiver) = self.send_request(method, params, true)?;
        let Some(receiver) = receiver else {
            return Err(RpcError::Closed);
        };
        match receiver.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(RpcError::Remote(describe_error(&error))),
            Err(RecvTimeoutError::Disconnected) => Err(RpcError::Closed),
            Err(RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(RpcError::Timeout)
            }
        }
    }

    /// Send a request and do not wait; the response is discarded.
    pub fn send(&mut self, method: &str, params: Vec<Value>) -> Result<(), RpcError> {
        self.send_request(method, params, false).map(|_| ())
    }

    fn send_request(
        &mut self,
        method: &str,
        params: Vec<Value>,
        wait: bool,
    ) -> Result<Sent, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let receiver = if wait {
            let (sender, receiver) = mpsc::channel();
            let mut pending = self.pending.lock().unwrap();
            let Some(waiters) = pending.as_mut() else {
                return Err(RpcError::Closed);
            };
            waiters.insert(id, sender);
            Some(receiver)
        } else {
            None
        };
        let message = Value::Array(vec![
            Value::from(0),
            Value::from(id),
            Value::from(method),
            Value::Array(params),
        ]);
        let written = rmpv::encode::write_value(&mut self.writer, &message)
            .map_err(|error| RpcError::Io(std::io::Error::other(error.to_string())))
            .and_then(|()| self.writer.flush().map_err(RpcError::Io));
        if let Err(error) = written {
            self.forget(id);
            return Err(error);
        }
        Ok((id, receiver))
    }

    fn forget(&self, id: u64) {
        if let Some(waiters) = self.pending.lock().unwrap().as_mut() {
            waiters.remove(&id);
        }
    }

    /// Everything the reader thread has delivered, without blocking.
    pub fn drain(&mut self) -> Vec<Incoming> {
        self.incoming.try_iter().collect()
    }

    /// Wait up to `timeout` for the next incoming item.
    pub fn next_incoming(&mut self, timeout: Duration) -> Option<Incoming> {
        self.incoming.recv_timeout(timeout).ok()
    }
}

fn describe_error(error: &Value) -> String {
    match error {
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.as_str())
            .collect::<Vec<_>>()
            .join(": "),
        other => other.to_string(),
    }
}

fn read_loop(mut reader: impl Read, pending: Pending, sender: Sender<Incoming>) {
    while let Ok(message) = rmpv::decode::read_value(&mut reader) {
        let Value::Array(parts) = message else {
            continue;
        };
        dispatch(parts, &pending, &sender);
    }
    // Dropping the waiters disconnects every blocked caller.
    *pending.lock().unwrap() = None;
    let _ = sender.send(Incoming::Closed);
}

fn dispatch(parts: Vec<Value>, pending: &Pending, sender: &Sender<Incoming>) {
    let kind = parts.first().and_then(Value::as_u64);
    match (kind, parts.len()) {
        (Some(1), 4) => {
            let Some(id) = parts[1].as_u64() else {
                return;
            };
            let waiter = pending
                .lock()
                .unwrap()
                .as_mut()
                .and_then(|waiters| waiters.remove(&id));
            let Some(waiter) = waiter else {
                return;
            };
            let outcome = if parts[2].is_nil() {
                Ok(parts[3].clone())
            } else {
                Err(parts[2].clone())
            };
            let _ = waiter.send(outcome);
        }
        (Some(2), 3) => {
            let Some(method) = parts[1].as_str() else {
                return;
            };
            let params = match &parts[2] {
                Value::Array(params) => params.clone(),
                _ => Vec::new(),
            };
            let _ = sender.send(Incoming::Notification {
                method: method.to_string(),
                params,
            });
        }
        // Requests from the peer are not used by this client.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A writer that records bytes the test can decode afterwards.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn encode(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value).unwrap();
        bytes
    }

    /// A reader that yields `first`, then blocks until the sender is
    /// dropped, then ends.
    struct Gated {
        first: Cursor<Vec<u8>>,
        release: Receiver<()>,
    }

    impl Read for Gated {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.first.read(buffer)?;
            if count > 0 {
                return Ok(count);
            }
            let _ = self.release.recv();
            Ok(0)
        }
    }

    fn gated(bytes: Vec<u8>) -> (Gated, Sender<()>) {
        let (hold, release) = mpsc::channel();
        (
            Gated {
                first: Cursor::new(bytes),
                release,
            },
            hold,
        )
    }

    #[test]
    fn should_deliver_notifications_when_the_peer_sends_them() {
        let message = Value::Array(vec![
            Value::from(2),
            Value::from("redraw"),
            Value::Array(vec![Value::from("flush")]),
        ]);
        let mut rpc = Rpc::new(Cursor::new(encode(&message)), Sink::default());
        match rpc.next_incoming(Duration::from_secs(2)).unwrap() {
            Incoming::Notification { method, params } => {
                assert_eq!(method, "redraw");
                assert_eq!(params, vec![Value::from("flush")]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            rpc.next_incoming(Duration::from_secs(2)),
            Some(Incoming::Closed)
        ));
    }

    #[test]
    fn should_return_the_result_when_the_response_id_matches() {
        let response = Value::Array(vec![
            Value::from(1),
            Value::from(1),
            Value::Nil,
            Value::from("ok"),
        ]);
        let (reader, _hold) = gated(encode(&response));
        let sink = Sink::default();
        let mut rpc = Rpc::new(reader, sink.clone());
        let result = rpc.request("nvim_eval", vec![Value::from("1")]).unwrap();
        assert_eq!(result, Value::from("ok"));
        let written = sink.0.lock().unwrap().clone();
        let sent = rmpv::decode::read_value(&mut Cursor::new(written)).unwrap();
        assert_eq!(sent[2], Value::from("nvim_eval"));
    }

    #[test]
    fn should_report_the_remote_error_when_the_peer_answers_with_one() {
        let response = Value::Array(vec![
            Value::from(1),
            Value::from(1),
            Value::Array(vec![Value::from(0), Value::from("bad call")]),
            Value::Nil,
        ]);
        let (reader, _hold) = gated(encode(&response));
        let mut rpc = Rpc::new(reader, Sink::default());
        match rpc.request("nope", vec![]) {
            Err(RpcError::Remote(message)) => assert_eq!(message, "bad call"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn should_fail_with_closed_when_the_peer_hangs_up_before_answering() {
        let mut rpc = Rpc::new(Cursor::new(Vec::new()), Sink::default());
        // Wait for the reader to see the hang-up, so the request is certainly
        // issued after it; the other order is covered by the gated test below.
        assert!(matches!(
            rpc.next_incoming(Duration::from_secs(5)),
            Some(Incoming::Closed)
        ));
        let outcome = rpc.request("nvim_eval", vec![]);
        assert!(matches!(outcome, Err(RpcError::Closed)), "{outcome:?}");
    }

    #[test]
    fn should_fail_with_closed_when_the_peer_hangs_up_while_a_request_waits() {
        let (reader, hang_up) = gated(Vec::new());
        let mut rpc = Rpc::new(reader, Sink::default());
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(hang_up);
        });
        let outcome = rpc.request("nvim_eval", vec![]);
        releaser.join().unwrap();
        assert!(matches!(outcome, Err(RpcError::Closed)), "{outcome:?}");
    }

    #[test]
    fn should_time_out_when_the_peer_never_answers() {
        let (reader, _hold) = gated(Vec::new());
        let mut rpc = Rpc::new(reader, Sink::default());
        let outcome = rpc.request_timeout("slow", vec![], Duration::from_millis(50));
        assert!(matches!(outcome, Err(RpcError::Timeout)), "{outcome:?}");
    }

    #[test]
    fn should_stop_reading_when_the_stream_is_garbage() {
        let mut rpc = Rpc::new(Cursor::new(vec![0xc1, 0xc1, 0xc1]), Sink::default());
        assert!(matches!(
            rpc.next_incoming(Duration::from_secs(2)),
            Some(Incoming::Closed)
        ));
    }

    #[test]
    fn should_ignore_responses_with_unknown_ids_when_nobody_waits() {
        let stray = Value::Array(vec![
            Value::from(1),
            Value::from(99),
            Value::Nil,
            Value::Nil,
        ]);
        let mut rpc = Rpc::new(Cursor::new(encode(&stray)), Sink::default());
        assert!(matches!(
            rpc.next_incoming(Duration::from_secs(2)),
            Some(Incoming::Closed)
        ));
    }
}
