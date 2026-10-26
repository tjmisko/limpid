//! Yanking: the text of blocks, sections and the page as Markdown source, and
//! the `y` operator (`yy`, `yj`, `3yk`, `yis`, `yas`, `yG`).
//!
//! The text is a slice of the original source by `src` range, so nothing is
//! reconstructed from layout. A single code block yields its code without the
//! fences. The text leaves through the `reply.command` setting.

use crate::nav::keymap::Chord;
use limpid_core::doc::{BlockKind, Doc};
use std::{ops::Range, path::PathBuf};

/// How long the operator waits for its next key.
pub const OPERATOR_TIMEOUT_MS: u64 = 2000;

/// Where yanked text goes.
pub trait Clipboard {
    fn copy(&mut self, text: &str) -> Result<(), String>;
}

/// Pipes the text into `reply.command`, as a reply is delivered.
pub struct ReplyCommand {
    pub command: String,
    pub source: Option<PathBuf>,
}

impl Clipboard for ReplyCommand {
    fn copy(&mut self, text: &str) -> Result<(), String> {
        limpid_core::replies::deliver_reply(
            &self.command,
            text,
            "file",
            None,
            self.source.as_deref(),
            None,
        )
        .map_err(|error| error.to_string())
    }
}

/// A clipboard that is not there. Pages built without one fail loudly rather
/// than run a command nobody configured.
pub struct NoClipboard;

impl Clipboard for NoClipboard {
    fn copy(&mut self, _text: &str) -> Result<(), String> {
        Err("no clipboard command configured".into())
    }
}

/// The body of a fenced code block, without the fences. `None` when the text
/// does not start with a fence.
pub fn fence_body(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines();
    let opening = lines.next()?;
    let indent = opening.len() - opening.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = opening.trim_start_matches(' ');
    let fence_char = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let fence_len = rest.chars().take_while(|c| *c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let mut body = Vec::new();
    for line in lines {
        let trimmed = line.trim_start_matches(' ').trim_end();
        let closing = trimmed.len() >= fence_len
            && trimmed.chars().all(|c| c == fence_char)
            && line.len() - line.trim_start_matches(' ').len() <= 3;
        if closing {
            break;
        }
        body.push(line);
    }
    Some(body.join("\n"))
}

fn slice<'a>(source: &'a str, range: &Range<usize>) -> &'a str {
    source.get(range.clone()).unwrap_or("")
}

/// The text to yank for stops at `ranges` (source byte ranges). One stop gives
/// its own slice, code without fences; several give the span from the first
/// start to the last end.
pub fn blocks_text(source: &str, ranges: &[Range<usize>]) -> String {
    let (Some(start), Some(end)) = (
        ranges.iter().map(|range| range.start).min(),
        ranges.iter().map(|range| range.end).max(),
    ) else {
        return String::new();
    };
    let text = slice(source, &(start..end));
    if ranges.len() == 1
        && let Some(code) = fence_body(text)
    {
        return finish(&code);
    }
    finish(text)
}

fn finish(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{trimmed}\n")
}

/// The section holding top-level block `block`: from its heading to the next
/// heading of the same or a shallower level. `inner` leaves the heading out.
pub fn section_text(doc: &Doc, block: usize, inner: bool) -> String {
    let headings: Vec<(usize, u8)> = doc
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| match candidate.kind {
            BlockKind::Heading { level, .. } => Some((index, level)),
            _ => None,
        })
        .collect();
    let at = headings.partition_point(|&(index, _)| index <= block);
    // Before the first heading the whole preamble is the section.
    let Some(current) = at.checked_sub(1) else {
        let end = headings
            .first()
            .map_or(doc.source.len(), |&(index, _)| doc.blocks[index].src.start);
        return finish(slice(&doc.source, &(0..end)));
    };
    let (head_index, level) = headings[current];
    let head = &doc.blocks[head_index];
    let end = headings[current + 1..]
        .iter()
        .find(|&&(_, next)| next <= level)
        .map_or(doc.source.len(), |&(index, _)| doc.blocks[index].src.start);
    let start = if inner { head.src.end } else { head.src.start };
    finish(slice(&doc.source, &(start..end.max(start))))
}

pub fn page_text(doc: &Doc) -> String {
    finish(&doc.source)
}

/// What a finished `y` command asks to yank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `yy`: the focused stop.
    Block,
    /// `yj` / `yk`: the focused stop and `count` more in that direction.
    Motion(i64),
    /// `yis` / `yas`.
    Section { inner: bool },
    /// `yG`: from the focused stop to the end.
    ToEnd,
}

