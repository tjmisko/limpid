//! Fenced code: a bordered block with an optional language caption, token
//! colours from the highlight palette, and either wrapped lines or a
//! horizontal scroll.

use super::{
    Content, LaidBlock, PaintCx,
    block::{Cx, Flow},
    inline::{self, Flat, RichText},
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::doc::HlSpan;
use std::ops::Range;

/// Code text size, CSS px at 17px body text.
const CODE_SIZE: f32 = 13.0;
const CODE_LINE_HEIGHT: f32 = 1.6;

/// Split `text` into byte ranges of one colour each, innermost highlight
/// span winning. Spans the palette has no entry for leave the outer colour.
pub fn token_runs(text: &str, spans: &[HlSpan], theme: &Theme) -> Vec<(Range<usize>, Color, bool)> {
    let length = text.len();
    let mut class_at: Vec<u32> = vec![u32::MAX; length];
    for (index, span) in spans.iter().enumerate() {
        if theme.hl.for_class(&span.class).is_none() {
            continue;
        }
        let start = span.range.start.min(length);
        let end = span.range.end.min(length);
        if start < end {
            class_at[start..end].fill(index as u32);
        }
    }
    let mut runs = Vec::new();
    let mut start = 0;
    while start < length {
        let class = class_at[start];
        let mut end = start + 1;
        while end < length && class_at[end] == class {
            end += 1;
        }
        while end < length && !text.is_char_boundary(end) {
            end += 1;
        }
        let (color, italic) = match spans.get(class as usize) {
            Some(span) => (
                theme.hl.for_class(&span.class).unwrap_or(theme.ink),
                span.class.trim_start_matches("hl-").starts_with("comment"),
            ),
            None => (theme.ink, false),
        };
        runs.push((start..end, color, italic));
        start = end;
    }
    runs
}

pub fn lay(
    cx: &mut Cx,
    lang: Option<&str>,
    text: &str,
    spans: &[HlSpan],
    flow: Flow,
) -> (Content, f32) {
    let border = cx.px(1.0).round().max(1.0);
    let pad_x = cx.px(18.0);
    let pad_y = cx.px(16.0);
    let size = cx.font_px(CODE_SIZE);
    let line = size * CODE_LINE_HEIGHT;
    let inner_width = (flow.width - 2.0 * border - 2.0 * pad_x).max(1.0);

    let trimmed = text.trim_end_matches(['\n', '\r']);
    let mut flat = Flat::default();
    let base = TextStyle::mono(cx.theme.ink);
    for (range, color, italic) in token_runs(trimmed, spans, cx.theme) {
        let style = TextStyle {
            color,
            italic,
            ..base
        };
        flat.push(&trimmed[range], style);
    }
    let wrap = cx.options.code_wrap;
    let body = inline::build(
        cx.fonts,
        &flat,
        base,
        cx.theme,
        size,
        line,
        wrap.then_some(inner_width),
    );
    let text_width = body.content_width();
    let overflow = !wrap && text_width > inner_width + 0.5;

    let caption = lang.filter(|lang| !lang.trim().is_empty()).map(|lang| {
        let caption_size = cx.font_px(10.5);
        let style = TextStyle::mono(cx.theme.faint);
        let flat = Flat::plain(&lang.trim().to_uppercase(), style);
        inline::build(
            cx.fonts,
            &flat,
            style,
            cx.theme,
            caption_size,
            caption_size * 1.4,
            None,
        )
    });
    let caption_height = caption
        .as_ref()
        .map_or(0.0, |caption| caption.height() + 2.0 * cx.px(8.0) + border);
    let height = 2.0 * border + caption_height + 2.0 * pad_y + body.height();
    (
        Content::Code {
            text: body,
            gutter: pad_x,
            scroll_x: 0.0,
            overflow,
            text_width,
            caption,
            caption_height,
            pad_y,
            border,
        },
        height,
    )
}

pub fn paint(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    let Content::Code {
        text,
        gutter,
        scroll_x,
        overflow,
        caption,
        caption_height,
        pad_y,
        border,
        ..
    } = &block.content
    else {
        return;
    };
    let theme = paint.theme;
    let area = paint.window_rect(&block.rect);
    canvas.fill_rect(area, theme.code_bg);
    canvas.stroke_rect(area, *border, theme.rule);
    let inner_x = area.x + border;
    let inner_width = area.w - 2.0 * border;
    if let Some(caption) = caption {
        let band = Rect::new(inner_x, area.y + border, inner_width, *caption_height);
        canvas.fill_rect(band, theme.surface);
        canvas.fill_rect(
            Rect::new(inner_x, band.bottom() - border, inner_width, *border),
            theme.rule,
        );
        inline::paint(
            canvas,
            fonts,
            caption,
            inner_x + 14.0 * paint.scale,
            band.y + 8.0 * paint.scale,
            theme,
            paint.scale,
        );
    }
    let body_top = area.y + border + caption_height;
    let body_height = area.h - 2.0 * border - caption_height;
    if !overflow {
        inline::paint(
            canvas,
            fonts,
            text,
            inner_x + gutter,
            body_top + pad_y,
            theme,
            paint.scale,
        );
        return;
    }
    paint_clipped(
        canvas,
        fonts,
        text,
        Rect::new(inner_x, body_top, inner_width, body_height),
        (gutter - scroll_x, *pad_y),
        paint,
    );
}

/// Draw `text` inside `clip` through an off-screen buffer, because the canvas
/// has no clip rectangle of its own. Only the rows that are on screen are
/// buffered. `offset` is the text origin relative to the clip's top left.
fn paint_clipped(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    text: &RichText,
    clip: Rect,
    offset: (f32, f32),
    paint: &PaintCx,
) {
    let theme = paint.theme;
    let top = clip.y.max(0.0).round();
    let bottom = clip.bottom().min(canvas.height() as f32).round();
    let left = clip.x.max(0.0).round();
    let right = clip.right().min(canvas.width() as f32).round();
    if bottom <= top || right <= left {
        return;
    }
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    let mut pixels = vec![theme.code_bg.to_pixel(); width as usize * height as usize];
    {
        let mut buffer = Canvas::new(&mut pixels, width, height);
        inline::paint(
            &mut buffer,
            fonts,
            text,
            clip.x + offset.0 - left,
            clip.y + offset.1 - top,
            theme,
            paint.scale,
        );
    }
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return;
    };
    for (pixel, out) in pixels
        .iter()
        .zip(pixmap.data_mut().as_chunks_mut::<4>().0.iter_mut())
    {
        out.copy_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255]);
    }
    canvas.draw_pixmap(&pixmap, left, top);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, LayoutOptions, lay_out_with};
    use limpid_core::{
        doc::{Doc, Mode, parse},
        settings::Settings,
    };

    fn span(range: Range<usize>, class: &str) -> HlSpan {
        HlSpan {
            range,
            class: class.to_owned(),
        }
    }

    #[test]
    fn should_colour_runs_by_class_when_spans_cover_the_text() {
        let theme = Theme::dark();
        let runs = token_runs("fn x", &[span(0..2, "keyword")], &theme);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], (0..2, theme.hl.keyword, false));
        assert_eq!(runs[1], (2..4, theme.ink, false));
    }

    #[test]
    fn should_let_the_inner_span_win_when_spans_nest() {
        let theme = Theme::dark();
        let spans = [span(0..6, "string"), span(2..4, "constant")];
        let runs = token_runs("abcdef", &spans, &theme);
        let colors: Vec<_> = runs.iter().map(|r| r.1).collect();
        assert_eq!(
            colors,
            vec![theme.hl.string, theme.hl.constant, theme.hl.string]
        );
    }

    #[test]
    fn should_keep_the_outer_colour_when_the_inner_class_is_unknown() {
        let theme = Theme::dark();
        let spans = [span(0..4, "keyword"), span(1..2, "punctuation-bracket")];
        let runs = token_runs("abcd", &spans, &theme);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, theme.hl.keyword);
    }

    #[test]
    fn should_italicise_comments_and_ignore_out_of_range_spans() {
        let theme = Theme::dark();
        let runs = token_runs(
            "// hi",
            &[span(0..5, "comment"), span(3..99, "string")],
            &theme,
        );
        assert!(runs.iter().any(|r| r.2));
        assert_eq!(runs.last().unwrap().0.end, 5);
    }

    #[test]
    fn should_never_split_inside_a_character_when_span_edges_are_misaligned() {
        let theme = Theme::dark();
        let text = "a\u{e9}\u{4e16}b";
        let runs = token_runs(text, &[span(2..3, "keyword")], &theme);
        for (range, _, _) in &runs {
            assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
        }
        assert_eq!(runs.iter().map(|r| r.0.len()).sum::<usize>(), text.len());
    }

    #[test]
    fn should_return_no_runs_when_the_text_is_empty() {
        assert!(token_runs("", &[span(0..3, "keyword")], &Theme::dark()).is_empty());
    }

    fn layout_with(markdown: &str, width: f32, options: LayoutOptions) -> (Doc, Layout) {
        let doc = parse(markdown, Mode::File, &Settings::default());
        let layout = lay_out_with(&doc, width, &Theme::dark(), 1.0, &mut Fonts::new(), options);
        (doc, layout)
    }

    #[test]
    fn should_flag_overflow_when_a_line_is_wider_than_the_block_and_wrap_is_off() {
        let source = format!("```\n{}\n```\n", "w".repeat(300));
        let (_, layout) = layout_with(&source, 600.0, LayoutOptions::default());
        let Content::Code {
            overflow,
            text_width,
            ..
        } = &layout.blocks[0].content
        else {
            panic!("not code");
        };
        assert!(*overflow && *text_width > 500.0);
    }

    #[test]
    fn should_wrap_and_grow_taller_when_code_wrap_is_on() {
        let source = format!("```\n{}\n```\n", "w ".repeat(150));
        let (_, scrolled) = layout_with(&source, 600.0, LayoutOptions::default());
        let (_, wrapped) = layout_with(
            &source,
            600.0,
            LayoutOptions {
                code_wrap: true,
                ..LayoutOptions::default()
            },
        );
        let Content::Code { overflow, .. } = &wrapped.blocks[0].content else {
            panic!("not code");
        };
        assert!(!*overflow);
        assert!(wrapped.blocks[0].rect.h > scrolled.blocks[0].rect.h * 1.5);
    }

    #[test]
    fn should_add_a_caption_only_when_the_fence_names_a_language() {
        let (_, layout) = layout_with(
            "```rust\nfn a() {}\n```\n\n```\nplain\n```\n",
            800.0,
            LayoutOptions::default(),
        );
        let heights: Vec<_> = layout
            .blocks
            .iter()
            .map(|b| match &b.content {
                Content::Code {
                    caption,
                    caption_height,
                    ..
                } => (caption.is_some(), *caption_height),
                _ => panic!("not code"),
            })
            .collect();
        assert!(heights[0].0 && heights[0].1 > 0.0);
        assert!(!heights[1].0 && heights[1].1 == 0.0);
    }

    #[test]
    fn should_handle_an_unclosed_fence_and_unknown_language_when_laying_out() {
        let (_, layout) = layout_with(
            "```zzz-unknown\nstill open",
            800.0,
            LayoutOptions::default(),
        );
        assert_eq!(layout.blocks.len(), 1);
        assert!(matches!(layout.blocks[0].content, Content::Code { .. }));
    }

    #[test]
    fn should_draw_highlight_colours_inside_the_block_when_painting() {
        // `parse` leaves spans empty; the viewer fills them on demand.
        let mut doc = parse(
            "```rust\nfn main() {}\n```\n",
            Mode::File,
            &Settings::default(),
        );
        let mut highlighter = limpid_core::highlight::Highlighter::new();
        if let limpid_core::doc::BlockKind::Code { text, spans, .. } = &mut doc.blocks[0].kind {
            *spans = limpid_core::highlight::highlight_spans("rust", text, &mut highlighter);
        }
        let mut fonts = Fonts::new();
        let mut layout = lay_out_with(
            &doc,
            700.0,
            &Theme::dark(),
            1.0,
            &mut fonts,
            LayoutOptions::default(),
        );
        let mut pixels = vec![0u32; 700 * 200];
        layout.paint(&mut Canvas::new(&mut pixels, 700, 200), &mut fonts, 0.0);
        let theme = Theme::dark();
        let keyword = theme.hl.keyword.to_pixel();
        let near = |pixel: u32| {
            let (r, g, b) = ((pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255);
            let k = (keyword >> 16 & 255, keyword >> 8 & 255, keyword & 255);
            r.abs_diff(k.0) < 40 && g.abs_diff(k.1) < 40 && b.abs_diff(k.2) < 40
        };
        assert!(
            pixels.iter().any(|&p| near(p)),
            "no keyword-coloured pixels"
        );
    }

    #[test]
    fn should_keep_overflowing_text_inside_the_block_when_painting_scrolled_code() {
        let source = format!("```\n{}\n```\n", "w".repeat(600));
        let (_, mut layout) = layout_with(&source, 700.0, LayoutOptions::default());
        let mut fonts = Fonts::new();
        let theme = Theme::dark();
        let background = theme.bg.to_pixel();
        let mut pixels = vec![background; 700 * 200];
        layout.paint(&mut Canvas::new(&mut pixels, 700, 200), &mut fonts, 0.0);
        let block = layout.blocks[0].rect;
        let right_edge = (layout.column_x + block.right()) as usize;
        // Nothing but background to the right of the block's border.
        for y in 0..200usize {
            for x in right_edge + 2..700 {
                assert_eq!(pixels[y * 700 + x], background, "({x},{y})");
            }
        }
    }
}
