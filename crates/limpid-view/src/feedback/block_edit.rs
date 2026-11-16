//! Editing a focused Markdown block, distinct from writing feedback about it.

use super::*;
use limpid_core::replies::FileRevision;
use std::sync::Arc;

// A list item's first marker sits before its source span. Present its content
// without the continuation indent and restore that indent when saving.
fn continuation_indent(source: &str, target: &Range<usize>) -> usize {
    let line = source[..target.start].rfind('\n').map_or(0, |at| at + 1);
    let prefix = &source[line..target.start];
    if prefix.chars().all(|ch| {
        ch.is_ascii_whitespace()
            || ch.is_ascii_digit()
            || matches!(ch, '-' | '+' | '*' | '.' | ')' | '[' | ']' | 'x' | 'X')
    }) {
        prefix
            .chars()
            .map(|ch| if ch == '\t' { 4 } else { 1 })
            .sum()
    } else {
        0
    }
}

fn buffer_text(source: &str, target: &Range<usize>) -> String {
    let indent = continuation_indent(source, target);
    let text = source[target.clone()].replace("\r\n", "\n");
    text.trim_end_matches('\n')
        .split('\n')
        .enumerate()
        .map(|(at, line)| {
            if at == 0 {
                line
            } else {
                line.strip_prefix(&" ".repeat(indent)).unwrap_or(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replacement(source: &str, target: &Range<usize>, text: &str) -> String {
    let original = &source[target.clone()];
    let ending = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let indent = " ".repeat(continuation_indent(source, target));
    let text = text.replace("\r\n", "\n");
    let body = text
        .trim_end_matches('\n')
        .split('\n')
        .enumerate()
        .map(|(at, line)| {
            if at == 0 || line.is_empty() {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join(ending);
    let suffix = &original[original.trim_end_matches(['\r', '\n']).len()..];
    format!("{body}{suffix}")
}

impl Feedback {
    pub(super) fn open_block(&mut self, stop: Option<&Range<usize>>) -> ActionResult {
        let Some(stop) = stop else {
            return ActionResult::Nothing("no block here".into());
        };
        match self.classify(stop) {
            StopRef::Comment(_) => return self.open_comment(Some(stop)),
            StopRef::Reply(_) => return self.open_reply(Some(stop)),
            StopRef::Box => return ActionResult::Nothing("editor open: :wq or :q!".into()),
            _ => {}
        }
        if stop.is_empty() || self.source.get(stop.clone()).is_none() {
            return ActionResult::Nothing("no source for this block".into());
        }
        let text = buffer_text(&self.source, stop);
        let target = BoxTarget::Block {
            target: stop.clone(),
            source: Arc::from(self.source.as_str()),
        };
        self.begin_box(target, &text, BoxKind::Block)
    }

    pub(super) fn save_block(
        &mut self,
        target: Range<usize>,
        source: &str,
        text: &str,
    ) -> Result<(), String> {
        let path = self.handoff.source.as_ref().unwrap_or(&self.handoff.path);
        let revision = FileRevision::open_checked(path, &replies::source_hash(source.as_bytes()))
            .map_err(|error| error.to_string())?;
        let text = replacement(source, &target, text);
        let updated = revision
            .replace_range(target.clone(), &text)
            .map_err(|error| error.to_string())?;
        self.pending
            .rebase_block(&target, text.len(), &updated, self.file_backed);
        if self.file_backed {
            self.handoff.source_hash = Some(replies::source_hash(updated.as_bytes()));
        }
        let new_target = target.start..target.start + text.len();
        if let Some(open) = self.open.as_mut() {
            open.plan.target = BoxTarget::Block {
                target: new_target.clone(),
                source: Arc::from(updated.as_str()),
            };
        }
        self.source = updated;
        self.reload = true;
        self.focus = Some(Focus::Real(new_target));
        self.flashes.push("Block saved".into());
        Ok(())
    }

    pub(super) fn reopen_block(&mut self, text: &str, error: &str) {
        let result =
            self.host
                .borrow_mut()
                .begin(self.owner, &self.editor_config(), text, BoxKind::Block);
        if let Err(begin) = result {
            self.flashes.push(format!(
                "block not saved: {error}; could not reopen editor: {begin:?}"
            ));
            return;
        }
        self.host
            .borrow_mut()
            .input(self.owner, "<C-\\><C-n>:setlocal modified<CR>");
        if let Some(open) = self.open.as_mut() {
            open.message = Some(format!("not saved: {error}"));
        }
        self.stale = true;
        self.focus = Some(Focus::Box);
        self.fit_box_to_content();
    }
}
