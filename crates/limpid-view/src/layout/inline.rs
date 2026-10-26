//! Inline content: a run of `Inline` nodes flattened into one string with
//! styled byte ranges, shaped by cosmic-text, plus the rectangles for the
//! decorations cosmic-text does not draw (inline code backgrounds, strikes).

use crate::layout::PendingKind;
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, Span, TextBlock, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::{doc::Inline, math::Rendered};
use std::ops::Range;

/// CSS pixels per em the math cache sizes are measured against.
pub const MATH_REFERENCE_EM: f32 = 16.0;

/// Deepest inline nesting followed; anything deeper is dropped.
const MAX_INLINE_DEPTH: usize = 64;

/// Reserves the horizontal padding of an inline code box inside the text
/// advance: a narrow no-break space (about 0.2em wide, never a wrap point)
/// on either side of the code, set in the surrounding style.
const CODE_PAD: &str = "\u{202f}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationKind {
    /// Inline code and inline math source: a tinted, outlined box behind text.
    CodeSpan,
    /// A line through the middle of the text.
    Strike,
    /// A typed hole in the line (see [`InlinePlaceholder`]), drawn as a box.
    Placeholder,
}

/// A hole in an inline run that a later renderer fills: inline math today.
/// The text under `range` is the monospace source when `size` is `None`, or
/// non-breaking spaces as wide as the typeset formula when it is known.
#[derive(Debug, Clone, PartialEq)]
pub struct InlinePlaceholder {
    pub kind: PendingKind,
    /// Byte range into [`Flat::text`], padding included.
    pub range: Range<usize>,
    pub hash: String,
    pub source: String,
    /// Typeset `(width, height, baseline)` in CSS px, from the math cache.
    pub size: Option<(f32, f32, f32)>,
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
    pub placeholders: Vec<InlinePlaceholder>,
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
    flatten_with(inlines, base, theme, upper, &MathSizing::default())
}

/// How inline math is looked up and sized.
pub struct MathSizing<'a> {
    /// The cache entry for a formula hash.
    pub lookup: &'a dyn Fn(&str) -> Option<Rendered>,
    /// Advance of the reserved space as a fraction of the em.
    pub advance_em: f32,
}

impl Default for MathSizing<'_> {
    fn default() -> Self {
        Self {
            lookup: &limpid_core::math::cached,
            advance_em: 0.6,
        }
    }
}

/// [`flatten`] with explicit math sizing.
pub fn flatten_with(
    inlines: &[Inline],
    base: TextStyle,
    theme: &Theme,
    upper: bool,
    math: &MathSizing<'_>,
) -> Flat {
    let mut flattener = Flattener {
        theme,
        upper,
        math,
        flat: Flat::default(),
    };
    flattener.walk(inlines, base, 0);
    flattener.flat
}

