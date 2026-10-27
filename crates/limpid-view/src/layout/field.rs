//! Question fields, saved or pending annotations and the editor box.
//!
//! A field shows its prompt with the suggested answers numbered, so `1`-`9`
//! picks one. An annotation is a labelled box with the reply or comment text.
//! The editor box is a bordered area of a fixed number of grid rows, painted
//! over by the embedded editor.

use super::{
    Content, LaidBlock, PendingBox, PendingKind,
    block::{Cx, Flow},
    inline::{self, Flat, RichText},
    note,
};
use crate::{
    editor::paint::{EDITOR_FONT_SIZE, GRID_LINE_HEIGHT},
    nav::cursor::BLEED_INSET,
    paint::Rect,
    text::TextStyle,
    theme::{Color, Theme},
};
use limpid_core::doc::{AnnotationKind, Block, Inline};

/// Marks an annotation block as the editor box. The text after the prefix is
/// the number of grid rows the box reserves.
pub const BOX_PREFIX: &str = "\u{1}limpid-box:";

/// Leads the inlines of the option a reply has chosen; it draws nothing.
pub const CHOSEN_MARK: &str = "limpid-chosen";

/// The text of the annotation block that reserves `rows` rows for the editor.
pub fn box_text(rows: u16) -> String {
    format!("{BOX_PREFIX}{rows}")
}

/// The rows an editor box annotation reserves, or `None` for any other text.
pub fn box_rows(text: &str) -> Option<u16> {
    text.strip_prefix(BOX_PREFIX)?.parse().ok()
}

/// Height in device pixels of one editor grid row at `font_size` device px.
pub fn grid_row_height(font_size: f32) -> f32 {
    (font_size * GRID_LINE_HEIGHT).round().max(1.0)
}

/// Padding and border around the editor box text, device pixels.
fn frame(cx: &Cx) -> (f32, f32) {
    (cx.px(12.0), cx.px(1.0).round().max(1.0))
}

/// How a box of text looks: padding and radius in CSS px.
struct Look {
    pad_x: f32,
    pad_y: f32,
    radius: f32,
    /// Inset from the column on both sides when at the top level.
    bleed: bool,
}

const FIELD: Look = Look {
    pad_x: 16.0,
    pad_y: 10.0,
    radius: 8.0,
    bleed: false,
};
const COMMENT: Look = Look {
    pad_x: 14.0,
    pad_y: 10.0,
    radius: 10.0,
    bleed: true,
};
const REPLY: Look = Look {
    pad_x: 10.0,
    pad_y: 8.0,
    radius: 6.0,
    bleed: false,
};

/// The column slice a box with `look` takes: inset like the focus band at the
/// top level, the whole slice inside a list item.
fn slot(cx: &Cx, flow: Flow, look: &Look) -> (f32, f32) {
    if !(look.bleed && flow.top_level) {
        return (flow.x, flow.width);
    }
    let inset = cx.px(BLEED_INSET).min(flow.width / 4.0);
    (flow.x + inset, (flow.width - 2.0 * inset).max(1.0))
}

/// Append `tail` to `head`, moving its ranges past the text already there.
fn append(head: &mut Flat, tail: Flat) {
    let shift = head.text.len();
    head.text.push_str(&tail.text);
    head.runs.extend(
        tail.runs
            .into_iter()
            .map(|(range, style)| (range.start + shift..range.end + shift, style)),
    );
    head.decorations
        .extend(tail.decorations.into_iter().map(|mut decoration| {
            decoration.range = decoration.range.start + shift..decoration.range.end + shift;
            decoration
        }));
    head.placeholders
        .extend(tail.placeholders.into_iter().map(|mut hole| {
            hole.range = hole.range.start + shift..hole.range.end + shift;
            hole
        }));
}

fn is_chosen(option: &[Inline]) -> bool {
    matches!(option.first(), Some(Inline::Html(mark)) if mark == CHOSEN_MARK)
}

/// The field as text: the prompt in bold, then one numbered line per
/// suggested answer. Numbers are teal monospace; the chosen answer is teal
/// and bold.
fn field_flat(theme: &Theme, ink: Color, prompt: &[Inline], options: &[Vec<Inline>]) -> Flat {
    let plain = TextStyle::sans(ink);
    let mut flat = inline::flatten(prompt, plain.bold(), theme, false);
    for (at, option) in options.iter().enumerate() {
        flat.push("\n", plain);
        flat.push(&format!("{}. ", at + 1), TextStyle::mono(theme.teal));
        let style = if is_chosen(option) {
            TextStyle::sans(theme.teal).bold()
        } else {
            plain
        };
        append(&mut flat, inline::flatten(option, style, theme, false));
    }
    flat
}

