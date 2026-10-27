//! Block layout for a [`Doc`]: vertical stacking with the margins of
//! `page.css`, lazy so that only the blocks near the viewport are shaped.
//!
//! All geometry is in device pixels. A block's `rect` is relative to the left
//! edge of the text column (`x`) and the top of the document (`y`); painting
//! adds the column offset and subtracts the scroll.

pub mod block;
pub mod boxes;
pub mod code;
pub mod field;
pub mod image;
pub mod inline;
pub mod list;
pub mod note;
pub mod quote;
pub mod rail;
pub mod svg;
pub mod table;

use crate::{
    paint::{Canvas, Rect},
    text::Fonts,
    theme::Theme,
};
use block::{Cx, Flow};
use boxes::BoxEnv;
use inline::RichText;
use limpid_core::{doc::Doc, settings::RailMode};
use rail::RailLayout;
use std::rc::Rc;

/// Layout choices that are not part of the [`Theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Wrap long code lines instead of scrolling the block.
    pub code_wrap: bool,
    pub rail: RailMode,
    /// Show the compact contents block expanded.
    pub contents_open: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            code_wrap: false,
            rail: RailMode::Auto,
            contents_open: false,
        }
    }
}

/// What a block that later work fills in is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Table,
    Image,
    Svg,
    Math,
    Mermaid,
    Field,
    Annotation,
}

/// A block drawn as a plain box of source text until its real rendering
/// exists.
pub struct PendingBox {
    pub kind: PendingKind,
    pub text: RichText,
    /// Padding inside the box, device pixels.
    pub padding: f32,
    /// Padding above and below the text, device pixels.
    pub pad_y: f32,
    /// Corner radius, device pixels.
    pub radius: f32,
    /// Border colour instead of `rule`.
    pub border: Option<crate::theme::Color>,
    /// The math or Mermaid hash the box waits for.
    pub hash: Option<String>,
    /// Background instead of `surface` (math source uses `code_bg`).
    pub fill: Option<crate::theme::Color>,
    /// The paragraphs and list items of a comment or reply body, which
    /// `text` leaves empty.
    pub parts: Vec<NotePart>,
}

/// One shaped piece of a note body, placed from the box's text origin.
pub struct NotePart {
    pub text: RichText,
    pub dx: f32,
    pub dy: f32,
}

/// How a list item is marked.
pub enum Marker {
    Text(Box<RichText>),
    Task { checked: bool },
}

// Blocks live in a `Vec`; boxing the big variants would add a pointer chase
// to every paint for no saving worth having.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    /// A paragraph or other run of inline text.
    Text(RichText),
    Heading {
        text: RichText,
        level: u8,
        /// A rule above the text (`h2`).
        rule: bool,
        /// Space between the rule and the text.
        pad_top: f32,
    },
    Code {
        text: inline::RichText,
        /// Left inset of the text inside the block.
        gutter: f32,
        scroll_x: f32,
        /// The text is wider than the block and scrolls sideways.
        overflow: bool,
        /// Widest line, for clamping `scroll_x`.
        text_width: f32,
        caption: Option<RichText>,
        caption_height: f32,
        pad_y: f32,
        border: f32,
    },
    Rule,
    Children(Vec<LaidBlock>),
    Item {
        marker: Marker,
        children: Vec<LaidBlock>,
    },
    Quote {
        bar: crate::theme::Color,
        tint: crate::theme::Color,
        title: Option<RichText>,
        /// Distance from the left edge of the block to its children.
        inset: f32,
        pad_y: f32,
        children: Vec<LaidBlock>,
    },
    Pending(PendingBox),
    Table(table::TableBox),
    /// A pixmap: an image, SVG, typeset display math or a Mermaid diagram.
    Media(boxes::MediaBox),
}

pub struct LaidBlock {
    /// Index in the containing block list (`Doc::blocks` at the top level).
    pub index: usize,
    /// Byte range in `Doc::source`.
    pub src: std::ops::Range<usize>,
    pub rect: Rect,
    /// The cursor can rest here.
    pub stop: bool,
    pub content: Content,
    pub depth: u8,
}

