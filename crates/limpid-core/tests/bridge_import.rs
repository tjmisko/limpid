//! Stage 2: a page sent over the bridge lands in a separate local cache and is
//! handed to the viewer like an ordinary local agent page.

use limpid_core::{
    bridge::{Delivery, ErrorCode, Hub, OpenPage, Relay},
    bridge_import::{Imported, Importer, LauncherOpener, Opener, RemoteOrigin, handoff_for},
    replies::{AgentReplies, read_annotations_file, sidecar_path},
    transport::{Handoff, Scheme},
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Recorder {
    opened: Mutex<Vec<(Imported, Scheme)>>,
    fail: Mutex<Option<String>>,
}

impl Opener for Recorder {
    fn open(&self, imported: &Imported, theme: Scheme) -> Result<(), String> {
        if let Some(message) = self.fail.lock().unwrap().clone() {
            return Err(message);
        }
        self.opened.lock().unwrap().push((imported.clone(), theme));
        Ok(())
    }
}

struct World {
    root: PathBuf,
    /// Where the sender keeps its own files; the receiver never sees it.
    sender: PathBuf,
    receiver: PathBuf,
    opener: Arc<Recorder>,
    hub: Hub,
    sock: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn world(name: &str) -> World {
    let root = std::env::temp_dir().join(format!("limpid-import-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let sender = root.join("sender");
    let receiver = root.join("receiver");
    fs::create_dir_all(&sender).unwrap();
    let opener = Arc::new(Recorder::default());
    let sock = root.join("b.sock");
    let importer = Importer::new(receiver.clone(), Scheme::Dark, opener.clone());
    let hub = Hub::bind(&sock, Arc::new(importer))
        .unwrap()
        .with_timeout(TIMEOUT);
    World {
        root,
        sender,
        receiver,
        opener,
        hub,
        sock,
    }
}

fn page(instance: &str, markdown: &str, annotations: serde_json::Value) -> OpenPage {
    OpenPage {
        route: "r1".into(),
        session: "sess-a".into(),
        page_id: "page-1".into(),
        instance: instance.into(),
        remote_path: Some("/home/remote/only-there/page.md".into()),
        markdown: markdown.into(),
        annotations,
        delivery: Delivery::Remote,
    }
}

fn relay(world: &World) -> Relay {
    Relay::connect(&world.sock, "r1")
        .unwrap()
        .with_timeout(TIMEOUT)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn sample_annotations() -> serde_json::Value {
    json!({
        "replies": {"q1": "SQLite, 日本語 ✓"},
        "comments": {
            "0-9": {"text": "tighten", "quote": "# Plan", "start_line": 1, "end_line": 2}
        }
    })
}

const MARKDOWN: &str = "# Plan\r\n\r\nUnicode: naïve café 日本語 🙂\r\n\r\n## Questions\r\n\r\n1. Which store?\r\n   - SQLite";

#[test]
fn should_import_markdown_byte_for_byte_when_the_page_has_unicode_and_crlf() {
    let w = world("bytes");
    relay(&w)
        .open(page("i1", MARKDOWN, sample_annotations()))
        .unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    assert_eq!(fs::read(&imported.markdown).unwrap(), MARKDOWN.as_bytes());
    assert!(imported.markdown.starts_with(&w.receiver));
}

#[test]
fn should_import_annotations_into_the_sidecar_when_the_page_has_feedback() {
    let w = world("annotations");
    relay(&w)
        .open(page("i1", MARKDOWN, sample_annotations()))
        .unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    let saved = read_annotations_file(&imported.markdown).unwrap();
    assert_eq!(saved.replies["q1"], "SQLite, 日本語 ✓");
    assert_eq!(saved.comments["0-9"].text, "tighten");
    assert_eq!(saved.comments["0-9"].end_line, 2);
    assert_eq!(imported.sidecar, sidecar_path(&imported.page));
    assert_eq!(imported.sidecar, sidecar_path(&imported.markdown));
}

#[test]
fn should_not_touch_the_sender_directory_when_importing() {
    let w = world("isolation");
    let sender_file = w.sender.join("page.md");
    fs::write(&sender_file, "sender-side bytes").unwrap();
    // The remote path names a sender file; the importer must not read it.
    let mut open = page("i1", "# Imported\n", json!(null));
    open.remote_path = Some(sender_file.to_string_lossy().into_owned());
    relay(&w).open(open).unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    assert_eq!(
        fs::read_to_string(&imported.markdown).unwrap(),
        "# Imported\n"
    );
    assert_eq!(files_in(&w.sender), vec!["page.md"]);
    assert!(!imported.markdown.starts_with(&w.sender));
    fs::remove_file(&sender_file).unwrap();
    assert!(imported.markdown.is_file(), "no dependence on remote paths");
}

#[test]
fn should_record_the_remote_origin_in_the_meta_when_importing() {
    let w = world("origin");
    relay(&w).open(page("i1", "# Plan\n", json!(null))).unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&imported.meta).unwrap()).unwrap();
    let origin: RemoteOrigin = serde_json::from_value(meta["remote"].clone()).unwrap();
    assert_eq!(origin.route, "r1");
    assert_eq!(origin.session, "sess-a");
    assert_eq!(origin.page_id, "page-1");
    assert_eq!(origin.instance, "i1");
    assert_eq!(
        origin.remote_path.as_deref(),
        Some("/home/remote/only-there/page.md")
    );
    assert!(origin.preview);
    assert_eq!(meta["title"], "Plan");
    assert_eq!(meta["session_id"], "sess-a");
}

#[test]
fn should_match_an_ordinary_local_agent_page_when_comparing_files_and_handoff() {
    let w = world("parity");
    // A local page written by the real `limpid-core claude`.
    let local_cache = w.root.join("local-cache");
    fs::create_dir_all(&local_cache).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpid-core"))
        .args(["claude", "0"])
        .env("XDG_CACHE_HOME", &local_cache)
        .env("XDG_CONFIG_HOME", &local_cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({
        "model": "m",
        "session_id": "sess-a",
        "last_assistant_message": "## Plan\n\nUnicode café 日本語\n\n## Questions\n\n1. Which?\n   - A\n",
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let local_html = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let local_md = local_html.with_extension("md");
    let local_meta: serde_json::Value =
        serde_json::from_slice(&fs::read(local_html.with_extension("meta.json")).unwrap()).unwrap();

    let markdown = fs::read_to_string(&local_md).unwrap();
    relay(&w).open(page("i1", &markdown, json!(null))).unwrap();
    let (imported, theme) = w.opener.opened.lock().unwrap()[0].clone();

    assert_eq!(
        fs::read(&imported.markdown).unwrap(),
        fs::read(&local_md).unwrap()
    );
    let remote_meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&imported.meta).unwrap()).unwrap();
    for key in [
        "agent",
        "session_id",
        "turn_spec",
        "source",
        "title",
        "feedback_sock",
    ] {
        assert_eq!(remote_meta[key], local_meta[key], "meta key {key}");
    }
    let extra: Vec<_> = remote_meta
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| !local_meta.as_object().unwrap().contains_key(*k))
        .collect();
    assert_eq!(extra, ["remote"]);

    // The handoff has the shape the launcher builds for a local agent page:
    // the Markdown path, no source file, not source-backed.
    let handoff = handoff_for(&imported, theme);
    assert_eq!(
        handoff,
        Handoff {
            path: imported.markdown.clone(),
            theme: Scheme::Dark,
            pane: None,
            source: None,
            source_hash: None,
            source_backed: false,
        }
    );
    assert!(
        imported.meta.is_file(),
        "meta beside the Markdown selects agent mode"
    );
    assert_eq!(
        imported.markdown.with_extension("meta.json"),
        imported.meta,
        "viewer finds the meta by swapping the extension"
    );
}

#[test]
fn should_keep_two_open_instances_apart_when_the_same_page_is_opened_twice() {
    let w = world("two");
    let r = relay(&w);
    r.open(page("i1", "# Same\n", json!({"replies": {"q1": "one"}})))
        .unwrap();
    r.open(page("i2", "# Same\n", json!({"replies": {"q1": "two"}})))
        .unwrap();
    let opened = w.opener.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 2);
    assert_ne!(opened[0].0.markdown, opened[1].0.markdown);
    let first = read_annotations_file(&opened[0].0.markdown).unwrap();
    let second = read_annotations_file(&opened[1].0.markdown).unwrap();
    assert_eq!(first.replies["q1"], "one");
    assert_eq!(second.replies["q1"], "two");
    assert_eq!(w.hub.open_instances(), vec!["i1", "i2"]);
}

#[test]
fn should_keep_the_imported_files_when_the_page_is_closed() {
    let w = world("close");
    let r = relay(&w);
    r.open(page("i1", "# Keep\n", json!({"replies": {"q1": "x"}})))
        .unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    r.close_page("i1").unwrap();
    assert!(imported.markdown.is_file());
    assert!(imported.sidecar.is_file());
}

fn assert_rejected(w: &World, open: OpenPage, needle: &str) {
    let error = relay(w).open(open).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("Rejected"), "{text}");
    assert!(text.contains(needle), "{text}");
    assert!(
        files_in(&w.receiver).is_empty(),
        "{:?}",
        files_in(&w.receiver)
    );
    assert!(w.opener.opened.lock().unwrap().is_empty());
    assert!(
        w.hub.open_instances().is_empty(),
        "rejected page stays unbound"
    );
}

#[test]
fn should_reject_the_open_when_annotations_are_not_an_object() {
    let w = world("bad-shape");
    assert_rejected(
        &w,
        page("i1", "# x\n", json!("nope")),
        "malformed annotations",
    );
}

#[test]
fn should_reject_the_open_when_annotations_have_unknown_fields() {
    let w = world("bad-field");
    assert_rejected(
        &w,
        page("i1", "# x\n", json!({"replies": {}, "extra": 1})),
        "malformed annotations",
    );
}

#[test]
fn should_reject_the_open_when_a_question_id_is_invalid() {
    let w = world("bad-qid");
    assert_rejected(
        &w,
        page("i1", "# x\n", json!({"replies": {"../evil": "x"}})),
        "bad question id",
    );
}

#[test]
fn should_reject_the_open_when_a_comment_ends_before_it_starts() {
    let w = world("bad-range");
    assert_rejected(
        &w,
        page(
            "i1",
            "# x\n",
            json!({"comments": {"0-1": {"text": "t", "quote": "q", "start_line": 5, "end_line": 2}}}),
        ),
        "ends before it starts",
    );
}

#[test]
fn should_reject_the_open_when_an_identity_field_is_empty() {
    let w = world("empty-id");
    let mut open = page("i1", "# x\n", json!(null));
    open.page_id = String::new();
    assert_rejected(&w, open, "empty page_id");
}

#[test]
fn should_open_when_remote_path_and_annotations_are_missing() {
    let w = world("minimal");
    let mut open = page("i1", "# Bare\n", json!(null));
    open.remote_path = None;
    relay(&w).open(open).unwrap();
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&imported.meta).unwrap()).unwrap();
    assert!(meta["remote"]["remote_path"].is_null());
    assert_eq!(
        read_annotations_file(&imported.markdown).unwrap(),
        AgentReplies::default()
    );
}

