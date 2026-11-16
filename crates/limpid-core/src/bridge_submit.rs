//! The sender's half of submitting: compose the feedback saved on this host
//! and deliver it to the listener of the agent session the page came from.
//!
//! Submission only follows persistence. The page's stored revision must be the
//! one the viewer names, so what is composed is exactly what the viewer saw
//! acknowledged. The target is the listener recorded in the page's own meta,
//! which belongs to one session: when that session is gone (an exited agent,
//! or a `/clear` that moved the agent to a new session id) the delivery fails
//! and no other listener is tried. There is no local clipboard, pane or
//! `reply.command` fallback.
//!
//! The outcomes are kept apart on purpose. A listener that was never reached
//! or that refused the prompt leaves nothing submitted. A listener that took
//! the frame but never confirmed may have started a turn, so that is reported
//! as [`ErrorCode::SubmitUncertain`] and is never retried here. The feedback
//! stays saved in every case.
//!
//! A listener confirms only after the integration accepted the prompt (the
//! Claude Code listener waits for the plugin's acknowledgment of
//! `$.prompt.submit`), so `Accepted` means a turn was queued, not that a frame
//! was printed.

use crate::{
    bridge::ErrorCode,
    bridge_save::{SaveError, page_id_of, read_feedback, read_revision},
    replies::{compose_for_page, wake_message},
    transport::{FeedbackOutcome, FeedbackSignal, Handoff, Scheme, send_feedback_checked},
};
use std::{fs::File, path::Path, time::Duration};

/// How long the sender waits for the listener to confirm the prompt. It is
/// shorter than the hub's request timeout, so the hub hears an answer rather
/// than giving up first.
pub const LISTENER_WAIT: Duration = Duration::from_secs(3);

fn unavailable(message: impl Into<String>) -> SaveError {
    (ErrorCode::DeliveryUnavailable, message.into())
}

/// Submit the feedback of the page at `html` as saved at `revision`.
/// Returns the revision submitted.
pub fn submit_feedback(
    html: &Path,
    session: &str,
    page_id: &str,
    revision: u64,
    wait: Duration,
) -> Result<u64, SaveError> {
    let markdown_path = html.with_extension("md");
    let markdown = File::open(&markdown_path)
        .map_err(|error| unavailable(format!("{}: {error}", markdown_path.display())))?;
    // The same lock saves take: no save lands between composing and delivering.
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
    let stored = read_revision(html).map_err(unavailable)?;
    if stored.revision != revision {
        return Err((
            ErrorCode::Rejected,
            format!(
                "the remote page is at revision {} but the submission is for {}; save again first",
                stored.revision, revision
            ),
        ));
    }
    let handoff = Handoff {
        path: html.to_path_buf(),
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    let meta = crate::replies::page_meta(&handoff)
        .map_err(|error| unavailable(format!("{}: {error}", html.display())))?;
    let meta_text = |key: &str| {
        meta.get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    if meta_text("session_id") != Some(session) {
        return Err((
            ErrorCode::Rejected,
            "the page no longer names the agent session it was opened for".into(),
        ));
    }
    let replies = read_feedback(html).map_err(unavailable)?;
    if !replies.has_feedback() {
        return Err((ErrorCode::Rejected, "there is no feedback to submit".into()));
    }
    let Some(listener) = meta_text("feedback_sock") else {
        return Err(unavailable(
            "the agent session has no feedback listener (it may have exited); \
             the feedback is saved and can be pulled with `limpid-core feedback`",
        ));
    };
    let message = compose_for_page(&handoff, meta_text("title").unwrap_or(""), &replies)
        .map_err(|error| unavailable(format!("could not compose the feedback: {error}")))?;
    let signal = FeedbackSignal {
        page: html.to_path_buf(),
        message: wake_message(html, &message),
    };
    match send_feedback_checked(Path::new(listener), &signal, wait) {
        FeedbackOutcome::Accepted => Ok(revision),
        FeedbackOutcome::NotDelivered(message) => Err(unavailable(format!(
            "the agent session is not reachable: {message}"
        ))),
        FeedbackOutcome::Refused(message) => Err((
            ErrorCode::Rejected,
            format!("the agent refused the feedback: {message}"),
        )),
        FeedbackOutcome::Uncertain(message) => Err((
            ErrorCode::SubmitUncertain,
            format!("the feedback may have been submitted: {message}"),
        )),
    }
}
