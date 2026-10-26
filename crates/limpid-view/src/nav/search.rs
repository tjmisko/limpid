//! `/` search: smartcase matching over the document text, match navigation
//! across the whole (lazily laid out) document, and highlight painting.
//!
//! Matches are found per line of text, never across a hard line break. A match
//! is addressed by the top-level block that holds it and its ordinal inside
//! that block, which lets the search run over the `Doc` before anything is
//! laid out and be located in the layout afterwards.

use crate::{
    layout::{Content, LaidBlock, Layout, inline::RichText},
    nav::cursor::Env,
    paint::{Canvas, Rect},
    theme::{Color, Theme},
};
use limpid_core::doc::{Block, BlockKind, Doc, Inline};
use std::ops::Range;

/// A search pattern. Smartcase: any upper-case letter makes it case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
}

impl Query {
    /// `None` for an empty pattern.
    pub fn smartcase(text: &str) -> Option<Query> {
        if text.is_empty() {
            return None;
        }
        Some(Query {
            text: text.to_owned(),
            case_sensitive: text.chars().any(char::is_uppercase),
        })
    }

    /// Non-overlapping byte ranges of the matches in one line.
    pub fn find_in_line(&self, line: &str) -> Vec<Range<usize>> {
        let needle: Vec<char> = self.text.chars().map(|c| self.fold(c)).collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let haystack: Vec<(usize, char)> = line.char_indices().collect();
        let mut hits = Vec::new();
        let mut at = 0;
        while at + needle.len() <= haystack.len() {
            let same = haystack[at..at + needle.len()]
                .iter()
                .zip(&needle)
                .all(|((_, found), wanted)| self.fold(*found) == *wanted);
            if !same {
                at += 1;
                continue;
            }
            let start = haystack[at].0;
            let end = haystack
                .get(at + needle.len())
                .map_or(line.len(), |(offset, _)| *offset);
            hits.push(start..end);
            at += needle.len();
        }
        hits
    }

    fn fold(&self, c: char) -> char {
        if self.case_sensitive {
            return c;
        }
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(single), None) => single,
            _ => c,
        }
    }

    /// Number of matches in `text`, counting each line on its own.
    pub fn count_in(&self, text: &str) -> usize {
        text.split('\n')
            .map(|line| self.find_in_line(line).len())
            .sum()
    }
}

/// The searchable text of the leaves of `block`, in document order. Mirrors
/// what the layout shows: markers and callout titles are not searched.
pub fn leaf_texts(doc: &Doc, block: &Block) -> Vec<String> {
    let mut out = Vec::new();
    collect_leaf_texts(doc, block, &mut out);
    out
}

fn collect_leaf_texts(doc: &Doc, block: &Block, out: &mut Vec<String>) {
    let slice = || {
        doc.source
            .get(block.src.clone())
            .unwrap_or("")
            .trim_end()
            .to_owned()
    };
    match &block.kind {
        BlockKind::Heading { inlines, .. } | BlockKind::Paragraph(inlines) => {
            out.push(plain_text(inlines));
        }
        BlockKind::Code { text, .. } => out.push(text.clone()),
        BlockKind::Html(html) => out.push(html.trim_end().to_owned()),
        BlockKind::Math { latex, .. } => out.push(latex.clone()),
        BlockKind::Mermaid { source, .. } => out.push(source.clone()),
        BlockKind::Annotation { text, .. } => out.push(text.clone()),
        BlockKind::Table { .. } | BlockKind::Image { .. } | BlockKind::Field { .. } => {
            out.push(slice());
        }
        BlockKind::List { items, .. } => {
            for item in items {
                for child in &item.blocks {
                    collect_leaf_texts(doc, child, out);
                }
            }
        }
        BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
            for child in blocks {
                collect_leaf_texts(doc, child, out);
            }
        }
        BlockKind::Rule => {}
    }
}

/// The text of inline content as the layout flattens it.
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    push_plain(inlines, &mut out);
    out
}