#[test]
fn should_name_files_from_identity_when_remote_strings_contain_path_separators() {
    let w = world("hostile");
    let mut open = page("../../escape", "# x\n", json!(null));
    open.session = "../sess".into();
    open.remote_path = Some("../../../etc/passwd".into());
    relay(&w).open(open).unwrap();
    let names = files_in(&w.receiver);
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names
            .iter()
            .all(|n| n.starts_with("remote-") && !n.contains(".."))
    );
    assert!(!w.root.join("escape").exists());
}

#[test]
fn should_report_failure_and_leave_no_files_when_the_viewer_refuses_the_page() {
    let w = world("refused");
    *w.opener.fail.lock().unwrap() = Some("no viewer".into());
    let error = relay(&w)
        .open(page("i1", "# x\n", json!(null)))
        .unwrap_err();
    assert!(error.to_string().contains("no viewer"), "{error}");
    assert!(files_in(&w.receiver).is_empty());
    assert!(w.hub.open_instances().is_empty());
}

#[test]
fn should_acknowledge_only_after_the_viewer_accepts_when_opening() {
    let w = world("ack");
    relay(&w).open(page("i1", "# x\n", json!(null))).unwrap();
    assert_eq!(
        w.opener.opened.lock().unwrap().len(),
        1,
        "the ack arrives after the opener ran"
    );
}

