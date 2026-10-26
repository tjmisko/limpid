//! Inline content: a run of `Inline` nodes flattened into one string with
//! styled byte ranges, shaped by cosmic-text, plus the rectangles for the
//! decorations cosmic-text does not draw (inline code backgrounds, strikes).

use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, Span, TextBlock, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::doc::Inline;
use std::ops::Range;

/// Deepest inline nesting followed; anything deeper is dropped.
const MAX_INLINE_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationKind {
    /// Inline code and inline math source: a tinted, outlined box behind text.
    CodeSpan,
    /// A line through the middle of the text.
    Strike,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoration {
    pub kind: DecorationKind,
    /// Byte range into [`Flat::text`].
    pub range: Range<usize>,
    pub color: Color,
}

/// Inline nodes flattened to a string and styled byte ranges.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Flat {
    pub text: String,
    pub runs: Vec<(Range<usize>, TextStyle)>,
    pub decorations: Vec<Decoration>,
}

impl Flat {
    /// A single run of `text` in `style`, with line-break characters other
    /// than `\n` and tabs made safe for the shaper.
    pub fn plain(text: &str, style: TextStyle) -> Self {
        let mut flat = Flat::default();
        flat.push(text, style);
        flat
    }

    /// Append `text` in `style`, merging with the previous run when equal.
    pub fn push(&mut self, text: &str, style: TextStyle) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        for character in text.chars() {
            match character {
                '\t' => self.text.push_str("    "),
                '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                    self.text.push(' ')
                }
                other => self.text.push(other),
            }
        }
        let end = self.text.len();
        if end == start {
            return;
        }
        if let Some((range, last)) = self.runs.last_mut()
            && *last == style
            && range.end == start
        {
            range.end = end;
            return;
        }
        self.runs.push((start..end, style));
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Flatten `inlines`. `upper` upper-cases the text (the `h4` style).
pub fn flatten(inlines: &[Inline], base: TextStyle, theme: &Theme, upper: bool) -> Flat {
    let mut flattener = Flattener {
        theme,
        upper,
        flat: Flat::default(),
    };
    flattener.walk(inlines, base, 0);
    flattener.flat
}

struct Flattener<'a> {
    theme: &'a Theme,
    upper: bool,
    flat: Flat,
}

impl Flattener<'_> {
    fn text(&mut self, text: &str, style: TextStyle) {
        if self.upper {
            self.flat.push(&text.to_uppercase(), style);
            return;
        }
        self.flat.push(text, style);
    }

    fn walk(&mut self, inlines: &[Inline], style: TextStyle, depth: usize) {
        if depth > MAX_INLINE_DEPTH {
            return;
        }
        for inline in inlines {
            match inline {
                Inline::Text(text) => self.text(text, style),
                Inline::Code(code) => {
                    let code_style = TextStyle {
                        mono: true,
                        color: self.theme.blue,
                        ..style
                    };
                    self.decorated(DecorationKind::CodeSpan, code_style, |this| {
                        this.text(code, code_style)
                    });
                }
                Inline::SoftBreak => self.text(" ", style),
                Inline::HardBreak => self.flat.push("\n", style),
                Inline::Emph(children) => self.walk(children, style.italic(), depth + 1),
                Inline::Strong(children) => self.walk(children, style.bold(), depth + 1),
                Inline::Strike(children) => {
                    self.decorated(DecorationKind::Strike, style, |this| {
                        this.walk(children, style, depth + 1)
                    });
                }
                Inline::Link { inlines, .. } => {
                    let link = TextStyle {
                        color: self.theme.teal,
                        ..style
                    };
                    self.walk(inlines, link, depth + 1);
                }
                Inline::Image { alt, .. } => {
                    let label = if alt.is_empty() { "image" } else { alt };
                    let muted = TextStyle {
                        color: self.theme.muted,
                        ..style
                    };
                    self.text(&format!("[{label}]"), muted);
                }
                Inline::Math { latex, .. } => {
                    // Typeset boxes arrive with the pending-box work; until
                    // the cache has an image the source shows as code.
                    let math_style = TextStyle {
                        mono: true,
                        color: self.theme.purple,
                        ..style
                    };
                    self.decorated(DecorationKind::CodeSpan, math_style, |this| {
                        this.text(latex, math_style)
                    });
                }
                Inline::FootnoteRef(label) => {
                    let muted = TextStyle {
                        color: self.theme.teal,
                        ..style
                    };
                    self.text(&format!("[{label}]"), muted);
                }
                Inline::Html(_) => {}
            }
        }
    }

    fn decorated(&mut self, kind: DecorationKind, style: TextStyle, body: impl FnOnce(&mut Self)) {
        let start = self.flat.text.len();
        body(self);
        let end = self.flat.text.len();
        if end > start {
            self.flat.decorations.push(Decoration {
                kind,
                range: start..end,
                color: style.color,
            });
        }
    }
}