/// Index of the first block that may reach below document y `top`. A nested
/// reply ends above its question box, so bottoms are not sorted; step back to
/// the box when the search lands just after a reply.
pub(crate) fn first_visible(blocks: &[LaidBlock], top: f32) -> usize {
    let mut start = blocks.partition_point(|block| block.rect.bottom() < top);
    while let [.., outer, inner] = &blocks[..start]
        && inner.rect.y >= outer.rect.y
        && inner.rect.bottom() <= outer.rect.bottom()
    {
        start -= 2;
    }
    start
}

impl LaidBlock {
    pub fn children(&self) -> &[LaidBlock] {
        match &self.content {
            Content::Children(children)
            | Content::Item { children, .. }
            | Content::Quote { children, .. } => children,
            _ => &[],
        }
    }

    /// Scroll a wide table sideways. Returns whether it moved.
    pub fn scroll_table_x(&mut self, delta: f32) -> bool {
        let rect_width = self.rect.w;
        let Content::Table(table) = &mut self.content else {
            return false;
        };
        if !table.overflow {
            return false;
        }
        let max = table.max_scroll(rect_width);
        let target = (table.scroll_x + delta).clamp(0.0, max);
        let moved = (target - table.scroll_x).abs() > f32::EPSILON;
        table.scroll_x = target;
        moved
    }

    /// Scroll a wide code block sideways. Returns whether it moved.
    pub fn scroll_code_x(&mut self, delta: f32) -> bool {
        let rect_width = self.rect.w;
        let Content::Code {
            scroll_x,
            overflow: true,
            text_width,
            gutter,
            border,
            ..
        } = &mut self.content
        else {
            return false;
        };
        let visible = rect_width - 2.0 * *border - 2.0 * *gutter;
        let max = (*text_width - visible).max(0.0);
        let target = (*scroll_x + delta).clamp(0.0, max);
        let moved = (target - *scroll_x).abs() > f32::EPSILON;
        *scroll_x = target;
        moved
    }
}

/// Where the rail and the text column sit in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// Width of the rail, `0.0` when there is none.
    pub rail_width: f32,
    /// Absolute x of the text column.
    pub column_x: f32,
    pub column_width: f32,
    /// From the page edge to the text column, device pixels: the page's side
    /// padding, which the focus band and callouts reach into.
    pub gutter: f32,
}

struct State {
    theme: Rc<Theme>,
    scale: f32,
    options: LayoutOptions,
    env: Rc<BoxEnv>,
    viewport_css_width: f32,
    /// Next `Doc::blocks` index to lay out.
    next: usize,
    /// Bottom margin of the last laid block, CSS px.
    prev_margin: f32,
    /// Bottom margin of every laid top-level block, CSS px.
    margins: Vec<f32>,
    /// Bottom edge of the last laid block.
    bottom: f32,
    /// Where the first block starts.
    first_top: f32,
    complete: bool,
}

pub struct Layout {
    pub blocks: Vec<LaidBlock>,
    /// Document height laid out so far; final once [`Layout::is_complete`].
    pub height: f32,
    pub rail: Option<RailLayout>,
    /// The contents block under the page top when the window is too narrow for the rail.
    pub compact: Option<rail::Compact>,
    /// What decides the active rail entry.
    pub rail_follow: rail::RailFollow,
    pub column_x: f32,
    pub column_width: f32,
    /// See [`Geometry::gutter`].
    pub gutter: f32,
    /// The heading (by source range) whose text paints teal.
    pub heading_tint: Option<std::ops::Range<usize>>,
    state: State,
}

/// Distance from the top of the page to the first block, CSS px.
const PAGE_TOP_PAD: f32 = 22.0;
/// Space below the last block, CSS px.
const PAGE_BOTTOM_PAD: f32 = 120.0;
/// How far below the origin the first layout reaches, CSS px.
const INITIAL_COVERAGE: f32 = 2400.0;

pub fn lay_out(doc: &Doc, width: f32, theme: &Theme, scale: f32, fonts: &mut Fonts) -> Layout {
    lay_out_with(doc, width, theme, scale, fonts, LayoutOptions::default())
}

/// [`lay_out`] with the layout choices from settings. `width` is the window
/// width in device pixels.
pub fn lay_out_with(
    doc: &Doc,
    width: f32,
    theme: &Theme,
    scale: f32,
    fonts: &mut Fonts,
    options: LayoutOptions,
) -> Layout {
    lay_out_in(
        doc,
        width,
        theme,
        scale,
        fonts,
        options,
        boxes::default_env(),
    )
}

