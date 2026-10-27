//! Lists: markers hung to the left of an indented column of item blocks.

use super::{
    Content, LaidBlock, Marker, PaintCx,
    block::{Cx, Flow, stack},
    inline::{self, Flat},
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
};
use limpid_core::doc::{Block, ListItem};
use std::ops::Range;

/// Indent of a list item's content, CSS px.
const INDENT: f32 = 26.0;
/// Indent of a footnote body, CSS px.
const FOOTNOTE_INDENT: f32 = 34.0;
/// Space between items, CSS px.
const ITEM_GAP: f32 = 6.0;
/// Space between the marker and the text, CSS px.
const MARKER_GAP: f32 = 8.0;

/// The span of source the item's blocks cover, else `fallback`.
pub fn item_source(children: &[Block], fallback: &Range<usize>) -> Range<usize> {
    let start = children.iter().map(|block| block.src.start).min();
    let end = children.iter().map(|block| block.src.end).max();
    match (start, end) {
        (Some(start), Some(end)) if start <= end => start..end,
        _ => fallback.clone(),
    }
}

pub fn lay_items(
    cx: &mut Cx,
    ordered: bool,
    start: u64,
    items: &[ListItem],
    list: &Block,
    y: f32,
    flow: Flow,
) -> (Vec<LaidBlock>, f32) {
    let indent = cx.px(INDENT).min(flow.width * 0.5);
    let mut laid = Vec::with_capacity(items.len());
    let mut item_y = y;
    let mut bottom = y;
    for (index, item) in items.iter().enumerate() {
        let item_flow = Flow {
            x: flow.x + indent,
            width: (flow.width - indent).max(1.0),
            ink: flow.ink,
            depth: flow.depth.saturating_add(1),
            compact: true,
            top_level: false,
        };
        let (children, children_bottom) = stack(cx, &item.blocks, item_y, item_flow);
        let content_bottom = if children.is_empty() {
            item_y + cx.body_line()
        } else {
            children_bottom
        };
        let marker = match item.checked {
            Some(checked) => Marker::Task { checked },
            None => {
                let label = if ordered {
                    format!("{}.", start.saturating_add(index as u64))
                } else {
                    "\u{2022}".to_owned()
                };
                Marker::Text(Box::new(marker_text(cx, &label, flow)))
            }
        };
        laid.push(LaidBlock {
            index,
            src: item_source(&item.blocks, &list.src),
            rect: Rect::new(flow.x, item_y, flow.width, content_bottom - item_y),
            stop: true,
            content: Content::Item { marker, children },
            depth: flow.depth,
        });
        bottom = content_bottom;
        item_y = bottom + cx.px(ITEM_GAP);
    }
    (laid, bottom)
}

fn marker_text(cx: &mut Cx, label: &str, flow: Flow) -> inline::RichText {
    let style = TextStyle::sans(flow.ink);
    let flat = Flat::plain(label, style);
    inline::build(
        cx.fonts,
        &flat,
        style,
        cx.theme,
        cx.body_size(),
        cx.body_line(),
        None,
    )
}

/// A footnote definition: a `[label]` hung beside its blocks.
pub fn lay_footnote(
    cx: &mut Cx,
    label: &str,
    blocks: &[Block],
    y: f32,
    flow: Flow,
) -> (Content, f32) {
    let indent = cx.px(FOOTNOTE_INDENT).min(flow.width * 0.5);
    let child_flow = Flow {
        x: flow.x + indent,
        width: (flow.width - indent).max(1.0),
        ink: cx.theme.muted,
        depth: flow.depth.saturating_add(1),
        compact: true,
        top_level: false,
    };
    let (children, bottom) = stack(cx, blocks, y, child_flow);
    let bottom = if children.is_empty() {
        y + cx.body_line()
    } else {
        bottom
    };
    let style = TextStyle::sans(cx.theme.teal);
    let flat = Flat::plain(&format!("[{label}]"), style);
    let marker = inline::build(
        cx.fonts,
        &flat,
        style,
        cx.theme,
        cx.body_size(),
        cx.body_line(),
        None,
    );
    (
        Content::Item {
            marker: Marker::Text(Box::new(marker)),
            children,
        },
        bottom - y,
    )
}

pub fn paint_item(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    let Content::Item { marker, children } = &block.content else {
        return;
    };
    let theme = paint.theme;
    let area = paint.window_rect(&block.rect);
    let content_x = children
        .first()
        .map_or(area.x + INDENT * paint.scale, |child| {
            paint.origin_x + child.rect.x
        });
    let marker_right = content_x - MARKER_GAP * paint.scale;
    match marker {
        Marker::Text(text) => {
            let x = marker_right - text.content_width();
            inline::paint(canvas, fonts, text, x, area.y, theme, paint.scale);
        }
        Marker::Task { checked } => {
            let line = theme.font_size * theme.line_height * paint.scale;
            let side = (theme.font_size * paint.scale * 0.78).round().max(6.0);
            let square = Rect::new(
                (marker_right - side).round(),
                (area.y + (line - side) / 2.0).round(),
                side,
                side,
            );
            let border = paint.scale.round().max(1.0);
            if *checked {
                canvas.fill_rect(square, theme.teal);
                draw_check(canvas, square, theme.bg, border);
            } else {
                canvas.stroke_rect(square, border, theme.muted);
            }
        }
    }
    for child in children {
        super::paint_block(paint, canvas, fonts, child);
    }
}

