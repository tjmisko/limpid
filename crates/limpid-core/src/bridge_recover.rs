//! Explicit recovery of feedback that is saved locally but not on the sender.
//!
//! The viewer keeps every saved annotation in the local cache and records, per
//! imported page, the digest of the feedback the sender last acknowledged
//! (`<stem>.sync.json`). When the login that opened a page is gone (the shell
//! exited, the network dropped, the launcher restarted) nothing can push that
//! page any more, so the cache is the only copy. Recovery lists those pages and
//! prints the saved feedback as the message `:wq` would have composed, so the
//! user can give it to an agent by hand.
//!
//! Nothing here contacts a remote host or an agent, and nothing is deleted or
//! rewritten: an agent session is never moved to another client, and a page
//! reopened under a new login is a new import with its own copy.

use crate::{
    bridge_import::{SyncState, replies_digest},
    replies::{AgentReplies, compose_message, sidecar_path},
};
use std::path::{Path, PathBuf};

/// One imported page whose local feedback the sender does not hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Unsent {
    pub stem: String,
    pub markdown: PathBuf,
    pub sidecar: PathBuf,
    pub route: String,
    pub session: String,
    pub remote_path: Option<String>,
    pub title: String,
    pub replies: AgentReplies,
}

/// The result of reading a cache: what is unsent, and entries that could not
/// be read (reported, never fatal).
#[derive(Debug, Default, PartialEq)]
pub struct Scan {
    pub unsent: Vec<Unsent>,
    pub warnings: Vec<String>,
}

impl Unsent {
    /// Whether there is text to hand to an agent. A page whose local feedback
    /// was deleted is unsent too (the sender still holds the old copy), but it
    /// has nothing to say.
    pub fn has_feedback(&self) -> bool {
        self.replies.has_feedback()
    }

    /// One line for a listing.
    pub fn describe(&self) -> String {
        let origin = self.remote_path.as_deref().unwrap_or("unknown remote path");
        let what = if self.has_feedback() {
            "unsent feedback"
        } else {
            "unsent deletion (the remote still holds older feedback)"
        };
        format!(
            "{}  {}  [{}]  {origin}  session {}  login {}",
            self.stem, self.title, what, self.session, self.route
        )
    }

    /// The message `:wq` would have sent, for pasting into the agent session.
    pub fn compose(&self) -> String {
        compose_message(
            &self.title,
            "claude",
            &format!("recovered from login {}", self.route),
            &self.replies,
        )
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn read_entry(meta_path: &Path, stem: &str, cache: &Path) -> Result<Option<Unsent>, String> {
    let meta = read_json(meta_path)?;
    let Some(remote) = meta.get("remote").filter(|value| value.is_object()) else {
        return Ok(None);
    };
    let markdown = cache.join(format!("{stem}.md"));
    let sidecar = sidecar_path(&cache.join(format!("{stem}.html")));
    let replies =
        AgentReplies::load(&sidecar).map_err(|error| format!("{}: {error}", sidecar.display()))?;
    let sync_path = markdown.with_extension("sync.json");
    let synced = match std::fs::read(&sync_path) {
        Ok(bytes) => {
            serde_json::from_slice::<SyncState>(&bytes)
                .map_err(|error| format!("{}: {error}", sync_path.display()))?
                .synced_digest
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", sync_path.display())),
    };
    if synced.as_deref() == Some(replies_digest(&replies).as_str()) {
        return Ok(None);
    }
    // No acknowledged digest and nothing local: there is nothing to lose.
    if synced.is_none() && !replies.has_feedback() {
        return Ok(None);
    }
    Ok(Some(Unsent {
        stem: stem.to_string(),
        markdown,
        sidecar,
        route: text(remote, "route").unwrap_or_default(),
        session: text(remote, "session").unwrap_or_default(),
        remote_path: text(remote, "remote_path"),
        title: text(&meta, "title").unwrap_or_default(),
        replies,
    }))
}

/// Read `cache` (the directory imports are written to). A missing cache is an
/// empty result.
pub fn scan(cache: &Path) -> Result<Scan, String> {
    let entries = match std::fs::read_dir(cache) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(error) => return Err(format!("{}: {error}", cache.display())),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".meta.json").map(str::to_string))
        .collect();
    names.sort();
    let mut result = Scan::default();
    for stem in names {
        let meta_path = cache.join(format!("{stem}.meta.json"));
        match read_entry(&meta_path, &stem, cache) {
            Ok(Some(unsent)) => result.unsent.push(unsent),
            Ok(None) => {}
            Err(warning) => result.warnings.push(warning),
        }
    }
    Ok(result)
}

pub fn usage() -> &'static str {
    "Usage: limpid recover [--cache DIR] [--text STEM]"
}

