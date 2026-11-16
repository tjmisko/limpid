//! The sender's half of saving: persist a page's feedback on the machine that
//! owns the canonical page, and acknowledge the revision.
//!
//! Feedback lives where `limpid-core feedback` reads it: embedded in the agent
//! page's HTML and in its `<page>.replies.json` sidecar, which are merged on
//! read. A save replaces both with the same set, so deleted comments stay
//! deleted. Revisions live in `<page>.revision`. Saves to one page are
//! serialised by a lock on the page's Markdown, which also covers two logins
//! that opened the same page. Nothing here wakes or messages the agent.
//!
//! Revision rules, with `current` the stored revision and `writer` the
//! instance that stored it:
//! - a save for `current + 1` applies and becomes the new revision;
//! - a save for `current` from the same writer replaces its own earlier save,
//!   which makes a repeated save safe, including after a lost acknowledgment;
//! - anything else is stale and is refused without touching the page.

use crate::{
    bridge::ErrorCode,
    bridge_import::parse_annotations,
    replies::{
        AgentReplies, read_annotations_file, sidecar_path, source_hash, write_annotations_file,
        write_atomic,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

pub type SaveError = (ErrorCode, String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRevision {
    pub revision: u64,
    pub writer: String,
}

pub fn revision_path(html: &Path) -> PathBuf {
    html.with_extension("revision")
}

/// The stored revision of a page; none stored is revision 0.
pub fn read_revision(html: &Path) -> Result<StoredRevision, String> {
    let path = revision_path(html);
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredRevision {
            revision: 0,
            writer: String::new(),
        }),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The page's feedback as `limpid-core feedback` reads it: the embedded
/// annotations with the sidecar's entries over them.
pub fn read_feedback(html: &Path) -> Result<AgentReplies, String> {
    let mut merged = read_annotations_file(html).map_err(|error| error.to_string())?;
    let sidecar = AgentReplies::load(&sidecar_path(html)).map_err(|error| error.to_string())?;
    merged.replies.extend(sidecar.replies);
    merged.comments.extend(sidecar.comments);
    if sidecar_path(html).exists() {
        merged.edits = sidecar.edits;
    }
    Ok(merged)
}

/// The identity the page had when it was opened: a hash of its Markdown.
pub fn page_id_of(markdown: &[u8]) -> String {
    format!("md-{}", source_hash(markdown))
}

fn unavailable(message: impl Into<String>) -> SaveError {
    (ErrorCode::DeliveryUnavailable, message.into())
}

/// Save `annotations` as revision `revision` of the page at `html`, on behalf
/// of `writer`. Returns the revision now stored.
pub fn apply_save(
    html: &Path,
    page_id: &str,
    writer: &str,
    revision: u64,
    annotations: &serde_json::Value,
) -> Result<u64, SaveError> {
    let markdown_path = html.with_extension("md");
    let markdown = File::open(&markdown_path)
        .map_err(|error| unavailable(format!("{}: {error}", markdown_path.display())))?;
    markdown
        .lock()
        .map_err(|error| unavailable(format!("could not lock the page: {error}")))?;
    let bytes = std::fs::read(&markdown_path)
        .map_err(|error| unavailable(format!("{}: {error}", markdown_path.display())))?;
    if page_id_of(&bytes) != page_id {
        return Err((
            ErrorCode::PageMismatch,
            "the page changed on the remote since it was opened".into(),
        ));
    }
    let replies =
        parse_annotations(annotations).map_err(|message| (ErrorCode::Malformed, message))?;
    let stored = read_revision(html).map_err(unavailable)?;
    let current = read_feedback(html).map_err(unavailable)?;
    let own_repeat = revision == stored.revision && stored.writer == writer && revision > 0;
    if own_repeat && current == replies {
        return Ok(stored.revision);
    }
    if revision != stored.revision + 1 && !own_repeat {
        return Err((
            ErrorCode::Rejected,
            format!(
                "stale revision: the remote page is at revision {} and this save is for {}",
                stored.revision, revision
            ),
        ));
    }
    // The sidecar first, so an interrupted save leaves the older embedded set
    // under newer entries and a retry of the same revision completes it.
    replies
        .save(&sidecar_path(html))
        .map_err(|error| unavailable(format!("could not save the remote feedback: {error}")))?;
    if html.exists() {
        write_annotations_file(html, &replies)
            .map_err(|error| unavailable(format!("could not save the remote page: {error}")))?;
    }
    let record = StoredRevision {
        revision,
        writer: writer.to_string(),
    };
    let encoded = serde_json::to_vec(&record).map_err(|error| unavailable(error.to_string()))?;
    write_atomic(&revision_path(html), &encoded)
        .map_err(|error| unavailable(format!("could not record the revision: {error}")))?;
    Ok(revision)
}
