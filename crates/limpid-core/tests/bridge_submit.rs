//! Stage 5: `:wq` on a transferred page submits the saved feedback to the
//! agent session the page came from, and to no other.
//!
//! Each login is a hub, an importer cache and a save service. The sender keeps
//! agent pages whose meta names a feedback listener; the listeners here are
//! stand-ins that record what they receive, and one test runs the real Claude
//! Code listener script with a stand-in for the plugin's acknowledgment.

use limpid_core::{
    bridge::{Body, Hub, OpenPage, Relay},
    bridge_import::{Imported, Importer, Opener, SyncState},
    bridge_push::{PushReply, PushRequest, PushService, PushState, SubmitState, request_push},
    bridge_relay::{RouteEnv, page_from_agent_files, serve},
    bridge_save::{apply_save, read_feedback, read_revision},
    replies::AgentReplies,
    transport::{FeedbackOutcome, FeedbackSignal, Scheme, send_feedback_checked},
};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MARKDOWN: &str =
    "# Pick\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - Postgres\n\nnaïve 日本語\n";
const TIMEOUT: Duration = Duration::from_millis(1500);

struct Recorder;

impl Opener for Recorder {
    fn open(&self, _: &Imported, _: Scheme) -> Result<(), String> {
        Ok(())
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-submit-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// How a stand-in agent listener answers a frame.
#[derive(Clone, Copy)]
enum Answer {
    Accept,
    Refuse,
    /// Read the frame, then close without answering.
    Silent,
}

/// A stand-in feedback listener that records every frame it is sent.
struct Listener {
    path: PathBuf,
    frames: Arc<Mutex<Vec<FeedbackSignal>>>,
    stop: Arc<AtomicBool>,
}

impl Listener {
    fn bind(path: &Path, answer: Answer) -> Listener {
        let listener = UnixListener::bind(path).unwrap();
        let frames: Arc<Mutex<Vec<FeedbackSignal>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, halt) = (frames.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if halt.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_str(line.trim()).unwrap());
                let mut writer = &stream;
                match answer {
                    Answer::Accept => writer.write_all(b"{\"ok\":true}\n").unwrap(),
                    Answer::Refuse => writer
                        .write_all(b"{\"ok\":false,\"error\":\"busy\"}\n")
                        .unwrap(),
                    Answer::Silent => {}
                }
            }
        });
        Listener {
            path: path.to_path_buf(),
            frames,
            stop,
        }
    }

    fn frames(&self) -> Vec<FeedbackSignal> {
        self.frames.lock().unwrap().clone()
    }

    /// The agent exits: the socket is gone.
    fn exit(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

/// The sender's agent page for `session`, whose meta names `listener`.
fn agent_page(dir: &Path, session: &str, listener: Option<&Path>) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let html = dir.join("claude-1.html");
    fs::write(&html, "<html><body>page</body></html>").unwrap();
    fs::write(html.with_extension("md"), MARKDOWN).unwrap();
    fs::write(
        html.with_extension("meta.json"),
        json!({
            "agent": "claude", "session_id": session, "turn_spec": "0", "title": "Pick",
            "feedback_sock": listener,
        })
        .to_string(),
    )
    .unwrap();
    html
}

struct Login {
    root: PathBuf,
    importer: Arc<Importer>,
    hub: Arc<Hub>,
    service: PushService,
    route: String,
}

fn login(root: &Path, route: &str) -> Login {
    let root = root.join(route);
    fs::create_dir_all(&root).unwrap();
    let save_sock = root.join("s.sock");
    let importer = Arc::new(
        Importer::new(root.join("cache"), Scheme::Dark, Arc::new(Recorder))
            .with_save_socket(save_sock.clone()),
    );
    let hub = Arc::new(
        Hub::bind(&root.join("b.sock"), importer.clone())
            .unwrap()
            .with_timeout(TIMEOUT),
    );
    let service = PushService::bind(&save_sock, importer.clone(), hub.clone()).unwrap();
    Login {
        root,
        importer,
        hub,
        service,
        route: route.into(),
    }
}

