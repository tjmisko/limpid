//! The block cursor: one stop is focused at a time, the viewport follows only
//! when the stop would otherwise be off screen, and section and alignment
//! moves put it where the key table says.

use crate::{
    layout::{Content, LaidBlock, Layout, PendingKind},
    paint::{Canvas, Rect},
    text::Fonts,
};
use limpid_core::doc::{BlockKind, Doc};
use std::ops::Range;

/// Distance kept between a revealed stop and the window edge, CSS px.
pub const REVEAL_MARGIN: f32 = 12.0;
pub const ALIGN_MARGIN: f32 = 8.0;
/// How far the focus band reaches past the text column, CSS px.
pub const BLEED_INSET: f32 = 10.0;
/// Space kept above a heading when a section jump puts it at the top, CSS px.
const HEADING_SCROLL_MARGIN: f32 = 24.0;
/// Corner radius of the focus band and the selection, CSS px.
const BAND_RADIUS: f32 = 6.0;
/// Corner radius of the outline around code, tables and diagrams, CSS px.
const BLOCK_RADIUS: f32 = 10.0;
/// Corner radius of the outline around a question, CSS px.
const QUESTION_RADIUS: f32 = 8.0;
/// Outline thickness and distance from the block, CSS px.
const OUTLINE_WIDTH: f32 = 2.0;
const OUTLINE_OFFSET: f32 = 2.0;

/// Everything a cursor move reads or changes.
pub struct Env<'a> {
    pub doc: &'a Doc,
    pub layout: &'a mut Layout,
    pub fonts: &'a mut Fonts,
    /// Document y at the top of the visible area, device pixels.
    pub scroll: &'a mut f32,
    /// Height of the visible area, device pixels.
    pub viewport: f32,
}

impl Env<'_> {
    pub fn scale(&self) -> f32 {
        self.layout.scale()
    }

    pub fn margin(&self) -> f32 {
        REVEAL_MARGIN * self.scale()
    }

    pub fn align_margin(&self) -> f32 {
        ALIGN_MARGIN * self.scale()
    }

    /// Lay out one more top-level block. False once the document is done.
    pub fn lay_out_more(&mut self) -> bool {
        if self.layout.is_complete() {
            return false;
        }
        let next = self.layout.blocks.len();
        self.layout.ensure_block(next, self.doc, self.fonts);
        true
    }

    pub fn lay_out_through(&mut self, block: usize) {
        self.layout.ensure_block(block, self.doc, self.fonts);
    }

    pub fn lay_out_all(&mut self) {
        while self.lay_out_more() {}
    }

    pub fn max_scroll(&self) -> Option<f32> {
        self.layout.max_scroll()
    }

    /// Scroll to `target`, clamped to the document. Returns whether it moved.
    pub fn set_scroll(&mut self, target: f32) -> bool {
        // Lay out ahead so the clamp knows whether the end is near.
        let reach = if target.is_finite() { target } else { 0.0 };
        self.layout
            .ensure_to(reach + self.viewport * 2.0, self.doc, self.fonts);
        let mut target = if target.is_finite() {
            target.max(0.0)
        } else {
            0.0
        };
        if let Some(max) = self.max_scroll() {
            target = target.min(max);
        }
        let moved = (target - *self.scroll).abs() > 0.5;
        *self.scroll = target;
        moved
    }

    /// Scroll just far enough to show `y..y + height`. A block taller than the
    /// window shows its top.
    pub fn reveal(&mut self, y: f32, height: f32) -> bool {
        let margin = self.margin();
        let scroll = *self.scroll;
        if y - margin < scroll {
            return self.set_scroll(y - margin);
        }
        let bottom = y + height;
        if bottom + margin > scroll + self.viewport {
            let target = (bottom + margin - self.viewport).min(y - margin);
            return self.set_scroll(target);
        }
        false
    }
}

/// One cursor stop and where it sits in the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    /// Index of the top-level block that holds it.
    pub top: usize,
    pub src: Range<usize>,
    /// Document geometry: x is relative to the text column.
    pub rect: Rect,
    pub kind: StopKind,
}

/// How a stop shows focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopKind {
    /// A band behind the text.
    #[default]
    Text,
    /// The text turns teal; no band, no outline.
    Heading,
    /// Code, a diagram or a table: an outline, no band.
    Special,
    /// A question: a teal outline around its box.
    Question,
}

impl StopKind {
    fn of(block: &LaidBlock) -> Self {
        match &block.content {
            Content::Heading { .. } => Self::Heading,
            Content::Code { .. } | Content::Table(_) => Self::Special,
            // Only the framed media is a diagram; images and math get the band.
            Content::Media(media) if media.framed => Self::Special,
            Content::Pending(pending) => match pending.kind {
                PendingKind::Field => Self::Question,
                PendingKind::Table | PendingKind::Mermaid => Self::Special,
                _ => Self::Text,
            },
            _ => Self::Text,
        }
    }
}