#[allow(clippy::too_many_arguments)]
fn pending_block(
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    kind: PendingKind,
    text: RichText,
    shape: Shape,
) -> LaidBlock {
    LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(shape.x, y, shape.width, shape.height),
        stop,
        content: Content::Pending(PendingBox {
            kind,
            text,
            padding: shape.padding,
            pad_y: shape.pad_y,
            radius: shape.radius,
            border: shape.border,
            hash: None,
            fill: None,
        }),
        depth: flow.depth,
    }
}

/// Where a box goes and how it is drawn, device pixels.
struct Shape {
    x: f32,
    width: f32,
    height: f32,
    padding: f32,
    pad_y: f32,
    radius: f32,
    border: Option<Color>,
}

/// Shape `flat` into a box with `look`.
#[allow(clippy::too_many_arguments)]
fn text_box(
    cx: &mut Cx,
    flow: Flow,
    look: &Look,
    flat: &Flat,
    base: TextStyle,
    size: f32,
    line: f32,
    border: Option<Color>,
) -> (RichText, Shape) {
    let (x, width) = slot(cx, flow, look);
    let rule = cx.px(1.0).round().max(1.0);
    let (padding, pad_y) = (cx.px(look.pad_x), cx.px(look.pad_y));
    let inner = (width - 2.0 * (padding + rule)).max(1.0);
    let text = inline::build(cx.fonts, flat, base, cx.theme, size, line, Some(inner));
    let height = text.height().max(line) + 2.0 * (pad_y + rule);
    let shape = Shape {
        x,
        width,
        height,
        padding,
        pad_y,
        radius: cx.px(look.radius),
        border,
    };
    (text, shape)
}

/// A question: the prompt and its numbered suggested answers.
#[allow(clippy::too_many_arguments)]
pub fn lay_field(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    prompt: &[Inline],
    options: &[Vec<Inline>],
) -> LaidBlock {
    let flat = field_flat(cx.theme, flow.ink, prompt, options);
    let base = TextStyle::sans(flow.ink);
    let (size, line) = (cx.body_size(), cx.body_line());
    let (text, shape) = text_box(cx, flow, &FIELD, &flat, base, size, line, None);
    pending_block(block, flow, y, index, stop, PendingKind::Field, text, shape)
}

/// A saved or pending reply or comment, or the editor box when `text` is a
/// box marker. Comments are monospace and blue; replies stand out in the
/// ink colour.
#[allow(clippy::too_many_arguments)]
pub fn lay_annotation(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    kind: AnnotationKind,
    text: &str,
) -> LaidBlock {
    if let Some(rows) = box_rows(text) {
        return lay_box(cx, block, flow, y, index, stop, rows);
    }
    let theme = cx.theme;
    let (look, base, size, line, border) = match kind {
        AnnotationKind::Comment => {
            let size = cx.font_px(15.0);
            (
                &COMMENT,
                TextStyle::mono(theme.blue),
                size,
                size * 1.7,
                None,
            )
        }
        AnnotationKind::Reply => (
            &REPLY,
            TextStyle::sans(theme.ink),
            cx.body_size(),
            cx.body_line(),
            Some(inline::rule_soft(theme)),
        ),
    };
    let flat = inline::flatten(&note::inlines(text), base, theme, false);
    let (rich, shape) = text_box(cx, flow, look, &flat, base, size, line, border);
    pending_block(
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Annotation,
        rich,
        shape,
    )
}

fn lay_box(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    rows: u16,
) -> LaidBlock {
    let (padding, border) = frame(cx);
    let rows = rows.max(1);
    let grid_height = f32::from(rows) * grid_row_height(cx.px(EDITOR_FONT_SIZE));
    let height = grid_height + 2.0 * (padding + border);
    let (x, width) = slot(cx, flow, &COMMENT);
    let style = TextStyle::sans(flow.ink);
    let empty = inline::build_inlines(
        cx.fonts,
        &[],
        style,
        cx.theme,
        cx.body_size(),
        cx.body_line(),
        Some(width),
        false,
    );
    let shape = Shape {
        x,
        width,
        height,
        padding,
        pad_y: padding,
        radius: cx.px(COMMENT.radius),
        border: None,
    };
    pending_block(
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Annotation,
        empty,
        shape,
    )
}

