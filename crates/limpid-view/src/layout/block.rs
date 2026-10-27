//! Single blocks and the vertical stacking shared by every container.

use super::{
    Content, LaidBlock, LayoutOptions, PaintCx, PendingBox, PendingKind,
    boxes::{self, BoxEnv},
    code, field, image,
    inline::{self, Flat, RichText},
    list, quote, table,
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::doc::{AnnotationKind, Block, BlockKind};

/// Everything laying out a block reads and writes.
pub struct Cx<'a> {
    pub theme: &'a Theme,
    pub scale: f32,
    pub options: &'a LayoutOptions,
    /// What the image, SVG, math and Mermaid boxes read from outside.
    pub boxes: &'a BoxEnv,
    /// The document Markdown, for source fallbacks.
    pub source: &'a str,
    pub fonts: &'a mut Fonts,
    pub viewport_css_width: f32,
}

impl Cx<'_> {
    /// CSS px to device pixels.
    pub fn px(&self, css: f32) -> f32 {
        css * self.scale
    }

    /// Ratio of the configured font size to the 17px the stylesheet assumes.
    pub fn font_ratio(&self) -> f32 {
        self.theme.font_size / 17.0
    }

    /// A stylesheet font size (CSS px at 17px body text), in device pixels.
    pub fn font_px(&self, css: f32) -> f32 {
        css * self.font_ratio() * self.scale
    }

    pub fn body_size(&self) -> f32 {
        self.theme.font_size * self.scale
    }

    pub fn body_line(&self) -> f32 {
        self.body_size() * self.theme.line_height
    }
}

/// Where a block sits: the column slice it may use and what it inherits.
#[derive(Debug, Clone, Copy)]
pub struct Flow {
    pub x: f32,
    pub width: f32,
    /// Colour of body text here (muted inside quotes).
    pub ink: Color,
    pub depth: u8,
    /// Inside a list item: paragraphs sit closer together.
    pub compact: bool,
    /// A direct child of the document, where stop rules apply by kind.
    pub top_level: bool,
}

impl Flow {
    pub fn child(self, x: f32, width: f32, ink: Color) -> Self {
        Self {
            x,
            width: width.max(1.0),
            ink,
            depth: self.depth.saturating_add(1),
            compact: false,
            top_level: false,
        }
    }
}

/// Margins above and below a block, in CSS px (they collapse with neighbours).
pub fn margins(kind: &BlockKind, flow: Flow) -> (f32, f32) {
    match kind {
        BlockKind::Heading { level, .. } => match level {
            1 => (0.0, 14.0),
            2 => (36.0, 14.0),
            3 => (30.0, 10.0),
            4 => (24.0, 10.0),
            _ => (20.0, 8.0),
        },
        BlockKind::Paragraph(_) => (0.0, if flow.compact { 6.0 } else { 16.0 }),
        BlockKind::Code { .. } | BlockKind::Mermaid { .. } => (22.0, 22.0),
        BlockKind::Math { .. } => (20.0, 20.0),
        BlockKind::List { .. } => {
            if flow.depth > 0 {
                (6.0, 6.0)
            } else {
                (0.0, 18.0)
            }
        }
        BlockKind::Quote { .. } => (20.0, 20.0),
        BlockKind::Table { .. } => (22.0, 22.0),
        BlockKind::Rule => (36.0, 36.0),
        BlockKind::Image { .. } => (16.0, 16.0),
        BlockKind::Html(_) => (0.0, 16.0),
        BlockKind::Field { .. } => (12.0, 12.0),
        BlockKind::Annotation {
            kind: AnnotationKind::Reply,
            ..
        } => (10.0, 12.0),
        BlockKind::Annotation { .. } => (12.0, 20.0),
        BlockKind::FootnoteDef { .. } => (0.0, 8.0),
    }
}

fn is_comment(kind: &BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Annotation {
            kind: AnnotationKind::Comment,
            ..
        }
    )
}

