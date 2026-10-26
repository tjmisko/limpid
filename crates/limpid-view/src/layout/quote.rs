//! Block quotes and GitHub-style callouts: a coloured bar and tint, with the
//! quoted blocks inset beside the bar.

use super::{
    Content, LaidBlock, PaintCx,
    block::{Cx, Flow, stack},
    inline::{self, Flat},
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::{
    doc::{Block, Callout, CalloutKind},
    transport::Scheme,
};

const BAR_WIDTH: f32 = 3.0;
const PAD_X: f32 = 18.0;
const PAD_Y: f32 = 4.0;
const TITLE_GAP: f32 = 6.0;

/// The accent of a callout kind. A plain quote uses the teal of `page.css`.
pub fn accent(theme: &Theme, kind: Option<CalloutKind>) -> Color {
    let Some(kind) = kind else {
        return theme.teal;
    };
    match kind {
        CalloutKind::Note => theme.blue,
        CalloutKind::Tip | CalloutKind::Ask => theme.teal,
        CalloutKind::Important => theme.purple,
        CalloutKind::Warning => theme.amber,
        CalloutKind::Caution => match theme.scheme {
            Scheme::Dark => Color::rgb(0xf0, 0x8a, 0x8a),
            Scheme::Light => Color::rgb(0xb0, 0x3a, 0x3a),
        },
    }
}

/// `color` as a translucent wash behind the quote.
pub fn tint(color: Color, callout: bool) -> Color {
    Color::rgba(color.r, color.g, color.b, if callout { 22 } else { 13 })
}

pub fn lay(
    cx: &mut Cx,
    callout: Option<&Callout>,
    blocks: &[Block],
    y: f32,
    flow: Flow,
) -> (Content, f32) {
    let bar = cx.px(BAR_WIDTH);
    let inset = (bar + cx.px(PAD_X)).min(flow.width * 0.4);
    let pad_y = cx.px(PAD_Y);
    let color = accent(cx.theme, callout.map(|callout| callout.kind));
    let ink = if callout.is_some() {
        flow.ink
    } else {
        cx.theme.muted
    };

    let title = callout
        .filter(|callout| !callout.title.is_empty())
        .map(|callout| {
            let size = cx.font_px(11.0);
            let style = TextStyle::mono(color).bold();
            let flat = Flat::plain(&callout.title.to_uppercase(), style);
            inline::build(
                cx.fonts,
                &flat,
                style,
                cx.theme,
                size,
                size * 1.5,
                Some((flow.width - inset - cx.px(PAD_X)).max(1.0)),
            )
        });
    let title_height = title
        .as_ref()
        .map_or(0.0, |title| title.height() + cx.px(TITLE_GAP));

    let child_flow = flow.child(flow.x + inset, flow.width - inset - cx.px(PAD_X), ink);
    let (children, children_bottom) = stack(cx, blocks, y + pad_y + title_height, child_flow);
    let children_height = if children.is_empty() {
        0.0
    } else {
        children_bottom - (y + pad_y + title_height)
    };
    let height = 2.0 * pad_y + title_height + children_height;
    (
        Content::Quote {
            bar: color,
            tint: tint(color, callout.is_some()),
            title,
            inset,
            pad_y,
            children,
        },
        height,
    )
}

pub fn paint(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    let Content::Quote {
        bar,
        tint,
        title,
        inset,
        pad_y,
        children,
    } = &block.content
    else {
        return;
    };
    let area = paint.window_rect(&block.rect);
    canvas.fill_rect(area, *tint);
    canvas.fill_rect(
        Rect::new(
            area.x,
            area.y,
            (BAR_WIDTH * paint.scale).round().max(1.0),
            area.h,
        ),
        *bar,
    );
    if let Some(title) = title {
        inline::paint(
            canvas,
            fonts,
            title,
            area.x + inset,
            area.y + pad_y,
            paint.theme,
            paint.scale,
        );
    }
    for child in children {
        super::paint_block(paint, canvas, fonts, child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, lay_out};
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn layout(markdown: &str) -> (Layout, Fonts) {
        let doc = parse(markdown, Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let layout = lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut fonts);
        (layout, fonts)
    }

    fn quote_parts(
        layout: &Layout,
        index: usize,
    ) -> (&Option<inline::RichText>, f32, &Vec<LaidBlock>) {
        match &layout.blocks[index].content {
            Content::Quote {
                title,
                inset,
                children,
                ..
            } => (title, *inset, children),
            _ => panic!("not a quote"),
        }
    }

    #[test]
    fn should_inset_children_beside_the_bar_when_laying_out_a_quote() {
        let (layout, _) = layout("> quoted\n");
        let (title, inset, children) = quote_parts(&layout, 0);
        assert!(title.is_none());
        assert_eq!(inset, 21.0);
        assert_eq!(children[0].rect.x, 21.0);
        assert!(layout.blocks[0].rect.h > children[0].rect.h);
        assert!(layout.blocks[0].stop && !children[0].stop);
    }

    #[test]
    fn should_add_a_title_row_when_the_quote_is_a_callout() {
        let (layout, _) = layout("> [!WARNING]\n> careful\n\n> plain\n");
        let (title, _, _) = quote_parts(&layout, 0);
        assert!(title.is_some());
        assert!(layout.blocks[0].rect.h > layout.blocks[1].rect.h);
    }

    #[test]
    fn should_pick_a_distinct_accent_per_kind_when_resolving_callouts() {
        let theme = Theme::dark();
        let kinds = [
            CalloutKind::Note,
            CalloutKind::Important,
            CalloutKind::Warning,
            CalloutKind::Caution,
        ];
        let colors: Vec<_> = kinds.iter().map(|&k| accent(&theme, Some(k))).collect();
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(accent(&theme, None), theme.teal);
        assert_eq!(accent(&theme, Some(CalloutKind::Tip)), theme.teal);
    }

    #[test]
    fn should_handle_nested_quotes_and_an_empty_quote_when_laying_out() {
        let (layout, _) = layout("> a\n>\n> > inner\n\n>\n");
        assert_eq!(layout.blocks.len(), 2);
        let (_, _, outer) = quote_parts(&layout, 0);
        assert!(
            outer
                .iter()
                .any(|c| matches!(c.content, Content::Quote { .. }))
        );
        assert!(layout.blocks[1].rect.h > 0.0);
    }

    #[test]
    fn should_paint_the_bar_and_a_tint_when_painting_a_quote() {
        let doc = parse("> [!TIP]\n> hint\n", Mode::File, &Settings::default());
        let theme = Theme::dark();
        let mut fonts = Fonts::new();
        let mut layout = lay_out(&doc, 800.0, &theme, 1.0, &mut fonts);
        let mut pixels = vec![theme.bg.to_pixel(); 800 * 200];
        layout.paint(&mut Canvas::new(&mut pixels, 800, 200), &mut fonts, 0.0);
        let block = layout.blocks[0].rect;
        let x = layout.column_x as usize + 1;
        let y = (block.y + block.h / 2.0) as usize;
        assert_eq!(pixels[y * 800 + x], theme.teal.to_pixel(), "bar");
        let tinted = pixels[y * 800 + x + 12];
        assert_ne!(tinted, theme.bg.to_pixel());
        assert_ne!(tinted, theme.teal.to_pixel());
    }
}
