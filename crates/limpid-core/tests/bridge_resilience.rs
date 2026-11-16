//! Stage 6: several pages on one login, viewer and launcher restarts, bounded
//! timeouts, cleanup, and explicit recovery of feedback the sender never got.
//!
//! Each login is a hub, an importer cache and a save service, as in the
//! submission tests; the sender's pages and agent listeners are stand-ins.

use limpid_core::{
    bridge::{Delivery, HANDSHAKE_TIMEOUT, Hub, OpenPage, REQUEST_TIMEOUT, Relay},
    bridge_import::{Imported, Importer, LAUNCH_TIMEOUT, Opener},
    bridge_push::{PUSH_WAIT, PushReply, PushRequest, PushService, PushState, request_push},
    bridge_recover,
    bridge_relay::{OPEN_TIMEOUT, RouteEnv, STATUS_TIMEOUT, page_from_agent_files, serve},
    bridge_save::{page_id_of, read_feedback, read_revision},
    bridge_submit::LISTENER_WAIT,
    replies::AgentReplies,
    transport::{FeedbackSignal, Scheme},
};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_millis(1500);

/// Counts how many times the viewer was asked to show a page.
#[derive(Default)]
struct CountingViewer {
    opened: AtomicUsize,
}

impl Opener for CountingViewer {
    fn open(&self, _: &Imported, _: Scheme) -> Result<(), String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-resilience-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stand-in agent listener that records every frame and accepts it.
struct Listener {
    path: PathBuf,
    frames: Arc<Mutex<Vec<FeedbackSignal>>>,
    stop: Arc<AtomicBool>,
}

impl Listener {
    fn bind(path: &Path) -> Listener {
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
                (&stream).write_all(b"{\"ok\":true}\n").unwrap();
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
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

/// The sender's agent page `name` in `dir`, for `session`.
fn agent_page(dir: &Path, name: &str, session: &str, listener: Option<&Path>) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let html = dir.join(format!("{name}.html"));
    fs::write(&html, "<html><body>page</body></html>").unwrap();
    fs::write(
        html.with_extension("md"),
        format!("# {name}\n\n## Questions\n\n1. Which?\n   - A\n   - B\n"),
    )
    .unwrap();
    fs::write(
        html.with_extension("meta.json"),
        json!({
            "agent": "claude", "session_id": session, "turn_spec": "0", "title": name,
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
    viewer: Arc<CountingViewer>,
}

/// One launcher: its sockets live under `root`, its imports under `cache`.
fn login(root: &Path, route: &str, cache: &Path) -> Login {
    let root = root.join(route);
    fs::create_dir_all(&root).unwrap();
    let save_sock = root.join("s.sock");
    let viewer = Arc::new(CountingViewer::default());
    let importer = Arc::new(
        Importer::new(cache.to_path_buf(), Scheme::Dark, viewer.clone())
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
        viewer,
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

    /// What `:w` / `:wq` do: save the local copy, then ask the launcher.
    fn save(&self, page: &OpenPage, replies: &AgentReplies, submit: bool) -> PushReply {
        replies.save(&self.imported(page).sidecar).unwrap();
        self.service.push(&PushRequest {
            instance: page.instance.clone(),
            page_id: page.page_id.clone(),
            submit,
        })
    }

    /// The launcher exits: its sockets are removed.
    fn exit(&self) {
        self.service.shutdown();
        self.hub.shutdown();
    }

    fn save_socket(&self) -> PathBuf {
        self.root.join("s.sock")
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

fn open_descriptors() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
fn should_keep_each_page_with_its_own_session_when_several_pages_are_open_on_one_login() {
    let root = scratch("several");
    let cache = root.join("cache");
    let names = ["one", "two", "three"];
    let listeners: Vec<Listener> = names
        .iter()
        .map(|name| Listener::bind(&root.join(format!("{name}.sock"))))
        .collect();
    let htmls: Vec<PathBuf> = names
        .iter()
        .zip(&listeners)
        .map(|(name, listener)| {
            agent_page(
                &root.join("sender"),
                name,
                &format!("sess-{name}"),
                Some(&listener.path),
            )
        })
        .collect();
    let login = login(&root, "r1", &cache);
    let opened: Vec<(OpenPage, JoinHandle<()>)> =
        htmls.iter().map(|html| login.open(html)).collect();
    assert_eq!(login.hub.open_instances().len(), 3);
    assert_eq!(login.viewer.opened.load(Ordering::SeqCst), 3);

    // Interleaved: the third saves, the first saves and submits, then the
    // second submits, then the third submits.
    let pages: Vec<&OpenPage> = opened.iter().map(|(page, _)| page).collect();
    let third_text = replies(&[("q1", "third answer")]);
    let first_text = replies(&[("q1", "first answer")]);
    let second_text = replies(&[("q1", "second answer")]);
    assert_eq!(
        login.save(pages[2], &third_text, false).state,
        PushState::RemoteSaved
    );
    assert!(login.save(pages[0], &first_text, true).submitted());
    assert!(listeners[1].frames().is_empty());
    assert!(login.save(pages[1], &second_text, true).submitted());
    assert!(login.save(pages[2], &third_text, true).submitted());

    for (index, listener) in listeners.iter().enumerate() {
        let frames = listener.frames();
        assert_eq!(frames.len(), 1, "{} got {frames:?}", names[index]);
        assert_eq!(frames[0].page, htmls[index]);
    }
    assert!(listeners[0].frames()[0].message.contains("first answer"));
    assert!(listeners[1].frames()[0].message.contains("second answer"));
    assert!(listeners[2].frames()[0].message.contains("third answer"));
    for (index, html) in htmls.iter().enumerate() {
        let expected = [&first_text, &second_text, &third_text][index];
        assert_eq!(&read_feedback(html).unwrap(), expected);
    }
    login.exit();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_push_feedback_saved_while_the_viewer_was_down_when_the_viewer_comes_back() {
    let root = scratch("viewer");
    let html = agent_page(&root.join("sender"), "p", "sess", None);
    let login = login(&root, "r1", &root.join("cache"));
    let (page, _relay) = login.open(&html);
    // The viewer window is gone: nothing in the launcher depends on it. The
    // feedback it had saved is on disk, and a restarted viewer asks again.
    let imported = login.imported(&page);
    replies(&[("q1", "A")]).save(&imported.sidecar).unwrap();
    let reply = request_push(
        &login.save_socket(),
        &PushRequest {
            instance: page.instance.clone(),
            page_id: page.page_id.clone(),
            submit: false,
        },
        PUSH_WAIT,
    );
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    assert_eq!(read_feedback(&html).unwrap(), replies(&[("q1", "A")]));
    assert_eq!(
        login.viewer.opened.load(Ordering::SeqCst),
        1,
        "the launcher did not reopen anything by itself"
    );
    login.exit();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_feedback_recoverable_when_the_launcher_exits_before_the_push() {
    let root = scratch("restart");
    let cache = root.join("cache");
    let listener = Listener::bind(&root.join("a.sock"));
    let html = agent_page(&root.join("sender"), "p", "sess", Some(&listener.path));
    let first = login(&root, "r1", &cache);
    let (page, _relay) = first.open(&html);
    assert_eq!(
        first.save(&page, &replies(&[("q1", "A")]), false).state,
        PushState::RemoteSaved
    );
    // Edited, saved locally, and then the login ends before the next push.
    replies(&[("q1", "B, naïve 日本語")])
        .save(&first.imported(&page).sidecar)
        .unwrap();
    first.exit();

    let reply = request_push(
        &first.save_socket(),
        &PushRequest {
            instance: page.instance.clone(),
            page_id: page.page_id.clone(),
            submit: true,
        },
        Duration::from_secs(2),
    );
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(
        reply.describe().contains("not sent"),
        "{}",
        reply.describe()
    );
    assert!(listener.frames().is_empty(), "nothing reached the agent");
    assert_eq!(
        read_feedback(&html).unwrap(),
        replies(&[("q1", "A")]),
        "the sender holds the last acknowledged copy"
    );

    let scan = bridge_recover::scan(&cache).unwrap();
    assert_eq!(scan.unsent.len(), 1, "{scan:?}");
    let unsent = &scan.unsent[0];
    assert_eq!(unsent.route, "r1");
    assert_eq!(unsent.session, "sess");
    assert!(unsent.compose().contains("q1: B, naïve 日本語"));
    assert!(
        unsent
            .remote_path
            .as_deref()
            .is_some_and(|path| path.ends_with("p.html"))
    );

    // A new launcher opens the same page: a separate import, no migration.
    // The old copy stays listed until the user has dealt with it.
    let second = login(&root, "r2", &cache);
    let (again, _relay) = second.open(&html);
    assert_ne!(second.imported(&again).markdown, unsent.markdown);
    assert_eq!(
        second.imported(&again).markdown.parent(),
        unsent.markdown.parent()
    );
    let scan = bridge_recover::scan(&cache).unwrap();
    assert_eq!(scan.unsent.len(), 1, "the new import is in sync: {scan:?}");
    assert_eq!(scan.unsent[0].stem, unsent.stem);
    // The new import carries the sender's copy and pushes on its own.
    assert!(
        second
            .save(&again, &replies(&[("q1", "C")]), true)
            .submitted()
    );
    assert_eq!(listener.frames().len(), 1);
    assert_eq!(read_revision(&html).unwrap().revision, 2);
    second.exit();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_stop_listing_the_page_when_the_remote_has_the_current_feedback() {
    let root = scratch("clear");
    let cache = root.join("cache");
    let html = agent_page(&root.join("sender"), "p", "sess", None);
    let login = login(&root, "r1", &cache);
    let (page, _relay) = login.open(&html);
    replies(&[("q1", "A")])
        .save(&login.imported(&page).sidecar)
        .unwrap();
    assert_eq!(bridge_recover::scan(&cache).unwrap().unsent.len(), 1);
    let reply = login.service.push(&PushRequest {
        instance: page.instance.clone(),
        page_id: page.page_id.clone(),
        submit: false,
    });
    assert_eq!(reply.state, PushState::RemoteSaved);
    assert!(bridge_recover::scan(&cache).unwrap().unsent.is_empty());
    login.exit();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_answer_within_the_bound_and_keep_the_copy_when_the_sender_never_answers() {
    let root = scratch("stuck");
    let cache = root.join("cache");
    let login = login(&root, "r1", &cache);
    // A relay that opens a page and then goes quiet: connected, never replying.
    let relay = Relay::connect(&login.root.join("b.sock"), "r1").unwrap();
    let html = agent_page(&root.join("sender"), "p", "sess", None);
    let mut page = page_from_agent_files(&html, "r1").unwrap();
    page.delivery = Delivery::Remote;
    relay.open(page.clone()).unwrap();
    wait_until(|| login.importer.imported(&page.instance).is_some());
    replies(&[("q1", "A")])
        .save(&login.imported(&page).sidecar)
        .unwrap();

    let started = Instant::now();
    let reply = login.service.push(&PushRequest {
        instance: page.instance.clone(),
        page_id: page.page_id.clone(),
        submit: true,
    });
    assert!(
        started.elapsed() < TIMEOUT * 2,
        "waited {:?} for a sender that never answers",
        started.elapsed()
    );
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(!reply.submitted());
    assert_eq!(bridge_recover::scan(&cache).unwrap().unsent.len(), 1);
    login.exit();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_nest_the_timeouts_so_each_layer_answers_before_the_one_above_gives_up() {
    // Launch the viewer < wait for it < the opening command's wait.
    assert!(LAUNCH_TIMEOUT < OPEN_TIMEOUT);
    assert!(OPEN_TIMEOUT < STATUS_TIMEOUT);
    // The agent's confirmation < the sender's reply to the hub.
    assert!(LISTENER_WAIT < REQUEST_TIMEOUT);
    assert!(HANDSHAKE_TIMEOUT <= REQUEST_TIMEOUT);
    // A save and a submit each wait one request; the viewer waits for both.
    assert!(REQUEST_TIMEOUT * 2 < PUSH_WAIT);
}

#[test]
fn should_release_everything_when_many_pages_open_and_the_login_ends() {
    let root = scratch("cleanup");
    let cache = root.join("cache");
    let html = agent_page(&root.join("sender"), "p", "sess", None);
    // Warm up once so lazily opened descriptors do not count as leaks.
    {
        let warm = login(&root, "warm", &cache);
        let (_page, relay) = warm.open(&html);
        warm.exit();
        relay.join().unwrap();
    }
    thread::sleep(Duration::from_millis(200));
    let before = open_descriptors();
    for round in 0..8 {
        let login = login(&root, &format!("c{round}"), &cache);
        let pages: Vec<_> = (0..4).map(|_| login.open(&html)).collect();
        assert_eq!(login.hub.open_instances().len(), 4);
        let socket = login.root.join("b.sock");
        let save = login.save_socket();
        login.exit();
        for (_, relay) in pages {
            relay.join().unwrap();
        }
        assert!(!socket.exists(), "bridge socket left behind");
        assert!(!save.exists(), "save socket left behind");
        assert!(
            UnixStream::connect(&socket).is_err(),
            "the bridge still accepts connections"
        );
    }
    let mut after = open_descriptors();
    for _ in 0..50 {
        if after <= before + 2 {
            break;
        }
        thread::sleep(Duration::from_millis(100));
        after = open_descriptors();
    }
    assert!(
        after <= before + 2,
        "descriptors grew from {before} to {after}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_change_the_page_identity_when_the_same_page_is_opened_by_two_launchers() {
    let root = scratch("identity");
    let html = agent_page(&root.join("sender"), "p", "sess", None);
    let (a, b) = (
        login(&root, "ra", &root.join("cache-a")),
        login(&root, "rb", &root.join("cache-b")),
    );
    let (page_a, _ra) = a.open(&html);
    let (page_b, _rb) = b.open(&html);
    assert_eq!(page_a.page_id, page_b.page_id);
    assert_eq!(
        page_a.page_id,
        page_id_of(&fs::read(html.with_extension("md")).unwrap())
    );
    assert_ne!(page_a.instance, page_b.instance);
    a.exit();
    b.exit();
    let _ = fs::remove_dir_all(root);
}