/// Every stop laid out so far, in document order. A container's own stop
/// comes before its children's.
pub fn collect_stops(layout: &Layout) -> Vec<Stop> {
    fn walk(top: usize, block: &LaidBlock, out: &mut Vec<Stop>) {
        if block.stop {
            out.push(Stop {
                top,
                src: block.src.clone(),
                rect: block.rect,
                kind: StopKind::of(block),
            });
        }
        for child in block.children() {
            walk(top, child, out);
        }
    }
    let mut out = Vec::new();
    for (top, block) in layout.blocks.iter().enumerate() {
        walk(top, block, &mut out);
    }
    out
}

/// Top-level indices of the headings, in document order.
pub fn heading_blocks(doc: &Doc) -> Vec<usize> {
    doc.blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| matches!(block.kind, BlockKind::Heading { .. }))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Top,
    Center,
    Bottom,
}

/// The focused stop, remembered by its source range so it survives
/// re-layout, zoom and reload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    pub src: Option<Range<usize>>,
}

impl Cursor {
    /// Index of the cursor's stop in `stops`: an exact source match, else the
    /// first stop at or after where it was, else the last stop.
    pub fn locate(&self, stops: &[Stop]) -> Option<usize> {
        let src = self.src.as_ref()?;
        if let Some(exact) = stops.iter().position(|stop| &stop.src == src) {
            return Some(exact);
        }
        let after = stops.iter().position(|stop| stop.src.start >= src.start);
        after.or_else(|| stops.len().checked_sub(1))
    }

    /// The focused stop, if it has been laid out.
    pub fn stop(&self, layout: &Layout) -> Option<Stop> {
        let stops = collect_stops(layout);
        let at = self.locate(&stops)?;
        stops.into_iter().nth(at)
    }

    /// Put the cursor on the first stop that is in view, or the first stop.
    pub fn init(&mut self, env: &mut Env) -> bool {
        let stops = collect_stops(env.layout);
        if self.locate(&stops).is_some() {
            return false;
        }
        let visible = stops
            .iter()
            .find(|stop| stop.rect.bottom() > *env.scroll)
            .or_else(|| stops.first());
        let Some(stop) = visible else {
            return false;
        };
        self.src = Some(stop.src.clone());
        true
    }

    fn set(&mut self, stop: &Stop) {
        self.src = Some(stop.src.clone());
    }

    /// Move `delta` stops, laying out more of the document if needed, and
    /// reveal the new stop. Returns whether anything changed.
    pub fn move_by(&mut self, env: &mut Env, delta: i64) -> bool {
        if delta == 0 {
            return false;
        }
        loop {
            let stops = collect_stops(env.layout);
            let Some(current) = self.locate(&stops) else {
                if env.lay_out_more() {
                    continue;
                }
                return false;
            };
            let want = current as i64 + delta;
            if want >= stops.len() as i64 && env.lay_out_more() {
                continue;
            }
            let target = want.clamp(0, stops.len() as i64 - 1) as usize;
            let changed = target != current;
            let stop = stops[target].clone();
            self.set(&stop);
            let scrolled = env.reveal(stop.rect.y, stop.rect.h);
            return changed || scrolled;
        }
    }

    /// Go to the `nth` stop (1-based), counting from the top.
    pub fn go_to_nth(&mut self, env: &mut Env, nth: usize) -> bool {
        loop {
            let stops = collect_stops(env.layout);
            if nth > stops.len() && env.lay_out_more() {
                continue;
            }
            let Some(stop) = stops.get(nth.max(1) - 1).or(stops.last()) else {
                return false;
            };
            let stop = stop.clone();
            let changed = self.src.as_ref() != Some(&stop.src);
            self.set(&stop);
            let scrolled = env.reveal(stop.rect.y, stop.rect.h);
            return changed || scrolled;
        }
    }

    /// `gg`: the first stop, with the page scrolled to its top.
    pub fn to_top(&mut self, env: &mut Env, nth: Option<usize>) -> bool {
        if let Some(nth) = nth {
            return self.go_to_nth(env, nth);
        }
        let first = collect_stops(env.layout).into_iter().next();
        let mut changed = env.set_scroll(0.0);
        if let Some(stop) = first {
            changed |= self.src.as_ref() != Some(&stop.src);
            self.set(&stop);
        }
        changed
    }

    /// `G`: the last stop, with the page scrolled to its end.
    pub fn to_bottom(&mut self, env: &mut Env, nth: Option<usize>) -> bool {
        if let Some(nth) = nth {
            return self.go_to_nth(env, nth);
        }
        env.lay_out_all();
        let last = collect_stops(env.layout).pop();
        let Some(stop) = last else {
            return false;
        };
        let changed = self.src.as_ref() != Some(&stop.src);
        self.set(&stop);
        let scrolled = env.reveal(stop.rect.y, stop.rect.h);
        changed || scrolled
    }

