//! What the reader has said so far: the pending replies and comments, how
//! they show on the page, and how they go back to `limpid-core` to be saved.
//!
//! The page shows feedback by blocks. [`overlay`] takes the blocks parsed from
//! the Markdown and returns them with the pending replies and comments
//! spliced in as annotation blocks (and the editor box, while one is open).
//! Those blocks have no place in the source, so each gets a zero-width source
//! range inside the block it follows; [`Synthetic`] says what each one is.

use crate::layout::{
    field::{CHOSEN_MARK, box_text},
    list::item_source,
};
use limpid_core::{
    doc::{AnnotationKind, Block, BlockKind, Inline, ListItem},
    replies::{AgentReplies, Comment},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Range,
};

/// Longest quote of a block that goes into a comment, in characters.
pub const QUOTE_CHARS: usize = 100;

/// Prefix of the key of a comment that is already written into the Markdown
/// file; the rest is the `start-end` byte range of its annotation.
pub const FILE_KEY_PREFIX: &str = "file:";

/// A question in the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub id: String,
    pub src: Range<usize>,
    /// The suggested answers as plain text.
    pub options: Vec<String>,
}

/// A reply or comment already saved in the Markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedNote {
    pub src: Range<usize>,
    pub target: Range<usize>,
    pub kind: AnnotationKind,
    /// The question a reply answers. A reply is written after the whole list
    /// its question is in, so this is not always the block it follows.
    pub field: Option<String>,
    pub text: String,
}

/// What the feedback layer needs to know about the parsed document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocIndex {
    pub fields: Vec<FieldInfo>,
    /// Source ranges of the top-level blocks.
    pub blocks: Vec<Range<usize>>,
    pub notes: Vec<SavedNote>,
}

/// The plain text of `inlines`, line breaks as spaces.
pub fn inlines_text(inlines: &[Inline]) -> String {
    fn walk(inlines: &[Inline], out: &mut String) {
        for inline in inlines {
            match inline {
                Inline::Text(text) | Inline::Code(text) => out.push_str(text),
                Inline::SoftBreak | Inline::HardBreak => out.push(' '),
                Inline::Emph(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                    walk(inner, out)
                }
                Inline::Link { inlines, .. } => walk(inlines, out),
                Inline::Image { alt, .. } => out.push_str(alt),
                Inline::Math { latex, .. } => out.push_str(latex),
                Inline::FootnoteRef(_) | Inline::Html(_) => {}
            }
        }
    }
    let mut out = String::new();
    walk(inlines, &mut out);
    out.trim().to_owned()
}

/// The question id in the first line of a saved reply comment.
fn reply_field_of(comment: &str) -> Option<String> {
    let rest = comment.trim().strip_prefix("<!-- limpid reply ")?;
    let (field, _) = rest.split_once('\n')?;
    limpid_core::replies::valid_question_id(field).then(|| field.to_owned())
}

impl DocIndex {
    pub fn from_blocks(blocks: &[Block], source: &str) -> Self {
        let mut index = DocIndex::default();
        for block in blocks {
            index.blocks.push(block.src.clone());
        }
        index.add(blocks, source);
        index
    }

    /// Index the fields and notes in `blocks`, and in the list items inside.
    fn add(&mut self, blocks: &[Block], source: &str) {
        for block in blocks {
            match &block.kind {
                BlockKind::List { items, .. } => {
                    for item in items {
                        self.add(&item.blocks, source);
                    }
                }
                BlockKind::Field { id, options, .. } => self.fields.push(FieldInfo {
                    id: id.clone(),
                    src: block.src.clone(),
                    options: options.iter().map(|option| inlines_text(option)).collect(),
                }),
                BlockKind::Annotation {
                    target, kind, text, ..
                } => self.notes.push(SavedNote {
                    src: block.src.clone(),
                    target: target.clone(),
                    kind: *kind,
                    field: source
                        .get(block.src.clone())
                        .and_then(reply_field_of)
                        .filter(|_| *kind == AnnotationKind::Reply),
                    text: text.clone(),
                }),
                _ => {}
            }
        }
    }

    pub fn field(&self, id: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| field.id == id)
    }

    pub fn field_at(&self, src: &Range<usize>) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| &field.src == src)
    }

    /// The reply already saved in the file for field `id`.
    pub fn saved_reply(&self, id: &str) -> Option<&SavedNote> {
        self.notes
            .iter()
            .find(|note| note.kind == AnnotationKind::Reply && note.field.as_deref() == Some(id))
    }

    /// The saved comment annotation whose block is `src`.
    pub fn saved_comment(&self, src: &Range<usize>) -> Option<&SavedNote> {
        self.notes
            .iter()
            .find(|note| note.kind == AnnotationKind::Comment && &note.src == src)
    }

    /// Index of the first top-level block that contains `range`, else the
    /// last block.
    pub fn block_containing(&self, range: &Range<usize>) -> Option<usize> {
        let last = self.blocks.len().checked_sub(1)?;
        let found = self
            .blocks
            .iter()
            .position(|block| block.start <= range.start && range.end <= block.end);
        Some(found.unwrap_or(last))
    }
}