/// Whether a direct child of the document is a cursor stop.
fn is_stop(kind: &BlockKind) -> bool {
    match kind {
        BlockKind::Paragraph(_)
        | BlockKind::Code { .. }
        | BlockKind::Math { .. }
        | BlockKind::Mermaid { .. }
        | BlockKind::Quote { .. }
        | BlockKind::Table { .. }
        | BlockKind::Image { .. }
        | BlockKind::Field { .. } => true,
        BlockKind::Annotation { kind, .. } => *kind == AnnotationKind::Comment,
        BlockKind::Heading { .. }
        | BlockKind::Rule
        | BlockKind::List { .. }
        | BlockKind::Html(_)
        | BlockKind::FootnoteDef { .. } => false,
    }
}

/// Stack `blocks` from `y` downwards, collapsing margins between them. The
/// first block's top margin and the last block's bottom margin are dropped.
/// Returns the laid blocks and the bottom edge of the last.
pub fn stack(cx: &mut Cx, blocks: &[Block], y: f32, flow: Flow) -> (Vec<LaidBlock>, f32) {
    let mut laid: Vec<LaidBlock> = Vec::with_capacity(blocks.len());
    let mut bottom = y;
    let mut previous_margin: f32 = 0.0;
    for (index, block) in blocks.iter().enumerate() {
        let (margin_top, margin_bottom) = margins(&block.kind, flow);
        let gap = if laid.is_empty() {
            0.0
        } else {
            cx.px(previous_margin.max(margin_top))
        };
        let child = lay_block(cx, block, index, bottom + gap, flow);
        bottom = child.rect.bottom();
        previous_margin = margin_bottom;
        laid.push(child);
    }
    (laid, bottom)
}

pub fn lay_block(cx: &mut Cx, block: &Block, index: usize, y: f32, flow: Flow) -> LaidBlock {
    // A comment inside a list item is a stop of its own.
    let stop = (flow.top_level || is_comment(&block.kind)) && is_stop(&block.kind);
    let make = |content: Content, height: f32| LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height),
        stop,
        content,
        depth: flow.depth,
    };
    match &block.kind {
        BlockKind::Heading { level, inlines, .. } => {
            let (content, height) = heading(cx, *level, inlines, flow);
            make(content, height)
        }
        BlockKind::Paragraph(inlines) => {
            let style = TextStyle::sans(flow.ink);
            let text = inline::build_inlines(
                cx.fonts,
                inlines,
                style,
                cx.theme,
                cx.body_size(),
                cx.body_line(),
                Some(flow.width),
                false,
            );
            let height = text.height();
            make(Content::Text(text), height)
        }
        BlockKind::Code { lang, text, spans } => {
            let (content, height) = code::lay(cx, lang.as_deref(), text, spans, flow);
            make(content, height)
        }
        BlockKind::List {
            ordered,
            start,
            items,
        } => {
            let (children, bottom) = list::lay_items(cx, *ordered, *start, items, block, y, flow);
            make(Content::Children(children), bottom - y)
        }
        BlockKind::Quote { callout, blocks } => {
            let (content, height) = quote::lay(cx, callout.as_ref(), blocks, y, flow);
            make(content, height)
        }
        BlockKind::Rule => {
            let height = cx.px(1.0).round().max(1.0);
            make(Content::Rule, height)
        }
        BlockKind::Html(html) => {
            let (content, height) = html_source(cx, html, flow);
            make(content, height)
        }
        BlockKind::FootnoteDef { label, blocks } => {
            let (content, height) = list::lay_footnote(cx, label, blocks, y, flow);
            make(content, height)
        }
        BlockKind::Math { latex, hash } => {
            boxes::lay_math(cx, block, flow, y, index, stop, latex, hash)
        }
        BlockKind::Mermaid { source, hash } => {
            boxes::lay_mermaid(cx, block, flow, y, index, stop, source, hash)
        }
        BlockKind::Table { align, head, rows } => {
            table::lay(cx, block, flow, y, index, stop, align, head, rows)
        }
        BlockKind::Image { src, alt, .. } => image::lay(cx, block, flow, y, index, stop, src, alt),
        BlockKind::Field {
            prompt, options, ..
        } => field::lay_field(cx, block, flow, y, index, stop, prompt, options),
        BlockKind::Annotation { kind, text, .. } => {
            field::lay_annotation(cx, block, flow, y, index, stop, *kind, text)
        }
    }
}

