//! Stage 4: feedback saved in the local viewer reaches the page on the sender,
//! survives every failure locally, and never wakes the agent.
//!
//! Each login is a hub, an importer cache and a save service on its own
//! sockets. The sender keeps one agent page directory that several logins can
//! open; the receiver never reads it.

use limpid_core::{
    bridge::{Body, ErrorCode, Hub, OpenPage, Relay},
    bridge_import::{Imported, Importer, Opener, SyncState, replies_digest},
    bridge_push::{PushReply, PushRequest, PushService, PushState, request_push},
    bridge_relay::{RouteEnv, page_from_agent_files, serve},
    bridge_save::{apply_save, read_feedback, read_revision},
    replies::{AgentReplies, Comment, sidecar_path, write_annotations},
    transport::Scheme,
};
use serde_json::json;
use std::{
    fs,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

const MARKDOWN: &str =
    "# Pick\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - Postgres\n\nnaïve 日本語\n";
const TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Default)]
struct Recorder;

impl Opener for Recorder {
    fn open(&self, _: &Imported, _: Scheme) -> Result<(), String> {
        Ok(())
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-save-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The sender's agent page: Markdown, meta and HTML, optionally with feedback
/// already embedded.
fn agent_page(dir: &Path, embedded: Option<&AgentReplies>) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let html = dir.join("claude-1.html");
    let base = "<html><body>page</body></html>";
    let body = match embedded {
        Some(replies) => write_annotations(base, replies),
        None => base.to_string(),
    };
    fs::write(&html, body).unwrap();
    fs::write(html.with_extension("md"), MARKDOWN).unwrap();
    fs::write(
        html.with_extension("meta.json"),
        r#"{"agent":"claude","session_id":"sess-a","turn_spec":"0","title":"Pick"}"#,
    )
    .unwrap();
    html
}

fn comment(text: &str) -> Comment {
    Comment {
        text: text.into(),
        quote: "# Pick".into(),
        start_line: 1,
        end_line: 1,
    }
}

/// One local login: a hub, an importer and the save service.
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
    fn route_env(&self) -> RouteEnv {
        RouteEnv {
            socket: self.root.join("b.sock"),
            route: self.route.clone(),
        }
    }