fn push_plain(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) | Inline::Html(text) => out.push_str(text),
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
            Inline::Emph(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                push_plain(inner, out);
            }
            Inline::Link { inlines, .. } => push_plain(inlines, out),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Math { latex, .. } => out.push_str(latex),
            Inline::FootnoteRef(label) => out.push_str(label),
        }
    }
}

/// Where one match is: the top-level block and its ordinal within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub block: usize,
    pub nth: usize,
}

/// A search over a document: the matches and which one is current.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pub query: Option<Query>,
    /// `(block, matches in it)` for blocks with at least one match.
    counts: Vec<(usize, usize)>,
    /// Running total after each entry of `counts`.
    totals: Vec<usize>,
    current: Option<usize>,
}

impl Search {
    /// Search `doc` for `query`, forgetting the current match.
    pub fn run(&mut self, doc: &Doc, query: Option<Query>) {
        self.counts.clear();
        self.totals.clear();
        self.current = None;
        if let Some(query) = &query {
            let mut total = 0;
            for (index, block) in doc.blocks.iter().enumerate() {
                let found: usize = leaf_texts(doc, block)
                    .iter()
                    .map(|text| query.count_in(text))
                    .sum();
                if found == 0 {
                    continue;
                }
                total += found;
                self.counts.push((index, found));
                self.totals.push(total);
            }
        }
        self.query = query;
    }

    pub fn clear(&mut self) {
        self.query = None;
        self.counts.clear();
        self.totals.clear();
        self.current = None;
    }

    pub fn len(&self) -> usize {
        self.totals.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 1-based position of the current match, with the total.
    pub fn position(&self) -> Option<(usize, usize)> {
        Some((self.current? + 1, self.len()))
    }

    fn hit_at(&self, index: usize) -> Option<Hit> {
        let entry = self.totals.partition_point(|&total| total <= index);
        let (block, _) = *self.counts.get(entry)?;
        let before = if entry == 0 {
            0
        } else {
            self.totals[entry - 1]
        };
        Some(Hit {
            block,
            nth: index - before,
        })
    }

    pub fn current_hit(&self) -> Option<Hit> {
        self.hit_at(self.current?)
    }

    /// Index of the first match at or after top-level block `block`.
    fn first_from(&self, block: usize) -> Option<usize> {
        let entry = self.counts.partition_point(|&(held, _)| held < block);
        if entry >= self.counts.len() {
            return None;
        }
        Some(if entry == 0 {
            0
        } else {
            self.totals[entry - 1]
        })
    }

    /// Index of the last match at or before top-level block `block`.
    fn last_through(&self, block: usize) -> Option<usize> {
        let entry = self.counts.partition_point(|&(held, _)| held <= block);
        entry.checked_sub(1).map(|at| self.totals[at] - 1)
    }

    /// Number of matches held by top-level blocks before `block`.
    pub fn matches_before(&self, block: usize) -> usize {
        let entry = self.counts.partition_point(|&(held, _)| held < block);
        if entry == 0 {
            0
        } else {
            self.totals[entry - 1]
        }
    }

    /// Make the match with global index `index` current (wrapping to the first
    /// when it is past the end). Returns the hit and whether it wrapped.
    pub fn start_at(&mut self, index: usize) -> Option<(Hit, bool)> {
        if self.is_empty() {
            return None;
        }
        let (index, wrapped) = if index < self.len() {
            (index, false)
        } else {
            (0, true)
        };
        self.current = Some(index);
        Some((self.hit_at(index)?, wrapped))
    }

    /// Step with no current match: `before` is the number of matches that lie
    /// before the cursor. Forward takes the first match at or after it,
    /// backward the last one before it.
    pub fn step_from_cursor(&mut self, forward: bool, before: usize) -> Option<(Hit, bool)> {
        if forward {
            return self.start_at(before);
        }
        if self.is_empty() {
            return None;
        }
        let (index, wrapped) = match before.checked_sub(1) {
            Some(index) => (index.min(self.len() - 1), false),
            None => (self.len() - 1, true),
        };
        self.current = Some(index);
        Some((self.hit_at(index)?, wrapped))
    }

    /// Make the first match at or after `block` current (wrapping to the
    /// first). Returns the hit and whether it wrapped.
    pub fn start_from(&mut self, block: usize) -> Option<(Hit, bool)> {
        if self.is_empty() {
            return None;
        }
        let (index, wrapped) = match self.first_from(block) {
            Some(index) => (index, false),
            None => (0, true),
        };
        self.current = Some(index);
        Some((self.hit_at(index)?, wrapped))
    }

    /// Step to the next or previous match, wrapping. With no current match,
    /// start from `block`.
    pub fn step(&mut self, forward: bool, block: usize) -> Option<(Hit, bool)> {
        if self.is_empty() {
            return None;
        }
        let len = self.len();
        let (index, wrapped) = match (self.current, forward) {
            (Some(at), true) if at + 1 < len => (at + 1, false),
            (Some(_), true) => (0, true),
            (Some(at), false) if at > 0 => (at - 1, false),
            (Some(_), false) => (len - 1, true),
            (None, true) => match self.first_from(block) {
                Some(index) => (index, false),
                None => (0, true),
            },
            (None, false) => match self.last_through(block.saturating_sub(1)) {
                Some(index) if block > 0 => (index, false),
                _ => (len - 1, true),
            },
        };
        self.current = Some(index);
        Some((self.hit_at(index)?, wrapped))
    }
}

/// A text leaf of the layout and where it is painted. `x` is relative to the
/// text column, `y` is a document coordinate.
pub struct Leaf<'a> {
    pub text: &'a RichText,
    pub x: f32,
    pub y: f32,
    /// Painting is clipped to this rectangle (document coordinates).
    pub clip: Rect,
}