/// The key of a comment annotation already in the file.
pub fn file_key(src: &Range<usize>) -> String {
    format!("{FILE_KEY_PREFIX}{}-{}", src.start, src.end)
}

/// `start-end` of a range, the form comment keys use.
pub fn range_text(range: &Range<usize>) -> String {
    format!("{}-{}", range.start, range.end)
}

fn parse_range_text(text: &str) -> Option<Range<usize>> {
    let (start, end) = text.split_once('-')?;
    Some(start.parse().ok()?..end.parse().ok()?)
}

/// The one-based line a byte offset falls on.
pub fn line_of(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    source.as_bytes()[..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// The start of `text` with whitespace collapsed, cut to `max_chars` on a word
/// boundary.
pub fn quote_of(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max_chars).collect();
    let next_is_space = collapsed.chars().nth(max_chars) == Some(' ');
    let trimmed = if next_is_space {
        cut.as_str()
    } else {
        cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head)
    };
    format!("{}…", trimmed.trim_end())
}

/// A comment the reader wrote, saved or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentEntry {
    /// The key `limpid-core` stores it under (see [`FILE_KEY_PREFIX`]).
    pub key: String,
    pub turn: String,
    pub text: String,
    pub quote: String,
    pub start_line: usize,
    pub end_line: usize,
    /// The block it is about.
    pub target: Range<usize>,
}

/// Replies and comments that differ from what is saved, plus (for a page that
/// keeps its feedback in a sidecar) everything saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub replies: BTreeMap<String, String>,
    pub comments: BTreeMap<String, CommentEntry>,
    pub deletions: BTreeSet<String>,
    unsaved: bool,
}

impl Pending {
    /// Everything a sidecar holds, as saved.
    pub fn from_saved(saved: &AgentReplies) -> Self {
        let mut pending = Pending {
            replies: saved.replies.clone(),
            ..Pending::default()
        };
        for (key, comment) in &saved.comments {
            let range_part = key
                .rsplit_once(':')
                .map_or(key.as_str(), |(_, range)| range);
            let Some(target) = parse_range_text(range_part) else {
                continue;
            };
            pending.comments.insert(
                key.clone(),
                CommentEntry {
                    key: key.clone(),
                    turn: key
                        .rsplit_once(':')
                        .map(|(turn, _)| turn.to_owned())
                        .unwrap_or_default(),
                    text: comment.text.clone(),
                    quote: comment.quote.clone(),
                    start_line: comment.start_line,
                    end_line: comment.end_line,
                    target,
                },
            );
        }
        pending
    }

    pub fn has_unsaved(&self) -> bool {
        self.unsaved
    }

    pub fn reply(&self, id: &str) -> Option<&str> {
        self.replies.get(id).map(String::as_str)
    }

    /// Set the reply to `id`. An empty reply with nothing saved to replace is
    /// dropped.
    pub fn set_reply(&mut self, id: &str, text: &str, has_saved: bool) {
        let text = text.trim_end();
        if text.is_empty() && !has_saved && !self.replies.contains_key(id) {
            return;
        }
        if self.replies.get(id).map(String::as_str) == Some(text) {
            return;
        }
        self.replies.insert(id.to_owned(), text.to_owned());
        self.unsaved = true;
    }

    /// Set a comment, or remove it when `entry.text` is empty.
    pub fn set_comment(&mut self, entry: CommentEntry) {
        if entry.text.trim().is_empty() {
            // A comment that never existed has nothing to remove.
            if self.comments.contains_key(&entry.key) || entry.key.starts_with(FILE_KEY_PREFIX) {
                self.remove_comment(&entry.key);
            }
            return;
        }
        let entry = CommentEntry {
            text: entry.text.trim_end().to_owned(),
            ..entry
        };
        if self.comments.get(&entry.key) == Some(&entry) {
            return;
        }
        self.deletions.remove(&entry.key);
        self.comments.insert(entry.key.clone(), entry);
        self.unsaved = true;
    }

    /// Delete a comment, remembering to delete it from storage on save.
    pub fn remove_comment(&mut self, key: &str) {
        let had = self.comments.remove(key).is_some();
        let new_deletion = self.deletions.insert(key.to_owned());
        self.unsaved |= had || new_deletion;
    }

    /// After a successful save. A file is re-read afterwards, so nothing
    /// pending survives; a sidecar keeps what it holds.
    pub fn saved(&mut self, file_backed: bool) {
        self.unsaved = false;
        self.deletions.clear();
        if file_backed {
            self.replies.clear();
            self.comments.clear();
        }
    }

