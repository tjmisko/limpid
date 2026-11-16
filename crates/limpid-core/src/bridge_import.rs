//! Receiving side of the bridge: import a transferred page into the local
//! cache and open it in the native viewer.
//!
//! The import is laid out like a page `limpid-core <agent>` writes
//! (`<stem>.md`, `<stem>.meta.json`, `<stem>.replies.json`), so the viewer
//! treats it as an ordinary agent page. Names derive from a hash of the page's
//! identity, never from sender-supplied paths, and the sender's paths stay
//! metadata. Nothing is fetched from the sender: the Markdown is stored
//! byte for byte, and no asset beside it is created or linked.
//!
//! Until the return path exists the page is a preview: its meta carries a
//! `remote` object, which makes delivery refuse to run.

use crate::{
    agent_page,
    bridge::{OpenPage, PageSink},
    replies::{AgentReplies, sidecar_path, valid_question_id},
    transport::{Handoff, Scheme},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Where an imported page came from, as written under `remote` in its meta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOrigin {
    pub route: String,
    pub session: String,
    pub page_id: String,
    pub instance: String,
    /// The sender's own path; display only.
    pub remote_path: Option<String>,
    /// Submission is unavailable while true.
    pub preview: bool,
    /// The launcher's socket that pushes saved feedback to the sender. Absent
    /// when the page was imported without a return route.
    #[serde(default)]
    pub save_sock: Option<String>,
}

/// The files an import created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub markdown: PathBuf,
    /// The `.html` path the existing launcher takes for an agent page. The
    /// file need not exist; its siblings are what the launcher reads.
    pub page: PathBuf,
    pub meta: PathBuf,
    pub sidecar: PathBuf,
}

fn reject(message: impl Into<String>) -> String {
    message.into()
}

/// A short, stable file stem for one open instance.
pub fn stem_for(page: &OpenPage) -> String {
    let joined = format!(
        "{}\0{}\0{}\0{}",
        page.route, page.session, page.page_id, page.instance
    );
    let digest = blake3::hash(joined.as_bytes()).to_hex().to_string();
    format!("remote-{}", &digest[..16])
}

fn annotations_of(page: &OpenPage) -> Result<AgentReplies, String> {
    parse_annotations(&page.annotations)
}

/// Annotations as sent over the bridge: null is none; anything else must be
/// well-formed.
pub fn parse_annotations(value: &serde_json::Value) -> Result<AgentReplies, String> {
    if value.is_null() {
        return Ok(AgentReplies::default());
    }
    let replies: AgentReplies = serde_json::from_value(value.clone())
        .map_err(|error| reject(format!("malformed annotations: {error}")))?;
    if let Some(field) = replies.replies.keys().find(|f| !valid_question_id(f)) {
        return Err(reject(format!(
            "malformed annotations: bad question id {field:?}"
        )));
    }
    if let Some((key, _)) = replies
        .comments
        .iter()
        .find(|(_, c)| c.start_line > c.end_line)
    {
        return Err(reject(format!(
            "malformed annotations: comment {key:?} ends before it starts"
        )));
    }
    if replies
        .edits
        .iter()
        .any(|edit| edit.start_line == 0 || edit.start_line > edit.end_line)
    {
        return Err(reject("malformed annotations: invalid edit line range"));
    }
    Ok(replies)
}

fn check_identity(page: &OpenPage) -> Result<(), String> {
    for (name, value) in [
        ("route", &page.route),
        ("session", &page.session),
        ("page_id", &page.page_id),
        ("instance", &page.instance),
    ] {
        if value.is_empty() {
            return Err(reject(format!("malformed page: empty {name}")));
        }
    }
    Ok(())
}

/// What the launcher knows about one page's feedback on the sender: the last
/// revision the sender acknowledged and the digest of the feedback it holds.
/// The page is saved remotely exactly when that digest is the local one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    pub revision: u64,
    pub synced_digest: Option<String>,
}

/// The sync state file of an import, beside its Markdown.
pub fn sync_path(imported: &Imported) -> PathBuf {
    imported.markdown.with_extension("sync.json")
}

/// A stable digest of a feedback set.
pub fn replies_digest(replies: &AgentReplies) -> String {
    let bytes = serde_json::to_vec(replies).unwrap_or_default();
    crate::replies::source_hash(&bytes)
}