#[test]
fn should_not_copy_or_link_assets_when_the_markdown_references_files() {
    let w = world("assets");
    let sentinel = w.root.join("secret.png");
    fs::write(&sentinel, "local secret").unwrap();
    let markdown = format!(
        "# Assets\n\n![a](diagram.png)\n\n![b]({})\n\n![c](file://{})\n",
        sentinel.display(),
        sentinel.display()
    );
    relay(&w).open(page("i1", &markdown, json!(null))).unwrap();
    let names = files_in(&w.receiver);
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(
        names
            .iter()
            .all(|n| n.ends_with(".md") || n.ends_with(".json"))
    );
    assert!(!w.receiver.join("diagram.png").exists());
    let (imported, _) = w.opener.opened.lock().unwrap()[0].clone();
    assert_eq!(fs::read_to_string(&imported.markdown).unwrap(), markdown);
}

#[test]
fn should_refuse_delivery_when_the_page_is_a_remote_preview() {
    let w = world("preview");
    relay(&w)
        .open(page("i1", "# x\n", json!({"replies": {"q1": "answer"}})))
        .unwrap();
    let (imported, theme) = w.opener.opened.lock().unwrap()[0].clone();
    let marker = w.root.join("delivered");
    let command = format!("touch {}", marker.display());
    let handoff = handoff_for(&imported, theme);
    let error = limpid_core::replies::deliver_or_wake_with(&handoff, "msg", &command).unwrap_err();
    assert!(
        error.to_string().contains("submission is unavailable"),
        "{error}"
    );
    let error = limpid_core::replies::deliver_with_command(&handoff, "msg", &command).unwrap_err();
    assert!(
        error.to_string().contains("submission is unavailable"),
        "{error}"
    );
    assert!(!marker.exists(), "no local fallback ran");
}

