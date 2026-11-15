use limpid_core::bridge::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Recorder {
    opened: Mutex<Vec<OpenPage>>,
    closed: Mutex<Vec<(String, String)>>,
    reject: Mutex<Option<String>>,
}

impl PageSink for Recorder {
    fn open(&self, page: &OpenPage) -> Result<(), String> {
        if let Some(m) = self.reject.lock().unwrap().clone() {
            return Err(m);
        }
        self.opened.lock().unwrap().push(page.clone());
        Ok(())
    }
    fn close(&self, instance: &str, page_id: &str) {
        self.closed
            .lock()
            .unwrap()
            .push((instance.into(), page_id.into()));
    }
}

struct Harness {
    dir: PathBuf,
    sock: PathBuf,
    sink: Arc<Recorder>,
    hub: Hub,
}

fn harness(name: &str) -> Harness {
    let dir = std::env::temp_dir().join(format!("limpid-bridge-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("b.sock");
    let sink = Arc::new(Recorder::default());
    let hub = Hub::bind(&sock, sink.clone())
        .unwrap()
        .with_timeout(Duration::from_millis(800));
    Harness {
        dir,
        sock,
        sink,
        hub,
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn page(route: &str, session: &str, page_id: &str, instance: &str, md: &str) -> OpenPage {
    OpenPage {
        route: route.into(),
        session: session.into(),
        page_id: page_id.into(),
        instance: instance.into(),
        remote_path: Some("/home/remote/nonexistent/page.md".into()),
        markdown: md.into(),
        annotations: json!({"replies": []}),
        revision: 0,
        delivery: Delivery::Remote,
    }
}

fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..300 {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn raw_connect(h: &Harness) -> UnixStream {
    let s = UnixStream::connect(&h.sock).unwrap();
    s.set_read_timeout(Some(WAIT)).unwrap();
    s
}

fn raw_frame(body: Body) -> Vec<u8> {
    encode_frame(&Frame { id: 7, body }).unwrap()
}

fn read_one(s: &mut UnixStream) -> Option<Frame> {
    let mut dec = Decoder::default();
    loop {
        match dec.next().unwrap() {
            Some(Decoded::Frame(f)) => return Some(f),
            Some(Decoded::Invalid(_)) => panic!("invalid frame"),
            None => {}
        }
        let mut buf = [0u8; 4096];
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => dec.push(&buf[..n]),
        }
    }
}

#[test]
fn should_transfer_page_without_shared_paths_when_relay_opens() {
    let h = harness("transfer");
    let relay = Relay::connect(&h.sock, "r1").unwrap();
    let p = page("r1", "s1", "pg", "i1", "# Title\n\nbody\n");
    relay.open(p.clone()).unwrap();
    assert_eq!(h.sink.opened.lock().unwrap().as_slice(), &[p]);
    assert!(!PathBuf::from("/home/remote/nonexistent/page.md").exists());
    assert_eq!(h.hub.session_of("i1").as_deref(), Some("s1"));
    assert_eq!(h.hub.route_of("i1").as_deref(), Some("r1"));
}

#[test]
fn should_route_save_to_opening_connection_when_pages_interleave() {
    let h = harness("route");
    let a = Relay::connect(&h.sock, "ra").unwrap();
    let b = Relay::connect(&h.sock, "rb").unwrap();
    a.open(page("ra", "sa", "pa", "ia", "A")).unwrap();
    b.open(page("rb", "sb", "pb", "ib", "B")).unwrap();
    let a2 = a.open(page("ra", "sa", "pa2", "ia2", "A2"));
    assert!(a2.is_ok());

    let hub = &h.hub;
    thread::scope(|s| {
        let t = s.spawn(|| hub.save("ib", "pb", 3, json!({"k": "b"})));
        let req = b.next_request(WAIT).unwrap();
        assert!(matches!(&req.body, Body::Save { instance, revision: 3, .. } if instance == "ib"));
        b.ack(&req, Some(3)).unwrap();
        assert_eq!(t.join().unwrap().unwrap(), Some(3));
    });
    assert!(matches!(
        a.next_request(Duration::from_millis(100)),
        Err(BridgeError::Timeout)
    ));

    thread::scope(|s| {
        let t = s.spawn(|| hub.submit("ia2", "pa2", 1));
        let req = a.next_request(WAIT).unwrap();
        assert!(matches!(&req.body, Body::Submit { instance, .. } if instance == "ia2"));
        a.ack(&req, None).unwrap();
        t.join().unwrap().unwrap();
    });
    assert!(matches!(
        b.next_request(Duration::from_millis(100)),
        Err(BridgeError::Timeout)
    ));
}

#[test]
fn should_keep_unicode_intact_when_page_and_annotations_round_trip() {
    let h = harness("unicode");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    let md = "# Zażółć gęślą jaźń \u{1F600} \u{202E}rtl\n\n日本語 e\u{301}\n\0 nul\r\n";
    relay.open(page("r", "s", "p", "i", md)).unwrap();
    assert_eq!(h.sink.opened.lock().unwrap()[0].markdown, md);

    let ann = json!({"comment": "naïve \u{1F4A9} 漢字"});
    let hub = &h.hub;
    thread::scope(|s| {
        let t = s.spawn(|| hub.save("i", "p", 1, ann.clone()));
        let req = relay.next_request(WAIT).unwrap();
        match &req.body {
            Body::Save { annotations, .. } => assert_eq!(annotations, &ann),
            other => panic!("unexpected {other:?}"),
        }
        relay.ack(&req, Some(1)).unwrap();
        t.join().unwrap().unwrap();
    });
}

#[test]
fn should_reassemble_frames_when_bytes_arrive_one_at_a_time() {
    let frame = Frame {
        id: 9,
        body: Body::Hello {
            version: 1,
            route: "ro\u{e9}\u{1F600}".into(),
        },
    };
    let bytes = encode_frame(&frame).unwrap();
    let mut dec = Decoder::default();
    for (i, b) in bytes.iter().enumerate() {
        assert!(dec.next().unwrap().is_none(), "early frame at byte {i}");
        dec.push(&[*b]);
    }
    match dec.next().unwrap() {
        Some(Decoded::Frame(f)) => assert_eq!(f, frame),
        other => panic!("{other:?}"),
    }
    assert!(dec.next().unwrap().is_none());
}

#[test]
fn should_split_back_to_back_frames_when_delivered_in_one_chunk() {
    let f1 = Frame { id: 1, body: Body::Close { instance: "a".into() } };
    let f2 = Frame { id: 2, body: Body::Close { instance: "b".into() } };
    let mut bytes = encode_frame(&f1).unwrap();
    bytes.extend(encode_frame(&f2).unwrap());
    let mut dec = Decoder::default();
    dec.push(&bytes);
    assert!(matches!(dec.next().unwrap(), Some(Decoded::Frame(f)) if f == f1));
    assert!(matches!(dec.next().unwrap(), Some(Decoded::Frame(f)) if f == f2));
    assert!(dec.next().unwrap().is_none());
}

#[test]
fn should_accept_fragmented_open_over_socket_when_sender_writes_in_pieces() {
    let h = harness("fragsock");
    let mut s = raw_connect(&h);
    s.write_all(&raw_frame(Body::Hello { version: 1, route: "r".into() }))
        .unwrap();
    assert!(matches!(read_one(&mut s).unwrap().body, Body::HelloAck { .. }));
    let bytes = raw_frame(Body::Open(page("r", "s", "p", "i", "héllo \u{1F600}")));
    for chunk in bytes.chunks(5) {
        s.write_all(chunk).unwrap();
        s.flush().unwrap();
    }
    let reply = read_one(&mut s).unwrap();
    assert!(matches!(reply.body, Body::Ack { reply_to: 7, .. }));
    assert_eq!(h.sink.opened.lock().unwrap().len(), 1);
}

#[test]
fn should_reject_oversized_length_when_frame_exceeds_limit() {
    let mut dec = Decoder::default();
    dec.push(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes());
    assert!(matches!(dec.next(), Err(BridgeError::FrameTooLarge(_))));
}

#[test]
fn should_refuse_to_encode_when_page_exceeds_limit() {
    let big = "x".repeat(MAX_FRAME_BYTES + 1);
    let f = Frame { id: 1, body: Body::Open(page("r", "s", "p", "i", &big)) };
    assert!(matches!(encode_frame(&f), Err(BridgeError::FrameTooLarge(_))));
}

#[test]
fn should_transfer_large_page_when_just_under_limit() {
    let h = harness("large");
    let relay = Relay::connect(&h.sock, "r").unwrap().with_timeout(Duration::from_secs(10));
    let md = "y".repeat(MAX_FRAME_BYTES - 4096);
    relay.open(page("r", "s", "p", "i", &md)).unwrap();
    assert_eq!(h.sink.opened.lock().unwrap()[0].markdown.len(), md.len());
}

#[test]
fn should_report_frame_too_large_and_close_when_peer_sends_oversized_length() {
    let h = harness("oversize");
    let mut s = raw_connect(&h);
    s.write_all(&raw_frame(Body::Hello { version: 1, route: "r".into() }))
        .unwrap();
    read_one(&mut s).unwrap();
    s.write_all(&0xFFFF_FFFFu32.to_be_bytes()).unwrap();
    let f = read_one(&mut s).unwrap();
    assert!(matches!(f.body, Body::Error { code: ErrorCode::FrameTooLarge, .. }));
    assert!(read_one(&mut s).is_none());
}

#[test]
fn should_report_incompatible_version_when_handshake_versions_differ() {
    let h = harness("version");
    let err = Relay::connect_with_version(&h.sock, "r", PROTOCOL_VERSION + 1)
        .err()
        .unwrap();
    assert!(matches!(
        err,
        BridgeError::Remote { code: ErrorCode::IncompatibleVersion, .. }
    ));
}

#[test]
fn should_require_hello_when_first_message_is_open() {
    let h = harness("nohello");
    let mut s = raw_connect(&h);
    s.write_all(&raw_frame(Body::Open(page("r", "s", "p", "i", "x"))))
        .unwrap();
    let f = read_one(&mut s).unwrap();
    assert!(matches!(f.body, Body::Error { code: ErrorCode::HandshakeRequired, .. }));
    assert!(h.sink.opened.lock().unwrap().is_empty());
}

#[test]
fn should_answer_malformed_and_keep_connection_when_body_is_not_json() {
    let h = harness("badjson");
    let mut s = raw_connect(&h);
    s.write_all(&raw_frame(Body::Hello { version: 1, route: "r".into() }))
        .unwrap();
    read_one(&mut s).unwrap();
    let junk = b"{not json";
    s.write_all(&(junk.len() as u32).to_be_bytes()).unwrap();
    s.write_all(junk).unwrap();
    let f = read_one(&mut s).unwrap();
    assert!(matches!(f.body, Body::Error { code: ErrorCode::Malformed, .. }));
    s.write_all(&raw_frame(Body::Open(page("r", "s", "p", "i", "x"))))
        .unwrap();
    assert!(matches!(read_one(&mut s).unwrap().body, Body::Ack { .. }));
}

#[test]
fn should_reject_open_when_route_differs_from_handshake() {
    let h = harness("route-mismatch");
    let relay = Relay::connect(&h.sock, "r1").unwrap();
    let err = relay.open(page("other", "s", "p", "i", "x")).unwrap_err();
    assert!(matches!(
        err,
        BridgeError::Remote { code: ErrorCode::RouteMismatch, .. }
    ));
    assert!(h.hub.open_instances().is_empty());
}

#[test]
fn should_reject_open_when_instance_already_open() {
    let h = harness("dup");
    let a = Relay::connect(&h.sock, "r").unwrap();
    let b = Relay::connect(&h.sock, "r").unwrap();
    a.open(page("r", "s", "p", "i", "x")).unwrap();
    let err = b.open(page("r", "s2", "p2", "i", "y")).unwrap_err();
    assert!(matches!(
        err,
        BridgeError::Remote { code: ErrorCode::DuplicateInstance, .. }
    ));
    assert_eq!(h.hub.session_of("i").as_deref(), Some("s"));
}

#[test]
fn should_report_real_failure_and_unbind_when_sink_rejects_open() {
    let h = harness("reject");
    *h.sink.reject.lock().unwrap() = Some("no viewer".into());
    let relay = Relay::connect(&h.sock, "r").unwrap();
    let err = relay.open(page("r", "s", "p", "i", "x")).unwrap_err();
    assert!(matches!(
        err,
        BridgeError::Remote { code: ErrorCode::Rejected, ref message } if message == "no viewer"
    ));
    assert!(h.hub.open_instances().is_empty());
    assert!(matches!(
        h.hub.save("i", "p", 1, json!(null)),
        Err(BridgeError::Remote { code: ErrorCode::DeliveryUnavailable, .. })
    ));
}

#[test]
fn should_refuse_operation_when_page_id_differs_from_accepted_page() {
    let h = harness("pagemismatch");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "pg-1", "i", "x")).unwrap();
    for res in [
        h.hub.save("i", "pg-2", 1, json!(null)),
        h.hub.submit("i", "pg-2", 1),
    ] {
        assert!(matches!(
            res,
            Err(BridgeError::Remote { code: ErrorCode::PageMismatch, .. })
        ));
    }
    assert!(matches!(
        relay.next_request(Duration::from_millis(100)),
        Err(BridgeError::Timeout)
    ));
}

#[test]
fn should_fail_delivery_without_fallback_when_instance_unknown() {
    let h = harness("nofallback");
    let err = h.hub.submit("ghost", "p", 1).unwrap_err();
    assert!(matches!(
        err,
        BridgeError::Remote { code: ErrorCode::DeliveryUnavailable, .. }
    ));
}

#[test]
fn should_fail_delivery_and_notify_sink_when_relay_disconnects() {
    let h = harness("disconnect");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p", "i", "x")).unwrap();
    drop(relay);
    assert!(wait_until(|| h.hub.open_instances().is_empty()));
    assert_eq!(
        h.sink.closed.lock().unwrap().as_slice(),
        &[("i".to_string(), "p".to_string())]
    );
    assert!(matches!(
        h.hub.submit("i", "p", 1),
        Err(BridgeError::Remote { code: ErrorCode::DeliveryUnavailable, .. })
    ));
}

#[test]
fn should_unblock_waiting_save_when_relay_disconnects_mid_request() {
    let h = harness("midreq");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p", "i", "x")).unwrap();
    let hub = &h.hub;
    thread::scope(|s| {
        let t = s.spawn(|| hub.save("i", "p", 1, json!({})));
        let _req = relay.next_request(WAIT).unwrap();
        relay.disconnect();
        let err = t.join().unwrap().unwrap_err();
        assert!(matches!(err, BridgeError::Disconnected | BridgeError::Timeout));
    });
}

#[test]
fn should_time_out_when_relay_never_acknowledges() {
    let h = harness("timeout");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p", "i", "x")).unwrap();
    let err = h.hub.save("i", "p", 1, json!({})).unwrap_err();
    assert!(matches!(err, BridgeError::Timeout));
    // The binding survives a lost acknowledgment.
    assert_eq!(h.hub.open_instances(), vec!["i".to_string()]);
}

#[test]
fn should_surface_relay_failure_when_remote_refuses_save() {
    let h = harness("refuse");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p", "i", "x")).unwrap();
    let hub = &h.hub;
    thread::scope(|s| {
        let t = s.spawn(|| hub.save("i", "p", 1, json!({})));
        let req = relay.next_request(WAIT).unwrap();
        relay.fail(&req, ErrorCode::Rejected, "stale revision").unwrap();
        let err = t.join().unwrap().unwrap_err();
        assert!(matches!(
            err,
            BridgeError::Remote { code: ErrorCode::Rejected, ref message } if message == "stale revision"
        ));
    });
}

#[test]
fn should_unbind_only_that_page_when_relay_closes_one_instance() {
    let h = harness("close");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p1", "i1", "x")).unwrap();
    relay.open(page("r", "s", "p2", "i2", "y")).unwrap();
    relay.close_page("i1").unwrap();
    assert_eq!(h.hub.open_instances(), vec!["i2".to_string()]);
    assert!(matches!(
        relay.close_page("i1"),
        Err(BridgeError::Remote { code: ErrorCode::UnknownInstance, .. })
    ));
}

#[test]
fn should_not_let_other_connection_close_page_when_instance_not_its_own() {
    let h = harness("foreignclose");
    let a = Relay::connect(&h.sock, "r").unwrap();
    let b = Relay::connect(&h.sock, "r").unwrap();
    a.open(page("r", "s", "p", "i", "x")).unwrap();
    assert!(b.close_page("i").is_err());
    assert_eq!(h.hub.open_instances(), vec!["i".to_string()]);
}

#[test]
fn should_keep_other_connections_pages_when_one_relay_disconnects() {
    let h = harness("independent");
    let a = Relay::connect(&h.sock, "ra").unwrap();
    let b = Relay::connect(&h.sock, "rb").unwrap();
    a.open(page("ra", "sa", "pa", "ia", "A")).unwrap();
    b.open(page("rb", "sb", "pb", "ib", "B")).unwrap();
    drop(a);
    assert!(wait_until(|| h.hub.open_instances() == vec!["ib".to_string()]));
    let hub = &h.hub;
    thread::scope(|s| {
        let t = s.spawn(|| hub.submit("ib", "pb", 1));
        let req = b.next_request(WAIT).unwrap();
        b.ack(&req, None).unwrap();
        t.join().unwrap().unwrap();
    });
}

#[test]
fn should_handle_concurrent_relays_when_many_open_in_parallel() {
    let h = harness("parallel");
    thread::scope(|s| {
        for n in 0..8 {
            let sock = h.sock.clone();
            s.spawn(move || {
                let route = format!("r{n}");
                let relay = Relay::connect(&sock, &route).unwrap();
                for k in 0..5 {
                    let inst = format!("i{n}-{k}");
                    relay
                        .open(page(&route, &format!("s{n}"), &format!("p{n}-{k}"), &inst, "md"))
                        .unwrap();
                }
                thread::sleep(Duration::from_millis(100));
            });
        }
    });
    assert_eq!(h.sink.opened.lock().unwrap().len(), 40);
}

#[test]
fn should_remove_socket_and_refuse_connections_when_hub_shuts_down() {
    let h = harness("shutdown");
    let relay = Relay::connect(&h.sock, "r").unwrap();
    relay.open(page("r", "s", "p", "i", "x")).unwrap();
    h.hub.shutdown();
    assert!(!h.sock.exists());
    assert!(wait_until(|| !relay.is_open()));
    assert!(Relay::connect(&h.sock, "r").is_err());
}

#[test]
fn should_bind_socket_privately_when_hub_starts() {
    use std::os::unix::fs::PermissionsExt;
    let h = harness("perm");
    let mode = std::fs::metadata(&h.sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn should_not_replace_existing_endpoint_when_socket_path_in_use() {
    let h = harness("inuse");
    let again = Hub::bind(&h.sock, Arc::new(Recorder::default()));
    assert!(again.is_err());
    assert!(h.sock.exists());
}