/// The text leaves of `block` in document order, with their paint origins.
pub fn leaves<'a>(block: &'a LaidBlock, scale: f32, out: &mut Vec<Leaf<'a>>) {
    let rule_px = scale.round().max(1.0);
    let rect = block.rect;
    match &block.content {
        Content::Text(text) => out.push(Leaf {
            text,
            x: rect.x,
            y: rect.y,
            clip: rect,
        }),
        Content::Heading {
            text,
            rule,
            pad_top,
            ..
        } => out.push(Leaf {
            text,
            x: rect.x,
            y: rect.y + if *rule { rule_px } else { 0.0 } + pad_top,
            clip: rect,
        }),
        Content::Code {
            text,
            gutter,
            scroll_x,
            overflow,
            caption_height,
            pad_y,
            border,
            ..
        } => {
            let inner_x = rect.x + border;
            let shift = if *overflow { *scroll_x } else { 0.0 };
            out.push(Leaf {
                text,
                x: inner_x + gutter - shift,
                y: rect.y + border + caption_height + pad_y,
                clip: Rect::new(
                    inner_x,
                    rect.y + border + caption_height,
                    rect.w - 2.0 * border,
                    rect.h - 2.0 * border - caption_height,
                ),
            });
        }
        Content::Pending(pending) => {
            let inset = rule_px + pending.padding;
            out.push(Leaf {
                text: &pending.text,
                x: rect.x + inset,
                y: rect.y + inset,
                clip: rect,
            });
        }
        Content::Children(children)
        | Content::Item { children, .. }
        | Content::Quote { children, .. } => {
            for child in children {
                leaves(child, scale, out);
            }
        }
        Content::Rule => {}
    }
}

/// The lines of text in a shaped block.
fn lines_of(text: &RichText) -> impl Iterator<Item = &str> {
    text.block.buffer().lines.iter().map(|line| line.text())
}

/// One run of a match on one visual row, in block-local pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x0: f32,
    pub x1: f32,
    pub baseline: f32,
}