/// [`lay_out_with`] with an explicit environment for images, SVG, math and
/// Mermaid instead of the process default.
pub fn lay_out_in(
    doc: &Doc,
    width: f32,
    theme: &Theme,
    scale: f32,
    fonts: &mut Fonts,
    options: LayoutOptions,
    env: BoxEnv,
) -> Layout {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let width = if width.is_finite() {
        width.max(1.0)
    } else {
        1.0
    };
    let theme = Rc::new(theme.clone());
    let geometry = rail::geometry(width, &theme, scale, options.rail, !doc.toc.is_empty());
    let rail = (geometry.rail_width > 0.0)
        .then(|| rail::build(doc, geometry.rail_width, &theme, scale, fonts));
    let compact = (geometry.rail_width == 0.0 && width / scale < rail::COMPACT_BELOW)
        .then(|| {
            rail::build_compact(
                doc,
                geometry.column_width,
                &theme,
                scale,
                options.contents_open,
                fonts,
            )
        })
        .flatten();
    let top = PAGE_TOP_PAD * scale + compact.as_ref().map_or(0.0, |compact| compact.height);
    let mut layout = Layout {
        blocks: Vec::new(),
        height: top,
        rail,
        compact,
        rail_follow: rail::RailFollow::default(),
        column_x: geometry.column_x,
        column_width: geometry.column_width,
        gutter: geometry.gutter,
        heading_tint: None,
        state: State {
            theme,
            scale,
            options,
            env: Rc::new(env),
            viewport_css_width: width / scale,
            next: 0,
            prev_margin: 0.0,
            margins: Vec::new(),
            bottom: top,
            first_top: top,
            complete: false,
        },
    };
    if doc.blocks.is_empty() {
        layout.finish();
        return layout;
    }
    layout.ensure_to(INITIAL_COVERAGE * scale, doc, fonts);
    layout
}

impl Layout {
    pub fn theme(&self) -> &Theme {
        &self.state.theme
    }

    pub fn scale(&self) -> f32 {
        self.state.scale
    }

    pub fn options(&self) -> LayoutOptions {
        self.state.options
    }

    /// Every block of the document has been laid out.
    pub fn is_complete(&self) -> bool {
        self.state.complete
    }

    /// The bottom edge of what has been laid out so far.
    pub fn covered(&self) -> f32 {
        self.state.bottom
    }

    /// Lay out further blocks until the document is covered down to `y`.
    pub fn ensure_to(&mut self, y: f32, doc: &Doc, fonts: &mut Fonts) {
        while !self.state.complete && self.state.bottom < y {
            self.lay_next(doc, fonts);
        }
    }

    /// Lay out through top-level block `index`.
    pub fn ensure_block(&mut self, index: usize, doc: &Doc, fonts: &mut Fonts) {
        while !self.state.complete && self.state.next <= index {
            self.lay_next(doc, fonts);
        }
    }

    fn lay_next(&mut self, doc: &Doc, fonts: &mut Fonts) {
        let Some(block) = doc.blocks.get(self.state.next) else {
            self.finish();
            return;
        };
        let theme = self.state.theme.clone();
        let scale = self.state.scale;
        let options = self.state.options;
        let env = self.state.env.clone();
        let flow = Flow {
            x: 0.0,
            width: self.column_width,
            ink: theme.ink,
            depth: 0,
            compact: false,
            top_level: true,
        };
        let (margin_top, margin_bottom) = block::margins(&block.kind, flow);
        let first = self.blocks.is_empty();
        let gap = if first {
            0.0
        } else {
            self.state.prev_margin.max(margin_top) * scale
        };
        let mut cx = Cx {
            theme: &theme,
            scale,
            options: &options,
            boxes: &env,
            source: &doc.source,
            fonts,
            viewport_css_width: self.state.viewport_css_width,
            column_width: self.column_width,
            gutter: self.gutter,
        };
        let nested = match self.blocks.last_mut() {
            Some(field) if field::answers(doc, self.state.next, field) => {
                field::nest_reply(&mut cx, block, field, self.state.next)
            }
            _ => None,
        };
        // A reply sits inside its question box, which has grown to hold it.
        let inside = nested.is_some();
        let laid = nested.unwrap_or_else(|| {
            block::lay_block(
                &mut cx,
                block,
                self.state.next,
                self.state.bottom + gap,
                flow,
            )
        });
        self.state.bottom = match self.blocks.last() {
            Some(field) if inside => field.rect.bottom(),
            _ => laid.rect.bottom(),
        };
        self.state.prev_margin = margin_bottom;
        self.state.margins.push(margin_bottom);
        self.state.next += 1;
        self.blocks.push(laid);
        self.height = self.state.bottom;
        if self.state.next >= doc.blocks.len() {
            self.finish();
            return;
        }
        // Lay a question's reply with it, so the box never grows later.
        if let Some(field) = self.blocks.last()
            && field::answers(doc, self.state.next, field)
        {
            self.lay_next(doc, fonts);
        }
    }