/// Raw HTML shown as muted source.
fn html_source(cx: &mut Cx, html: &str, flow: Flow) -> (Content, f32) {
    let style = TextStyle::mono(cx.theme.muted);
    let flat = Flat::plain(html.trim_end(), style);
    let text = inline::build(
        cx.fonts,
        &flat,
        style,
        cx.theme,
        cx.font_px(13.0),
        cx.font_px(13.0) * 1.6,
        Some(flow.width),
    );
    let height = text.height();
    (Content::Text(text), height)
}

/// The Markdown under `range`, or nothing when the range is not valid text.
pub fn source_slice<'a>(source: &'a str, range: &std::ops::Range<usize>) -> &'a str {
    source.get(range.clone()).unwrap_or("").trim_end()
}

fn heading_size_css(level: u8, viewport_css_width: f32) -> f32 {
    match level {
        1 => (viewport_css_width * 0.03).clamp(24.0, 32.0),
        2 => (viewport_css_width * 0.026).clamp(21.0, 27.0),
        3 => 18.0,
        4 => 15.5,
        5 => 14.0,
        _ => 13.0,
    }
}

fn heading(
    cx: &mut Cx,
    level: u8,
    inlines: &[limpid_core::doc::Inline],
    flow: Flow,
) -> (Content, f32) {
    let level = level.clamp(1, 6);
    let size = cx.font_px(heading_size_css(level, cx.viewport_css_width));
    let color = if level >= 4 {
        cx.theme.muted
    } else {
        cx.theme.ink
    };
    let style = TextStyle::sans(color).bold();
    let text = inline::build_inlines(
        cx.fonts,
        inlines,
        style,
        cx.theme,
        size,
        size * 1.25,
        Some(flow.width),
        level == 4,
    );
    let rule = level == 2;
    let rule_height = if rule {
        cx.px(1.0).round().max(1.0)
    } else {
        0.0
    };
    let pad_top = if rule { cx.px(20.0) } else { 0.0 };
    let height = rule_height + pad_top + text.height();
    (
        Content::Heading {
            text,
            level,
            rule,
            pad_top,
        },
        height,
    )
}

/// A box of source text. `hash` names the math or Mermaid entry it waits for;
/// `fill` replaces the `surface` background.
#[allow(clippy::too_many_arguments)]
pub fn pending_with(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    kind: PendingKind,
    source: &str,
    hash: Option<String>,
    fill: Option<Color>,
) -> LaidBlock {
    let padding = cx.px(12.0);
    let border = cx.px(1.0).round().max(1.0);
    let inner = (flow.width - 2.0 * (padding + border)).max(1.0);
    let style = if kind == PendingKind::Annotation {
        TextStyle::sans(cx.theme.ink)
    } else {
        TextStyle::mono(cx.theme.muted)
    };
    let (size, line) = if kind == PendingKind::Annotation {
        (cx.body_size(), cx.body_line())
    } else {
        (cx.font_px(13.0), cx.font_px(13.0) * 1.6)
    };
    let flat = Flat::plain(source.trim_end_matches('\n'), style);
    let text: RichText = inline::build(cx.fonts, &flat, style, cx.theme, size, line, Some(inner));
    let height = text.height() + 2.0 * (padding + border);
    LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height),
        stop,
        content: Content::Pending(PendingBox {
            kind,
            text,
            padding,
            pad_y: padding,
            radius: 0.0,
            border: None,
            hash,
            fill,
        }),
        depth: flow.depth,
    }
}