/// What the operator did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Pending,
    Cancel,
    Run(Target),
}

/// The state of a `y` command waiting for its motion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    /// A count typed before the `y`, as in `3yk`.
    prefix_count: Option<u32>,
    count: Option<u32>,
    object: Option<bool>,
    pub started_ms: u64,
}

impl Operator {
    pub fn start(now_ms: u64) -> Self {
        Self::start_with_count(now_ms, None)
    }

    /// Start with the count typed before the `y`; it multiplies any count
    /// typed after it, as in Vim.
    pub fn start_with_count(now_ms: u64, prefix_count: Option<u32>) -> Self {
        Self {
            prefix_count,
            count: None,
            object: None,
            started_ms: now_ms,
        }
    }

    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.started_ms) > OPERATOR_TIMEOUT_MS
    }

    /// Text for the status bar while the operator waits.
    pub fn pending_text(&self) -> String {
        let mut text = String::new();
        if let Some(prefix) = self.prefix_count {
            text.push_str(&prefix.to_string());
        }
        text.push('y');
        if let Some(count) = self.count {
            text.push_str(&count.to_string());
        }
        match self.object {
            Some(true) => text.push('i'),
            Some(false) => text.push('a'),
            None => {}
        }
        text
    }

    pub fn feed(&mut self, chord: &Chord) -> Step {
        if let Some(digit) = chord.digit()
            && self.object.is_none()
            && (digit != 0 || self.count.is_some())
        {
            let next = self.count.unwrap_or(0).saturating_mul(10) + u32::from(digit);
            self.count = Some(next.min(99_999));
            return Step::Pending;
        }
        let plain = !chord.ctrl && !chord.alt;
        if let Some(inner) = self.object {
            return match (plain && !chord.shift, chord.key.as_str()) {
                (true, "s") => Step::Run(Target::Section { inner }),
                _ => Step::Cancel,
            };
        }
        let count = i64::from(self.prefix_count.unwrap_or(1).max(1))
            .saturating_mul(i64::from(self.count.unwrap_or(1).max(1)))
            .min(99_999);
        match (plain, chord.shift, chord.key.as_str()) {
            (true, false, "y") if count > 1 => Step::Run(Target::Motion(count - 1)),
            (true, false, "y") => Step::Run(Target::Block),
            (true, false, "j" | "down") => Step::Run(Target::Motion(count)),
            (true, false, "k" | "up") => Step::Run(Target::Motion(-count)),
            (true, true, "g") => Step::Run(Target::ToEnd),
            (true, false, "i") => {
                self.object = Some(true);
                Step::Pending
            }
            (true, false, "a") => {
                self.object = Some(false);
                Step::Pending
            }
            _ => Step::Cancel,
        }
    }
}