    /// The last stop, with the page scrolled to the document end.
    fn go_to_end(&mut self, env: &mut Env) -> bool {
        env.lay_out_all();
        let last = collect_stops(env.layout).pop();
        let mut changed = env.set_scroll(f32::MAX / 4.0);
        if let Some(stop) = last {
            changed |= self.src.as_ref() != Some(&stop.src);
            self.set(&stop);
        }
        changed
    }

    /// `zt`, `zz`, `zb`.
    pub fn align(&mut self, env: &mut Env, align: Align) -> bool {
        let Some(stop) = self.stop(env.layout) else {
            return false;
        };
        let margin = env.align_margin();
        let target = match align {
            Align::Top => stop.rect.y - margin,
            Align::Center => stop.rect.y - (env.viewport - stop.rect.h) / 2.0,
            Align::Bottom => stop.rect.bottom() - env.viewport + margin,
        };
        env.set_scroll(target)
    }

    /// Scroll `steps` times `fraction` of the viewport, then move the cursor
    /// to the stop at the new viewport centre and centre it. A stop taller
    /// than the viewport, one already focused, or one whose centring would
    /// jump more than a viewport just gets the plain scroll.
    pub fn half_page(&mut self, env: &mut Env, steps: i64, fraction: f32) -> bool {
        let fraction = if fraction.is_finite() && fraction > 0.0 {
            fraction.min(1.0)
        } else {
            0.5
        };
        let viewport = env.viewport;
        let start = *env.scroll;
        let delta = steps as f32 * viewport * fraction;
        let reach = if delta.is_finite() {
            start + delta
        } else {
            start
        };
        env.layout
            .ensure_to(reach.max(0.0) + viewport * 2.0, env.doc, env.fonts);
        let limit = env.max_scroll().unwrap_or(f32::MAX);
        let probe = reach.clamp(0.0, limit);
        let stops = collect_stops(env.layout);
        if stops.is_empty() {
            return env.set_scroll(start + delta);
        }
        let centre = probe + viewport / 2.0;
        let mut containing: Option<&Stop> = None;
        for stop in stops
            .iter()
            .filter(|stop| stop.rect.y <= centre && stop.rect.bottom() >= centre)
        {
            if containing.is_none_or(|best| stop.rect.h < best.rect.h) {
                containing = Some(stop);
            }
        }
        let distance = |stop: &Stop| (stop.rect.y + stop.rect.h / 2.0 - centre).abs();
        let mut nearest = &stops[0];
        for stop in &stops {
            if distance(stop) < distance(nearest) {
                nearest = stop;
            }
        }
        let target = containing.unwrap_or(nearest).clone();
        let centred = (target.rect.y + target.rect.h / 2.0 - viewport / 2.0).clamp(0.0, limit);
        let focused = self.src.as_ref() == Some(&target.src);
        if focused || target.rect.h > viewport || (centred - start).abs() > viewport {
            let scrolled = env.set_scroll(start + delta);
            self.set(&target);
            return scrolled || !focused;
        }
        self.set(&target);
        env.set_scroll(centred);
        true
    }

    /// `{` and `}`: step `steps` sections (negative goes back). The cursor
    /// lands on the section's heading, which sits at the top of the window. A
    /// document with no headings has no sections, so nothing moves.
    pub fn section(&mut self, env: &mut Env, steps: i64) -> bool {
        let heads = heading_blocks(env.doc);
        if heads.is_empty() {
            return false;
        }
        let current_top = match self.stop(env.layout) {
            Some(stop) => stop.top,
            None => env.layout.block_at(*env.scroll).unwrap_or(0),
        };
        let current = heads.partition_point(|&head| head <= current_top) as i64 - 1;
        let target = current + steps;
        if target < 0 || target >= heads.len() as i64 {
            return if target < 0 {
                self.to_top(env, None)
            } else {
                self.go_to_end(env)
            };
        }
        let head = heads[target as usize];
        let bound = heads
            .get(target as usize + 1)
            .copied()
            .unwrap_or(usize::MAX);
        env.lay_out_through(head);
        let Some(head_rect) = env.layout.blocks.get(head).map(|block| block.rect) else {
            return false;
        };
        let scrolled = env.set_scroll(head_rect.y - HEADING_SCROLL_MARGIN * env.scale());
        let first_stop = loop {
            let stops = collect_stops(env.layout);
            if let Some(stop) = stops.into_iter().find(|stop| stop.top >= head) {
                break Some(stop);
            }
            if !env.lay_out_more() {
                break None;
            }
        };
        let Some(stop) = first_stop.filter(|stop| stop.top < bound) else {
            return scrolled;
        };
        let moved = self.src.as_ref() != Some(&stop.src);
        self.set(&stop);
        scrolled || moved
    }

