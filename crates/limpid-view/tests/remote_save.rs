//! Stage 4 in the viewer: `:w` on an imported page saves the local copy at
//! once, pushes to the sender from a worker, and says which copies exist.

mod common;

use common::{fixture, fonts, shot};
use limpid_core::{
    bridge::{Delivery, OpenPage},
    bridge_import::{handoff_for, import_page_with},
    bridge_push::{PushReply, PushRequest, PushState, Submission, SubmitState},
    doc::Mode,
    replies::AgentReplies,
    settings::Settings,
    transport::Scheme,
};
use limpid_view::{
    feedback::Feedback,
    nav::{CloseKind, keymap::KeyInput},
    page::{Closer, DocPage},
    window::Page,
};
use std::{
    cell::Cell,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    rc::Rc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("limpid-remote-save-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_page() -> OpenPage {
    OpenPage {
        route: "r1".into(),
        session: "sess-a".into(),
        page_id: "p1".into(),
        instance: "i1".into(),
        remote_path: Some("/nonexistent/remote/claude-1.html".into()),
        markdown: fixture("questions.md"),
        annotations: serde_json::Value::Null,
        revision: 0,
        delivery: Delivery::Remote,
    }
}

/// A launcher stand-in: answers one request after `delay`.
fn fake_launcher(socket: &Path, delay: Duration, reply: PushReply) -> JoinHandle<PushRequest> {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: PushRequest = serde_json::from_str(line.trim()).unwrap();
        thread::sleep(delay);
        let mut writer = &stream;
        writeln!(writer, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
        request
    })
}

/// Counts how often the page asked its window to close.
struct CloseCounter(Rc<Cell<u32>>);

impl Closer for CloseCounter {
    fn close(&self, _: CloseKind) -> bool {
        self.0.set(self.0.get() + 1);
        true
    }
}

struct Viewer {
    page: DocPage,
    sidecar: PathBuf,
    closes: Rc<Cell<u32>>,
}

fn viewer(root: &Path, save_sock: Option<&Path>, settings: &Settings) -> Viewer {
    let imported = import_page_with(&root.join("cache"), &open_page(), save_sock).unwrap();
    let handoff = handoff_for(&imported, Scheme::Dark);
    let page = DocPage::open_as(
        &imported.markdown,
        settings,
        Scheme::Dark,
        fonts(),
        Mode::Agent,
    )
    .unwrap();
    let title = page.title();
    let closes = Rc::new(Cell::new(0));
    let mut page = page
        .with_feedback(Feedback::new(handoff, title, settings))
        .with_closer(Box::new(CloseCounter(closes.clone())));
    shot(&mut page, 900, 700, 1.0);
    Viewer {
        page,
        sidecar: imported.sidecar,
        closes,
    }
}

impl Viewer {
    fn press(&mut self, key: &str) {
        self.page.handle_input(&KeyInput::named(key));
        shot(&mut self.page, 900, 700, 1.0);
    }

    /// Pick the first option of the first question.
    fn answer(&mut self) {
        for _ in 0..3 {
            self.press("j");
        }
        self.press("1");
    }

    fn command(&mut self, line: &str) {
        self.press(":");
        for character in line.chars() {
            self.press(&character.to_string());
        }
        self.press("enter");
    }

    fn status(&self) -> String {
        self.page.nav().status_view(self.page.elapsed_ms()).message
    }