impl SyncState {
    pub fn load(imported: &Imported) -> Result<Self, String> {
        let path = sync_path(imported);
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn store(&self, imported: &Imported) -> Result<(), String> {
        let path = sync_path(imported);
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        crate::replies::write_atomic(&path, &bytes)
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Write `page` into `cache`. Nothing is written when the page is malformed.
pub fn import_page(cache: &Path, page: &OpenPage) -> Result<Imported, String> {
    import_page_with(cache, page, None)
}

/// [`import_page`], naming the socket the viewer pushes saved feedback to.
pub fn import_page_with(
    cache: &Path,
    page: &OpenPage,
    save_sock: Option<&Path>,
) -> Result<Imported, String> {
    check_identity(page)?;
    let annotations = annotations_of(page)?;
    std::fs::create_dir_all(cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    let stem = stem_for(page);
    let imported = Imported {
        markdown: cache.join(format!("{stem}.md")),
        page: cache.join(format!("{stem}.html")),
        meta: cache.join(format!("{stem}.meta.json")),
        sidecar: sidecar_path(&cache.join(format!("{stem}.html"))),
    };
    // The sidecar is keyed by the page stem, as for a local agent page.
    let origin = RemoteOrigin {
        route: page.route.clone(),
        session: page.session.clone(),
        page_id: page.page_id.clone(),
        instance: page.instance.clone(),
        remote_path: page.remote_path.clone(),
        preview: true,
        save_sock: save_sock.map(|path| path.to_string_lossy().into_owned()),
    };
    let meta = serde_json::json!({
        "agent": "claude",
        "session_id": page.session,
        "turn_spec": "0",
        "source": null,
        "title": agent_page::first_line(&page.markdown),
        "feedback_sock": null,
        "remote": origin,
    });
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(&imported.markdown, page.markdown.as_bytes())?;
        annotations.save(&imported.sidecar)?;
        SyncState {
            revision: page.revision,
            synced_digest: Some(replies_digest(&annotations)),
        }
        .store(&imported)?;
        // The meta goes last: its presence is what makes this an agent page.
        std::fs::write(&imported.meta, serde_json::to_vec_pretty(&meta)?)?;
        Ok(())
    };
    if let Err(error) = write() {
        remove(&imported);
        return Err(format!("could not import page: {error}"));
    }
    Ok(imported)
}

fn remove(imported: &Imported) {
    for path in [
        &imported.meta,
        &imported.sidecar,
        &imported.markdown,
        &sync_path(imported),
    ] {
        let _ = std::fs::remove_file(path);
    }
}

/// The handoff the viewer receives for an import: the same shape the launcher
/// builds for a local agent page (Markdown path, no source file).
pub fn handoff_for(imported: &Imported, theme: Scheme) -> Handoff {
    Handoff {
        path: imported.markdown.clone(),
        theme,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    }
}

/// Shows an imported page. Success means the viewer accepted it.
pub trait Opener: Send + Sync + 'static {
    fn open(&self, imported: &Imported, theme: Scheme) -> Result<(), String>;
}

/// How long the launcher may take before it is killed.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens through the existing launcher: `limpid --viewer view <page.html>`,
/// which starts the viewer when it is not running. A launcher that outlives
/// `timeout` is killed and the open fails.
pub struct LauncherOpener {
    pub program: PathBuf,
    pub timeout: Duration,
}

impl LauncherOpener {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            timeout: LAUNCH_TIMEOUT,
        }
    }
}

impl Opener for LauncherOpener {
    fn open(&self, imported: &Imported, theme: Scheme) -> Result<(), String> {
        // Stderr goes to a file, not a pipe: a viewer the launcher started
        // inherits it and would hold a pipe open after the launcher exits.
        let errors = imported.markdown.with_extension("launcher.err");
        let result = self.run(imported, theme, &errors);
        let _ = std::fs::remove_file(&errors);
        result
    }
}

impl LauncherOpener {
    fn run(&self, imported: &Imported, theme: Scheme, errors: &Path) -> Result<(), String> {
        let name = self.program.display();
        let stderr = std::fs::File::create(errors).map_err(|error| format!("{name}: {error}"))?;
        let mut child = Command::new(&self.program)
            .args(["--viewer", "view", theme_flag(theme)])
            .arg(&imported.page)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("{name}: {error}"))?;
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "viewer did not respond within {:?}: {name} was killed",
                        self.timeout
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{name}: {error}"));
                }
            }
        };
        if status.success() {
            return Ok(());
        }
        let stderr = std::fs::read(errors).unwrap_or_default();
        Err(format!(
            "viewer did not accept the page: {}",
            String::from_utf8_lossy(&stderr).trim()
        ))
    }
}

fn theme_flag(theme: Scheme) -> &'static str {
    match theme {
        Scheme::Dark => "--dark",
        Scheme::Light => "--light",
    }
}

/// A [`PageSink`] that imports pages into a cache and opens them.
pub struct Importer {
    cache: PathBuf,
    theme: Scheme,
    opener: Arc<dyn Opener>,
    save_sock: Option<PathBuf>,
    /// Open instances by id, with the page identity each was accepted under.
    imports: Mutex<HashMap<String, (Imported, String)>>,
}

impl Importer {
    pub fn new(cache: PathBuf, theme: Scheme, opener: Arc<dyn Opener>) -> Self {
        Self {
            cache,
            theme,
            opener,
            save_sock: None,
            imports: Mutex::new(HashMap::new()),
        }
    }

    /// Imported pages name `socket` as their return route for saved feedback.
    pub fn with_save_socket(mut self, socket: PathBuf) -> Self {
        self.save_sock = Some(socket);
        self
    }

    /// The import for an open instance and the page identity it was accepted
    /// under. Kept after the sender closes it: the saved copy is still local.
    pub fn imported(&self, instance: &str) -> Option<(Imported, String)> {
        self.imports.lock().unwrap().get(instance).cloned()
    }
}

impl PageSink for Importer {
    fn open(&self, page: &OpenPage) -> Result<(), String> {
        let imported = import_page_with(&self.cache, page, self.save_sock.as_deref())?;
        // Registered before the viewer can see the page: its first save may
        // arrive as soon as the window exists.
        self.imports.lock().unwrap().insert(
            page.instance.clone(),
            (imported.clone(), page.page_id.clone()),
        );
        if let Err(error) = self.opener.open(&imported, self.theme) {
            self.imports.lock().unwrap().remove(&page.instance);
            remove(&imported);
            return Err(error);
        }
        Ok(())
    }

    /// The saved copy stays in the cache: it may hold local feedback.
    fn close(&self, _instance: &str, _page_id: &str) {}
}