/// A short description for the status bar: "2 blocks", "code".
pub fn describe(count: usize, code: bool) -> String {
    if code {
        return "code".into();
    }
    format!("{count} block{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
// The tests hand `blocks_text` a single range on purpose.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn doc(markdown: &str) -> Doc {
        parse(markdown, Mode::File, &Settings::default())
    }

    #[test]
    fn should_yank_the_source_slice_when_a_stop_is_a_paragraph() {
        let doc = doc("first *para*\n\nsecond\n");
        let text = blocks_text(&doc.source, &[doc.blocks[0].src.clone()]);
        assert_eq!(text, "first *para*\n");
    }

    #[test]
    fn should_yank_the_code_without_fences_when_the_stop_is_a_code_block() {
        let doc = doc("intro\n\n```rust\nfn main() {}\nlet x = 1;\n```\n");
        let text = blocks_text(&doc.source, &[doc.blocks[1].src.clone()]);
        assert_eq!(text, "fn main() {}\nlet x = 1;\n");
    }

    #[test]
    fn should_keep_the_fences_when_yanking_code_together_with_other_blocks() {
        let doc = doc("intro\n\n```\ncode\n```\n");
        let ranges = [doc.blocks[0].src.clone(), doc.blocks[1].src.clone()];
        let text = blocks_text(&doc.source, &ranges);
        assert_eq!(text, "intro\n\n```\ncode\n```\n");
    }

    #[test]
    fn should_span_first_start_to_last_end_when_yanking_a_range() {
        let doc = doc("one\n\ntwo\n\nthree\n\nfour\n");
        let ranges = [doc.blocks[2].src.clone(), doc.blocks[1].src.clone()];
        assert_eq!(blocks_text(&doc.source, &ranges), "two\n\nthree\n");
    }

    #[test]
    fn should_return_nothing_when_there_are_no_ranges_or_the_range_is_blank() {
        assert_eq!(blocks_text("abc", &[]), "");
        assert_eq!(blocks_text("abc   \n", &[3..7]), "");
        assert_eq!(
            blocks_text("abc", &[50..60]),
            "",
            "out of bounds is not a panic"
        );
    }

    #[test]
    fn should_not_panic_when_a_range_splits_a_multibyte_character() {
        let text = "caf\u{e9}";
        assert_eq!(blocks_text(text, &[0..4]), "");
        assert_eq!(blocks_text(text, &[0..5]), "caf\u{e9}\n");
    }

    #[test]
    fn should_read_fence_bodies_of_every_shape_when_extracting_code() {
        assert_eq!(fence_body("```\na\nb\n```").as_deref(), Some("a\nb"));
        assert_eq!(fence_body("~~~py\nx\n~~~").as_deref(), Some("x"));
        assert_eq!(
            fence_body("````\n```\ninner\n```\n````").as_deref(),
            Some("```\ninner\n```")
        );
        assert_eq!(
            fence_body("```\nunclosed\nstill").as_deref(),
            Some("unclosed\nstill")
        );
        assert_eq!(fence_body("```\n```").as_deref(), Some(""));
        assert_eq!(
            fence_body("   ```\n  indented\n   ```").as_deref(),
            Some("  indented")
        );
        assert_eq!(fence_body("plain text"), None);
        assert_eq!(fence_body("``\nnot a fence\n``"), None);
        assert_eq!(fence_body("    ```\ncode block\n"), None);
        assert_eq!(fence_body(""), None);
    }

    #[test]
    fn should_yank_the_heading_and_everything_below_it_when_yanking_a_section() {
        let doc = doc("# One\n\nalpha\n\n## Two\n\nbeta\n\n# Three\n\ngamma\n");
        assert_eq!(
            section_text(&doc, 1, false),
            "# One\n\nalpha\n\n## Two\n\nbeta\n"
        );
        assert_eq!(section_text(&doc, 3, false), "## Two\n\nbeta\n");
        assert_eq!(section_text(&doc, 5, false), "# Three\n\ngamma\n");
    }

    #[test]
    fn should_leave_out_the_heading_when_yanking_the_inner_section() {
        let doc = doc("# One\n\nalpha\n\n## Two\n\nbeta\n\n# Three\n\ngamma\n");
        assert_eq!(section_text(&doc, 1, true), "alpha\n\n## Two\n\nbeta\n");
        assert_eq!(section_text(&doc, 3, true), "beta\n");
    }

    #[test]
    fn should_use_the_preamble_when_the_block_comes_before_any_heading() {
        let doc = doc("intro\n\n# One\n\nalpha\n");
        assert_eq!(section_text(&doc, 0, false), "intro\n");
        let doc = doc_without_headings();
        assert_eq!(section_text(&doc, 1, false), "a\n\nb\n");
    }

    fn doc_without_headings() -> Doc {
        doc("a\n\nb\n")
    }

    #[test]
    fn should_run_to_the_end_when_the_last_section_has_no_successor() {
        let doc = doc("# One\n\nalpha\n\n- last\n");
        assert_eq!(section_text(&doc, 2, false), "# One\n\nalpha\n\n- last\n");
    }

    #[test]
    fn should_yank_the_whole_source_when_yanking_the_page() {
        let doc = doc("# T\n\nbody\n\n\n");
        assert_eq!(page_text(&doc), "# T\n\nbody\n");
        assert_eq!(page_text(&doc_empty()), "");
    }

    fn doc_empty() -> Doc {
        doc("")
    }

    fn feed(operator: &mut Operator, keys: &[&str]) -> Step {
        let mut step = Step::Pending;
        for key in keys {
            step = operator.feed(&Chord::parse(key).unwrap());
            if step != Step::Pending {
                break;
            }
        }
        step
    }

    #[test]
    fn should_run_the_block_target_when_the_operator_gets_y() {
        let mut operator = Operator::start(0);
        assert_eq!(feed(&mut operator, &["y"]), Step::Run(Target::Block));
    }

    #[test]
    fn should_yank_count_blocks_when_y_is_doubled_with_a_count() {
        assert_eq!(
            feed(&mut Operator::start_with_count(0, Some(3)), &["y"]),
            Step::Run(Target::Motion(2))
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["3", "y"]),
            Step::Run(Target::Motion(2))
        );
        assert_eq!(
            feed(&mut Operator::start_with_count(0, Some(1)), &["y"]),
            Step::Run(Target::Block)
        );
    }

    #[test]
    fn should_multiply_the_counts_when_one_comes_before_the_y_and_one_after() {
        assert_eq!(
            feed(&mut Operator::start_with_count(0, Some(2)), &["3", "j"]),
            Step::Run(Target::Motion(6))
        );
        assert_eq!(
            feed(&mut Operator::start_with_count(0, Some(3)), &["k"]),
            Step::Run(Target::Motion(-3))
        );
    }

    #[test]
    fn should_run_a_motion_with_the_count_when_the_operator_gets_j_or_k() {
        assert_eq!(
            feed(&mut Operator::start(0), &["j"]),
            Step::Run(Target::Motion(1))
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["3", "j"]),
            Step::Run(Target::Motion(3))
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["1", "2", "k"]),
            Step::Run(Target::Motion(-12))
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["down"]),
            Step::Run(Target::Motion(1))
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["up"]),
            Step::Run(Target::Motion(-1))
        );
    }

    #[test]
    fn should_run_a_section_target_when_the_operator_gets_is_or_as() {
        assert_eq!(
            feed(&mut Operator::start(0), &["i", "s"]),
            Step::Run(Target::Section { inner: true })
        );
        assert_eq!(
            feed(&mut Operator::start(0), &["a", "s"]),
            Step::Run(Target::Section { inner: false })
        );
    }

    #[test]
    fn should_run_to_the_end_when_the_operator_gets_capital_g() {
        assert_eq!(
            feed(&mut Operator::start(0), &["shift+g"]),
            Step::Run(Target::ToEnd)
        );
    }

    #[test]
    fn should_cancel_when_the_operator_gets_an_unknown_key() {
        assert_eq!(feed(&mut Operator::start(0), &["q"]), Step::Cancel);
        assert_eq!(feed(&mut Operator::start(0), &["escape"]), Step::Cancel);
        assert_eq!(feed(&mut Operator::start(0), &["i", "x"]), Step::Cancel);
        assert_eq!(
            feed(&mut Operator::start(0), &["i", "shift+s"]),
            Step::Cancel
        );
        assert_eq!(feed(&mut Operator::start(0), &["ctrl+y"]), Step::Cancel);
        assert_eq!(feed(&mut Operator::start(0), &["i", "2"]), Step::Cancel);
    }

    #[test]
    fn should_treat_a_leading_zero_as_a_cancel_not_a_count() {
        assert_eq!(feed(&mut Operator::start(0), &["0"]), Step::Cancel);
    }

    #[test]
    fn should_time_out_when_the_next_key_is_late() {
        let operator = Operator::start(1000);
        assert!(!operator.expired(1000 + OPERATOR_TIMEOUT_MS));
        assert!(operator.expired(1000 + OPERATOR_TIMEOUT_MS + 1));
        assert!(
            !operator.expired(0),
            "a clock that went backwards is not a timeout"
        );
    }

    #[test]
    fn should_show_what_was_typed_when_the_operator_is_pending() {
        let mut operator = Operator::start(0);
        assert_eq!(operator.pending_text(), "y");
        operator.feed(&Chord::parse("2").unwrap());
        assert_eq!(operator.pending_text(), "y2");
        assert_eq!(Operator::start_with_count(0, Some(3)).pending_text(), "3y");
        let mut operator = Operator::start(0);
        operator.feed(&Chord::parse("i").unwrap());
        assert_eq!(operator.pending_text(), "yi");
    }

    #[test]
    fn should_describe_what_was_yanked_when_flashing() {
        assert_eq!(describe(1, false), "1 block");
        assert_eq!(describe(3, false), "3 blocks");
        assert_eq!(describe(1, true), "code");
    }

    #[test]
    fn should_refuse_to_copy_when_no_clipboard_is_configured() {
        assert!(NoClipboard.copy("text").is_err());
    }

    #[test]
    fn should_pipe_the_text_into_the_command_when_copying() {
        let dir = std::env::temp_dir().join(format!("limpid-view-yank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("copied.txt");
        let mut clipboard = ReplyCommand {
            command: format!("cat > '{}'", out.display()),
            source: None,
        };
        clipboard.copy("hello\nworld\n").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "hello\nworld\n");
        let mut failing = ReplyCommand {
            command: "exit 3".into(),
            source: None,
        };
        assert!(failing.copy("x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