    fn finish(&mut self) {
        self.state.complete = true;
        self.height = self.state.bottom + PAGE_BOTTOM_PAD * self.state.scale;
    }

    /// The furthest the view can scroll once the document is laid out: the
    /// last block, the page padding and one viewport of empty scroll space
    /// below, so the last lines can reach the top of the window.
    pub fn max_scroll(&self) -> Option<f32> {
        self.is_complete().then_some(self.height)
    }

    /// The top-level block containing document y, else the one just above
    /// it. `None` above the first block or when nothing is laid out.
    pub fn block_at(&self, y: f32) -> Option<usize> {
        let after = self.blocks.partition_point(|block| block.rect.y <= y);
        after.checked_sub(1)
    }

    /// Every stop in document order, descending into containers.
    pub fn stops(&self) -> impl Iterator<Item = &LaidBlock> {
        Stops {
            stack: vec![self.blocks.iter()],
        }
    }

    /// The first top-level code block that overlaps `top..bottom`.
    pub fn code_block_in(&mut self, top: f32, bottom: f32) -> Option<&mut LaidBlock> {
        let start = first_visible(&self.blocks, top);
        self.blocks[start..]
            .iter_mut()
            .take_while(|block| block.rect.y <= bottom)
            .find(|block| matches!(block.content, Content::Code { overflow: true, .. }))
    }

    /// The first top-level table that overlaps `top..bottom` and scrolls.
    pub fn table_block_in(&mut self, top: f32, bottom: f32) -> Option<&mut LaidBlock> {
        let start = first_visible(&self.blocks, top);
        self.blocks[start..]
            .iter_mut()
            .take_while(|block| block.rect.y <= bottom)
            .find(|block| matches!(block.content, Content::Table(ref table) if table.overflow))
    }

    /// Math or Mermaid output for `hashes` has reached the cache: forget the
    /// layout from the first block that shows one of them, so that
    /// [`Layout::ensure_to`] draws it again. Returns whether anything changed;
    /// the caller then calls `ensure_to` and repaints.
    pub fn refresh_pending(&mut self, hashes: &[String]) -> bool {
        if hashes.is_empty() {
            return false;
        }
        let Some(first) = self
            .blocks
            .iter()
            .position(|block| boxes::mentions(block, hashes))
        else {
            return false;
        };
        self.blocks.truncate(first);
        self.state.margins.truncate(first);
        self.state.next = first;
        self.state.bottom = match first.checked_sub(1) {
            // A reply ends inside its question box; the flow resumes below the box.
            Some(before) if self.nested_in_previous(before) => {
                self.blocks[before - 1].rect.bottom()
            }
            Some(before) => self.blocks[before].rect.bottom(),
            None => self.state.first_top,
        };
        self.state.prev_margin = self.state.margins.last().copied().unwrap_or(0.0);
        self.state.complete = false;
        self.height = self.state.bottom;
        true
    }

    /// Whether top-level block `index` is a reply laid inside the question box
    /// before it: it ends above that box's bottom.
    fn nested_in_previous(&self, index: usize) -> bool {
        index.checked_sub(1).is_some_and(|outer| {
            self.blocks[index].rect.bottom() < self.blocks[outer].rect.bottom()
        })
    }