pub fn paint_pending(
    paint: &PaintCx,
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    area: Rect,
    pending: &PendingBox,
) {
    let theme = paint.theme;
    let border = paint.scale.round().max(1.0);
    canvas.fill_round_rect(area, pending.radius, pending.fill.unwrap_or(theme.surface));
    canvas.stroke_round_rect(
        area,
        pending.radius,
        border,
        pending.border.unwrap_or(theme.rule),
    );
    inline::paint(
        canvas,
        fonts,
        &pending.text,
        area.x + border + pending.padding,
        area.y + border + pending.pad_y,
        theme,
        paint.scale,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, lay_out};
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn layout(markdown: &str) -> Layout {
        let doc = parse(markdown, Mode::File, &Settings::default());
        lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut Fonts::new())
    }

    #[test]
    fn should_size_headings_down_by_level_when_laying_out_h1_to_h4() {
        let layout = layout("# a\n\n### b\n\n#### c\n");
        let heights: Vec<f32> = layout.blocks.iter().map(|b| b.rect.h).collect();
        assert!(
            heights[0] > heights[1] && heights[1] > heights[2],
            "{heights:?}"
        );
    }

    #[test]
    fn should_add_a_rule_and_padding_above_h2_text_when_laying_out_h2() {
        let layout = layout("## a\n\n### b\n");
        let Content::Heading { rule, pad_top, .. } = &layout.blocks[0].content else {
            panic!("not a heading");
        };
        assert!(*rule && *pad_top == 20.0);
        let Content::Heading { rule, .. } = &layout.blocks[1].content else {
            panic!("not a heading");
        };
        assert!(!*rule);
    }

    #[test]
    fn should_wrap_a_paragraph_onto_more_lines_when_the_column_is_narrow() {
        let text = "word ".repeat(80);
        let doc = parse(&text, Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let wide = lay_out(&doc, 1400.0, &Theme::dark(), 1.0, &mut fonts);
        let narrow = lay_out(&doc, 500.0, &Theme::dark(), 1.0, &mut fonts);
        assert!(narrow.blocks[0].rect.h > wide.blocks[0].rect.h * 1.5);
    }

    #[test]
    fn should_double_the_geometry_when_scale_is_two() {
        let doc = parse("# T\n\nbody text\n", Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let one = lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut fonts);
        let two = lay_out(&doc, 2000.0, &Theme::dark(), 2.0, &mut fonts);
        let ratio = two.blocks[1].rect.y / one.blocks[1].rect.y;
        assert!((1.9..2.1).contains(&ratio), "{ratio}");
    }

    #[test]
    fn should_lay_a_table_out_as_a_table_box_when_the_block_is_a_table() {
        let layout = layout("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(matches!(layout.blocks[0].content, Content::Table(_)));
        assert!(layout.blocks[0].stop);
        assert!(layout.blocks[0].rect.h > 40.0);
    }

    #[test]
    fn should_show_source_boxes_when_math_mermaid_and_image_are_not_available() {
        let layout = layout(
            "$$\n\\unseenMacroQ7 + x\n$$\n\n```mermaid\ngraph TD; ZZ9QQ-->B7\n```\n\n![alt](missing-unseen-q7.png)\n",
        );
        let kinds: Vec<_> = layout
            .blocks
            .iter()
            .filter_map(|b| match &b.content {
                Content::Pending(p) => Some(p.kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            vec![PendingKind::Math, PendingKind::Mermaid, PendingKind::Image]
        );
    }

    #[test]
    fn should_give_a_rule_a_thin_height_and_no_stop_when_laying_out_hr() {
        let layout = layout("a\n\n---\n\nb\n");
        assert_eq!(layout.blocks[1].rect.h, 1.0);
        assert!(!layout.blocks[1].stop);
    }

    #[test]
    fn should_return_empty_source_when_the_range_is_not_valid_text() {
        assert_eq!(source_slice("héllo", &(0..2)), "");
        assert_eq!(source_slice("abc", &(1..99)), "");
        assert_eq!(source_slice("abc\n\n", &(0..5)), "abc");
    }

    #[test]
    fn should_render_html_blocks_as_muted_source_when_html_is_present() {
        let layout = layout("<div>raw</div>\n\nafter\n");
        assert!(matches!(layout.blocks[0].content, Content::Text(_)));
        assert!(!layout.blocks[0].stop);
    }
}