    /// Run the real relay for `html` and wait until the page is open here.
    fn open(&self, html: &Path) -> (OpenPage, JoinHandle<()>) {
        let page = page_from_agent_files(html, &self.route).unwrap();
        let sent = page.clone();
        let env = self.route_env();
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

    fn request(&self, page: &OpenPage) -> PushRequest {
        PushRequest {
            instance: page.instance.clone(),
            page_id: page.page_id.clone(),
        }
    }

    /// What the viewer does for `:w`: save the local copy, then ask for a push.
    fn save_locally(&self, page: &OpenPage, replies: &AgentReplies) {
        replies.save(&self.imported(page).sidecar).unwrap();
    }

    fn save_and_push(&self, page: &OpenPage, replies: &AgentReplies) -> PushReply {
        self.save_locally(page, replies);
        self.service.push(&self.request(page))
    }

    fn local(&self, page: &OpenPage) -> AgentReplies {
        AgentReplies::load(&self.imported(page).sidecar).unwrap()
    }

    fn sync(&self, page: &OpenPage) -> SyncState {
        SyncState::load(&self.imported(page)).unwrap()
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

/// What `limpid-core feedback --json` reads on the sender.
fn cli_feedback(html: &Path) -> AgentReplies {
    let cache = html.parent().unwrap().join("cache-home");
    let output = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .args(["feedback", "--json"])
        .arg(html)
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_CONFIG_HOME", &cache)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn should_read_the_saved_reply_with_limpid_core_feedback_when_a_reply_is_pushed() {
    let root = scratch("reply");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_push(&page, &replies(&[("q1", "Postgres, naïve 日本語")]));
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    assert_eq!(reply.revision, Some(1));
    assert_eq!(cli_feedback(&html).replies["q1"], "Postgres, naïve 日本語");
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_replace_a_comment_when_it_is_edited_and_pushed_again() {
    let root = scratch("edit");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let mut set = AgentReplies::default();
    set.comments.insert("0-7".into(), comment("first thought"));
    assert_eq!(login.save_and_push(&page, &set).revision, Some(1));
    set.comments.insert("0-7".into(), comment("second thought"));
    let reply = login.save_and_push(&page, &set);
    assert_eq!(reply.revision, Some(2), "{reply:?}");
    let remote = cli_feedback(&html);
    assert_eq!(remote.comments.len(), 1);
    assert_eq!(remote.comments["0-7"].text, "second thought");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_remove_remote_feedback_when_it_is_deleted_locally() {
    let root = scratch("delete");
    // Embedded in the page, as `limpid-core claude` leaves earlier feedback.
    let mut embedded = replies(&[("q1", "SQLite")]);
    embedded.comments.insert("0-7".into(), comment("drop me"));
    embedded.comments.insert("9-20".into(), comment("keep me"));
    let html = agent_page(&root.join("sender"), Some(&embedded));
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    assert_eq!(login.local(&page), embedded, "the opened copy has it");
    let mut after = embedded.clone();
    after.comments.remove("0-7");
    after.replies.clear();
    let reply = login.save_and_push(&page, &after);
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    let remote = cli_feedback(&html);
    assert_eq!(remote, after, "the deleted comment and reply are gone");
    assert!(!fs::read_to_string(&html).unwrap().contains("drop me"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_not_resend_or_advance_the_revision_when_the_same_feedback_is_saved_again() {
    let root = scratch("repeat");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let set = replies(&[("q1", "SQLite")]);
    assert_eq!(login.save_and_push(&page, &set).revision, Some(1));
    for _ in 0..3 {
        let again = login.service.push(&login.request(&page));
        assert_eq!(again.state, PushState::RemoteSaved);
        assert_eq!(again.revision, Some(1));
    }
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    assert_eq!(login.sync(&page).synced_digest, Some(replies_digest(&set)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_the_stale_save_and_keep_the_local_copy_when_another_login_saved_first() {
    let root = scratch("stale");
    let html = agent_page(&root.join("sender"), None);
    let first = login(&root, "r1");
    let second = login(&root, "r2");
    let (page_one, _a) = first.open(&html);
    let (page_two, _b) = second.open(&html);
    let theirs = replies(&[("q1", "from the second login")]);
    assert_eq!(second.save_and_push(&page_two, &theirs).revision, Some(1));
    let mine = replies(&[("q1", "from the first login")]);
    let reply = first.save_and_push(&page_one, &mine);
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(reply.error.unwrap().contains("stale"), "names the cause");
    assert_eq!(first.local(&page_one), mine, "the local copy is kept");
    assert_eq!(first.sync(&page_one).synced_digest, None);
    assert_eq!(cli_feedback(&html), theirs, "the remote is untouched");
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_exactly_one_winner_when_two_logins_save_at_the_same_moment() {
    let root = scratch("race");
    let html = agent_page(&root.join("sender"), None);
    let logins = [login(&root, "r1"), login(&root, "r2")];
    let opened: Vec<_> = logins.iter().map(|l| l.open(&html)).collect();
    let barrier = Arc::new(Barrier::new(2));
    let results = Arc::new(Mutex::new(Vec::new()));
    thread::scope(|scope| {
        for (index, login) in logins.iter().enumerate() {
            let page = &opened[index].0;
            let (barrier, results) = (barrier.clone(), results.clone());
            scope.spawn(move || {
                let text = format!("login {index}");
                login.save_locally(page, &replies(&[("q1", &text)]));
                barrier.wait();
                let reply = login.service.push(&login.request(page));
                results.lock().unwrap().push((index, reply));
            });
        }
    });
    let results = results.lock().unwrap();
    let winners: Vec<_> = results
        .iter()
        .filter(|(_, reply)| reply.state == PushState::RemoteSaved)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let remote = cli_feedback(&html);
    assert_eq!(remote.replies["q1"], format!("login {}", winners[0].0));
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_keep_the_local_copy_when_the_connection_is_gone_before_the_remote_saved() {
    let root = scratch("before");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let relay = Relay::connect(&login.route_env().socket, &login.route).unwrap();
    let page = page_from_agent_files(&html, &login.route).unwrap();
    relay.open(page.clone()).unwrap();
    relay.disconnect();
    wait_until(|| login.hub.open_instances().is_empty());
    let set = replies(&[("q1", "SQLite")]);
    let reply = login.save_and_push(&page, &set);
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(reply.error.unwrap().contains("saved copy kept"));
    assert_eq!(login.local(&page), set);
    assert_eq!(login.sync(&page).synced_digest, None);
    assert!(
        !html.with_extension("revision").exists(),
        "remote untouched"
    );
    assert_eq!(cli_feedback(&html), AgentReplies::default());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_it_may_have_saved_and_stay_safe_to_retry_when_the_ack_is_lost() {
    let root = scratch("after");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let relay = Relay::connect(&login.route_env().socket, &login.route).unwrap();
    let page = page_from_agent_files(&html, &login.route).unwrap();
    relay.open(page.clone()).unwrap();
    let set = replies(&[("q1", "SQLite")]);
    login.save_locally(&page, &set);
    let request = login.request(&page);
    let pusher = thread::scope(|scope| {
        let handle = scope.spawn(|| login.service.push(&request));
        let asked = relay.next_request(Duration::from_secs(5)).unwrap();
        let Body::Save {
            revision,
            annotations,
            ..
        } = &asked.body
        else {
            panic!("expected a save, got {:?}", asked.body)
        };
        // Persisted on the sender; the acknowledgment never leaves.
        apply_save(&html, &page.page_id, &page.instance, *revision, annotations).unwrap();
        relay.disconnect();
        handle.join().unwrap()
    });
    assert_eq!(pusher.state, PushState::LocalOnly, "{pusher:?}");
    assert_eq!(login.local(&page), set, "the local copy is kept");
    assert_eq!(cli_feedback(&html), set, "the remote did save");
    // A retry of the same revision by the same writer changes nothing.
    assert_eq!(
        apply_save(&html, &page.page_id, &page.instance, 1, &json!(set)).unwrap(),
        1
    );
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    // And the writer may revise its own unconfirmed save at that revision.
    let revised = replies(&[("q1", "Postgres")]);
    apply_save(&html, &page.page_id, &page.instance, 1, &json!(revised)).unwrap();
    assert_eq!(cli_feedback(&html), revised);
    assert_eq!(read_revision(&html).unwrap().revision, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_another_writer_at_the_stored_revision_when_saving() {
    let root = scratch("writer");
    let html = agent_page(&root.join("sender"), None);
    let page = page_from_agent_files(&html, "r").unwrap();
    apply_save(
        &html,
        &page.page_id,
        "i-a",
        1,
        &json!(replies(&[("q1", "a")])),
    )
    .unwrap();
    for revision in [0, 1, 3] {
        let error = apply_save(
            &html,
            &page.page_id,
            "i-b",
            revision,
            &json!(replies(&[("q1", "b")])),
        )
        .unwrap_err();
        assert_eq!(error.0, ErrorCode::Rejected, "revision {revision}");
    }
    assert_eq!(read_feedback(&html).unwrap().replies["q1"], "a");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_a_malformed_save_without_touching_the_page_when_annotations_are_bad() {
    let root = scratch("malformed");
    let html = agent_page(&root.join("sender"), None);
    let page = page_from_agent_files(&html, "r").unwrap();
    let before = fs::read(&html).unwrap();
    for bad in [
        json!({"replies": {"not-a-question": "x"}}),
        json!({"replies": 7}),
        json!({"comments": {"0-1": {"text": "t", "quote": "", "start_line": 5, "end_line": 2}}}),
    ] {
        let error = apply_save(&html, &page.page_id, "i", 1, &bad).unwrap_err();
        assert_eq!(error.0, ErrorCode::Malformed, "{bad}");
    }
    assert_eq!(fs::read(&html).unwrap(), before);
    assert!(!html.with_extension("revision").exists());
    assert!(!sidecar_path(&html).exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_the_save_when_the_remote_page_changed_since_it_was_opened() {
    let root = scratch("changed");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    fs::write(html.with_extension("md"), "# A different response\n").unwrap();
    let reply = login.save_and_push(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(reply.state, PushState::LocalOnly, "{reply:?}");
    assert!(reply.error.unwrap().contains("changed"));
    assert!(!html.with_extension("revision").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_never_contact_the_agent_listener_when_feedback_is_saved() {
    let root = scratch("idle");
    let html = agent_page(&root.join("sender"), None);
    let listener_path = root.join("agent.sock");
    let listener = UnixListener::bind(&listener_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(
        html.with_extension("meta.json"),
        json!({
            "agent": "claude", "session_id": "sess-a", "turn_spec": "0", "title": "Pick",
            "feedback_sock": listener_path,
        })
        .to_string(),
    )
    .unwrap();
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let reply = login.save_and_push(&page, &replies(&[("q1", "SQLite")]));
    assert_eq!(reply.state, PushState::RemoteSaved);
    assert!(
        matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the agent was contacted"
    );
    let meta = fs::read_to_string(html.with_extension("meta.json")).unwrap();
    assert!(meta.contains("feedback_sock"), "meta untouched");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_answer_over_the_save_socket_when_the_viewer_asks() {
    let root = scratch("socket");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    login.save_locally(&page, &replies(&[("q1", "SQLite")]));
    let reply = request_push(
        login.service.path(),
        &login.request(&page),
        Duration::from_secs(5),
    );
    assert_eq!(reply.state, PushState::RemoteSaved, "{reply:?}");
    assert_eq!(cli_feedback(&html).replies["q1"], "SQLite");
    // The recorded origin names this socket for the viewer to find.
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(login.imported(&page).meta).unwrap()).unwrap();
    assert_eq!(meta["remote"]["save_sock"], json!(login.service.path()));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_report_local_only_when_the_page_or_launcher_is_unknown() {
    let root = scratch("unknown");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let unknown = PushRequest {
        instance: "nope".into(),
        page_id: page.page_id.clone(),
    };
    let reply = request_push(login.service.path(), &unknown, Duration::from_secs(5));
    assert_eq!(reply.state, PushState::LocalOnly);
    let wrong_page = PushRequest {
        instance: page.instance.clone(),
        page_id: "md-other".into(),
    };
    let reply = request_push(login.service.path(), &wrong_page, Duration::from_secs(5));
    assert_eq!(reply.state, PushState::LocalOnly);
    let gone = request_push(
        &root.join("absent.sock"),
        &login.request(&page),
        Duration::from_secs(1),
    );
    assert_eq!(gone.state, PushState::LocalOnly);
    assert!(gone.error.unwrap().contains("gone"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_answer_a_malformed_request_with_local_only_when_the_line_is_not_json() {
    use std::io::{Read, Write};
    let root = scratch("garbage");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (_page, _relay) = login.open(&html);
    let mut stream = std::os::unix::net::UnixStream::connect(login.service.path()).unwrap();
    stream.write_all(b"{not json\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let reply: PushReply = serde_json::from_str(answer.trim()).unwrap();
    assert_eq!(reply.state, PushState::LocalOnly);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_refuse_a_submit_and_still_not_contact_the_agent_when_the_hub_submits() {
    let root = scratch("submit");
    let html = agent_page(&root.join("sender"), None);
    let login = login(&root, "r1");
    let (page, _relay) = login.open(&html);
    let error = login
        .hub
        .submit(&page.instance, &page.page_id, 1)
        .unwrap_err();
    assert!(error.to_string().contains("not available"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_read_remote_sidecar_feedback_into_the_opened_page_when_one_exists() {
    let root = scratch("sidecar");
    let html = agent_page(&root.join("sender"), None);
    replies(&[("q1", "from the sidecar")])
        .save(&sidecar_path(&html))
        .unwrap();
    let page = page_from_agent_files(&html, "r").unwrap();
    assert_eq!(page.annotations["replies"]["q1"], "from the sidecar");
    let _ = fs::remove_dir_all(root);
}
