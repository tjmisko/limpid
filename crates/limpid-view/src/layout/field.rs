//! Question fields, saved or pending annotations and the editor box.
//!
//! A field shows its prompt with the suggested answers numbered, so `1`-`9`
//! picks one. An annotation is a labelled box with the reply or comment text.
//! The editor box is a bordered area of a fixed number of grid rows, painted
//! over by the embedded editor.

use super::{
    Content, LaidBlock, PendingBox, PendingKind,
    block::{Cx, Flow},
    inline::{self, RichText},
};
use crate::{editor::paint::{EDITOR_FONT_SIZE, GRID_LINE_HEIGHT}, paint::Rect, text::TextStyle};
use limpid_core::doc::{AnnotationKind, Block, Inline};

/// Marks an annotation block as the editor box. The text after the prefix is
/// the number of grid rows the box reserves.
pub const BOX_PREFIX: &str = "\u{1}limpid-box:";

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

/// Padding and border around the box text, device pixels.
fn frame(cx: &Cx) -> (f32, f32) {
    (cx.px(12.0), cx.px(1.0).round().max(1.0))
}

/// The options of a field as one run of inlines: the prompt in bold, then one
/// numbered line per suggested answer.
pub fn field_inlines(prompt: &[Inline], options: &[Vec<Inline>]) -> Vec<Inline> {
    let mut inlines = vec![Inline::Strong(prompt.to_vec())];
    for (at, option) in options.iter().enumerate() {
        inlines.push(Inline::HardBreak);
        inlines.push(Inline::Text(format!("{}. ", at + 1)));
        inlines.extend(option.iter().cloned());
    }
    inlines
}

#[allow(clippy::too_many_arguments)]
fn pending_block(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    kind: PendingKind,
    text: RichText,
    height: f32,
) -> LaidBlock {
    let (padding, _) = frame(cx);
    LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height),
        stop,
        content: Content::Pending(PendingBox {
            kind,
            text,
            padding,
            hash: None,
            fill: None,
        }),
        depth: flow.depth,
    }
}

fn text_box(cx: &mut Cx, flow: Flow, inlines: &[Inline]) -> (RichText, f32) {
    let (padding, border) = frame(cx);
    let inner = (flow.width - 2.0 * (padding + border)).max(1.0);
    let style = TextStyle::sans(flow.ink);
    let text = inline::build_inlines(
        cx.fonts,
        inlines,
        style,
        cx.theme,
        cx.body_size(),
        cx.body_line(),
        Some(inner),
        false,
    );
    let height = text.height() + 2.0 * (padding + border);
    (text, height)
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
    let inlines = field_inlines(prompt, options);
    let (text, height) = text_box(cx, flow, &inlines);
    pending_block(
        cx,
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Field,
        text,
        height,
    )
}

/// The label an annotation of `kind` carries.
pub fn annotation_label(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Reply => "Reply",
        AnnotationKind::Comment => "Comment",
    }
}

/// A saved or pending reply or comment, or the editor box when `text` is a
/// box marker.
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
    let mut inlines = vec![
        Inline::Strong(vec![Inline::Text(annotation_label(kind).to_owned())]),
        Inline::HardBreak,
    ];
    for (at, line) in text.trim_end_matches('\n').split('\n').enumerate() {
        if at > 0 {
            inlines.push(Inline::HardBreak);
        }
        inlines.push(Inline::Text(line.to_owned()));
    }
    let (rich, height) = text_box(cx, flow, &inlines);
    pending_block(
        cx,
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Annotation,
        rich,
        height,
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
    let style = TextStyle::sans(flow.ink);
    let empty = inline::build_inlines(
        cx.fonts,
        &[],
        style,
        cx.theme,
        cx.body_size(),
        cx.body_line(),
        Some(flow.width),
        false,
    );
    pending_block(
        cx,
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Annotation,
        empty,
        height,
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
    use crate::{layout::lay_out, text::Fonts, theme::Theme};
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

    #[test]
    fn should_number_the_options_when_building_field_inlines() {
        let inlines = field_inlines(
            &[Inline::Text("Which?".into())],
            &[
                vec![Inline::Text("A".into())],
                vec![Inline::Text("B".into())],
            ],
        );
        let text: Vec<&str> = inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, ["1. ", "A", "2. ", "B"]);
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
    fn should_label_annotations_by_kind_when_laying_out_text() {
        let (_, layout) = laid("para\n\n<!-- limpid comment\nThis leaks.\n-->\n");
        let note = layout
            .blocks
            .iter()
            .find(|block| matches!(block.content, Content::Pending(ref p) if p.kind == PendingKind::Annotation))
            .expect("the saved comment is laid out");
        assert!(note.stop);
        assert!(note.rect.h > 0.0);
        assert_eq!(annotation_label(AnnotationKind::Comment), "Comment");
        assert_eq!(annotation_label(AnnotationKind::Reply), "Reply");
    }
}