    /// Tick until the status contains `needle`.
    fn wait_for(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.page.tick();
            shot(&mut self.page, 900, 700, 1.0);
            let status = self.status();
            if status.contains(needle) {
                return status;
            }
            assert!(Instant::now() < deadline, "no {needle:?} in {status:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn saved(&self) -> AgentReplies {
        AgentReplies::load(&self.sidecar).unwrap()
    }
}

fn settings() -> Settings {
    Settings::parse("").unwrap()
}

fn remote_saved(revision: u64) -> PushReply {
    PushReply {
        state: PushState::RemoteSaved,
        revision: Some(revision),
        error: None,
        submission: None,
    }
}

#[test]
fn should_save_locally_at_once_and_not_wait_when_the_remote_is_slow() {
    let root = scratch("slow");
    let socket = root.join("s.sock");
    let launcher = fake_launcher(&socket, Duration::from_millis(700), remote_saved(4));
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    let started = Instant::now();
    viewer.command("w");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the event loop waited {:?}",
        started.elapsed()
    );
    assert_eq!(viewer.saved().replies.len(), 1, "the local copy is on disk");
    assert!(
        viewer.status().contains("saving to the remote"),
        "{}",
        viewer.status()
    );
    assert!(viewer.page.wants_ticks(), "the window polls for the answer");
    let status = viewer.wait_for("on the remote");
    assert!(
        status.contains("Saved locally and on the remote (revision 4)"),
        "{status}"
    );
    assert!(!viewer.page.wants_ticks(), "nothing left to poll");
    let request = launcher.join().unwrap();
    assert_eq!(
        (request.instance.as_str(), request.page_id.as_str()),
        ("i1", "p1")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_say_saved_locally_only_when_the_remote_did_not_save() {
    let root = scratch("local-only");
    let socket = root.join("s.sock");
    let reply = PushReply {
        state: PushState::LocalOnly,
        revision: None,
        error: Some("stale revision: the remote page is at revision 2".into()),
        submission: None,
    };
    let _launcher = fake_launcher(&socket, Duration::ZERO, reply);
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("w");
    let status = viewer.wait_for("locally only");
    assert!(status.contains("not on the remote"), "{status}");
    assert!(status.contains("stale revision"), "{status}");
    assert_eq!(viewer.saved().replies.len(), 1, "the local copy is kept");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_local_copy_and_say_so_when_the_launcher_is_gone() {
    let root = scratch("gone");
    let mut viewer = viewer(&root, Some(&root.join("absent.sock")), &settings());
    viewer.answer();
    viewer.command("w");
    let status = viewer.wait_for("locally only");
    assert!(status.contains("gone"), "{status}");
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_say_there_is_no_route_when_the_page_was_imported_without_one() {
    let root = scratch("no-route");
    let mut viewer = viewer(&root, None, &settings());
    viewer.answer();
    viewer.command("w");
    let status = viewer.wait_for("no route to the remote");
    assert!(status.contains("Saved locally only"), "{status}");
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_push_but_not_deliver_when_writing_and_quitting_a_remote_page() {
    let root = scratch("wq");
    let marker = root.join("delivered.txt");
    let settings = Settings::parse(&format!(
        "[reply]\ncommand = \"cat > '{}'\"\n",
        marker.display()
    ))
    .unwrap();
    let socket = root.join("s.sock");
    let launcher = fake_launcher(&socket, Duration::ZERO, remote_saved(1));
    let mut viewer = viewer(&root, Some(&socket), &settings);
    viewer.answer();
    viewer.command("wq");
    launcher.join().unwrap();
    assert!(!marker.exists(), "no local delivery ran");
    let status = viewer.wait_for("on the remote");
    assert!(status.contains("revision 1"), "{status}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_contact_the_launcher_when_nothing_is_unsaved() {
    let root = scratch("clean");
    let socket = root.join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.command("w");
    assert!(
        viewer.status().contains("nothing to save"),
        "{}",
        viewer.status()
    );
    assert!(!viewer.page.wants_ticks());
    assert!(listener.accept().is_err(), "nobody asked the launcher");
    let _ = fs::remove_dir_all(root);
}

fn submitted(state: SubmitState, error: Option<&str>) -> PushReply {
    PushReply {
        state: PushState::RemoteSaved,
        revision: Some(1),
        error: None,
        submission: Some(Submission {
            state,
            error: error.map(str::to_owned),
        }),
    }
}

#[test]
fn should_close_the_window_only_after_the_agent_session_accepts_when_writing_and_quitting() {
    let root = scratch("wq-accepted");
    let socket = root.join("s.sock");
    let launcher = fake_launcher(
        &socket,
        Duration::from_millis(300),
        submitted(SubmitState::Submitted, None),
    );
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("wq");
    assert_eq!(viewer.closes.get(), 0, "still open while the request runs");
    assert!(
        viewer.status().contains("sending to the remote session"),
        "{}",
        viewer.status()
    );
    assert_eq!(
        viewer.saved().replies.len(),
        1,
        "the local copy is on disk first"
    );
    let status = viewer.wait_for("Sent to the agent session");
    assert!(status.contains("Sent"), "{status}");
    assert_eq!(viewer.closes.get(), 1, "closed once the session accepted");
    let request = launcher.join().unwrap();
    assert!(request.submit, "the launcher was asked to submit");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_wait_on_the_event_loop_when_the_remote_session_is_slow_to_accept() {
    let root = scratch("wq-slow");
    let socket = root.join("s.sock");
    let _launcher = fake_launcher(
        &socket,
        Duration::from_millis(700),
        submitted(SubmitState::Submitted, None),
    );
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    let started = Instant::now();
    viewer.command("wq");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the event loop waited {:?}",
        started.elapsed()
    );
    assert!(viewer.page.wants_ticks());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_page_open_and_say_not_sent_when_the_session_refuses() {
    let root = scratch("wq-refused");
    let socket = root.join("s.sock");
    let _launcher = fake_launcher(
        &socket,
        Duration::ZERO,
        submitted(
            SubmitState::NotSubmitted,
            Some("the agent session is not reachable"),
        ),
    );
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("wq");
    let status = viewer.wait_for("Not sent");
    assert!(status.contains("not reachable"), "{status}");
    assert!(
        status.contains("saved locally and on the remote"),
        "{status}"
    );
    assert_eq!(viewer.closes.get(), 0, "the page stays open");
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_page_open_and_warn_when_the_submission_is_uncertain() {
    let root = scratch("wq-uncertain");
    let socket = root.join("s.sock");
    let _launcher = fake_launcher(
        &socket,
        Duration::ZERO,
        submitted(SubmitState::Uncertain, Some("the agent did not confirm")),
    );
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("wq");
    let status = viewer.wait_for("Possibly sent");
    assert!(status.contains("before sending again"), "{status}");
    assert_eq!(viewer.closes.get(), 0, "the page stays open");
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_page_open_when_the_launcher_is_gone_at_write_and_quit() {
    let root = scratch("wq-gone");
    let mut viewer = viewer(&root, Some(&root.join("absent.sock")), &settings());
    viewer.answer();
    viewer.command("wq");
    let status = viewer.wait_for("Not sent");
    assert!(status.contains("gone"), "{status}");
    assert_eq!(viewer.closes.get(), 0);
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_page_open_when_it_has_no_route_to_the_remote_at_write_and_quit() {
    let root = scratch("wq-no-route");
    let mut viewer = viewer(&root, None, &settings());
    viewer.answer();
    viewer.command("wq");
    let status = viewer.status();
    assert!(status.contains("no route to the remote"), "{status}");
    assert_eq!(viewer.closes.get(), 0);
    assert_eq!(viewer.saved().replies.len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_close_without_contacting_anyone_when_there_is_no_feedback_to_send() {
    let root = scratch("wq-empty");
    let socket = root.join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.command("wq");
    assert!(
        viewer.status().contains("no feedback to send"),
        "{}",
        viewer.status()
    );
    assert_eq!(viewer.closes.get(), 1, "an empty page just closes");
    assert!(listener.accept().is_err(), "nobody was asked");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_send_twice_when_writing_and_quitting_again_while_a_send_is_running() {
    let root = scratch("wq-twice");
    let socket = root.join("s.sock");
    let launcher = fake_launcher(
        &socket,
        Duration::from_millis(400),
        submitted(SubmitState::Submitted, None),
    );
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("wq");
    viewer.command("wq");
    assert!(
        viewer.status().contains("already sending"),
        "{}",
        viewer.status()
    );
    viewer.wait_for("Sent to the agent session");
    launcher.join().unwrap();
    assert_eq!(viewer.closes.get(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_close_when_the_launcher_reports_no_submission_for_a_send() {
    let root = scratch("wq-unreported");
    let socket = root.join("s.sock");
    let _launcher = fake_launcher(&socket, Duration::ZERO, remote_saved(1));
    let mut viewer = viewer(&root, Some(&socket), &settings());
    viewer.answer();
    viewer.command("wq");
    let status = viewer.wait_for("not sent");
    assert!(status.contains("did not report a submission"), "{status}");
    assert_eq!(viewer.closes.get(), 0);
    let _ = fs::remove_dir_all(root);
}