    /// Put the cursor on the innermost stop of top-level block `block` that
    /// holds document y `y`, without scrolling. Falls back to the first stop
    /// at or after the block.
    pub fn go_to_row(&mut self, env: &mut Env, block: usize, y: f32) -> bool {
        env.lay_out_through(block);
        let slack = 0.5;
        let holder = collect_stops(env.layout)
            .into_iter()
            .filter(|stop| stop.top == block)
            .rfind(|stop| stop.rect.y - slack <= y && y <= stop.rect.bottom() + slack);
        let Some(stop) = holder else {
            return self.go_to_block(env, block);
        };
        let moved = self.src.as_ref() != Some(&stop.src);
        self.set(&stop);
        moved
    }

    /// Put the cursor on the first stop at or after top-level block `block`,
    /// without scrolling.
    pub fn go_to_block(&mut self, env: &mut Env, block: usize) -> bool {
        env.lay_out_through(block);
        let found = loop {
            let stops = collect_stops(env.layout);
            if let Some(stop) = stops.into_iter().find(|stop| stop.top >= block) {
                break Some(stop);
            }
            if !env.lay_out_more() {
                break collect_stops(env.layout).pop();
            }
        };
        let Some(stop) = found else {
            return false;
        };
        let moved = self.src.as_ref() != Some(&stop.src);
        self.set(&stop);
        moved
    }
}

/// The rectangle a band covers for `rect`: the column minus the inset on each
/// side, in window coordinates.
pub fn band_rect(layout: &Layout, rect: Rect, scroll: f32) -> Rect {
    let bleed = BLEED_INSET * layout.scale();
    Rect::new(
        layout.column_x + bleed,
        rect.y - scroll,
        layout.column_width - 2.0 * bleed,
        rect.h,
    )
}

/// Paint the band behind `rect` (document coordinates) in `tint`.
pub fn paint_band(
    canvas: &mut Canvas,
    layout: &Layout,
    rect: Rect,
    scroll: f32,
    tint: crate::theme::Color,
) {
    let band = band_rect(layout, rect, scroll);
    canvas.fill_rounded_rect(band, BAND_RADIUS * layout.scale(), tint);
}