/// The text `limpid recover` prints. `--text STEM` prints the composed
/// feedback for one page; otherwise the unsent pages are listed.
pub fn run(args: &[String], default_cache: &Path) -> Result<String, String> {
    let (mut cache, mut stem) = (default_cache.to_path_buf(), None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--cache" => cache = rest.next().ok_or("--cache requires a directory")?.into(),
            "--text" => stem = Some(rest.next().ok_or("--text requires a page name")?.clone()),
            other => return Err(format!("unknown argument {other:?}; {}", usage())),
        }
    }
    let scan = scan(&cache)?;
    let mut out = String::new();
    for warning in &scan.warnings {
        out.push_str(&format!("warning: skipped {warning}\n"));
    }
    if let Some(stem) = stem {
        let page = scan
            .unsent
            .iter()
            .find(|page| page.stem == stem)
            .ok_or_else(|| format!("{stem} has no unsent feedback in {}", cache.display()))?;
        if !page.has_feedback() {
            return Err(format!(
                "{stem} has no feedback text: only a deletion is unsent"
            ));
        }
        out.push_str(&page.compose());
        out.push('\n');
        return Ok(out);
    }
    if scan.unsent.is_empty() {
        out.push_str("No unsent feedback.\n");
        return Ok(out);
    }
    for page in &scan.unsent {
        out.push_str(&page.describe());
        out.push('\n');
    }
    out.push_str(&format!(
        "Print one with `limpid recover --text NAME` and give it to the agent. Nothing is \
         removed; delete NAME.* in {} once it is dealt with.\n",
        cache.display()
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_import::SyncState;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("limpid-recover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn page(cache: &Path, stem: &str, local: &[(&str, &str)], synced: Option<&[(&str, &str)]>) {
        let set = |pairs: &[(&str, &str)]| {
            let mut replies = AgentReplies::default();
            for (field, text) in pairs {
                replies.replies.insert((*field).into(), (*text).into());
            }
            replies
        };
        std::fs::write(cache.join(format!("{stem}.md")), "# T\n").unwrap();
        set(local)
            .save(&sidecar_path(&cache.join(format!("{stem}.html"))))
            .unwrap();
        let sync = SyncState {
            revision: 1,
            synced_digest: synced.map(|pairs| replies_digest(&set(pairs))),
        };
        std::fs::write(
            cache.join(format!("{stem}.sync.json")),
            serde_json::to_vec(&sync).unwrap(),
        )
        .unwrap();
        std::fs::write(
            cache.join(format!("{stem}.meta.json")),
            serde_json::json!({
                "title": "T",
                "remote": {"route": "r1", "session": "s1", "remote_path": "/home/u/p.html"}
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn should_list_only_pages_the_sender_lacks_when_scanning() {
        let cache = scratch("scan");
        page(&cache, "remote-a", &[("q1", "x")], Some(&[("q1", "x")]));
        page(
            &cache,
            "remote-b",
            &[("q1", "edited")],
            Some(&[("q1", "x")]),
        );
        page(&cache, "remote-c", &[("q1", "x")], None);
        page(&cache, "remote-d", &[], Some(&[("q1", "x")]));
        page(&cache, "remote-e", &[], None);
        let scan = scan(&cache).unwrap();
        let stems: Vec<&str> = scan.unsent.iter().map(|page| page.stem.as_str()).collect();
        assert_eq!(stems, ["remote-b", "remote-c", "remote-d"]);
        assert!(scan.unsent[0].has_feedback() && !scan.unsent[2].has_feedback());
        assert!(scan.warnings.is_empty());
    }

    #[test]
    fn should_ignore_local_pages_and_report_unreadable_entries_when_scanning() {
        let cache = scratch("mixed");
        std::fs::write(cache.join("claude-1.meta.json"), r#"{"title":"local"}"#).unwrap();
        std::fs::write(cache.join("remote-bad.meta.json"), "{not json").unwrap();
        page(&cache, "remote-ok", &[("q1", "x")], None);
        let scan = scan(&cache).unwrap();
        assert_eq!(scan.unsent.len(), 1);
        assert_eq!(scan.warnings.len(), 1);
        assert!(scan.warnings[0].contains("remote-bad.meta.json"));
    }

    #[test]
    fn should_return_nothing_when_the_cache_does_not_exist() {
        let cache = scratch("none").join("absent");
        assert_eq!(scan(&cache).unwrap(), Scan::default());
    }

    #[test]
    fn should_compose_the_saved_feedback_when_one_page_is_asked_for() {
        let cache = scratch("text");
        page(&cache, "remote-a", &[("q1", "SQLite, naïve 日本語")], None);
        let args = ["--text".to_string(), "remote-a".to_string()];
        let out = run(&args, &cache).unwrap();
        assert!(out.contains("re: T"), "{out}");
        assert!(out.contains("q1: SQLite, naïve 日本語"), "{out}");
        assert!(out.contains("login r1"), "{out}");
    }

    #[test]
    fn should_refuse_text_when_the_page_has_nothing_unsent_or_only_a_deletion() {
        let cache = scratch("refuse");
        page(&cache, "remote-a", &[("q1", "x")], Some(&[("q1", "x")]));
        page(&cache, "remote-d", &[], Some(&[("q1", "x")]));
        let ask = |stem: &str| run(&["--text".into(), stem.into()], &cache);
        assert!(ask("remote-a").unwrap_err().contains("no unsent feedback"));
        assert!(ask("remote-d").unwrap_err().contains("deletion"));
        assert!(ask("remote-missing").is_err());
    }

    #[test]
    fn should_list_pages_and_say_so_when_nothing_is_unsent() {
        let cache = scratch("list");
        assert_eq!(run(&[], &cache).unwrap(), "No unsent feedback.\n");
        page(&cache, "remote-a", &[("q1", "x")], None);
        let listing = run(&[], &cache).unwrap();
        assert!(
            listing.contains("remote-a") && listing.contains("/home/u/p.html"),
            "{listing}"
        );
        assert!(
            run(&["--bogus".into()], &cache)
                .unwrap_err()
                .contains("Usage")
        );
        assert!(run(&["--cache".into()], &cache).is_err());
    }
}