/// One decoration on one visual line, in block-local pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecoRect {
    pub kind: DecorationKind,
    pub x: f32,
    pub baseline: f32,
    pub width: f32,
    pub color: Color,
}

/// Shaped inline text with the data needed to paint its decorations.
pub struct RichText {
    pub block: TextBlock,
    pub decorations: Vec<DecoRect>,
    /// Device pixels.
    pub font_size: f32,
}

impl RichText {
    pub fn height(&self) -> f32 {
        self.block.height()
    }

    /// Widest line, in pixels.
    pub fn content_width(&self) -> f32 {
        self.block
            .glyphs()
            .map(|glyph| glyph.x + glyph.width)
            .fold(0.0, f32::max)
    }
}

/// Shape `flat` into a block. `width` is where lines wrap (`None` never
/// wraps); sizes are device pixels.
pub fn build(
    fonts: &mut Fonts,
    flat: &Flat,
    base: TextStyle,
    theme: &Theme,
    font_size: f32,
    line_height: f32,
    width: Option<f32>,
) -> RichText {
    let mut block = TextBlock::new(fonts, font_size, line_height);
    let spans: Vec<Span<'_>> = flat
        .runs
        .iter()
        .map(|(range, style)| Span {
            text: &flat.text[range.clone()],
            style: *style,
        })
        .collect();
    block.set_spans(fonts, &spans, base, &theme.families);
    block.set_width(fonts, width.map(|width| width.max(1.0)));
    let decorations = decoration_rects(&block, flat);
    RichText {
        block,
        decorations,
        font_size,
    }
}

/// Shape `inlines` in one go.
#[allow(clippy::too_many_arguments)]
pub fn build_inlines(
    fonts: &mut Fonts,
    inlines: &[Inline],
    base: TextStyle,
    theme: &Theme,
    font_size: f32,
    line_height: f32,
    width: Option<f32>,
    upper: bool,
) -> RichText {
    let flat = flatten(inlines, base, theme, upper);
    build(fonts, &flat, base, theme, font_size, line_height, width)
}

fn decoration_rects(block: &TextBlock, flat: &Flat) -> Vec<DecoRect> {
    if flat.decorations.is_empty() {
        return Vec::new();
    }
    let mut line_starts = vec![0usize];
    for (at, byte) in flat.text.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(at + 1);
        }
    }
    let mut order: Vec<usize> = (0..flat.decorations.len()).collect();
    order.sort_by_key(|&index| flat.decorations[index].range.start);

    struct Open {
        index: usize,
        baseline: f32,
        left: f32,
        right: f32,
    }
    let mut next = 0;
    let mut active: Vec<usize> = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    let mut rects = Vec::new();
    let flush = |entry: &Open, rects: &mut Vec<DecoRect>| {
        let decoration = &flat.decorations[entry.index];
        rects.push(DecoRect {
            kind: decoration.kind,
            x: entry.left,
            baseline: entry.baseline,
            width: entry.right - entry.left,
            color: decoration.color,
        });
    };
    for glyph in block.glyphs() {
        let Some(&line_start) = line_starts.get(glyph.line) else {
            continue;
        };
        let offset = line_start + glyph.start;
        while next < order.len() && flat.decorations[order[next]].range.start <= offset {
            active.push(order[next]);
            next += 1;
        }
        active.retain(|&index| flat.decorations[index].range.end > offset);
        for &index in &active {
            let existing = open.iter_mut().find(|entry| entry.index == index);
            match existing {
                Some(entry) if (entry.baseline - glyph.baseline).abs() < 0.5 => {
                    entry.left = entry.left.min(glyph.x);
                    entry.right = entry.right.max(glyph.x + glyph.width);
                }
                Some(entry) => {
                    flush(entry, &mut rects);
                    *entry = Open {
                        index,
                        baseline: glyph.baseline,
                        left: glyph.x,
                        right: glyph.x + glyph.width,
                    };
                }
                None => open.push(Open {
                    index,
                    baseline: glyph.baseline,
                    left: glyph.x,
                    right: glyph.x + glyph.width,
                }),
            }
        }
    }
    for entry in &open {
        flush(entry, &mut rects);
    }
    rects
}