    /// The pending buffer in the shape `limpid_core::replies::save_pending`
    /// takes.
    pub fn to_value(&self) -> Value {
        let comments: Vec<Value> = self
            .comments
            .values()
            .map(|entry| {
                if entry.key.starts_with(FILE_KEY_PREFIX) {
                    return json!({"key": entry.key, "text": entry.text});
                }
                json!({
                    "src": range_text(&entry.target),
                    "turn": entry.turn,
                    "text": entry.text,
                    "quote": entry.quote,
                    "lineStart": entry.start_line,
                    "lineEnd": entry.end_line,
                })
            })
            .collect();
        json!({
            "replies": self.replies,
            "comments": comments,
            "deletions": self.deletions,
        })
    }

    /// `saved` with the pending changes on top: what a send would carry.
    pub fn merged_into(&self, mut saved: AgentReplies) -> AgentReplies {
        for (id, text) in &self.replies {
            saved.replies.insert(id.clone(), text.clone());
        }
        for key in &self.deletions {
            saved.comments.remove(key);
        }
        for entry in self.comments.values() {
            let key = entry.key.clone();
            if key.starts_with(FILE_KEY_PREFIX) {
                // The annotation it replaces is listed under a file key too.
                saved.comments.remove(&key);
            }
            saved.comments.insert(
                key,
                Comment {
                    text: entry.text.clone(),
                    quote: entry.quote.clone(),
                    start_line: entry.start_line,
                    end_line: entry.end_line,
                },
            );
        }
        saved
    }
}

/// What the editor box is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxTarget {
    Reply {
        field: String,
    },
    /// `key` is the comment's storage key; `target` the block it is about.
    Comment {
        key: String,
        target: Range<usize>,
    },
}

/// The editor box as it should appear in the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxPlan {
    pub target: BoxTarget,
    pub rows: u16,
}

/// What a block added by [`overlay`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Synthetic {
    Reply(String),
    Comment(String),
    Box,
}

/// The blocks to show, and what each added block is, keyed by its source
/// range.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlay {
    pub blocks: Vec<Block>,
    pub synthetic: HashMap<Range<usize>, Synthetic>,
}

/// A zero-width range inside `anchor` that no other added block uses, so that
/// a yank spanning the new block still slices the source.
fn free_position(source: &str, anchor: &Range<usize>, taken: &HashSet<usize>) -> Range<usize> {
    let end = anchor.end.min(source.len());
    let start = anchor.start.min(end);
    let mut at = end;
    loop {
        if source.is_char_boundary(at) && !taken.contains(&at) {
            return at..at;
        }
        if at <= start {
            return end..end;
        }
        at -= 1;
    }
}

struct Splicer<'a> {
    source: &'a str,
    index: &'a DocIndex,
    pending: &'a Pending,
    editing_reply: Option<&'a str>,
    editing_comment: Option<&'a str>,
    /// Where the box for a comment not yet saved goes.
    new_box_target: Option<&'a Range<usize>>,
    rows: u16,
    /// Source spans of the list items in the document.
    items: HashSet<Range<usize>>,
    /// Pending comments about a top-level block, by its index.
    after_block: BTreeMap<usize, Vec<&'a CommentEntry>>,
    /// Pending comments about a list item, by its span.
    in_item: HashMap<Range<usize>, Vec<&'a CommentEntry>>,
    synthetic: HashMap<Range<usize>, Synthetic>,
    taken: HashSet<usize>,
    box_placed: bool,
}

/// The spans of every list item in `blocks`, nested lists included.
fn collect_items(blocks: &[Block], out: &mut HashSet<Range<usize>>) {
    for block in blocks {
        let BlockKind::List { items, .. } = &block.kind else {
            continue;
        };
        for item in items {
            out.insert(item_source(&item.blocks, &block.src));
            collect_items(&item.blocks, out);
        }
    }
}