    /// Index of the rail entry that follows document y.
    pub fn rail_active(&self, y: f32) -> Option<usize> {
        let rail = self.rail.as_ref()?;
        rail.active_for(self.block_at(y).unwrap_or(0))
    }

    /// Paint the rail and the visible blocks. `scroll` is the document y at
    /// the top of the canvas.
    pub fn paint(&mut self, canvas: &mut Canvas, fonts: &mut Fonts, scroll: f32) {
        let viewport = canvas.height() as f32;
        let start = first_visible(&self.blocks, scroll);
        let paint = PaintCx {
            theme: &self.state.theme,
            scale: self.state.scale,
            origin_x: self.column_x,
            origin_y: -scroll,
            viewport,
            heading_tint: self.heading_tint.as_ref(),
        };
        for block in &self.blocks[start..] {
            if block.rect.y - scroll > viewport {
                break;
            }
            paint_block(&paint, canvas, fonts, block);
        }
        let active = match self.rail_follow {
            rail::RailFollow::Block(index) => self.rail.as_ref().and_then(|rail| {
                // A block above every heading belongs to the first section.
                rail.active_for(index)
                    .or_else(|| (!rail.entries.is_empty()).then_some(0))
            }),
            rail::RailFollow::Scroll { viewport } => {
                let viewport = if viewport > 0.0 {
                    viewport
                } else {
                    canvas.height() as f32
                };
                self.rail_spy(scroll, viewport)
            }
        };
        let theme = self.state.theme.clone();
        let scale = self.state.scale;
        if let Some(compact) = &mut self.compact {
            let y = PAGE_TOP_PAD * scale - scroll;
            compact.paint(canvas, fonts, &theme, scale, self.column_x, y, active);
        }
        if let Some(rail) = &mut self.rail {
            rail.paint(canvas, fonts, &theme, scale, active);
        }
    }

    /// Index of the top-level block that holds source offset `start`.
    pub fn block_for_src(&self, start: usize) -> Option<usize> {
        let after = self
            .blocks
            .partition_point(|block| block.src.start <= start);
        after.checked_sub(1)
    }

    /// The rail entry the scroll position selects: the first at the page top,
    /// the last at the bottom, else the last heading above 90 CSS px.
    pub fn rail_spy(&self, scroll: f32, viewport: f32) -> Option<usize> {
        let rail = self.rail.as_ref()?;
        let scale = self.state.scale;
        if rail.entries.is_empty() {
            return None;
        }
        if scroll <= scale {
            return Some(0);
        }
        if self.is_complete() && scroll + viewport >= self.height - scale {
            return Some(rail.entries.len() - 1);
        }
        let limit = scroll + rail::SPY_THRESHOLD * scale;
        let above = rail.entries.iter().rposition(|entry| {
            entry
                .block_index
                .and_then(|index| self.blocks.get(index))
                .is_some_and(|block| block.rect.y <= limit)
        });
        Some(above.unwrap_or(0))
    }
}

struct Stops<'a> {
    stack: Vec<std::slice::Iter<'a, LaidBlock>>,
}

impl<'a> Iterator for Stops<'a> {
    type Item = &'a LaidBlock;

    fn next(&mut self) -> Option<&'a LaidBlock> {
        loop {
            let iter = self.stack.last_mut()?;
            let Some(block) = iter.next() else {
                self.stack.pop();
                continue;
            };
            // A container's own stop comes before its children's.
            let children = block.children();
            if !children.is_empty() {
                self.stack.push(children.iter());
            }
            if block.stop {
                return Some(block);
            }
        }
    }
}

/// What painting a block needs besides the block.
pub struct PaintCx<'a> {
    pub theme: &'a Theme,
    pub scale: f32,
    /// Window x of the column's left edge.
    pub origin_x: f32,
    /// Window y of document y = 0.
    pub origin_y: f32,
    pub viewport: f32,
    /// The heading painted in teal instead of ink.
    pub heading_tint: Option<&'a std::ops::Range<usize>>,
}

impl PaintCx<'_> {
    pub fn visible(&self, rect: &Rect) -> bool {
        rect.bottom() + self.origin_y >= 0.0 && rect.y + self.origin_y <= self.viewport
    }

    /// The block's rect in window coordinates.
    pub fn window_rect(&self, rect: &Rect) -> Rect {
        Rect::new(
            rect.x + self.origin_x,
            rect.y + self.origin_y,
            rect.w,
            rect.h,
        )
    }
}