/// Every match in `text` as visual segments. The outer index is the ordinal of
/// the match within the leaf.
pub fn match_segments(text: &RichText, query: &Query) -> Vec<Vec<Segment>> {
    let mut found: Vec<(usize, Range<usize>)> = Vec::new();
    for (line, content) in lines_of(text).enumerate() {
        for range in query.find_in_line(content) {
            found.push((line, range));
        }
    }
    if found.is_empty() {
        return Vec::new();
    }
    let mut by_line: Vec<Vec<_>> = Vec::new();
    for glyph in text.block.glyphs() {
        if by_line.len() <= glyph.line {
            by_line.resize_with(glyph.line + 1, Vec::new);
        }
        by_line[glyph.line].push(glyph);
    }
    found
        .into_iter()
        .map(|(line, range)| {
            let mut segments: Vec<Segment> = Vec::new();
            for glyph in by_line.get(line).into_iter().flatten() {
                if glyph.end <= range.start || glyph.start >= range.end {
                    continue;
                }
                let existing = segments
                    .iter_mut()
                    .find(|segment| (segment.baseline - glyph.baseline).abs() < 0.5);
                match existing {
                    Some(segment) => {
                        segment.x0 = segment.x0.min(glyph.x);
                        segment.x1 = segment.x1.max(glyph.x + glyph.width);
                    }
                    None => segments.push(Segment {
                        x0: glyph.x,
                        x1: glyph.x + glyph.width,
                        baseline: glyph.baseline,
                    }),
                }
            }
            segments
        })
        .collect()
}

/// Document y of the baseline of the first row of every match in top-level
/// block `block`, in order. A match without a laid-out row reports the top of
/// the block.
pub fn block_match_baselines(layout: &Layout, query: &Query, block: usize) -> Vec<f32> {
    let Some(laid) = layout.blocks.get(block) else {
        return Vec::new();
    };
    let mut all = Vec::new();
    leaves(laid, layout.scale(), &mut all);
    let mut out = Vec::new();
    for leaf in &all {
        for rows in match_segments(leaf.text, query) {
            out.push(
                rows.first()
                    .map_or(laid.rect.y, |row| leaf.y + row.baseline),
            );
        }
    }
    out
}

/// Document y of the baseline of the first row of the match.
pub fn locate_baseline(layout: &Layout, query: &Query, hit: Hit) -> Option<f32> {
    let baselines = block_match_baselines(layout, query, hit.block);
    baselines
        .get(hit.nth)
        .or(baselines.last())
        .copied()
        .or_else(|| layout.blocks.get(hit.block).map(|block| block.rect.y))
}

/// Document y of the first row of the match, for centring it.
pub fn locate_y(layout: &Layout, query: &Query, hit: Hit) -> Option<f32> {
    let block = layout.blocks.get(hit.block)?;
    let mut all = Vec::new();
    leaves(block, layout.scale(), &mut all);
    let mut remaining = hit.nth;
    let mut last: Option<f32> = None;
    for leaf in &all {
        for match_rows in match_segments(leaf.text, query) {
            let baseline = match_rows.first().map(|segment| segment.baseline);
            let y = baseline.map(|baseline| leaf.y + baseline - leaf.text.font_size);
            if y.is_some() {
                last = y;
            }
            if remaining == 0 {
                return y;
            }
            remaining -= 1;
        }
    }
    // The document text and the laid-out text disagreed; the closest match is
    // the last one.
    last.or(Some(block.rect.y))
}

/// Colours of search highlights.
pub fn highlight_colors(theme: &Theme) -> (Color, Color) {
    let amber = theme.amber;
    (
        Color::rgba(amber.r, amber.g, amber.b, 70),
        Color::rgba(amber.r, amber.g, amber.b, 150),
    )
}