/// Outline the stop at `rect` (document coordinates): `color`, 2 px wide,
/// 2 px outside the block and following its corners.
pub fn paint_outline(
    canvas: &mut Canvas,
    layout: &Layout,
    kind: StopKind,
    rect: Rect,
    scroll: f32,
    color: crate::theme::Color,
) {
    let scale = layout.scale();
    let reach = OUTLINE_OFFSET * scale + OUTLINE_WIDTH * scale;
    let (offset, radius) = match kind {
        StopKind::Question => (
            OUTLINE_WIDTH * scale,
            QUESTION_RADIUS * scale + OUTLINE_WIDTH * scale,
        ),
        _ => (reach, BLOCK_RADIUS * scale + reach),
    };
    let outer = Rect::new(
        layout.column_x + rect.x - offset,
        rect.y - scroll - offset,
        rect.w + 2.0 * offset,
        rect.h + 2.0 * offset,
    );
    canvas.stroke_rounded_rect(outer, radius, OUTLINE_WIDTH * scale, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::lay_out;
    use crate::theme::Theme;
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    struct Fixture {
        doc: Doc,
        layout: Layout,
        fonts: Fonts,
        scroll: f32,
        viewport: f32,
    }

    impl Fixture {
        fn new(markdown: &str, viewport: f32) -> Self {
            let doc = parse(markdown, Mode::File, &Settings::default());
            let mut fonts = Fonts::new();
            let layout = lay_out(&doc, 900.0, &Theme::dark(), 1.0, &mut fonts);
            Fixture {
                doc,
                layout,
                fonts,
                scroll: 0.0,
                viewport,
            }
        }

        fn env(&mut self) -> Env<'_> {
            Env {
                doc: &self.doc,
                layout: &mut self.layout,
                fonts: &mut self.fonts,
                scroll: &mut self.scroll,
                viewport: self.viewport,
            }
        }
    }

    fn paragraphs(count: usize) -> String {
        (0..count).map(|n| format!("paragraph {n}\n\n")).collect()
    }

    fn focused_text(fixture: &Fixture, cursor: &Cursor) -> String {
        let stop = cursor.stop(&fixture.layout).unwrap();
        fixture.doc.source[stop.src].trim().to_owned()
    }

    #[test]
    fn should_start_on_the_first_stop_when_nothing_is_focused() {
        let mut fixture = Fixture::new("# Title\n\nfirst\n\nsecond\n", 400.0);
        let mut cursor = Cursor::default();
        assert!(cursor.init(&mut fixture.env()));
        assert_eq!(focused_text(&fixture, &cursor), "# Title");
        assert!(!cursor.init(&mut fixture.env()), "already placed");
    }

    #[test]
    fn should_start_on_the_first_visible_stop_when_the_view_is_scrolled() {
        let mut fixture = Fixture::new(&paragraphs(100), 300.0);
        let mut env = fixture.env();
        env.set_scroll(900.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut env);
        let stop = cursor.stop(env.layout).unwrap();
        assert!(
            stop.rect.bottom() > 900.0 && stop.rect.y < 900.0 + 300.0,
            "{stop:?}"
        );
        assert!(stop.rect.y > 800.0);
    }

    #[test]
    fn should_have_no_cursor_when_the_document_has_no_stops() {
        let mut fixture = Fixture::new("---\n\n***\n", 300.0);
        let mut cursor = Cursor::default();
        assert!(!cursor.init(&mut fixture.env()));
        assert!(!cursor.move_by(&mut fixture.env(), 1));
        assert!(!cursor.to_bottom(&mut fixture.env(), None));
        assert!(!cursor.align(&mut fixture.env(), Align::Center));
        assert_eq!(cursor.src, None);
    }

    #[test]
    fn should_start_on_the_first_real_line_when_the_page_has_front_matter() {
        let fixture = Fixture::new("---\ntitle: Agent\nmodel: m\n---\n\nfirst line\n", 300.0);
        let mut cursor = Cursor::default();
        let mut fixture = fixture;
        cursor.init(&mut fixture.env());
        assert_eq!(focused_text(&fixture, &cursor), "first line");
    }

    #[test]
    fn should_move_one_stop_at_a_time_when_stepping() {
        let mut fixture = Fixture::new("one\n\ntwo\n\nthree\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.move_by(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "two");
        assert!(cursor.move_by(&mut fixture.env(), 1));
        assert!(
            !cursor.move_by(&mut fixture.env(), 1),
            "stays on the last stop"
        );
        assert_eq!(focused_text(&fixture, &cursor), "three");
        assert!(cursor.move_by(&mut fixture.env(), -2));
        assert_eq!(focused_text(&fixture, &cursor), "one");
        assert!(!cursor.move_by(&mut fixture.env(), -1));
    }

    #[test]
    fn should_clamp_a_huge_count_to_the_ends_when_moving() {
        let mut fixture = Fixture::new("one\n\ntwo\n\nthree\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.move_by(&mut fixture.env(), 99_999));
        assert_eq!(focused_text(&fixture, &cursor), "three");
        assert!(cursor.move_by(&mut fixture.env(), -99_999));
        assert_eq!(focused_text(&fixture, &cursor), "one");
    }

    #[test]
    fn should_stop_on_headings_and_skip_rules_when_moving() {
        let mut fixture = Fixture::new("a\n\n# H\n\n---\n\nb\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 1);
        assert_eq!(focused_text(&fixture, &cursor), "# H");
        cursor.move_by(&mut fixture.env(), 1);
        assert_eq!(focused_text(&fixture, &cursor), "b");
    }

    #[test]
    fn should_visit_list_items_one_by_one_when_moving_through_a_list() {
        let mut fixture = Fixture::new("intro\n\n- x\n  - y\n- z\n\nouter\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        let mut seen = Vec::new();
        for _ in 0..5 {
            cursor.move_by(&mut fixture.env(), 1);
            let stop = cursor.stop(&fixture.layout).unwrap();
            seen.push(
                fixture.doc.source[stop.src]
                    .lines()
                    .next()
                    .unwrap()
                    .trim()
                    .to_owned(),
            );
        }
        assert_eq!(seen, vec!["x", "y", "z", "outer", "outer"]);
    }

    #[test]
    fn should_not_scroll_while_the_stop_is_on_screen_when_stepping() {
        let mut fixture = Fixture::new(&paragraphs(30), 800.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        for _ in 0..3 {
            cursor.move_by(&mut fixture.env(), 1);
        }
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_scroll_just_enough_when_the_stop_falls_below_the_window() {
        let mut fixture = Fixture::new(&paragraphs(60), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        for _ in 0..20 {
            cursor.move_by(&mut fixture.env(), 1);
            let stop = cursor.stop(&fixture.layout).unwrap();
            assert!(stop.rect.y >= fixture.scroll, "{stop:?} {}", fixture.scroll);
            assert!(
                stop.rect.bottom() <= fixture.scroll + 300.0,
                "{stop:?} {}",
                fixture.scroll
            );
        }
        assert!(fixture.scroll > 0.0);
    }

    #[test]
    fn should_scroll_up_to_show_the_stop_when_it_rises_above_the_window() {
        let mut fixture = Fixture::new(&paragraphs(60), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 20);
        let low = fixture.scroll;
        cursor.move_by(&mut fixture.env(), -19);
        assert!(fixture.scroll < low);
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(stop.rect.y >= fixture.scroll);
    }

    #[test]
    fn should_show_the_top_of_a_block_taller_than_the_window_when_revealing() {
        let tall = format!("```\n{}\n```\n", "line\n".repeat(80));
        let mut fixture = Fixture::new(&format!("a\n\n{tall}\nb\n"), 200.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 1);
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(stop.rect.h > 200.0);
        assert!(fixture.scroll <= stop.rect.y, "top of the block is visible");
        assert!(stop.rect.y - fixture.scroll < 200.0);
    }

    #[test]
    fn should_lay_out_more_when_the_cursor_moves_past_what_is_laid_out() {
        let mut fixture = Fixture::new(&paragraphs(3000), 400.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        let laid_before = fixture.layout.blocks.len();
        assert!(laid_before < 3000);
        cursor.move_by(&mut fixture.env(), laid_before as i64 + 20);
        assert!(fixture.layout.blocks.len() > laid_before);
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(fixture.doc.source[stop.src].contains(&format!("paragraph {}", laid_before + 20)));
    }

    #[test]
    fn should_jump_to_the_ends_when_pressing_gg_and_capital_g() {
        let mut fixture = Fixture::new(&paragraphs(60), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.to_bottom(&mut fixture.env(), None));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 59");
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(stop.rect.bottom() <= fixture.scroll + 300.0);
        assert!(stop.rect.y >= fixture.scroll);
        assert!(cursor.to_top(&mut fixture.env(), None));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 0");
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_go_to_the_nth_stop_when_a_count_prefixes_gg_or_g() {
        let mut fixture = Fixture::new(&paragraphs(20), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.to_top(&mut fixture.env(), Some(5)));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 4");
        assert!(cursor.to_bottom(&mut fixture.env(), Some(2)));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 1");
        cursor.to_top(&mut fixture.env(), Some(0));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 0");
        cursor.to_top(&mut fixture.env(), Some(999));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 19");
    }

    #[test]
    fn should_align_the_focused_block_when_pressing_zt_zz_zb() {
        let mut fixture = Fixture::new(&paragraphs(60), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 25);
        let stop = cursor.stop(&fixture.layout).unwrap();

        assert!(cursor.align(&mut fixture.env(), Align::Top));
        assert!((fixture.scroll - (stop.rect.y - ALIGN_MARGIN)).abs() < 1.0);
        assert!(cursor.align(&mut fixture.env(), Align::Center));
        let middle = stop.rect.y + stop.rect.h / 2.0 - fixture.scroll;
        assert!((middle - 150.0).abs() < 1.0, "{middle}");
        assert!(cursor.align(&mut fixture.env(), Align::Bottom));
        let gap = fixture.scroll + 300.0 - stop.rect.bottom();
        assert!((gap - ALIGN_MARGIN).abs() < 1.0, "{gap}");
    }

    #[test]
    fn should_clamp_alignment_at_the_document_edges_when_the_block_is_near_them() {
        let mut fixture = Fixture::new(&paragraphs(60), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.align(&mut fixture.env(), Align::Center);
        assert_eq!(fixture.scroll, 0.0, "the first block cannot be centred");
        cursor.to_bottom(&mut fixture.env(), None);
        cursor.align(&mut fixture.env(), Align::Top);
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!((fixture.scroll - (stop.rect.y - ALIGN_MARGIN)).abs() < 1.0);
    }

    #[test]
    fn should_move_the_view_for_zt_zz_zb_when_the_cursor_is_on_the_last_lines() {
        let mut fixture = Fixture::new(&paragraphs(30), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.to_bottom(&mut fixture.env(), None);
        let stop = cursor.stop(&fixture.layout).unwrap();
        let resting = fixture.scroll;
        assert!(cursor.align(&mut fixture.env(), Align::Top));
        assert!((fixture.scroll - (stop.rect.y - ALIGN_MARGIN)).abs() < 1.0);
        assert!(fixture.scroll > resting, "the last line rises to the top");
        assert!(cursor.align(&mut fixture.env(), Align::Center));
        let middle = stop.rect.y + stop.rect.h / 2.0 - fixture.scroll;
        assert!((middle - 150.0).abs() < 1.0, "{middle}");
        assert!(cursor.align(&mut fixture.env(), Align::Bottom));
        let gap = fixture.scroll + 300.0 - stop.rect.bottom();
        assert!((gap - ALIGN_MARGIN).abs() < 1.0, "{gap}");
    }

    #[test]
    fn should_stop_scrolling_one_viewport_below_the_last_block_when_paging_far() {
        let mut fixture = Fixture::new(&paragraphs(30), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        for _ in 0..200 {
            cursor.half_page(&mut fixture.env(), 1, 0.5);
        }
        assert_eq!(fixture.scroll, fixture.layout.height);
        assert!(fixture.scroll > fixture.layout.blocks.last().unwrap().rect.bottom());
    }

    #[test]
    fn should_scroll_by_the_fraction_and_keep_the_cursor_when_it_stays_visible() {
        let mut fixture = Fixture::new(&"x\n\n".repeat(200), 400.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 8);
        let before = cursor.src.clone();
        let from = fixture.scroll;
        assert!(cursor.half_page(&mut fixture.env(), 1, 0.25));
        assert!((fixture.scroll - (from + 100.0)).abs() < 30.0);
        assert_ne!(cursor.src, before, "the cursor follows the new centre");
    }

    #[test]
    fn should_pull_the_cursor_into_view_when_a_half_page_scroll_passes_it() {
        let mut fixture = Fixture::new(&paragraphs(200), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.half_page(&mut fixture.env(), 6, 1.0));
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(stop.rect.bottom() > fixture.scroll && stop.rect.y < fixture.scroll + 300.0);
        assert!(cursor.half_page(&mut fixture.env(), -6, 1.0));
        let stop = cursor.stop(&fixture.layout).unwrap();
        assert!(stop.rect.bottom() > fixture.scroll && stop.rect.y < fixture.scroll + 300.0);
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_not_move_the_cursor_when_half_paging_on_a_single_stop_page() {
        let mut fixture = Fixture::new("only\n", 400.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        let before = cursor.src.clone();
        cursor.half_page(&mut fixture.env(), 1, 0.5);
        assert_eq!(cursor.src, before);
        cursor.half_page(&mut fixture.env(), -1, 0.5);
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_centre_the_stop_at_the_new_centre_when_half_paging() {
        let mut fixture = Fixture::new(&paragraphs(100), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.half_page(&mut fixture.env(), 1, 0.5));
        let stop = cursor.stop(&fixture.layout).unwrap();
        let middle = stop.rect.y + stop.rect.h / 2.0 - fixture.scroll;
        assert!((middle - 150.0).abs() < 1.0, "{middle}");
    }

    #[test]
    fn should_scroll_plainly_when_the_stop_at_the_centre_is_taller_than_the_window() {
        let tall = "line\n\n".to_string() + &"tall\nrow\n".repeat(60) + "\n\nlast\n";
        let mut fixture = Fixture::new(&tall, 100.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.half_page(&mut fixture.env(), 1, 0.5));
        assert_eq!(fixture.scroll, 50.0);
    }

    #[test]
    fn should_scroll_plainly_when_centring_would_jump_more_than_a_viewport() {
        let mut fixture = Fixture::new(&paragraphs(200), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.half_page(&mut fixture.env(), 20, 0.5);
        assert_eq!(fixture.scroll, 3000.0);
    }

    #[test]
    fn should_fall_back_to_half_when_the_fraction_is_nonsense() {
        for bad in [f32::NAN, -1.0, 0.0, f32::INFINITY] {
            let mut fixture = Fixture::new(&paragraphs(100), 400.0);
            let mut cursor = Cursor::default();
            cursor.init(&mut fixture.env());
            cursor.half_page(&mut fixture.env(), 1, bad);
            assert!((fixture.scroll - 200.0).abs() < 30.0, "{bad}");
        }
    }

    fn sectioned() -> String {
        "# One\n\nalpha\n\n## Two\n\nbeta\n\ngamma\n\n# Three\n\ndelta\n".to_owned()
    }

    #[test]
    fn should_land_on_the_heading_of_the_next_section_when_pressing_close_brace() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert_eq!(focused_text(&fixture, &cursor), "# One");
        assert!(cursor.section(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "## Two");
        assert!(cursor.section(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "# Three");
    }

    #[test]
    fn should_go_back_a_section_when_pressing_open_brace() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.to_bottom(&mut fixture.env(), None);
        assert!(cursor.section(&mut fixture.env(), -1));
        assert_eq!(focused_text(&fixture, &cursor), "## Two");
        assert!(cursor.section(&mut fixture.env(), -1));
        assert_eq!(focused_text(&fixture, &cursor), "# One");
    }

    #[test]
    fn should_go_back_to_the_section_heading_when_focus_is_inside_the_section() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 3);
        assert_eq!(focused_text(&fixture, &cursor), "beta");
        assert!(cursor.section(&mut fixture.env(), -1));
        assert_eq!(focused_text(&fixture, &cursor), "# One");
    }

    #[test]
    fn should_put_the_heading_at_the_top_of_the_window_when_jumping_sections() {
        let mut fixture = Fixture::new(&sectioned(), 120.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 1);
        let heads = heading_blocks(&fixture.doc);
        let head = fixture.layout.blocks[heads[1]].rect;
        assert!((fixture.scroll - (head.y - 24.0)).abs() < 1.0);
    }

    #[test]
    fn should_go_to_the_ends_when_stepping_past_the_first_or_last_section() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 50);
        assert_eq!(focused_text(&fixture, &cursor), "delta");
        assert_eq!(Some(fixture.scroll), fixture.env().max_scroll());
        cursor.section(&mut fixture.env(), -50);
        assert_eq!(focused_text(&fixture, &cursor), "# One");
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_go_to_the_document_end_when_jumping_past_the_last_section() {
        let long = format!("# One\n\n{}## Two\n\nlast\n", paragraphs(30));
        let mut fixture = Fixture::new(&long, 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 1);
        assert_eq!(focused_text(&fixture, &cursor), "## Two");
        assert!(cursor.section(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "last");
        assert_eq!(Some(fixture.scroll), fixture.env().max_scroll());
    }

    #[test]
    fn should_do_nothing_when_jumping_sections_in_a_document_with_no_headings() {
        let mut fixture = Fixture::new("a\n\nb\n\nc\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        let before = cursor.clone();
        assert!(!cursor.section(&mut fixture.env(), 1));
        assert_eq!(cursor, before);
        assert!(!cursor.section(&mut fixture.env(), -1));
        assert_eq!(cursor, before);
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_keep_the_cursor_on_its_source_when_the_document_is_laid_out_again() {
        let mut fixture = Fixture::new(&paragraphs(10), 400.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 4);
        let remembered = focused_text(&fixture, &cursor);
        let theme = Theme::dark();
        fixture.layout = lay_out(&fixture.doc, 500.0, &theme, 2.0, &mut fixture.fonts);
        assert_eq!(focused_text(&fixture, &cursor), remembered);
    }

    #[test]
    fn should_find_the_next_stop_when_the_remembered_source_vanished() {
        let mut fixture = Fixture::new("one\n\ntwo\n\nthree\n", 400.0);
        let cursor = Cursor { src: Some(5..6) };
        let stops = collect_stops(&fixture.layout);
        let at = cursor.locate(&stops).unwrap();
        assert!(stops[at].src.start >= 5);
        let beyond = Cursor {
            src: Some(900..901),
        };
        assert_eq!(beyond.locate(&stops), Some(stops.len() - 1));
        assert_eq!(Cursor::default().locate(&stops), None);
        assert_eq!(cursor.locate(&[]), None);
        let _ = &mut fixture;
    }

    #[test]
    fn should_move_the_cursor_to_a_block_without_scrolling_when_going_to_it() {
        let mut fixture = Fixture::new(&paragraphs(30), 300.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(cursor.go_to_block(&mut fixture.env(), 12));
        assert_eq!(focused_text(&fixture, &cursor), "paragraph 12");
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_survive_a_zero_height_viewport_and_nonfinite_scrolls() {
        let mut fixture = Fixture::new(&paragraphs(5), 0.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.move_by(&mut fixture.env(), 2);
        cursor.align(&mut fixture.env(), Align::Center);
        cursor.half_page(&mut fixture.env(), 1, 0.5);
        assert!(!fixture.env().set_scroll(f32::NAN) || fixture.scroll == 0.0);
        fixture.env().set_scroll(f32::INFINITY);
        assert!(fixture.scroll.is_finite());
    }

    #[test]
    fn should_band_the_column_minus_the_inset_when_painting() {
        let fixture = Fixture::new("para\n", 300.0);
        let rect = fixture.layout.blocks[0].rect;
        let band = band_rect(&fixture.layout, rect, 10.0);
        assert_eq!(band.x, fixture.layout.column_x + BLEED_INSET);
        assert_eq!(band.w, fixture.layout.column_width - 2.0 * BLEED_INSET);
        assert_eq!(band.y, rect.y - 10.0);
        assert_eq!(band.h, rect.h);
    }

    fn kinds(markdown: &str) -> Vec<StopKind> {
        let fixture = Fixture::new(markdown, 400.0);
        collect_stops(&fixture.layout)
            .iter()
            .map(|stop| stop.kind)
            .collect()
    }

    #[test]
    fn should_classify_stops_by_how_they_show_focus_when_collecting() {
        let all = kinds(
            "## Title\n\ntext\n\n```rust\nlet a = 1;\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n## Questions\n\n1. Which?\n",
        );
        assert_eq!(all[0], StopKind::Heading);
        assert_eq!(all[1], StopKind::Text);
        assert_eq!(all[2], StopKind::Special, "code");
        assert_eq!(all[3], StopKind::Special, "table");
        assert!(all.contains(&StopKind::Question));
    }

    #[test]
    fn should_classify_a_heading_block_as_a_heading_when_it_is_laid_out() {
        let fixture = Fixture::new("## Title\n\ntext\n", 400.0);
        assert_eq!(StopKind::of(&fixture.layout.blocks[0]), StopKind::Heading);
        assert_eq!(StopKind::of(&fixture.layout.blocks[1]), StopKind::Text);
    }

    #[test]
    fn should_band_a_display_equation_when_it_is_a_stop() {
        let all = kinds("$$\\frac{a}{b}$$\n\ntext\n");
        assert!(!all.contains(&StopKind::Special), "{all:?}");
    }
}