struct Flattener<'a> {
    theme: &'a Theme,
    upper: bool,
    math: &'a MathSizing<'a>,
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
                    self.code_span(style, code_style, code);
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
                Inline::Math { latex, hash } => self.math(style, latex, hash),
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

    /// Inline math: a placeholder sized from the cache when the formula is
    /// typeset, else its source in monospace.
    fn math(&mut self, style: TextStyle, latex: &str, hash: &str) {
        let math_style = TextStyle {
            mono: true,
            color: self.theme.purple,
            ..style
        };
        let size = (self.math.lookup)(hash)
            .filter(|rendered| rendered.width.is_finite() && rendered.width > 0.0)
            .map(|rendered| (rendered.width, rendered.height, rendered.baseline));
        let start = self.flat.text.len();
        match size {
            Some((width, ..)) => {
                let advance = (self.math.advance_em * MATH_REFERENCE_EM).max(1.0);
                let count = (width / advance).ceil().clamp(1.0, 400.0) as usize;
                let reserved = "\u{a0}".repeat(count);
                self.boxed(DecorationKind::Placeholder, style, math_style, &reserved);
            }
            None => self.boxed(DecorationKind::Placeholder, style, math_style, latex),
        }
        let end = self.flat.text.len();
        if end == start {
            return;
        }
        self.flat.placeholders.push(InlinePlaceholder {
            kind: PendingKind::Math,
            range: start..end,
            hash: hash.to_owned(),
            source: latex.to_owned(),
            size,
        });
    }

    /// A boxed span: `body` in `code_style`, between two padding characters
    /// in `outer` that sit inside the box so it never overlaps neighbours.
    fn code_span(&mut self, outer: TextStyle, code_style: TextStyle, body: &str) {
        self.boxed(DecorationKind::CodeSpan, outer, code_style, body);
    }

    fn boxed(&mut self, kind: DecorationKind, outer: TextStyle, code_style: TextStyle, body: &str) {
        if body.is_empty() {
            return;
        }
        self.decorated(kind, code_style, |this| {
            this.flat.push(CODE_PAD, outer);
            this.text(body, code_style);
            this.flat.push(CODE_PAD, outer);
        });
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
    /// Holes in the text for a later renderer, in text order; the boxes
    /// are the `Placeholder` entries of `decorations` (one per wrapped line).
    pub placeholders: Vec<InlinePlaceholder>,
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
        placeholders: flat.placeholders.clone(),
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
    // Typeset formulas replace the box drawn for their reserved space.
    let typeset = crate::layout::boxes::typeset_math(text);
    for deco in &text.decorations {
        if !matches!(
            deco.kind,
            DecorationKind::CodeSpan | DecorationKind::Placeholder
        ) || crate::layout::boxes::skip_placeholder_box(&typeset, deco)
        {
            continue;
        }
        let rect = Rect::new(
            x + deco.x,
            y + deco.baseline - size * 0.98,
            deco.width,
            size * 1.3,
        );
        canvas.fill_rect(rect, raised(theme));
        canvas.stroke_rect(rect, line, theme.rule);
    }
    canvas.draw_text(fonts, &text.block, x, y, theme.ink);
    crate::layout::boxes::paint_typeset_math(canvas, text, x, y, &typeset);
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
        let (range, style) = flat.runs.iter().find(|(_, s)| s.mono).unwrap();
        assert_eq!(&flat.text[range.clone()], "y");
        assert!(style.mono);
        assert_eq!(style.color, theme().blue);
        assert_eq!(flat.decorations.len(), 1);
        assert_eq!(flat.decorations[0].kind, DecorationKind::CodeSpan);
        let boxed = &flat.text[flat.decorations[0].range.clone()];
        assert_eq!(boxed.trim_matches(CODE_PAD.chars().next().unwrap()), "y");
    }

    #[test]
    fn should_keep_the_code_box_clear_of_neighbours_when_text_touches_the_code() {
        let theme = theme();
        let inlines = vec![text("word"), Inline::Code("x".into()), text("word")];
        let rich = build_inlines(
            &mut Fonts::new(),
            &inlines,
            base(),
            &theme,
            34.0,
            48.0,
            None,
            false,
        );
        let flat = flatten(&inlines, base(), &theme, false);
        let boxed = flat.decorations[0].range.clone();
        let before = flat.text[..boxed.start].len();
        let rect = rich
            .decorations
            .iter()
            .find(|d| d.kind == DecorationKind::CodeSpan)
            .unwrap();
        let glyphs: Vec<_> = rich.block.glyphs().collect();
        let last_before = glyphs.iter().rfind(|g| g.start < before).unwrap();
        let first_after = glyphs.iter().find(|g| g.start >= boxed.end).unwrap();
        assert!(rect.x >= last_before.x + last_before.width - 0.5);
        assert!(rect.x + rect.width <= first_after.x + 0.5);
        assert!(
            rect.width > 34.0 * 0.2 * 2.0,
            "padding reserved inside the box"
        );
    }

    #[test]
    fn should_not_box_anything_when_the_code_is_empty() {
        let flat = flatten(&[Inline::Code(String::new())], base(), &theme(), false);
        assert!(flat.decorations.is_empty());
        assert!(flat.text.is_empty());
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

    fn math(latex: &str) -> Inline {
        Inline::Math {
            latex: latex.to_owned(),
            hash: format!("hash-{latex}"),
        }
    }

    fn rendered(width: f32) -> Rendered {
        Rendered {
            svg: "x.svg".into(),
            width,
            height: 20.0,
            baseline: 15.0,
            backend: "test".into(),
        }
    }

    #[test]
    fn should_show_monospace_source_in_a_math_placeholder_when_cache_misses() {
        let sizing = MathSizing {
            lookup: &|_| None,
            ..MathSizing::default()
        };
        let flat = flatten_with(&[math("x^2")], base(), &theme(), false, &sizing);
        assert!(flat.text.contains("x^2"));
        assert_eq!(flat.placeholders.len(), 1);
        let hole = &flat.placeholders[0];
        assert_eq!(hole.kind, PendingKind::Math);
        assert_eq!(hole.size, None);
        assert_eq!(hole.source, "x^2");
        assert_eq!(hole.hash, "hash-x^2");
        assert_eq!(
            &flat.text[hole.range.clone()].trim_matches('\u{202f}'),
            &"x^2"
        );
    }

    #[test]
    fn should_reserve_width_from_the_cache_when_math_is_typeset() {
        let sizing = MathSizing {
            lookup: &|_| Some(rendered(96.0)),
            ..MathSizing::default()
        };
        let flat = flatten_with(&[math("x^2")], base(), &theme(), false, &sizing);
        assert!(!flat.text.contains("x^2"));
        let hole = &flat.placeholders[0];
        assert_eq!(hole.size, Some((96.0, 20.0, 15.0)));
        let reserved = flat.text[hole.range.clone()].matches('\u{a0}').count();
        assert_eq!(reserved, 10);
    }

    #[test]
    fn should_fall_back_to_source_when_cached_width_is_unusable() {
        for width in [0.0, -5.0, f32::NAN, f32::INFINITY] {
            let sizing = MathSizing {
                lookup: &move |_| Some(rendered(width)),
                ..MathSizing::default()
            };
            let flat = flatten_with(&[math("y")], base(), &theme(), false, &sizing);
            assert_eq!(flat.placeholders[0].size, None, "width {width}");
            assert!(flat.text.contains('y'));
        }
    }

    #[test]
    fn should_cap_the_reserved_width_when_the_formula_is_huge() {
        let sizing = MathSizing {
            lookup: &|_| Some(rendered(1.0e9)),
            ..MathSizing::default()
        };
        let flat = flatten_with(&[math("z")], base(), &theme(), false, &sizing);
        assert!(flat.text.chars().count() <= 410);
    }

    #[test]
    fn should_carry_math_placeholders_into_shaped_text_when_built() {
        let rich = build_inlines(
            &mut Fonts::new(),
            &[text("a "), math("q")],
            base(),
            &theme(),
            17.0,
            26.0,
            Some(400.0),
            false,
        );
        assert_eq!(rich.placeholders.len(), 1);
        assert!(
            rich.decorations
                .iter()
                .any(|deco| deco.kind == DecorationKind::Placeholder)
        );
    }
}