/// Paint highlights over the visible blocks. `current` is the match to
/// emphasise. `scroll` is the document y at the top of the canvas.
pub fn paint_highlights(
    canvas: &mut Canvas,
    layout: &Layout,
    query: &Query,
    current: Option<Hit>,
    scroll: f32,
) {
    let theme = layout.theme();
    let scale = layout.scale();
    let (soft, strong) = highlight_colors(theme);
    let viewport = canvas.height() as f32;
    let start = layout
        .blocks
        .partition_point(|block| block.rect.bottom() < scroll);
    let stroke = scale.round().max(1.0);
    for (offset, block) in layout.blocks[start..].iter().enumerate() {
        if block.rect.y - scroll > viewport {
            break;
        }
        let index = start + offset;
        let mut all = Vec::new();
        leaves(block, scale, &mut all);
        let mut nth = 0;
        for leaf in &all {
            let size = leaf.text.font_size;
            for rows in match_segments(leaf.text, query) {
                let is_current = current == Some(Hit { block: index, nth });
                nth += 1;
                for segment in rows {
                    let rect = Rect::new(
                        layout.column_x + leaf.x + segment.x0,
                        leaf.y + segment.baseline - size * 0.98 - scroll,
                        segment.x1 - segment.x0,
                        size * 1.3,
                    );
                    let rect = clip_to(rect, leaf.clip, layout.column_x, scroll);
                    let Some(rect) = rect else {
                        continue;
                    };
                    canvas.fill_rect(rect, if is_current { strong } else { soft });
                    if is_current {
                        canvas.stroke_rect(rect, stroke, theme.amber);
                    }
                }
            }
        }
    }
}

/// Intersect `rect` (window coordinates) with `clip` (column x, document y).
fn clip_to(rect: Rect, clip: Rect, column_x: f32, scroll: f32) -> Option<Rect> {
    let left = rect.x.max(clip.x + column_x);
    let right = rect.right().min(clip.right() + column_x);
    let top = rect.y.max(clip.y - scroll);
    let bottom = rect.bottom().min(clip.bottom() - scroll);
    (right > left && bottom > top).then(|| Rect::new(left, top, right - left, bottom - top))
}

