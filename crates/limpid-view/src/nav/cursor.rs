//! The block cursor: one stop is focused at a time, the viewport follows only
//! when the stop would otherwise be off screen, and section and alignment
//! moves put it where the key table says.

use crate::{
    layout::{LaidBlock, Layout},
    paint::{Canvas, Rect},
    text::Fonts,
    theme::Theme,
};
use limpid_core::doc::{BlockKind, Doc};
use std::ops::Range;

/// Distance kept between a revealed stop and the window edge, CSS px.
pub const REVEAL_MARGIN: f32 = 12.0;
/// How far the focus band reaches past the text column, CSS px.
pub const BLEED_INSET: f32 = 10.0;
/// Width of the accent bar on the band's left edge, CSS px.
const BAR_WIDTH: f32 = 3.0;

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
        self.layout
            .is_complete()
            .then(|| (self.layout.height - self.viewport).max(0.0))
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
        let margin = env.margin();
        let target = match align {
            Align::Top => stop.rect.y - margin,
            Align::Center => stop.rect.y - (env.viewport - stop.rect.h) / 2.0,
            Align::Bottom => stop.rect.bottom() - env.viewport + margin,
        };
        env.set_scroll(target)
    }

    /// Scroll `steps` times `fraction` of the viewport. The cursor stays where
    /// it is while it is still visible, else it follows to the stop nearest
    /// the edge it was pushed past.
    pub fn half_page(&mut self, env: &mut Env, steps: i64, fraction: f32) -> bool {
        let fraction = if fraction.is_finite() && fraction > 0.0 {
            fraction.min(1.0)
        } else {
            0.5
        };
        let delta = steps as f32 * env.viewport * fraction;
        let before = *env.scroll;
        let scrolled = env.set_scroll(before + delta);
        let (top, bottom) = (*env.scroll, *env.scroll + env.viewport);
        let stops = collect_stops(env.layout);
        let visible = |stop: &Stop| stop.rect.bottom() > top && stop.rect.y < bottom;
        let on_screen = self.locate(&stops).is_some_and(|at| visible(&stops[at]));
        if on_screen {
            return scrolled;
        }
        let pick = if steps >= 0 {
            stops.iter().find(|stop| visible(stop))
        } else {
            stops.iter().rev().find(|stop| visible(stop))
        };
        let Some(stop) = pick else {
            return scrolled;
        };
        let moved = self.src.as_ref() != Some(&stop.src);
        self.src = Some(stop.src.clone());
        scrolled || moved
    }

    /// `{` and `}`: step `steps` sections (negative goes back). The cursor
    /// lands on the section's first stop and its heading sits at the top of the
    /// window.
    pub fn section(&mut self, env: &mut Env, steps: i64) -> bool {
        let heads = heading_blocks(env.doc);
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
                self.to_bottom(env, None)
            };
        }
        let head = heads[target as usize];
        env.lay_out_through(head);
        let Some(head_rect) = env.layout.blocks.get(head).map(|block| block.rect) else {
            return false;
        };
        let scrolled = env.set_scroll(head_rect.y - env.margin());
        let first_stop = loop {
            let stops = collect_stops(env.layout);
            if let Some(stop) = stops.into_iter().find(|stop| stop.top >= head) {
                break Some(stop);
            }
            if !env.lay_out_more() {
                break None;
            }
        };
        let Some(stop) = first_stop else {
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

/// The rectangle a band covers for `rect`: the whole column plus the bleed,
/// in window coordinates.
pub fn band_rect(layout: &Layout, rect: Rect, scroll: f32) -> Rect {
    let bleed = BLEED_INSET * layout.scale();
    Rect::new(
        layout.column_x - bleed,
        rect.y - scroll,
        layout.column_width + 2.0 * bleed,
        rect.h,
    )
}

/// Paint the band behind `rect` (document coordinates) in `color`, with an
/// accent bar down its left edge so it still shows behind opaque blocks.
pub fn paint_band(
    canvas: &mut Canvas,
    layout: &Layout,
    theme: &Theme,
    rect: Rect,
    scroll: f32,
    tint: crate::theme::Color,
    bar: bool,
) {
    let band = band_rect(layout, rect, scroll);
    canvas.fill_rect(band, tint);
    if bar {
        let width = (BAR_WIDTH * layout.scale()).round().max(1.0);
        canvas.fill_rect(Rect::new(band.x, band.y, width, band.h), theme.teal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::lay_out;
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
        assert_eq!(focused_text(&fixture, &cursor), "first");
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
        let mut fixture = Fixture::new("# Only a heading\n\n---\n", 300.0);
        let mut cursor = Cursor::default();
        assert!(!cursor.init(&mut fixture.env()));
        assert!(!cursor.move_by(&mut fixture.env(), 1));
        assert!(!cursor.to_bottom(&mut fixture.env(), None));
        assert!(!cursor.align(&mut fixture.env(), Align::Center));
        assert_eq!(cursor.src, None);
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
    fn should_skip_headings_and_rules_when_moving() {
        let mut fixture = Fixture::new("a\n\n# H\n\n---\n\nb\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
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
        let max = fixture.layout.height - 300.0;
        assert_eq!(fixture.scroll, max);
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
        assert!((fixture.scroll - (stop.rect.y - REVEAL_MARGIN)).abs() < 1.0);
        assert!(cursor.align(&mut fixture.env(), Align::Center));
        let middle = stop.rect.y + stop.rect.h / 2.0 - fixture.scroll;
        assert!((middle - 150.0).abs() < 1.0, "{middle}");
        assert!(cursor.align(&mut fixture.env(), Align::Bottom));
        let gap = fixture.scroll + 300.0 - stop.rect.bottom();
        assert!((gap - REVEAL_MARGIN).abs() < 1.0, "{gap}");
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
        assert_eq!(fixture.scroll, fixture.layout.height - 300.0);
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
        assert_eq!(fixture.scroll, from + 100.0);
        assert_eq!(cursor.src, before, "still on screen");
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
    fn should_not_report_a_change_when_half_paging_at_the_top_of_a_short_page() {
        let mut fixture = Fixture::new("only\n", 400.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert!(!cursor.half_page(&mut fixture.env(), 1, 0.5));
        assert!(!cursor.half_page(&mut fixture.env(), -1, 0.5));
        assert_eq!(fixture.scroll, 0.0);
    }

    #[test]
    fn should_fall_back_to_half_when_the_fraction_is_nonsense() {
        for bad in [f32::NAN, -1.0, 0.0, f32::INFINITY] {
            let mut fixture = Fixture::new(&paragraphs(100), 400.0);
            let mut cursor = Cursor::default();
            cursor.init(&mut fixture.env());
            cursor.half_page(&mut fixture.env(), 1, bad);
            assert_eq!(fixture.scroll, 200.0, "{bad}");
        }
    }

    fn sectioned() -> String {
        "# One\n\nalpha\n\n## Two\n\nbeta\n\ngamma\n\n# Three\n\ndelta\n".to_owned()
    }

    #[test]
    fn should_land_on_the_first_stop_of_the_next_section_when_pressing_close_brace() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        assert_eq!(focused_text(&fixture, &cursor), "alpha");
        assert!(cursor.section(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "beta");
        assert!(cursor.section(&mut fixture.env(), 1));
        assert_eq!(focused_text(&fixture, &cursor), "delta");
    }

    #[test]
    fn should_go_back_a_section_when_pressing_open_brace() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.to_bottom(&mut fixture.env(), None);
        assert!(cursor.section(&mut fixture.env(), -1));
        assert_eq!(focused_text(&fixture, &cursor), "beta");
        assert!(cursor.section(&mut fixture.env(), -1));
        assert_eq!(focused_text(&fixture, &cursor), "alpha");
    }

    #[test]
    fn should_put_the_heading_at_the_top_of_the_window_when_jumping_sections() {
        let mut fixture = Fixture::new(&sectioned(), 120.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 1);
        let heads = heading_blocks(&fixture.doc);
        let head = fixture.layout.blocks[heads[1]].rect;
        assert!(
            (fixture.scroll - (head.y - REVEAL_MARGIN).min(fixture.layout.height - 120.0)).abs()
                < 1.0
        );
    }

    #[test]
    fn should_go_to_the_ends_when_stepping_past_the_first_or_last_section() {
        let mut fixture = Fixture::new(&sectioned(), 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 50);
        assert_eq!(focused_text(&fixture, &cursor), "delta");
        cursor.section(&mut fixture.env(), -50);
        assert_eq!(focused_text(&fixture, &cursor), "alpha");
    }

    #[test]
    fn should_jump_to_the_ends_when_the_document_has_no_headings() {
        let mut fixture = Fixture::new("a\n\nb\n\nc\n", 600.0);
        let mut cursor = Cursor::default();
        cursor.init(&mut fixture.env());
        cursor.section(&mut fixture.env(), 1);
        assert_eq!(focused_text(&fixture, &cursor), "c");
        cursor.section(&mut fixture.env(), -1);
        assert_eq!(focused_text(&fixture, &cursor), "a");
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
    fn should_band_the_whole_column_plus_the_bleed_when_painting() {
        let fixture = Fixture::new("para\n", 300.0);
        let rect = fixture.layout.blocks[0].rect;
        let band = band_rect(&fixture.layout, rect, 10.0);
        assert_eq!(band.x, fixture.layout.column_x - BLEED_INSET);
        assert_eq!(band.w, fixture.layout.column_width + 2.0 * BLEED_INSET);
        assert_eq!(band.y, rect.y - 10.0);
        assert_eq!(band.h, rect.h);
    }
}