impl Splicer<'_> {
    fn synthetic_block(
        &mut self,
        anchor: &Range<usize>,
        kind: AnnotationKind,
        text: String,
        what: Synthetic,
    ) -> Block {
        let src = free_position(self.source, anchor, &self.taken);
        self.taken.insert(src.start);
        self.synthetic.insert(src.clone(), what);
        Block {
            kind: BlockKind::Annotation {
                target: anchor.clone(),
                kind,
                text,
                field: None,
            },
            src,
            id: None,
        }
    }

    fn comment_block(&mut self, anchor: &Range<usize>, entry: &CommentEntry) -> Block {
        self.synthetic_block(
            anchor,
            AnnotationKind::Comment,
            entry.text.clone(),
            Synthetic::Comment(entry.key.clone()),
        )
    }

    fn box_block(&mut self, anchor: &Range<usize>) -> Block {
        self.box_placed = true;
        self.synthetic_block(
            anchor,
            AnnotationKind::Comment,
            box_text(self.rows),
            Synthetic::Box,
        )
    }

    /// The comments about the block or item at `anchor`, as blocks, and the
    /// box when one is being written for it.
    fn comments_about(
        &mut self,
        anchor: &Range<usize>,
        entries: &[&CommentEntry],
        out: &mut Vec<Block>,
    ) {
        for entry in entries {
            if self.editing_comment == Some(entry.key.as_str()) {
                let block = self.box_block(anchor);
                out.push(block);
                continue;
            }
            let block = self.comment_block(anchor, entry);
            out.push(block);
        }
    }

    /// `block`, with the answer a reply gives marked on a question and the
    /// comments about list items spliced into the items.
    fn real(&mut self, block: &Block) -> Block {
        match &block.kind {
            BlockKind::Field {
                id,
                prompt,
                options,
            } => self.field(block, id, prompt, options),
            BlockKind::List {
                ordered,
                start,
                items,
            } => {
                let items = items.iter().map(|item| self.item(block, item)).collect();
                Block {
                    kind: BlockKind::List {
                        ordered: *ordered,
                        start: *start,
                        items,
                    },
                    src: block.src.clone(),
                    id: block.id.clone(),
                }
            }
            _ => block.clone(),
        }
    }

    fn item(&mut self, list: &Block, item: &ListItem) -> ListItem {
        let span = item_source(&item.blocks, &list.src);
        let mut blocks = self.splice(&item.blocks, false);
        let entries = self.in_item.get(&span).cloned().unwrap_or_default();
        self.comments_about(&span, &entries, &mut blocks);
        if !self.box_placed && self.new_box_target == Some(&span) {
            let block = self.box_block(&span);
            blocks.push(block);
        }
        ListItem {
            checked: item.checked,
            blocks,
        }
    }

    fn field(&self, block: &Block, id: &str, prompt: &[Inline], options: &[Vec<Inline>]) -> Block {
        let reply = match self.pending.reply(id) {
            Some(text) => text.trim(),
            None => self
                .index
                .saved_reply(id)
                .map_or("", |note| note.text.trim()),
        };
        let chosen = self
            .index
            .field(id)
            .filter(|_| !reply.is_empty())
            .and_then(|field| field.options.iter().position(|option| option == reply));
        let Some(chosen) = chosen.filter(|&at| at < options.len()) else {
            return block.clone();
        };
        let mut options = options.to_vec();
        options[chosen].insert(0, Inline::Html(CHOSEN_MARK.to_owned()));
        Block {
            kind: BlockKind::Field {
                id: id.to_owned(),
                prompt: prompt.to_vec(),
                options,
            },
            src: block.src.clone(),
            id: block.id.clone(),
        }
    }

    /// One level of blocks. `top` is the document's own list, whose blocks
    /// are the ones comments and replies follow.
    fn splice(&mut self, base: &[Block], top: bool) -> Vec<Block> {
        let mut out = Vec::with_capacity(base.len());
        for (at, block) in base.iter().enumerate() {
            if let BlockKind::Annotation { kind, .. } = &block.kind {
                match kind {
                    AnnotationKind::Reply => {
                        let answered = self
                            .index
                            .notes
                            .iter()
                            .find(|note| note.src == block.src)
                            .and_then(|note| note.field.as_deref());
                        let hidden = answered.is_some_and(|id| {
                            self.pending.reply(id).is_some() || self.editing_reply == Some(id)
                        });
                        if hidden {
                            continue;
                        }
                    }
                    AnnotationKind::Comment => {
                        let key = file_key(&block.src);
                        if self.pending.deletions.contains(&key) {
                            continue;
                        }
                        if self.editing_comment == Some(key.as_str()) {
                            let boxed = self.box_block(&block.src);
                            out.push(boxed);
                            continue;
                        }
                        if let Some(entry) = self.pending.comments.get(&key) {
                            let comment = self.comment_block(&block.src, entry);
                            out.push(comment);
                            continue;
                        }
                    }
                }
            }
            let real = self.real(block);
            out.push(real);
            if !top {
                continue;
            }
            if let BlockKind::Field { id, .. } = &block.kind {
                if self.editing_reply == Some(id.as_str()) {
                    let boxed = self.box_block(&block.src);
                    out.push(boxed);
                } else if let Some(text) = self
                    .pending
                    .reply(id)
                    .filter(|text| !text.trim().is_empty())
                {
                    let reply = self.synthetic_block(
                        &block.src,
                        AnnotationKind::Reply,
                        text.to_owned(),
                        Synthetic::Reply(id.clone()),
                    );
                    out.push(reply);
                }
            }
            let entries = self.after_block.remove(&at).unwrap_or_default();
            self.comments_about(&block.src, &entries, &mut out);
            if !self.box_placed
                && let Some(target) = self.new_box_target
                && !self.items.contains(target)
                && self.index.block_containing(target) == Some(at)
            {
                let boxed = self.box_block(&block.src);
                out.push(boxed);
            }
        }
        out
    }
}