impl Login {
    fn open(&self, html: &Path) -> (OpenPage, JoinHandle<()>) {
        let page = page_from_agent_files(html, &self.route).unwrap();
        let sent = page.clone();
        let env = RouteEnv {
            socket: self.root.join("b.sock"),
            route: self.route.clone(),
        };
        let handle = thread::spawn(move || {
            let mut status = Vec::new();
            let _ = serve(&env, sent, &mut status);
        });
        wait_until(|| self.importer.imported(&page.instance).is_some());
        (page, handle)
    }

    fn imported(&self, page: &OpenPage) -> Imported {
        self.importer.imported(&page.instance).unwrap().0
    }

    /// What `:wq` does: save the local copy, then ask the launcher to push it
    /// and submit it.
    fn save_and_submit(&self, page: &OpenPage, replies: &AgentReplies) -> PushReply {
        replies.save(&self.imported(page).sidecar).unwrap();
        self.submit(page)
    }

    fn submit(&self, page: &OpenPage) -> PushReply {
        self.service.push(&PushRequest {
            instance: page.instance.clone(),
            page_id: page.page_id.clone(),
            submit: true,
        })
    }

    fn local(&self, page: &OpenPage) -> AgentReplies {
        AgentReplies::load(&self.imported(page).sidecar).unwrap()
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("condition not reached");
}

fn replies(pairs: &[(&str, &str)]) -> AgentReplies {
    let mut set = AgentReplies::default();
    for (field, text) in pairs {
        set.replies.insert((*field).into(), (*text).into());
    }
    set
}

fn state_of(reply: &PushReply) -> SubmitState {
    reply
        .submission
        .as_ref()
        .unwrap_or_else(|| panic!("no submission in {reply:?}"))
        .state
}

fn error_of(reply: &PushReply) -> String {
    reply
        .submission
        .as_ref()
        .and_then(|submission| submission.error.clone())
        .unwrap_or_default()
}

#[test]
fn should_deliver_the_composed_feedback_to_the_listener_when_the_page_is_saved_and_submitted() {
    let root = scratch("deliver");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_submit(&page, &replies(&[("q1", "Postgres, naïve 日本語")]));
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    assert_eq!(state_of(&reply), SubmitState::Submitted, "{reply:?}");
    assert!(reply.submitted());
    let frames = listener.frames();
    assert_eq!(frames.len(), 1);
    let message = &frames[0].message;
    assert!(
        message.starts_with("re: Pick (claude, turn 0)\nq1: Postgres, naïve 日本語"),
        "{message}"
    );
    // The pull command names the sender's own path, never a local one.
    let remote_html = html.canonicalize().unwrap();
    assert!(
        message.contains(&format!("limpid-core feedback {}", remote_html.display())),
        "{message}"
    );
    assert_eq!(frames[0].page, remote_html);
    assert!(!message.contains(login.root.to_str().unwrap()), "{message}");
    // Persisted first: the sender holds the feedback it submitted.
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    assert_eq!(
        read_feedback(&html).unwrap().replies["q1"],
        "Postgres, naïve 日本語"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_start_a_turn_only_in_the_originating_session_when_two_sessions_have_pages_open() {
    let root = scratch("two");
    let listener_a = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let listener_b = Listener::bind(&root.join("b.sock"), Answer::Accept);
    let html_a = agent_page(&root.join("sender-a"), "sess-a", Some(&listener_a.path));
    let html_b = agent_page(&root.join("sender-b"), "sess-b", Some(&listener_b.path));
    let login = login(&root, "r1");
    let (page_a, _ra) = login.open(&html_a);
    let (page_b, _rb) = login.open(&html_b);
    let reply = login.save_and_submit(&page_b, &replies(&[("q1", "for B")]));
    assert!(reply.submitted(), "{reply:?}");
    assert_eq!(listener_a.frames().len(), 0, "A is unaffected");
    assert_eq!(listener_b.frames().len(), 1);
    assert!(listener_b.frames()[0].message.contains("q1: for B"));
    let reply = login.save_and_submit(&page_a, &replies(&[("q1", "for A")]));
    assert!(reply.submitted(), "{reply:?}");
    assert_eq!(listener_a.frames().len(), 1);
    assert!(listener_a.frames()[0].message.contains("q1: for A"));
    assert_eq!(listener_b.frames().len(), 1, "B is unaffected");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_reach_a_second_logins_session_when_two_logins_open_the_same_kind_of_page() {
    let root = scratch("logins");
    let listener_a = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let listener_b = Listener::bind(&root.join("b.sock"), Answer::Accept);
    let html_a = agent_page(&root.join("sender-a"), "sess-a", Some(&listener_a.path));
    let html_b = agent_page(&root.join("sender-b"), "sess-b", Some(&listener_b.path));
    let (one, two) = (login(&root, "r1"), login(&root, "r2"));
    let (page_a, _ra) = one.open(&html_a);
    let (_page_b, _rb) = two.open(&html_b);
    let reply = one.save_and_submit(&page_a, &replies(&[("q1", "x")]));
    assert!(reply.submitted(), "{reply:?}");
    assert_eq!(listener_a.frames().len(), 1);
    assert_eq!(listener_b.frames().len(), 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_submit_to_a_newer_agent_when_the_session_was_cleared() {
    let root = scratch("clear");
    // /clear: the old session's listener is gone and a new session has its own.
    let old = Listener::bind(&root.join("old.sock"), Answer::Accept);
    let old_path = old.path.clone();
    let newer = Listener::bind(&root.join("new.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-old", Some(&old_path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    old.exit();
    let reply = login.save_and_submit(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert!(error_of(&reply).contains("not reachable"), "{reply:?}");
    assert_eq!(newer.frames().len(), 0, "the newer agent was not used");
    // The feedback is kept on both machines.
    assert_eq!(reply.state, PushState::RemoteSaved);
    assert_eq!(login.local(&page).replies["q1"], "SQLite");
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "SQLite");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_the_submit_when_the_page_no_longer_names_its_session() {
    let root = scratch("rebound");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    // The page was rewritten for another session after it was opened.
    agent_page(&root.join("sender"), "sess-b", Some(&listener.path));
    let reply = login.save_and_submit(&page, &replies(&[("q1", "x")]));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert_eq!(listener.frames().len(), 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_not_submitted_when_the_agent_has_exited() {
    let root = scratch("exited");
    let html = agent_page(&root.join("sender"), "sess-a", None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_submit(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert!(
        error_of(&reply).contains("no feedback listener"),
        "{reply:?}"
    );
    assert!(reply.describe().contains("saved locally and on the remote"));
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "SQLite");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_contact_the_agent_when_there_is_no_feedback() {
    let root = scratch("empty");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_submit(&page, &AgentReplies::default());
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert!(error_of(&reply).contains("no feedback"), "{reply:?}");
    let whitespace = login.save_and_submit(&page, &replies(&[("q1", "   ")]));
    assert_eq!(state_of(&whitespace), SubmitState::NotSubmitted);
    assert_eq!(listener.frames().len(), 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_feedback_and_say_not_sent_when_the_listener_refuses() {
    let root = scratch("refused");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Refuse);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_submit(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert!(error_of(&reply).contains("busy"), "{reply:?}");
    assert!(!reply.submitted());
    assert_eq!(login.local(&page).replies["q1"], "SQLite");
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "SQLite");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_uncertain_and_not_retry_when_the_listener_never_confirms() {
    let root = scratch("lost");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Silent);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_submit(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(state_of(&reply), SubmitState::Uncertain, "{reply:?}");
    assert!(!reply.submitted());
    let line = reply.describe();
    assert!(line.contains("Possibly sent"), "{line}");
    assert!(line.contains("before sending again"), "{line}");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(listener.frames().len(), 1, "no blind retry");
    assert_eq!(login.local(&page).replies["q1"], "SQLite");
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "SQLite");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_uncertain_when_the_sender_never_answers_the_submit() {
    let root = scratch("mute");
    let login = login(&root, "r1");
    let page = OpenPage {
        route: "r1".into(),
        session: "sess-a".into(),
        page_id: "p1".into(),
        instance: "i1".into(),
        remote_path: Some("/nonexistent/claude-1.html".into()),
        markdown: MARKDOWN.into(),
        annotations: serde_json::Value::Null,
        revision: 0,
        delivery: limpid_core::bridge::Delivery::Remote,
    };
    let relay = Relay::connect(&login.root.join("b.sock"), "r1").unwrap();
    relay.open(page.clone()).unwrap();
    // Acknowledges saves and swallows submits.
    let held = Arc::new(relay);
    let worker = held.clone();
    thread::spawn(move || {
        while let Ok(request) = worker.next_request(Duration::from_secs(5)) {
            if matches!(request.body, Body::Save { .. }) {
                let _ = worker.ack(&request, Some(1));
            }
        }
    });
    let reply = login.save_and_submit(&page, &replies(&[("q1", "x")]));
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    assert_eq!(state_of(&reply), SubmitState::Uncertain, "{reply:?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_submit_when_the_feedback_did_not_reach_the_sender() {
    let root = scratch("stale");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    // Another login saved first: the sender is at revision 1.
    apply_save(
        &html,
        &page.page_id,
        "other-instance",
        1,
        &serde_json::to_value(replies(&[("q1", "theirs")])).unwrap(),
    )
    .unwrap();
    let reply = login.save_and_submit(&page, &replies(&[("q1", "mine")]));
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert!(
        error_of(&reply).contains("not saved on the remote"),
        "{reply:?}"
    );
    assert_eq!(listener.frames().len(), 0);
    assert_eq!(login.local(&page).replies["q1"], "mine");
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "theirs");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_a_submit_for_another_revision_when_the_sender_moved_on() {
    let root = scratch("revision");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let saved = login.save_and_submit(&page, &replies(&[("q1", "one")]));
    assert!(saved.submitted());
    let error = login
        .hub
        .submit(&page.instance, &page.page_id, 7)
        .unwrap_err();
    assert!(error.to_string().contains("revision"), "{error}");
    assert_eq!(listener.frames().len(), 1);
    let error = login.hub.submit(&page.instance, "md-other", 1).unwrap_err();
    assert!(error.to_string().contains("different page"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_the_submit_when_the_remote_page_changed_since_it_was_opened() {
    let root = scratch("changed");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    fs::write(html.with_extension("md"), "# Another response\n").unwrap();
    let reply = login.save_and_submit(&page, &replies(&[("q1", "x")]));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert_eq!(listener.frames().len(), 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_submit_anywhere_when_the_connection_to_the_sender_is_gone() {
    let root = scratch("gone");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    login.hub.shutdown();
    wait_until(|| login.hub.open_instances().is_empty());
    let reply = login.save_and_submit(&page, &replies(&[("q1", "x")]));
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    assert_eq!(listener.frames().len(), 0);
    assert_eq!(login.local(&page).replies["q1"], "x");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_submit_the_saved_revision_when_the_page_was_already_saved_remotely() {
    let root = scratch("already");
    let listener = Listener::bind(&root.join("a.sock"), Answer::Accept);
    let html = agent_page(&root.join("sender"), "sess-a", Some(&listener.path));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    // `:w` first; the later `:wq` has nothing new to save.
    let saved = login.service.push(&PushRequest {
        instance: page.instance.clone(),
        page_id: page.page_id.clone(),
        submit: false,
    });
    assert!(saved.submission.is_none());
    replies(&[("q1", "SQLite")])
        .save(&login.imported(&page).sidecar)
        .unwrap();
    let saved = login.service.push(&PushRequest {
        instance: page.instance.clone(),
        page_id: page.page_id.clone(),
        submit: false,
    });
    assert_eq!(saved.revision, Some(1), "{saved:?}");
    assert_eq!(listener.frames().len(), 0, "saving alone never submits");
    let reply = login.submit(&page);
    assert_eq!(reply.revision, Some(1), "{reply:?}");
    assert!(reply.submitted(), "{reply:?}");
    assert_eq!(listener.frames().len(), 1);
    assert_eq!(SyncState::load(&login.imported(&page)).unwrap().revision, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_not_submitted_over_the_save_socket_when_the_launcher_is_gone() {
    let root = scratch("socket-gone");
    let request = PushRequest {
        instance: "i".into(),
        page_id: "p".into(),
        submit: true,
    };
    let reply = request_push(&root.join("absent.sock"), &request, Duration::from_secs(1));
    assert_eq!(state_of(&reply), SubmitState::NotSubmitted, "{reply:?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_uncertain_over_the_save_socket_when_the_launcher_stops_answering() {
    let root = scratch("socket-silent");
    let socket = root.join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        // Closes without answering.
    });
    let request = PushRequest {
        instance: "i".into(),
        page_id: "p".into(),
        submit: true,
    };
    let reply = request_push(&socket, &request, Duration::from_secs(2));
    server.join().unwrap();
    assert_eq!(state_of(&reply), SubmitState::Uncertain, "{reply:?}");
    assert!(reply.describe().contains("check the agent session"));
    let _ = fs::remove_dir_all(root);
}

// ---- the listener protocol ----

fn answer_with(path: &Path, reply: &'static [u8]) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        stream.write_all(reply).unwrap();
    })
}

fn signal() -> FeedbackSignal {
    FeedbackSignal {
        page: "/remote/page.html".into(),
        message: "q1: yes".into(),
    }
}

#[test]
fn should_tell_the_outcomes_apart_when_a_listener_answers_in_each_way() {
    let root = scratch("outcomes");
    let wait = Duration::from_secs(2);
    let missing = send_feedback_checked(&root.join("none.sock"), &signal(), wait);
    assert!(
        matches!(missing, FeedbackOutcome::NotDelivered(_)),
        "{missing:?}"
    );

    let accepted = root.join("ok.sock");
    let server = answer_with(&accepted, b"{\"ok\":true}\n");
    assert_eq!(
        send_feedback_checked(&accepted, &signal(), wait),
        FeedbackOutcome::Accepted
    );
    server.join().unwrap();

    let refused = root.join("no.sock");
    let server = answer_with(&refused, b"{\"ok\":false,\"error\":\"nope\"}\n");
    assert_eq!(
        send_feedback_checked(&refused, &signal(), wait),
        FeedbackOutcome::Refused("nope".into())
    );
    server.join().unwrap();

    for (name, bytes) in [
        ("closed", &b""[..]),
        ("garbage", b"not json\n"),
        ("partial", b"{\"ok\":tr"),
        ("unrelated", b"{\"fine\":1}\n"),
    ] {
        let path = root.join(format!("{name}.sock"));
        let server = answer_with(&path, Box::leak(bytes.to_vec().into_boxed_slice()));
        let outcome = send_feedback_checked(&path, &signal(), wait);
        assert!(
            matches!(outcome, FeedbackOutcome::Uncertain(_)),
            "{name}: {outcome:?}"
        );
        server.join().unwrap();
    }
    let _ = fs::remove_dir_all(root);
}

/// The Claude Code listener script, as the plugin embeds it.
fn claude_listener_source() -> String {
    let register = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integrations/claude-code/hooks/register.ts");
    let source = fs::read_to_string(register).unwrap();
    let start = source.find("const LISTENER = `").unwrap() + "const LISTENER = `".len();
    let end = start + source[start..].find("\n`\n").unwrap();
    // A template literal turns `\\n` into `\n`, which Python reads as a newline.
    source[start..end].replace("\\\\", "\\")
}

struct Plugin {
    child: std::process::Child,
    frames: BufReader<std::process::ChildStdout>,
    socket: PathBuf,
}

impl Plugin {
    fn start(root: &Path) -> Plugin {
        let script = root.join("listener.py");
        fs::write(&script, claude_listener_source()).unwrap();
        let socket = root.join("run").join("claude-test.sock");
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(&socket)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        wait_until(|| socket.exists());
        let frames = BufReader::new(child.stdout.take().unwrap());
        Plugin {
            child,
            frames,
            socket,
        }
    }

    /// The next frame the listener printed for the plugin to submit.
    fn next_frame(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.frames.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    /// What the plugin does once `$.prompt.submit` settled.
    fn acknowledge(&self, id: &str, ok: bool) {
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        writeln!(stream, "{}", json!({"ack": id, "ok": ok})).unwrap();
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn should_confirm_only_after_the_plugin_accepts_when_the_claude_listener_gets_a_frame() {
    let root = scratch("python");
    let mut plugin = Plugin::start(&root);
    let socket = plugin.socket.clone();
    let sender =
        thread::spawn(move || send_feedback_checked(&socket, &signal(), Duration::from_secs(5)));
    let frame = plugin.next_frame();
    assert_eq!(frame["message"], "q1: yes");
    // The frame was printed, but nothing is confirmed until the plugin says so.
    thread::sleep(Duration::from_millis(200));
    assert!(
        !sender.is_finished(),
        "confirmed before the plugin accepted"
    );
    plugin.acknowledge(frame["id"].as_str().unwrap(), true);
    assert_eq!(sender.join().unwrap(), FeedbackOutcome::Accepted);
    drop(plugin);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_when_the_plugin_could_not_submit_the_prompt() {
    let root = scratch("python-refused");
    let mut plugin = Plugin::start(&root);
    let socket = plugin.socket.clone();
    let sender =
        thread::spawn(move || send_feedback_checked(&socket, &signal(), Duration::from_secs(5)));
    let frame = plugin.next_frame();
    plugin.acknowledge(frame["id"].as_str().unwrap(), false);
    let outcome = sender.join().unwrap();
    assert!(
        matches!(outcome, FeedbackOutcome::Refused(_)),
        "{outcome:?}"
    );
    drop(plugin);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_uncertain_when_the_plugin_never_acknowledges_the_frame() {
    let root = scratch("python-lost");
    let mut plugin = Plugin::start(&root);
    let socket = plugin.socket.clone();
    let sender =
        thread::spawn(move || send_feedback_checked(&socket, &signal(), Duration::from_secs(5)));
    let _ = plugin.next_frame();
    let outcome = sender.join().unwrap();
    assert!(
        matches!(outcome, FeedbackOutcome::Uncertain(_)),
        "{outcome:?}"
    );
    drop(plugin);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_frames_apart_when_two_are_in_flight_at_the_claude_listener() {
    let root = scratch("python-two");
    let mut plugin = Plugin::start(&root);
    let senders: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|text| {
            let socket = plugin.socket.clone();
            thread::spawn(move || {
                let frame = FeedbackSignal {
                    page: "/p.html".into(),
                    message: text.into(),
                };
                send_feedback_checked(&socket, &frame, Duration::from_secs(5))
            })
        })
        .collect();
    let (first, second) = (plugin.next_frame(), plugin.next_frame());
    assert_ne!(first["id"], second["id"]);
    // Accept one, refuse the other: each sender hears its own answer.
    let (accepted, refused) = if first["message"] == "one" {
        (&first, &second)
    } else {
        (&second, &first)
    };
    plugin.acknowledge(accepted["id"].as_str().unwrap(), true);
    plugin.acknowledge(refused["id"].as_str().unwrap(), false);
    let outcomes: Vec<_> = senders.into_iter().map(|s| s.join().unwrap()).collect();
    let by_message = |text: &str| &outcomes[if text == "one" { 0 } else { 1 }];
    assert_eq!(
        by_message(accepted["message"].as_str().unwrap()),
        &FeedbackOutcome::Accepted
    );
    assert!(matches!(
        by_message(refused["message"].as_str().unwrap()),
        FeedbackOutcome::Refused(_)
    ));
    drop(plugin);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_a_malformed_frame_when_the_claude_listener_gets_garbage() {
    let root = scratch("python-garbage");
    let plugin = Plugin::start(&root);
    let mut stream = UnixStream::connect(&plugin.socket).unwrap();
    stream.write_all(b"{not json\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let value: serde_json::Value = serde_json::from_str(answer.trim()).unwrap();
    assert_eq!(value["ok"], false);
    drop(plugin);
    let _ = fs::remove_dir_all(root);
}
