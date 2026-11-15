//! Stage 4 in the viewer: `:w` on an imported page saves the local copy at
//! once, pushes to the sender from a worker, and says which copies exist.

mod common;

use common::{fixture, fonts, shot};
use limpid_core::{
    bridge::{Delivery, OpenPage},
    bridge_import::{handoff_for, import_page_with},
    bridge_push::{PushReply, PushRequest, PushState},
    doc::Mode,
    replies::AgentReplies,
    settings::Settings,
    transport::Scheme,
};
use limpid_view::{feedback::Feedback, nav::keymap::KeyInput, page::DocPage, window::Page};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
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

struct Viewer {
    page: DocPage,
    sidecar: PathBuf,
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
    let mut page = page.with_feedback(Feedback::new(handoff, title, settings));
    shot(&mut page, 900, 700, 1.0);
    Viewer {
        page,
        sidecar: imported.sidecar,
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