/// The rectangle inside a box block's frame where the editor grid goes, in
/// the same coordinates as `outer`.
pub fn box_inner(outer: Rect, scale: f32) -> Rect {
    let padding = 12.0 * scale;
    let border = scale.round().max(1.0);
    let inset = padding + border;
    Rect::new(
        outer.x + inset,
        outer.y + inset,
        (outer.w - 2.0 * inset).max(0.0),
        (outer.h - 2.0 * inset).max(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::lay_out, nav::cursor::BLEED_INSET, text::Fonts, theme::Theme};
    use limpid_core::{
        doc::{BlockKind, Mode, parse},
        settings::Settings,
    };

    fn laid(markdown: &str) -> (limpid_core::doc::Doc, crate::layout::Layout) {
        let doc = parse(markdown, Mode::File, &Settings::default());
        let layout = lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut Fonts::new());
        (doc, layout)
    }

    #[test]
    fn should_round_trip_the_row_count_when_marking_a_box() {
        assert_eq!(box_rows(&box_text(7)), Some(7));
    }

    #[test]
    fn should_not_treat_ordinary_text_as_a_box_when_reading_rows() {
        assert_eq!(box_rows("hello"), None);
        assert_eq!(box_rows(&format!("{BOX_PREFIX}many")), None);
        assert_eq!(box_rows(""), None);
    }

    fn pending(layout: &crate::layout::Layout, kind: PendingKind) -> &LaidBlock {
        layout
            .blocks
            .iter()
            .find(|block| matches!(block.content, Content::Pending(ref p) if p.kind == kind))
            .expect("the block is laid out")
    }

    fn style_at(flat: &Flat, needle: &str) -> TextStyle {
        let at = flat.text.find(needle).expect("text is present");
        flat.runs
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map(|(_, style)| *style)
            .unwrap()
    }

    fn words(text: &str) -> Vec<Inline> {
        vec![Inline::Text(text.into())]
    }

    #[test]
    fn should_number_the_options_in_teal_monospace_when_building_a_field() {
        let theme = Theme::dark();
        let flat = field_flat(
            &theme,
            theme.ink,
            &words("Which?"),
            &[words("Alpha"), words("Beta")],
        );
        assert_eq!(flat.text, "Which?\n1. Alpha\n2. Beta");
        let number = style_at(&flat, "2. ");
        assert!(number.mono && number.color == theme.teal);
        let option = style_at(&flat, "Beta");
        assert!(!option.mono && !option.bold && option.color == theme.ink);
        assert!(style_at(&flat, "Which?").bold);
    }

    #[test]
    fn should_color_the_chosen_option_teal_and_bold_when_it_carries_the_mark() {
        let theme = Theme::dark();
        let mut chosen = vec![Inline::Html(CHOSEN_MARK.into())];
        chosen.extend(words("Beta"));
        let flat = field_flat(&theme, theme.ink, &words("Q"), &[words("Alpha"), chosen]);
        assert_eq!(flat.text, "Q\n1. Alpha\n2. Beta");
        let option = style_at(&flat, "Beta");
        assert!(option.bold && option.color == theme.teal);
        assert_ne!(style_at(&flat, "Alpha").color, theme.teal);
    }

    #[test]
    fn should_keep_flat_ranges_in_step_when_appending() {
        let theme = Theme::dark();
        let style = TextStyle::sans(theme.ink);
        let mut head = Flat::plain("ab", style);
        let tail = inline::flatten(&[Inline::Code("cd".into())], style, &theme, false);
        let before = tail.decorations[0].range.clone();
        append(&mut head, tail);
        assert_eq!(head.decorations[0].range, before.start + 2..before.end + 2);
        assert!(
            head.runs
                .iter()
                .all(|(range, _)| range.end <= head.text.len())
        );
    }

    #[test]
    fn should_grow_a_field_with_its_options_when_laying_out() {
        let (_, short) = laid("## Questions\n\n1. Which?\n");
        let (_, long) = laid("## Questions\n\n1. Which?\n   - A\n   - B\n   - C\n");
        let field_height = |layout: &crate::layout::Layout| {
            layout
                .blocks
                .iter()
                .find(|block| matches!(block.content, Content::Pending(ref p) if p.kind == PendingKind::Field))
                .map(|block| block.rect.h)
                .unwrap()
        };
        assert!(field_height(&long) > field_height(&short) * 1.5);
    }

    #[test]
    fn should_reserve_the_requested_rows_when_laying_out_a_box() {
        let mut fonts = Fonts::new();
        let theme = Theme::dark();
        let settings = Settings::default();
        let mut doc = parse("para\n", Mode::File, &settings);
        let end = doc.source.len();
        let mut heights = Vec::new();
        for rows in [3u16, 6] {
            doc.blocks.truncate(1);
            doc.blocks.push(Block {
                kind: BlockKind::Annotation {
                    target: 0..end,
                    kind: AnnotationKind::Comment,
                    text: box_text(rows),
                    field: None,
                },
                src: end..end,
                id: None,
            });
            let layout = lay_out(&doc, 1000.0, &theme, 1.0, &mut fonts);
            heights.push(layout.blocks[1].rect.h);
            assert!(
                layout.blocks[1].stop,
                "the cursor must be able to rest on the box"
            );
        }
        let row = grid_row_height(EDITOR_FONT_SIZE);
        assert!(
            (heights[1] - heights[0] - 3.0 * row).abs() < 0.5,
            "{heights:?}"
        );
    }

    #[test]
    fn should_keep_the_grid_inside_the_frame_when_computing_the_inner_rect() {
        let outer = Rect::new(10.0, 20.0, 400.0, 100.0);
        let inner = box_inner(outer, 2.0);
        assert!(inner.x > outer.x && inner.y > outer.y);
        assert!(inner.right() < outer.right() && inner.bottom() < outer.bottom());
        assert_eq!(box_inner(Rect::new(0.0, 0.0, 1.0, 1.0), 1.0).w, 0.0);
    }

    #[test]
    fn should_draw_a_comment_in_one_line_without_a_label_when_the_text_is_one_line() {
        let (_, layout) = laid("para\n\n<!-- limpid comment\nThis leaks.\n-->\n");
        let note = pending(&layout, PendingKind::Annotation);
        assert!(note.stop);
        let expected = 15.0 * 1.7 + 2.0 * (10.0 + 1.0);
        assert!((note.rect.h - expected).abs() < 1.0, "{}", note.rect.h);
    }

    #[test]
    fn should_inset_a_comment_like_the_focus_band_when_it_sits_at_the_top_level() {
        let (_, layout) = laid("para\n\n<!-- limpid comment\nNote\n-->\n");
        let note = pending(&layout, PendingKind::Annotation);
        assert_eq!(note.rect.x, BLEED_INSET);
        assert_eq!(note.rect.w, layout.column_width - 2.0 * BLEED_INSET);
        let Content::Pending(shape) = &note.content else {
            unreachable!()
        };
        assert_eq!(
            (shape.radius, shape.padding, shape.pad_y),
            (10.0, 14.0, 10.0)
        );
    }

    #[test]
    fn should_leave_twelve_above_and_twenty_below_when_stacking_a_comment() {
        let (_, layout) = laid("para\n\n<!-- limpid comment\nNote\n-->\n\nafter\n");
        let paragraph = &layout.blocks[0];
        let note = &layout.blocks[1];
        let after = &layout.blocks[2];
        // The paragraph's own 16px margin below wins over the 12px above.
        assert!((note.rect.y - paragraph.rect.bottom() - 16.0).abs() < 0.5);
        assert!((after.rect.y - note.rect.bottom() - 20.0).abs() < 0.5);
    }

    #[test]
    fn should_round_a_question_with_eight_and_pad_it_ten_by_sixteen_when_laying_out() {
        let (_, layout) = laid("## Questions\n\n1. Which?\n   - A\n");
        let field = pending(&layout, PendingKind::Field);
        let Content::Pending(shape) = &field.content else {
            unreachable!()
        };
        assert_eq!(
            (shape.radius, shape.padding, shape.pad_y),
            (8.0, 16.0, 10.0)
        );
        assert_eq!(field.rect.x, 0.0);
    }

    #[test]
    fn should_render_markdown_in_a_comment_when_the_text_has_lists() {
        let (_, plain) = laid("para\n\n<!-- limpid comment\nonly one line\n-->\n");
        let (_, listed) = laid("para\n\n<!-- limpid comment\n- a\n- b\n- c\n-->\n");
        let height =
            |layout: &crate::layout::Layout| pending(layout, PendingKind::Annotation).rect.h;
        assert!(height(&listed) > height(&plain) + 2.0 * 15.0 * 1.7 - 1.0);
    }

    #[test]
    fn should_stop_on_a_comment_inside_a_list_item_when_it_is_saved_there() {
        let (_, layout) = laid("- item\n\n  <!-- limpid comment\n  Note\n  -->\n- other\n");
        assert!(
            layout.stops().any(|stop| matches!(
                stop.content,
                Content::Pending(ref p) if p.kind == PendingKind::Annotation
            )),
            "the nested comment is a stop"
        );
    }
}