/// Splice the pending feedback and the editor box into `base`.
pub fn overlay(
    base: &[Block],
    index: &DocIndex,
    pending: &Pending,
    editing: Option<&BoxPlan>,
    source: &str,
) -> Overlay {
    let editing_reply = editing.and_then(|plan| match &plan.target {
        BoxTarget::Reply { field } => Some(field.as_str()),
        BoxTarget::Comment { .. } => None,
    });
    let editing_comment = editing.and_then(|plan| match &plan.target {
        BoxTarget::Comment { key, .. } => Some(key.as_str()),
        BoxTarget::Reply { .. } => None,
    });
    let new_box_target = editing.and_then(|plan| match &plan.target {
        BoxTarget::Comment { key, target }
            if !pending.comments.contains_key(key) && !key.starts_with(FILE_KEY_PREFIX) =>
        {
            Some(target)
        }
        _ => None,
    });
    let mut items = HashSet::new();
    collect_items(base, &mut items);

    // Comments that are not replacing an annotation in the file go after the
    // block they are about, or at the end of the list item.
    let mut after_block: BTreeMap<usize, Vec<&CommentEntry>> = BTreeMap::new();
    let mut in_item: HashMap<Range<usize>, Vec<&CommentEntry>> = HashMap::new();
    for entry in pending.comments.values() {
        if entry.key.starts_with(FILE_KEY_PREFIX) {
            continue;
        }
        if items.contains(&entry.target) {
            in_item.entry(entry.target.clone()).or_default().push(entry);
        } else if let Some(at) = index.block_containing(&entry.target) {
            after_block.entry(at).or_default().push(entry);
        }
    }

    let mut splicer = Splicer {
        source,
        index,
        pending,
        editing_reply,
        editing_comment,
        new_box_target,
        rows: editing.map_or(1, |plan| plan.rows),
        items,
        after_block,
        in_item,
        synthetic: HashMap::new(),
        taken: HashSet::new(),
        box_placed: false,
    };
    let blocks = splicer.splice(base, true);
    Overlay {
        blocks,
        synthetic: splicer.synthetic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    const QUESTIONS: &str = "# T\n\nIntro paragraph.\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - **Postgres**\n2. Why?\n";

    const ONE_QUESTION: &str =
        "# T\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - Postgres\n";

    fn doc(source: &str) -> limpid_core::doc::Doc {
        parse(source, Mode::File, &Settings::default())
    }

    fn entry(key: &str, target: Range<usize>, text: &str) -> CommentEntry {
        CommentEntry {
            key: key.into(),
            turn: String::new(),
            text: text.into(),
            quote: "q".into(),
            start_line: 1,
            end_line: 2,
            target,
        }
    }

    #[test]
    fn should_index_fields_with_plain_option_text_when_building_from_blocks() {
        let doc = doc(QUESTIONS);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        assert_eq!(index.fields.len(), 2);
        assert_eq!(index.fields[0].id, "q1");
        assert_eq!(index.fields[0].options, ["SQLite", "Postgres"]);
        assert!(index.fields[1].options.is_empty());
        assert_eq!(index.field("q2").unwrap().id, "q2");
        assert!(index.field("q9").is_none());
        assert!(index.field_at(&index.fields[1].src).is_some());
    }

    #[test]
    fn should_find_a_saved_reply_by_field_when_the_file_has_one() {
        let source = format!("{ONE_QUESTION}\n<!-- limpid reply q1\nSQLite\n-->\n");
        let doc = doc(&source);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let note = index.saved_reply("q1").expect("saved reply");
        assert_eq!(note.text.trim(), "SQLite");
        assert!(index.saved_reply("q2").is_none());
    }

    #[test]
    fn should_cut_quotes_on_a_word_boundary_when_the_block_is_long() {
        let text = "alpha beta gamma delta";
        assert_eq!(quote_of(text, 100), text);
        assert_eq!(quote_of("alpha   beta\ngamma", 100), "alpha beta gamma");
        assert_eq!(quote_of(text, 12), "alpha beta…");
        assert_eq!(quote_of(text, 10), "alpha beta…");
        assert_eq!(quote_of("unbroken", 3), "unb…");
        assert_eq!(quote_of("", 5), "");
    }

    #[test]
    fn should_count_lines_from_one_when_converting_offsets() {
        let source = "a\nb\nc";
        assert_eq!(line_of(source, 0), 1);
        assert_eq!(line_of(source, 2), 2);
        assert_eq!(line_of(source, 4), 3);
        assert_eq!(line_of(source, 999), 3);
    }

    #[test]
    fn should_ignore_an_empty_reply_when_nothing_was_saved() {
        let mut pending = Pending::default();
        pending.set_reply("q1", "  \n", false);
        assert!(!pending.has_unsaved());
        assert!(pending.replies.is_empty());
        pending.set_reply("q1", "", true);
        assert!(pending.has_unsaved(), "clearing a saved reply is a change");
    }

    #[test]
    fn should_not_mark_unsaved_when_the_reply_is_unchanged() {
        let mut pending = Pending::default();
        pending.set_reply("q1", "yes", false);
        pending.saved(false);
        pending.set_reply("q1", "yes\n", false);
        assert!(!pending.has_unsaved());
        pending.set_reply("q1", "no", false);
        assert!(pending.has_unsaved());
    }

    #[test]
    fn should_remove_a_comment_and_remember_the_deletion_when_text_is_emptied() {
        let mut pending = Pending::default();
        pending.set_comment(entry("0-5", 0..5, "leaks"));
        assert!(pending.has_unsaved());
        pending.saved(false);
        pending.set_comment(entry("0-5", 0..5, "  "));
        assert!(pending.comments.is_empty());
        assert!(pending.deletions.contains("0-5"));
        assert!(pending.has_unsaved());
        pending.set_comment(entry("0-5", 0..5, "back"));
        assert!(!pending.deletions.contains("0-5"));
    }

    #[test]
    fn should_ignore_an_empty_new_comment_when_it_never_existed() {
        let mut pending = Pending::default();
        pending.set_comment(entry("0-5", 0..5, "   "));
        assert!(!pending.has_unsaved());
        assert!(pending.deletions.is_empty());
        pending.set_comment(entry("file:5-9", 5..9, ""));
        assert!(
            pending.deletions.contains("file:5-9"),
            "a file comment is deleted by key"
        );
    }

    #[test]
    fn should_forget_pending_changes_when_a_file_backed_save_completes() {
        let mut pending = Pending::default();
        pending.set_reply("q1", "yes", false);
        pending.set_comment(entry("0-5", 0..5, "x"));
        pending.saved(true);
        assert_eq!(pending, Pending::default());
    }

    #[test]
    fn should_keep_sidecar_contents_when_a_sidecar_save_completes() {
        let mut pending = Pending::default();
        pending.set_reply("q1", "yes", false);
        pending.saved(false);
        assert_eq!(pending.reply("q1"), Some("yes"));
        assert!(!pending.has_unsaved());
    }

    #[test]
    fn should_round_trip_a_sidecar_when_loading_saved_comments() {
        let mut saved = AgentReplies::default();
        saved.replies.insert("q1".into(), "A".into());
        saved.comments.insert(
            "turn-0:10-20".into(),
            Comment {
                text: "fix".into(),
                quote: "qq".into(),
                start_line: 3,
                end_line: 4,
            },
        );
        saved.comments.insert(
            "not-a-range".into(),
            Comment {
                text: "junk".into(),
                quote: String::new(),
                start_line: 0,
                end_line: 0,
            },
        );
        let pending = Pending::from_saved(&saved);
        assert!(!pending.has_unsaved());
        let loaded = &pending.comments["turn-0:10-20"];
        assert_eq!(loaded.turn, "turn-0");
        assert_eq!(loaded.target, 10..20);
        assert_eq!(pending.comments.len(), 1, "an unparseable key is skipped");
        let value = pending.to_value();
        assert_eq!(value["comments"][0]["src"], "10-20");
        assert_eq!(value["comments"][0]["turn"], "turn-0");
        assert_eq!(value["replies"]["q1"], "A");
    }

    #[test]
    fn should_replace_a_file_comment_when_merging_into_saved_replies() {
        let mut saved = AgentReplies::default();
        saved.comments.insert(
            "file:5-9".into(),
            Comment {
                text: "old".into(),
                quote: String::new(),
                start_line: 0,
                end_line: 0,
            },
        );
        let mut pending = Pending::default();
        pending.set_comment(entry("file:5-9", 5..9, "new"));
        pending.set_reply("q2", "b", false);
        let merged = pending.merged_into(saved);
        assert_eq!(merged.comments["file:5-9"].text, "new");
        assert_eq!(merged.replies["q2"], "b");
    }

    #[test]
    fn should_splice_a_pending_reply_after_its_field_when_overlaying() {
        let doc = doc(QUESTIONS);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_reply("q1", "SQLite", false);
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.blocks.len(), doc.blocks.len() + 1);
        let at = overlay
            .blocks
            .iter()
            .position(|block| matches!(block.kind, BlockKind::Field { ref id, .. } if id == "q1"))
            .unwrap();
        let added = &overlay.blocks[at + 1];
        let BlockKind::Annotation { kind, text, .. } = &added.kind else {
            panic!("expected an annotation, got {:?}", added.kind);
        };
        assert_eq!(*kind, AnnotationKind::Reply);
        assert_eq!(text, "SQLite");
        assert_eq!(overlay.synthetic[&added.src], Synthetic::Reply("q1".into()));
        assert!(
            doc.source.get(added.src.clone()).is_some(),
            "yank must slice"
        );
    }

    #[test]
    fn should_hide_the_saved_reply_when_a_pending_reply_replaces_it() {
        let source = format!("{ONE_QUESTION}\n<!-- limpid reply q1\nold\n-->\n");
        let doc = doc(&source);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_reply("q1", "new", true);
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        let texts: Vec<String> = overlay
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Annotation { text, .. } => Some(text.trim().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["new"]);
    }

    #[test]
    fn should_hide_a_deleted_file_comment_when_overlaying() {
        let source = "para\n\n<!-- limpid comment\nleaks\n-->\n\nafter\n";
        let doc = doc(source);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let note = index.notes[0].clone();
        let mut pending = Pending::default();
        pending.remove_comment(&file_key(&note.src));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.blocks.len(), doc.blocks.len() - 1);
        assert!(overlay.synthetic.is_empty());
    }

    #[test]
    fn should_place_a_new_comment_after_the_block_it_is_about_when_overlaying() {
        let doc = doc("first\n\nsecond\n\nthird\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let target = doc.blocks[1].src.clone();
        let key = range_text(&target);
        let mut pending = Pending::default();
        pending.set_comment(entry(&key, target, "check this"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.blocks.len(), 4);
        assert!(matches!(
            overlay.blocks[2].kind,
            BlockKind::Annotation {
                kind: AnnotationKind::Comment,
                ..
            }
        ));
        assert_eq!(
            overlay.synthetic[&overlay.blocks[2].src],
            Synthetic::Comment(key)
        );
    }

    #[test]
    fn should_give_every_added_block_its_own_range_when_two_follow_one_field() {
        let doc = doc(QUESTIONS);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let field = index.fields[0].src.clone();
        let mut pending = Pending::default();
        pending.set_reply("q1", "SQLite", false);
        pending.set_comment(entry(&range_text(&field), field, "why?"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.synthetic.len(), 2);
        let mut starts: Vec<usize> = overlay.synthetic.keys().map(|r| r.start).collect();
        starts.dedup();
        assert_eq!(
            starts.len(),
            2,
            "ranges must differ so the cursor can tell them apart"
        );
        for range in overlay.synthetic.keys() {
            assert_eq!(range.start, range.end);
            assert!(doc.source.is_char_boundary(range.start));
        }
    }

    #[test]
    fn should_land_on_char_boundaries_when_the_block_ends_in_multibyte_text() {
        let doc = doc("café ☕\n\nnext\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let target = doc.blocks[0].src.clone();
        let mut pending = Pending::default();
        pending.set_comment(entry("a", target.clone(), "one"));
        pending.set_comment(entry("b", target.clone(), "two"));
        pending.set_comment(entry("c", target, "three"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.synthetic.len(), 3);
        for range in overlay.synthetic.keys() {
            assert!(doc.source.is_char_boundary(range.start), "{range:?}");
        }
    }

    #[test]
    fn should_put_the_box_after_the_field_when_editing_a_reply() {
        let doc = doc(QUESTIONS);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_reply("q1", "old text", false);
        let plan = BoxPlan {
            target: BoxTarget::Reply { field: "q1".into() },
            rows: 4,
        };
        let overlay = overlay(&doc.blocks, &index, &pending, Some(&plan), &doc.source);
        let boxes: Vec<_> = overlay
            .synthetic
            .values()
            .filter(|what| **what == Synthetic::Box)
            .collect();
        assert_eq!(boxes.len(), 1);
        assert!(
            !overlay
                .synthetic
                .values()
                .any(|what| matches!(what, Synthetic::Reply(_))),
            "the reply is inside the box while it is edited"
        );
    }

    #[test]
    fn should_put_the_box_after_the_block_when_starting_a_new_comment() {
        let doc = doc("first\n\nsecond\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let target = doc.blocks[0].src.clone();
        let plan = BoxPlan {
            target: BoxTarget::Comment {
                key: range_text(&target),
                target,
            },
            rows: 3,
        };
        let overlay = overlay(
            &doc.blocks,
            &index,
            &Pending::default(),
            Some(&plan),
            &doc.source,
        );
        assert_eq!(overlay.blocks.len(), 3);
        assert_eq!(overlay.synthetic[&overlay.blocks[1].src], Synthetic::Box);
    }

    #[test]
    fn should_replace_the_saved_comment_with_the_box_when_editing_a_file_comment() {
        let source = "para\n\n<!-- limpid comment\nleaks\n-->\n\nafter\n";
        let doc = doc(source);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let key = file_key(&index.notes[0].src);
        let plan = BoxPlan {
            target: BoxTarget::Comment {
                key,
                target: index.notes[0].target.clone(),
            },
            rows: 3,
        };
        let overlay = overlay(
            &doc.blocks,
            &index,
            &Pending::default(),
            Some(&plan),
            &doc.source,
        );
        assert_eq!(overlay.blocks.len(), doc.blocks.len());
        assert_eq!(overlay.synthetic.len(), 1);
        assert!(
            overlay
                .synthetic
                .values()
                .all(|what| *what == Synthetic::Box)
        );
    }

    #[test]
    fn should_return_the_blocks_unchanged_when_there_is_no_feedback() {
        let doc = doc(QUESTIONS);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let overlay = overlay(&doc.blocks, &index, &Pending::default(), None, &doc.source);
        assert_eq!(overlay.blocks, doc.blocks);
        assert!(overlay.synthetic.is_empty());
    }

    #[test]
    fn should_not_panic_when_overlaying_an_empty_document() {
        let doc = doc("");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_comment(entry("0-1", 0..1, "x"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert!(overlay.blocks.is_empty());
    }
    fn item_blocks(block: &Block, at: usize) -> &[Block] {
        let BlockKind::List { items, .. } = &block.kind else {
            panic!("not a list");
        };
        &items[at].blocks
    }

    fn item_span(doc: &limpid_core::doc::Doc, at: usize) -> Range<usize> {
        let BlockKind::List { items, .. } = &doc.blocks[0].kind else {
            panic!("not a list");
        };
        item_source(&items[at].blocks, &doc.blocks[0].src)
    }

    #[test]
    fn should_put_a_comment_on_a_list_item_inside_the_item_when_overlaying() {
        let doc = doc("- one\n- two\n- three\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let target = item_span(&doc, 1);
        let key = range_text(&target);
        let mut pending = Pending::default();
        pending.set_comment(entry(&key, target, "about two"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        assert_eq!(overlay.blocks.len(), 1, "no block follows the list");
        assert_eq!(item_blocks(&overlay.blocks[0], 0).len(), 1);
        let second = item_blocks(&overlay.blocks[0], 1);
        assert_eq!(second.len(), 2);
        assert!(matches!(
            second[1].kind,
            BlockKind::Annotation {
                kind: AnnotationKind::Comment,
                ..
            }
        ));
        assert_eq!(overlay.synthetic[&second[1].src], Synthetic::Comment(key));
        assert_eq!(item_blocks(&overlay.blocks[0], 2).len(), 1);
    }

    #[test]
    fn should_put_the_box_inside_the_item_when_starting_a_comment_on_a_list_item() {
        let doc = doc("- one\n- two\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let target = item_span(&doc, 0);
        let plan = BoxPlan {
            target: BoxTarget::Comment {
                key: range_text(&target),
                target,
            },
            rows: 3,
        };
        let overlay = overlay(
            &doc.blocks,
            &index,
            &Pending::default(),
            Some(&plan),
            &doc.source,
        );
        assert_eq!(overlay.blocks.len(), 1);
        let first = item_blocks(&overlay.blocks[0], 0);
        assert_eq!(overlay.synthetic[&first[1].src], Synthetic::Box);
        assert_eq!(item_blocks(&overlay.blocks[0], 1).len(), 1);
    }

    #[test]
    fn should_find_a_saved_comment_inside_a_list_item_when_indexing() {
        let doc = doc("- one\n\n  <!-- limpid comment\n  note\n  -->\n- two\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        assert_eq!(index.notes.len(), 1);
        let note = &index.notes[0];
        assert_eq!(note.kind, AnnotationKind::Comment);
        assert!(index.saved_comment(&note.src).is_some());
    }

    #[test]
    fn should_replace_a_saved_comment_inside_a_list_item_when_it_is_edited() {
        let doc = doc("- one\n\n  <!-- limpid comment\n  note\n  -->\n- two\n");
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let saved = index.notes[0].src.clone();
        let mut pending = Pending::default();
        pending.set_comment(entry(&file_key(&saved), saved, "changed"));
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        let first = item_blocks(&overlay.blocks[0], 0);
        let BlockKind::Annotation { text, .. } = &first[1].kind else {
            panic!("no comment");
        };
        assert_eq!(text, "changed");
    }

    fn chosen(block: &Block, at: usize) -> bool {
        let BlockKind::Field { options, .. } = &block.kind else {
            panic!("not a field");
        };
        matches!(options[at].first(), Some(Inline::Html(mark)) if mark == CHOSEN_MARK)
    }

    #[test]
    fn should_mark_the_chosen_option_when_a_pending_reply_matches_it() {
        let doc = doc(ONE_QUESTION);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_reply("q1", "Postgres", false);
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        let field = overlay
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Field { .. }))
            .unwrap();
        assert!(!chosen(field, 0));
        assert!(chosen(field, 1));
    }

    #[test]
    fn should_mark_the_chosen_option_when_the_file_already_holds_the_reply() {
        let source = format!("{ONE_QUESTION}\n<!-- limpid reply q1\nSQLite\n-->\n");
        let doc = doc(&source);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let overlay = overlay(&doc.blocks, &index, &Pending::default(), None, &doc.source);
        let field = overlay
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Field { .. }))
            .unwrap();
        assert!(chosen(field, 0));
    }

    #[test]
    fn should_mark_nothing_when_the_reply_is_free_text() {
        let doc = doc(ONE_QUESTION);
        let index = DocIndex::from_blocks(&doc.blocks, &doc.source);
        let mut pending = Pending::default();
        pending.set_reply("q1", "neither of those", false);
        let overlay = overlay(&doc.blocks, &index, &pending, None, &doc.source);
        let field = overlay
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Field { .. }))
            .unwrap();
        assert!(!chosen(field, 0) && !chosen(field, 1));
    }
}