/// Backdrop of inline code, one step above `surface`.
pub fn raised(theme: &Theme) -> Color {
    match theme.scheme {
        limpid_core::transport::Scheme::Dark => Color::rgb(0x18, 0x21, 0x2a),
        limpid_core::transport::Scheme::Light => Color::rgb(0xe8, 0xe6, 0xdf),
    }
}

/// The faint rule colour used between table rows and above `h2`.
pub fn rule_soft(theme: &Theme) -> Color {
    match theme.scheme {
        limpid_core::transport::Scheme::Dark => Color::rgb(0x1b, 0x24, 0x2d),
        limpid_core::transport::Scheme::Light => Color::rgb(0xe3, 0xe0, 0xd8),
    }
}

/// Paint `text` with its top-left corner at `(x, y)`.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    text: &RichText,
    x: f32,
    y: f32,
    theme: &Theme,
    scale: f32,
) {
    let size = text.font_size;
    let line = scale.max(1.0).round();
    for deco in &text.decorations {
        if deco.kind != DecorationKind::CodeSpan {
            continue;
        }
        let rect = Rect::new(
            x + deco.x - size * 0.2,
            y + deco.baseline - size * 0.98,
            deco.width + size * 0.4,
            size * 1.3,
        );
        canvas.fill_rect(rect, raised(theme));
        canvas.stroke_rect(rect, line, theme.rule);
    }
    canvas.draw_text(fonts, &text.block, x, y, theme.ink);
    for deco in &text.decorations {
        if deco.kind != DecorationKind::Strike {
            continue;
        }
        canvas.fill_rect(
            Rect::new(x + deco.x, y + deco.baseline - size * 0.3, deco.width, line),
            deco.color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::dark()
    }

    fn base() -> TextStyle {
        TextStyle::sans(theme().ink)
    }

    fn text(value: &str) -> Inline {
        Inline::Text(value.to_owned())
    }

    #[test]
    fn should_style_runs_by_nesting_when_emphasis_wraps_strong() {
        let inlines = vec![
            text("a "),
            Inline::Emph(vec![text("b "), Inline::Strong(vec![text("c")])]),
        ];
        let flat = flatten(&inlines, base(), &theme(), false);
        assert_eq!(flat.text, "a b c");
        let styles: Vec<_> = flat.runs.iter().map(|(_, s)| (s.bold, s.italic)).collect();
        assert_eq!(styles, vec![(false, false), (false, true), (true, true)]);
    }

    #[test]
    fn should_merge_adjacent_runs_when_styles_match() {
        let inlines = vec![text("a"), text("b"), Inline::SoftBreak, text("c")];
        let flat = flatten(&inlines, base(), &theme(), false);
        assert_eq!(flat.text, "ab c");
        assert_eq!(flat.runs.len(), 1);
    }

    #[test]
    fn should_mark_code_in_mono_blue_with_a_decoration_when_flattening_code() {
        let flat = flatten(
            &[text("x "), Inline::Code("y".into())],
            base(),
            &theme(),
            false,
        );
        let (range, style) = &flat.runs[1];
        assert_eq!(&flat.text[range.clone()], "y");
        assert!(style.mono);
        assert_eq!(style.color, theme().blue);
        assert_eq!(flat.decorations.len(), 1);
        assert_eq!(flat.decorations[0].kind, DecorationKind::CodeSpan);
        assert_eq!(flat.decorations[0].range, 2..3);
    }

    #[test]
    fn should_colour_links_teal_and_strike_with_a_decoration() {
        let inlines = vec![
            Inline::Link {
                href: "u".into(),
                title: None,
                inlines: vec![text("link")],
            },
            Inline::Strike(vec![text("gone")]),
        ];
        let flat = flatten(&inlines, base(), &theme(), false);
        assert_eq!(flat.runs[0].1.color, theme().teal);
        assert_eq!(flat.decorations[0].kind, DecorationKind::Strike);
        assert_eq!(&flat.text[flat.decorations[0].range.clone()], "gone");
    }

    #[test]
    fn should_use_a_newline_for_hard_breaks_and_a_space_for_soft_breaks() {
        let inlines = vec![text("a"), Inline::HardBreak, text("b"), Inline::SoftBreak];
        let flat = flatten(&inlines, base(), &theme(), false);
        assert_eq!(flat.text, "a\nb ");
    }

    #[test]
    fn should_uppercase_text_when_requested() {
        let flat = flatten(&[text("straße")], base(), &theme(), true);
        assert_eq!(flat.text, "STRASSE");
    }

    #[test]
    fn should_drop_raw_html_and_label_images_and_footnotes_when_flattening() {
        let inlines = vec![
            Inline::Html("<b>".into()),
            Inline::Image {
                src: "a.png".into(),
                alt: "chart".into(),
            },
            Inline::Image {
                src: "b.png".into(),
                alt: String::new(),
            },
            Inline::FootnoteRef("1".into()),
        ];
        let flat = flatten(&inlines, base(), &theme(), false);
        assert_eq!(flat.text, "[chart][image][1]");
    }

    #[test]
    fn should_replace_tabs_and_odd_line_breaks_when_pushing_text() {
        let flat = Flat::plain("a\tb\rc\u{2028}d\ne", base());
        assert_eq!(flat.text, "a    b c d\ne");
    }

    #[test]
    fn should_stop_descending_when_nesting_is_hostile() {
        let mut inline = text("deep");
        for _ in 0..5000 {
            inline = Inline::Emph(vec![inline]);
        }
        let flat = flatten(&[inline], base(), &theme(), false);
        assert!(flat.text.is_empty());
    }

    #[test]
    fn should_produce_a_rect_per_visual_line_when_a_decorated_run_wraps() {
        let mut fonts = Fonts::new();
        let theme = theme();
        let inlines = vec![
            text("lead "),
            Inline::Code("alpha beta gamma delta epsilon zeta eta theta iota".into()),
        ];
        let rich = build_inlines(
            &mut fonts,
            &inlines,
            base(),
            &theme,
            16.0,
            24.0,
            Some(160.0),
            false,
        );
        let code_rects: Vec<_> = rich
            .decorations
            .iter()
            .filter(|rect| rect.kind == DecorationKind::CodeSpan)
            .collect();
        assert!(code_rects.len() >= 2, "{code_rects:?}");
        let baselines: std::collections::HashSet<i32> =
            code_rects.iter().map(|r| r.baseline as i32).collect();
        assert_eq!(baselines.len(), code_rects.len());
        assert!(code_rects.iter().all(|r| r.width > 0.0));
    }

    #[test]
    fn should_have_no_rects_when_nothing_is_decorated() {
        let mut fonts = Fonts::new();
        let rich = build_inlines(
            &mut fonts,
            &[text("plain")],
            base(),
            &theme(),
            16.0,
            24.0,
            Some(200.0),
            false,
        );
        assert!(rich.decorations.is_empty());
        assert!(rich.height() >= 24.0);
    }

    #[test]
    fn should_paint_ink_and_a_strike_line_when_text_is_struck() {
        let mut fonts = Fonts::new();
        let theme = theme();
        let rich = build_inlines(
            &mut fonts,
            &[Inline::Strike(vec![text("struck through")])],
            base(),
            &theme,
            16.0,
            24.0,
            Some(300.0),
            false,
        );
        assert_eq!(rich.decorations.len(), 1);
        let mut pixels = vec![theme.bg.to_pixel(); 320 * 40];
        let mut canvas = Canvas::new(&mut pixels, 320, 40);
        paint(&mut canvas, &mut fonts, &rich, 4.0, 4.0, &theme, 1.0);
        assert!(pixels.iter().any(|&p| p != theme.bg.to_pixel()));
    }

    #[test]
    fn should_shape_empty_flat_text_without_panicking() {
        let mut fonts = Fonts::new();
        let rich = build(
            &mut fonts,
            &Flat::default(),
            base(),
            &theme(),
            16.0,
            24.0,
            Some(0.0),
        );
        assert!(rich.height() >= 0.0);
    }
}