/// A tick mark inside `square`, drawn as two thick segments.
fn draw_check(canvas: &mut Canvas, square: Rect, color: crate::theme::Color, thickness: f32) {
    let point = |fx: f32, fy: f32| (square.x + square.w * fx, square.y + square.h * fy);
    let thickness = thickness.max(1.0) * 1.5;
    for ((x0, y0), (x1, y1)) in [
        (point(0.22, 0.52), point(0.42, 0.72)),
        (point(0.42, 0.72), point(0.80, 0.28)),
    ] {
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()) * 2.0).ceil().max(1.0) as u32;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            canvas.fill_rect(
                Rect::new(
                    x0 + (x1 - x0) * t - thickness / 2.0,
                    y0 + (y1 - y0) * t - thickness / 2.0,
                    thickness,
                    thickness,
                ),
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::lay_out, theme::Theme};
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn layout(markdown: &str) -> crate::layout::Layout {
        let doc = parse(markdown, Mode::File, &Settings::default());
        lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut Fonts::new())
    }

    fn items(layout: &crate::layout::Layout) -> &[LaidBlock] {
        layout.blocks[0].children()
    }

    #[test]
    fn should_lay_out_one_item_block_per_item_when_the_list_is_flat() {
        let layout = layout("- a\n- b\n- c\n");
        assert_eq!(items(&layout).len(), 3);
        let rows = items(&layout);
        assert_eq!(rows[1].rect.y - rows[0].rect.bottom(), 6.0);
        assert!(rows.iter().all(|item| item.stop));
    }

    #[test]
    fn should_indent_item_content_when_laying_out_a_list() {
        let layout = layout("- a\n");
        let item = &items(&layout)[0];
        assert_eq!(item.children()[0].rect.x - item.rect.x, 26.0);
        assert_eq!(item.children()[0].rect.w, item.rect.w - 26.0);
    }

    #[test]
    fn should_indent_further_when_lists_nest() {
        let layout = layout("- a\n  - b\n    - c\n");
        let outer = &items(&layout)[0];
        let nested_list = outer
            .children()
            .iter()
            .find(|child| matches!(child.content, Content::Children(_)))
            .expect("nested list");
        let inner = &nested_list.children()[0];
        assert_eq!(inner.rect.x, 26.0);
        let innermost = inner
            .children()
            .iter()
            .find(|child| matches!(child.content, Content::Children(_)))
            .expect("third level");
        assert_eq!(innermost.children()[0].rect.x, 52.0);
        // Every item at every depth is a stop.
        assert_eq!(layout.stops().count(), 3);
    }

    #[test]
    fn should_number_items_from_the_start_value_when_the_list_is_ordered() {
        let layout = layout("3. a\n4. b\n");
        let labels: Vec<_> = items(&layout)
            .iter()
            .map(|item| match &item.content {
                Content::Item {
                    marker: Marker::Text(text),
                    ..
                } => text.block.glyphs().count(),
                _ => 0,
            })
            .collect();
        assert_eq!(labels, vec![2, 2]);
    }

    #[test]
    fn should_use_checkbox_markers_when_items_are_tasks() {
        let layout = layout("- [x] done\n- [ ] todo\n- plain\n");
        let kinds: Vec<_> = items(&layout)
            .iter()
            .map(|item| match &item.content {
                Content::Item { marker, .. } => match marker {
                    Marker::Task { checked } => Some(*checked),
                    Marker::Text(_) => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec![Some(true), Some(false), None]);
    }

    #[test]
    fn should_give_an_empty_item_one_line_of_height_when_it_has_no_content() {
        let layout = layout("-\n- b\n");
        let rows = items(&layout);
        assert!(rows[0].rect.h > 20.0, "{}", rows[0].rect.h);
    }

    #[test]
    fn should_paint_marker_and_checkbox_pixels_left_of_the_text_when_painting() {
        let doc = parse("- [x] done\n- item\n", Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let theme = Theme::dark();
        let mut layout = lay_out(&doc, 800.0, &theme, 1.0, &mut fonts);
        let mut pixels = vec![theme.bg.to_pixel(); 800 * 200];
        layout.paint(&mut Canvas::new(&mut pixels, 800, 200), &mut fonts, 0.0);
        let teal = theme.teal.to_pixel();
        assert!(pixels.contains(&teal), "checked box is teal");
        let first = &layout.blocks[0].children()[0];
        let text_x = (layout.column_x + first.children()[0].rect.x) as usize;
        let marker_pixels = (0..200usize)
            .flat_map(|y| (text_x.saturating_sub(40)..text_x - 1).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[y * 800 + x] != theme.bg.to_pixel())
            .count();
        assert!(marker_pixels > 10, "{marker_pixels}");
    }

    #[test]
    fn should_not_panic_when_the_list_is_deeply_nested() {
        let mut source = String::new();
        for depth in 0..60 {
            source.push_str(&"  ".repeat(depth));
            source.push_str("- x\n");
        }
        let layout = layout(&source);
        assert!(!layout.blocks.is_empty());
    }
}