fn fake_launcher(dir: &Path, exit: i32, stderr: &str) -> (PathBuf, PathBuf) {
    let log = dir.join("launcher.log");
    let script = dir.join("limpid");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\necho '{stderr}' >&2\nexit {exit}\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

fn imported_fixture(dir: &Path) -> Imported {
    Imported {
        markdown: dir.join("remote-x.md"),
        page: dir.join("remote-x.html"),
        meta: dir.join("remote-x.meta.json"),
        sidecar: dir.join("remote-x.replies.json"),
    }
}

#[test]
fn should_launch_the_native_viewer_with_the_agent_page_when_using_the_launcher_opener() {
    let dir = std::env::temp_dir().join(format!("limpid-import-launch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (script, log) = fake_launcher(&dir, 0, "");
    let imported = imported_fixture(&dir);
    LauncherOpener::new(script)
        .open(&imported, Scheme::Light)
        .unwrap();
    let args = fs::read_to_string(log).unwrap();
    assert_eq!(
        args.trim(),
        format!("--viewer view --light {}", imported.page.display())
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_report_the_launcher_error_when_the_viewer_cannot_start() {
    let dir = std::env::temp_dir().join(format!("limpid-import-launchfail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (script, _) = fake_launcher(&dir, 1, "limpid: no display");
    let error = LauncherOpener::new(script)
        .open(&imported_fixture(&dir), Scheme::Dark)
        .unwrap_err();
    assert!(error.contains("no display"), "{error}");
    let error = LauncherOpener::new(dir.join("absent"))
        .open(&imported_fixture(&dir), Scheme::Dark)
        .unwrap_err();
    assert!(error.contains("absent"), "{error}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_surface_a_missing_viewer_as_a_rejection_when_opening_through_the_bridge() {
    let w = world("noviewer-bridge");
    let importer = Importer::new(
        w.receiver.join("c2"),
        Scheme::Dark,
        Arc::new(LauncherOpener::new(w.root.join("absent"))),
    );
    let sock = w.root.join("c2.sock");
    let hub = Hub::bind(&sock, Arc::new(importer))
        .unwrap()
        .with_timeout(TIMEOUT);
    let error = Relay::connect(&sock, "r1")
        .unwrap()
        .with_timeout(TIMEOUT)
        .open(page("i1", "# x\n", json!(null)))
        .unwrap_err();
    assert!(matches!(
        error,
        limpid_core::bridge::BridgeError::Remote {
            code: ErrorCode::Rejected,
            ..
        }
    ));
    drop(hub);
}

#[test]
fn should_kill_the_launcher_and_fail_when_it_outlives_the_deadline() {
    let dir = std::env::temp_dir().join(format!("limpid-import-hang-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let pidfile = dir.join("pid");
    let script = dir.join("limpid");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 60\n",
            pidfile.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let opener = LauncherOpener {
        program: script,
        timeout: Duration::from_millis(300),
    };
    let started = std::time::Instant::now();
    let error = opener
        .open(&imported_fixture(&dir), Scheme::Dark)
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(error.contains("did not respond"), "{error}");
    let pid = fs::read_to_string(&pidfile).unwrap().trim().to_string();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "launcher {pid} still running"
    );
    assert!(!dir.join("remote-x.launcher.err").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_clean_up_and_acknowledge_failure_when_the_launcher_hangs_during_a_bridge_open() {
    let w = world("hang-bridge");
    let script = w.root.join("limpid");
    fs::write(&script, "#!/bin/sh\nexec sleep 60\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let cache = w.receiver.join("c3");
    let importer = Importer::new(
        cache.clone(),
        Scheme::Dark,
        Arc::new(LauncherOpener {
            program: script,
            timeout: Duration::from_millis(300),
        }),
    );
    let sock = w.root.join("c3.sock");
    let hub = Hub::bind(&sock, Arc::new(importer))
        .unwrap()
        .with_timeout(TIMEOUT);
    let error = Relay::connect(&sock, "r1")
        .unwrap()
        .with_timeout(TIMEOUT)
        .open(page("i1", "# x\n", json!(null)))
        .unwrap_err();
    assert!(error.to_string().contains("did not respond"), "{error}");
    assert!(files_in(&cache).is_empty(), "{:?}", files_in(&cache));
    assert!(hub.open_instances().is_empty());
}