pub fn paint_block(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    if !paint.visible(&block.rect) {
        return;
    }
    let area = paint.window_rect(&block.rect);
    let theme = paint.theme;
    match &block.content {
        Content::Text(text) => {
            inline::paint(canvas, fonts, text, area.x, area.y, theme, paint.scale)
        }
        Content::Heading {
            text,
            level,
            rule,
            pad_top,
        } => {
            let tinted = paint.heading_tint == Some(&block.src);
            let ink = if tinted { theme.teal } else { theme.ink };
            let base = if *level >= 4 { theme.muted } else { theme.ink };
            let mut top = area.y;
            if *rule {
                canvas.fill_rect(
                    Rect::new(area.x, area.y, area.w, paint.scale.round().max(1.0)),
                    inline::rule_soft(theme),
                );
                top += paint.scale.round().max(1.0);
            }
            inline::paint_ink(
                canvas,
                fonts,
                text,
                area.x,
                top + pad_top,
                theme,
                paint.scale,
                ink,
                tinted.then_some(base),
            );
        }
        Content::Code { .. } => code::paint(paint, canvas, fonts, block),
        Content::Rule => canvas.fill_rect(area, theme.rule),
        Content::Children(children) => {
            for child in children {
                paint_block(paint, canvas, fonts, child);
            }
        }
        Content::Item { .. } => list::paint_item(paint, canvas, fonts, block),
        Content::Quote { .. } => quote::paint(paint, canvas, fonts, block),
        Content::Pending(pending) => block::paint_pending(paint, canvas, fonts, area, pending),
        Content::Table(_) => table::paint(paint, canvas, fonts, block),
        Content::Media(media) => boxes::paint_media(paint, canvas, area, media),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn doc(markdown: &str) -> Doc {
        parse(markdown, Mode::File, &Settings::default())
    }

    fn laid(markdown: &str, width: f32) -> (Doc, Layout, Fonts) {
        let doc = doc(markdown);
        let mut fonts = Fonts::new();
        let layout = lay_out(&doc, width, &Theme::dark(), 1.0, &mut fonts);
        (doc, layout, fonts)
    }

    #[test]
    fn should_stack_blocks_top_to_bottom_when_laying_out_paragraphs() {
        let (_, layout, _) = laid("one\n\ntwo\n\nthree\n", 900.0);
        assert_eq!(layout.blocks.len(), 3);
        for pair in layout.blocks.windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.bottom(),
                "{:?}",
                pair[1].rect
            );
        }
        assert!(layout.is_complete());
        assert!(layout.height > layout.blocks[2].rect.bottom());
    }

    #[test]
    fn should_collapse_margins_when_a_paragraph_follows_a_paragraph() {
        let (_, layout, _) = laid("one\n\ntwo\n", 900.0);
        let gap = layout.blocks[1].rect.y - layout.blocks[0].rect.bottom();
        assert!((gap - 16.0).abs() < 0.01, "{gap}");
    }

    #[test]
    fn should_use_the_larger_margin_when_a_heading_follows_a_paragraph() {
        let (_, layout, _) = laid("one\n\n## Two\n", 900.0);
        let gap = layout.blocks[1].rect.y - layout.blocks[0].rect.bottom();
        assert!((gap - 36.0).abs() < 0.01, "{gap}");
    }

    const REPLIED: &str =
        "## Questions\n\n1. Pick?\n   - A\n   - B\n\n<!-- limpid reply q1\nB\n-->\n\nTail.\n";

    #[test]
    fn should_paint_the_question_box_when_the_window_top_is_below_its_reply() {
        let (_, mut layout, mut fonts) = laid(REPLIED, 900.0);
        let field = &layout.blocks[1];
        let reply = &layout.blocks[2];
        assert!(reply.rect.bottom() < field.rect.bottom() - 3.0);
        let scroll = field.rect.bottom() - 2.0;
        let mut pixels = vec![0u32; 900 * 2];
        layout.paint(&mut Canvas::new(&mut pixels, 900, 2), &mut fonts, scroll);
        assert!(pixels.iter().any(|&pixel| pixel != 0));
    }

    #[test]
    fn should_start_at_the_question_box_when_the_search_lands_after_its_reply() {
        let (_, layout, _) = laid(REPLIED, 900.0);
        let field = &layout.blocks[1];
        let top = field.rect.bottom() - 2.0;
        assert_eq!(first_visible(&layout.blocks, top), 1);
    }

    fn pending_hash(block: &LaidBlock) -> String {
        match &block.content {
            Content::Pending(pending) => pending.hash.clone(),
            Content::Media(media) => media.hash.clone(),
            _ => None,
        }
        .expect("the block shows display math")
    }

    #[test]
    fn should_relay_a_block_below_the_question_box_when_it_follows_an_answered_question() {
        let markdown =
            "## Questions\n\n1. Pick?\n   - A\n   - B\n\n<!-- limpid reply q1\nB\n-->\n\n$$x^2$$\n";
        let (doc, mut layout, mut fonts) = laid(markdown, 900.0);
        let fresh: Vec<Rect> = layout.blocks.iter().map(|block| block.rect).collect();
        assert!(fresh[2].bottom() < fresh[1].bottom(), "the reply is nested");
        let hash = pending_hash(&layout.blocks[3]);
        assert!(layout.refresh_pending(&[hash]));
        while !layout.is_complete() {
            let next = layout.blocks.len();
            layout.ensure_block(next, &doc, &mut fonts);
        }
        let relaid: Vec<Rect> = layout.blocks.iter().map(|block| block.rect).collect();
        assert_eq!(relaid, fresh);
        assert!(
            relaid[3].y > relaid[1].bottom(),
            "{:?} {:?}",
            relaid[3],
            relaid[1]
        );
    }

    #[test]
    fn should_finish_immediately_when_the_document_is_empty() {
        let (_, layout, _) = laid("", 900.0);
        assert!(layout.is_complete());
        assert!(layout.blocks.is_empty());
        assert!(layout.height > 0.0);
        assert_eq!(layout.block_at(10.0), None);
    }

    #[test]
    fn should_find_the_containing_block_when_asking_block_at() {
        let (_, layout, _) = laid("one\n\ntwo\n", 900.0);
        let second = layout.blocks[1].rect;
        assert_eq!(layout.block_at(second.y + 1.0), Some(1));
        assert_eq!(layout.block_at(second.y - 1.0), Some(0));
        assert_eq!(layout.block_at(0.0), None);
        assert_eq!(layout.block_at(1e9), Some(1));
    }

    #[test]
    fn should_mark_stops_by_kind_when_the_document_mixes_blocks() {
        let (_, layout, _) = laid(
            "# Title\n\npara\n\n---\n\n```rs\nx\n```\n\n- a\n- b\n\n> q\n",
            900.0,
        );
        let stops: Vec<_> = layout.stops().map(|block| block.src.start).collect();
        // heading, paragraph, code, two list items, quote; not the rule.
        assert_eq!(stops.len(), 6, "{stops:?}");
        assert!(stops.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn should_survive_garbage_geometry_when_width_or_scale_is_not_usable() {
        let doc = doc("text\n");
        let mut fonts = Fonts::new();
        for (width, scale) in [
            (0.0, 1.0),
            (-5.0, 1.0),
            (f32::NAN, 1.0),
            (100.0, f32::NAN),
            (100.0, -2.0),
        ] {
            let layout = lay_out(&doc, width, &Theme::dark(), scale, &mut fonts);
            assert_eq!(layout.blocks.len(), 1);
            assert!(layout.column_width >= 1.0);
        }
    }

    #[test]
    fn should_scroll_code_sideways_only_within_the_overflow_when_scrolling() {
        let long = "x".repeat(400);
        let (_, mut layout, _) = laid(&format!("```\n{long}\n```\n"), 700.0);
        let block = &mut layout.blocks[0];
        assert!(block.scroll_code_x(50.0));
        assert!(block.scroll_code_x(1e6));
        assert!(!block.scroll_code_x(1e6));
        let Content::Code {
            scroll_x,
            text_width,
            ..
        } = &block.content
        else {
            panic!("not code");
        };
        assert!(*scroll_x > 0.0 && *scroll_x < *text_width);
    }
}