/// Centre the match's row in the window. Returns whether the view moved.
pub fn centre_on(env: &mut Env, query: &Query, hit: Hit) -> bool {
    env.lay_out_through(hit.block);
    let Some(y) = locate_y(env.layout, query, hit) else {
        return false;
    };
    let size = env.layout.theme().font_size * env.scale();
    env.set_scroll(y + size / 2.0 - env.viewport / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::lay_out, text::Fonts};
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn query(text: &str) -> Query {
        Query::smartcase(text).unwrap()
    }

    #[test]
    fn should_ignore_case_when_the_pattern_is_all_lower_case() {
        let q = query("foo");
        assert!(!q.case_sensitive);
        assert_eq!(q.find_in_line("Foo foo FOO"), vec![0..3, 4..7, 8..11]);
    }

    #[test]
    fn should_match_case_when_the_pattern_has_an_upper_case_letter() {
        let q = query("Foo");
        assert!(q.case_sensitive);
        assert_eq!(q.find_in_line("Foo foo FOO"), vec![0..3]);
    }

    #[test]
    fn should_return_no_query_when_the_pattern_is_empty() {
        assert_eq!(Query::smartcase(""), None);
        assert!(query(" ").find_in_line("a b c").len() == 2);
    }

    #[test]
    fn should_not_overlap_matches_when_the_pattern_repeats() {
        assert_eq!(query("aa").find_in_line("aaaaa"), vec![0..2, 2..4]);
    }

    #[test]
    fn should_return_byte_ranges_on_char_boundaries_when_text_is_multibyte() {
        let line = "caf\u{e9} \u{4e16}\u{754c} caf\u{e9}";
        let hits = query("caf\u{e9}").find_in_line(line);
        assert_eq!(hits.len(), 2);
        for hit in hits {
            assert!(line.is_char_boundary(hit.start) && line.is_char_boundary(hit.end));
            assert_eq!(&line[hit], "caf\u{e9}");
        }
        assert_eq!(query("\u{754c}").find_in_line(line).len(), 1);
    }

    #[test]
    fn should_fold_non_ascii_case_when_smartcase_is_insensitive() {
        assert_eq!(query("\u{e9}").find_in_line("\u{c9}t\u{e9}").len(), 2);
        assert_eq!(query("\u{c9}").find_in_line("\u{c9}t\u{e9}").len(), 1);
    }

    #[test]
    fn should_not_match_when_the_pattern_is_longer_than_the_line() {
        assert!(query("abcdef").find_in_line("abc").is_empty());
        assert!(query("a").find_in_line("").is_empty());
    }

    #[test]
    fn should_treat_regex_characters_literally_when_matching() {
        assert_eq!(query(".*").find_in_line("a.*b").len(), 1);
        assert_eq!(query("(").find_in_line("f(x)").len(), 1);
        assert_eq!(query("\\").find_in_line("a\\b").len(), 1);
    }

    #[test]
    fn should_count_per_line_when_text_has_hard_breaks() {
        let q = query("ab");
        assert_eq!(q.count_in("a\nb"), 0, "no match across the break");
        assert_eq!(q.count_in("ab\nab ab"), 3);
    }

    fn doc(markdown: &str) -> Doc {
        parse(markdown, Mode::File, &Settings::default())
    }

    fn searched(markdown: &str, pattern: &str) -> (Doc, Search) {
        let doc = doc(markdown);
        let mut search = Search::default();
        search.run(&doc, Query::smartcase(pattern));
        (doc, search)
    }

    #[test]
    fn should_count_matches_across_block_kinds_when_running_a_search() {
        let source = "# needle title\n\nplain needle and *needle*\n\n```\nneedle()\n```\n\n- item needle\n\n> quoted needle\n";
        let (_, search) = searched(source, "needle");
        assert_eq!(search.len(), 6);
        assert_eq!(search.position(), None);
    }

    #[test]
    fn should_search_link_text_but_not_the_url_when_a_paragraph_has_a_link() {
        let (_, search) = searched("[click here](http://example.com/needle)\n", "needle");
        assert_eq!(search.len(), 0);
        let (_, search) = searched("[needle here](http://example.com)\n", "needle");
        assert_eq!(search.len(), 1);
    }

    #[test]
    fn should_find_nothing_when_the_pattern_is_missing_or_empty() {
        let (_, mut search) = searched("alpha\n", "zebra");
        assert!(search.is_empty());
        assert_eq!(search.step(true, 0), None);
        assert_eq!(search.start_from(0), None);
        let (_, search) = searched("alpha\n", "");
        assert!(search.is_empty());
        assert!(search.query.is_none());
    }

    #[test]
    fn should_address_matches_by_block_and_ordinal_when_stepping() {
        let (_, mut search) = searched("x one\n\nnone\n\ntwo\n\none one\n", "one");
        assert_eq!(search.len(), 4);
        let (first, wrapped) = search.start_from(0).unwrap();
        assert_eq!((first, wrapped), (Hit { block: 0, nth: 0 }, false));
        assert_eq!(search.position(), Some((1, 4)));
        let (second, _) = search.step(true, 0).unwrap();
        assert_eq!(second, Hit { block: 1, nth: 0 });
        let (third, _) = search.step(true, 0).unwrap();
        assert_eq!(third, Hit { block: 3, nth: 0 });
        let (fourth, _) = search.step(true, 0).unwrap();
        assert_eq!(fourth, Hit { block: 3, nth: 1 });
        assert_eq!(search.position(), Some((4, 4)));
    }

    #[test]
    fn should_wrap_and_say_so_when_stepping_past_either_end() {
        let (_, mut search) = searched("one\n\nfiller\n\none\n", "one");
        search.start_from(0);
        search.step(true, 0);
        let (hit, wrapped) = search.step(true, 0).unwrap();
        assert!(wrapped);
        assert_eq!(hit, Hit { block: 0, nth: 0 });
        let (hit, wrapped) = search.step(false, 0).unwrap();
        assert!(wrapped);
        assert_eq!(hit, Hit { block: 2, nth: 0 });
        let (hit, wrapped) = search.step(false, 0).unwrap();
        assert!(!wrapped);
        assert_eq!(hit, Hit { block: 0, nth: 0 });
    }

    #[test]
    fn should_start_at_the_cursor_block_when_there_is_no_current_match() {
        let (_, mut search) = searched("one\n\ntwo\n\none\n\ntwo\n\none\n", "one");
        let (hit, wrapped) = search.start_from(3).unwrap();
        assert_eq!((hit.block, wrapped), (4, false));
        search.clear();
        let (_, mut search) = searched("one\n\ntwo\n\none\n\ntwo\n\none\n", "one");
        let (hit, _) = search.step(false, 3).unwrap();
        assert_eq!(hit.block, 2, "previous match before block 3");
        let (_, mut search) = searched("one\n\ntwo\n", "one");
        let (hit, wrapped) = search.start_from(2).unwrap();
        assert_eq!((hit.block, wrapped), (0, true), "nothing after: wrap");
    }

    #[test]
    fn should_find_the_previous_match_from_the_first_block_by_wrapping() {
        let (_, mut search) = searched("one\n\ntwo\n\none\n", "one");
        let (hit, wrapped) = search.step(false, 0).unwrap();
        assert_eq!((hit.block, wrapped), (2, true));
    }

    #[test]
    fn should_forget_everything_when_cleared() {
        let (_, mut search) = searched("one\n", "one");
        search.start_from(0);
        search.clear();
        assert!(search.is_empty() && search.query.is_none());
        assert_eq!(search.position(), None);
        assert_eq!(search.current_hit(), None);
    }

    #[test]
    fn should_not_search_markers_or_callout_titles() {
        let (_, search) = searched("1. first\n2. second\n\n> [!NOTE]\n> body\n", "note");
        assert_eq!(search.len(), 0);
        let (_, search) = searched("1. first\n", "1");
        assert_eq!(search.len(), 0);
    }

    #[test]
    fn should_search_table_and_math_sources_when_the_layout_shows_them() {
        let (_, search) = searched("| a | needle |\n|---|---|\n| 1 | 2 |\n", "needle");
        assert_eq!(search.len(), 1);
        let (_, search) = searched("$$\nneedle^2\n$$\n", "needle");
        assert_eq!(search.len(), 1);
    }

    fn laid(markdown: &str, width: f32) -> (Doc, Layout) {
        let doc = doc(markdown);
        let layout = lay_out(&doc, width, &Theme::dark(), 1.0, &mut Fonts::new());
        (doc, layout)
    }

    #[test]
    fn should_agree_with_the_document_count_when_matching_laid_out_text() {
        let source = "# needle\n\nneedle needle\n\n```\nneedle\n```\n\n- needle\n  - needle\n\n> needle\n\n| needle |\n|---|\n| x |\n";
        let (doc, layout) = laid(source, 900.0);
        let q = query("needle");
        let mut search = Search::default();
        search.run(&doc, Some(q.clone()));
        for (index, block) in layout.blocks.iter().enumerate() {
            let mut all = Vec::new();
            leaves(block, 1.0, &mut all);
            let laid_count: usize = all
                .iter()
                .map(|leaf| match_segments(leaf.text, &q).len())
                .sum();
            let doc_count: usize = leaf_texts(&doc, &doc.blocks[index])
                .iter()
                .map(|text| q.count_in(text))
                .sum();
            assert_eq!(laid_count, doc_count, "block {index}");
        }
    }

    #[test]
    fn should_split_a_match_into_one_segment_per_row_when_it_wraps() {
        let long = format!("{} needle text after", "word ".repeat(40));
        let (_, layout) = laid(&long, 300.0);
        let mut all = Vec::new();
        leaves(&layout.blocks[0], 1.0, &mut all);
        let rows = match_segments(all[0].text, &query("word"));
        assert_eq!(rows.len(), 40);
        assert!(rows.iter().all(|segments| segments.len() == 1));
        let baselines: std::collections::BTreeSet<i32> =
            rows.iter().map(|s| s[0].baseline as i32).collect();
        assert!(baselines.len() > 3, "wrapped onto several rows");
    }

    #[test]
    fn should_place_a_later_match_lower_when_locating_in_a_long_paragraph() {
        let long = format!("needle {} needle", "word ".repeat(80));
        let (_, layout) = laid(&long, 400.0);
        let q = query("needle");
        let first = locate_y(&layout, &q, Hit { block: 0, nth: 0 }).unwrap();
        let second = locate_y(&layout, &q, Hit { block: 0, nth: 1 }).unwrap();
        assert!(second > first + 50.0, "{first} {second}");
        let missing = locate_y(&layout, &q, Hit { block: 0, nth: 9 }).unwrap();
        assert_eq!(missing, second, "clamps to the last match");
        assert_eq!(locate_y(&layout, &q, Hit { block: 5, nth: 0 }), None);
    }

    #[test]
    fn should_paint_highlights_over_matches_only_when_the_block_is_visible() {
        let (_, layout) = laid("find the needle here\n\nunrelated words\n", 900.0);
        let q = query("needle");
        let mut plain = vec![0u32; 900 * 200];
        let mut marked = plain.clone();
        paint_highlights(
            &mut Canvas::new(&mut marked, 900, 200),
            &layout,
            &q,
            None,
            0.0,
        );
        assert_ne!(plain, marked);
        paint_highlights(
            &mut Canvas::new(&mut plain, 900, 200),
            &layout,
            &query("zebra"),
            None,
            0.0,
        );
        assert!(plain.iter().all(|&p| p == 0));
        let mut scrolled = vec![0u32; 900 * 200];
        paint_highlights(
            &mut Canvas::new(&mut scrolled, 900, 200),
            &layout,
            &q,
            None,
            5000.0,
        );
        assert!(scrolled.iter().all(|&p| p == 0));
    }

    #[test]
    fn should_emphasise_only_the_current_match_when_painting() {
        let (_, layout) = laid("needle and needle\n", 900.0);
        let q = query("needle");
        let mut first = vec![0u32; 900 * 100];
        let mut second = first.clone();
        let mut none = first.clone();
        let hit = |nth| Some(Hit { block: 0, nth });
        paint_highlights(
            &mut Canvas::new(&mut first, 900, 100),
            &layout,
            &q,
            hit(0),
            0.0,
        );
        paint_highlights(
            &mut Canvas::new(&mut second, 900, 100),
            &layout,
            &q,
            hit(1),
            0.0,
        );
        paint_highlights(
            &mut Canvas::new(&mut none, 900, 100),
            &layout,
            &q,
            None,
            0.0,
        );
        assert_ne!(first, second);
        assert_ne!(first, none);
    }

    #[test]
    fn should_centre_the_match_row_when_centring() {
        let source: String = (0..80)
            .map(|n| format!("filler {n}\n\n"))
            .collect::<String>()
            + "the needle\n";
        let doc = doc(&source);
        let mut fonts = Fonts::new();
        let mut layout = lay_out(&doc, 900.0, &Theme::dark(), 1.0, &mut fonts);
        let mut scroll = 0.0;
        let mut env = Env {
            doc: &doc,
            layout: &mut layout,
            fonts: &mut fonts,
            scroll: &mut scroll,
            viewport: 300.0,
        };
        let q = query("needle");
        assert!(centre_on(&mut env, &q, Hit { block: 80, nth: 0 }));
        let y = locate_y(env.layout, &q, Hit { block: 80, nth: 0 }).unwrap();
        let middle = y - *env.scroll;
        assert!((middle - 150.0).abs() < 20.0, "{middle}");
    }

    #[test]
    fn should_start_at_a_match_index_and_wrap_when_it_is_past_the_end() {
        let (_, mut search) = searched("x a\n\nb x\n", "x");
        assert_eq!(search.matches_before(1), 1);
        let (hit, wrapped) = search.start_at(1).unwrap();
        assert_eq!((hit.block, wrapped), (1, false));
        let (hit, wrapped) = search.start_at(2).unwrap();
        assert_eq!((hit.block, wrapped), (0, true));
        let (hit, wrapped) = search.step_from_cursor(false, 0).unwrap();
        assert_eq!((hit.block, wrapped), (1, true));
        let (hit, wrapped) = search.step_from_cursor(false, 2).unwrap();
        assert_eq!((hit.block, wrapped), (1, false));
    }
}
