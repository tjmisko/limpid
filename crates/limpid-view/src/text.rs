//! Text shaping and layout on top of cosmic-text.

use crate::theme::{Color, Families};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};

/// One `FontSystem` and `SwashCache` per process; fontdb scans the system
/// fonts once, here.
pub struct Fonts {
    pub system: FontSystem,
    pub cache: SwashCache,
}

impl Fonts {
    pub fn new() -> Self {
        Self {
            system: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

/// How a run of text looks. Sizes live on the block, not the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextStyle {
    pub mono: bool,
    pub bold: bool,
    pub italic: bool,
    pub color: Color,
}

impl TextStyle {
    pub fn sans(color: Color) -> Self {
        Self {
            mono: false,
            bold: false,
            italic: false,
            color,
        }
    }

    pub fn mono(color: Color) -> Self {
        Self {
            mono: true,
            ..Self::sans(color)
        }
    }

    pub fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub fn italic(self) -> Self {
        Self {
            italic: true,
            ..self
        }
    }
}

/// A family name from settings: the three generic names select generic
/// families, anything else is looked up by name.
pub fn family(name: &str) -> Family<'_> {
    match name {
        "sans-serif" => Family::SansSerif,
        "serif" => Family::Serif,
        "monospace" => Family::Monospace,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        other => Family::Name(other),
    }
}

pub fn attrs<'a>(style: TextStyle, families: &'a Families) -> Attrs<'a> {
    let name = if style.mono {
        &families.mono
    } else {
        &families.sans
    };
    Attrs::new()
        .family(family(name))
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
        .color(cosmic_text::Color::rgba(
            style.color.r,
            style.color.g,
            style.color.b,
            style.color.a,
        ))
}

#[derive(Debug, Clone, Copy)]
pub struct Span<'a> {
    pub text: &'a str,
    pub style: TextStyle,
}

/// A glyph after layout, in block-local pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub x: f32,
    /// Baseline of the line holding the glyph.
    pub baseline: f32,
    pub width: f32,
    pub line: usize,
    /// Byte range in the line's text.
    pub start: usize,
    pub end: usize,
}

/// A wrapped, shaped run of text.
pub struct TextBlock {
    buffer: Buffer,
}

impl TextBlock {
    /// `font_size` and `line_height` are in device pixels.
    pub fn new(fonts: &mut Fonts, font_size: f32, line_height: f32) -> Self {
        let metrics = Self::metrics(font_size, line_height);
        let mut buffer = Buffer::new(&mut fonts.system, metrics);
        buffer.set_size(&mut fonts.system, None, None);
        Self { buffer }
    }

    fn metrics(font_size: f32, line_height: f32) -> Metrics {
        let size = font_size.max(1.0);
        Metrics::new(size, line_height.max(size))
    }

    pub fn set_metrics(&mut self, fonts: &mut Fonts, font_size: f32, line_height: f32) {
        self.buffer
            .set_metrics(&mut fonts.system, Self::metrics(font_size, line_height));
    }

    /// Wrap at `width` device pixels; `None` never wraps.
    pub fn set_width(&mut self, fonts: &mut Fonts, width: Option<f32>) {
        self.buffer.set_size(&mut fonts.system, width, None);
        self.buffer.shape_until_scroll(&mut fonts.system, false);
    }

    pub fn set_spans(
        &mut self,
        fonts: &mut Fonts,
        spans: &[Span<'_>],
        default: TextStyle,
        families: &Families,
    ) {
        let default_attrs = attrs(default, families);
        self.buffer.set_rich_text(
            &mut fonts.system,
            spans
                .iter()
                .map(|span| (span.text, attrs(span.style, families))),
            default_attrs,
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(&mut fonts.system, false);
    }

    pub fn set_text(
        &mut self,
        fonts: &mut Fonts,
        text: &str,
        style: TextStyle,
        families: &Families,
    ) {
        self.set_spans(fonts, &[Span { text, style }], style, families);
    }

    /// Height of the wrapped text in device pixels.
    pub fn height(&self) -> f32 {
        self.buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0, f32::max)
    }

    /// Number of wrapped lines.
    pub fn line_count(&self) -> usize {
        self.buffer.layout_runs().count()
    }

    pub fn glyphs(&self) -> impl Iterator<Item = PlacedGlyph> + '_ {
        self.buffer.layout_runs().flat_map(|run| {
            let baseline = run.line_y;
            let line = run.line_i;
            run.glyphs.iter().map(move |glyph| PlacedGlyph {
                x: glyph.x,
                baseline,
                width: glyph.w,
                line,
                start: glyph.start,
                end: glyph.end,
            })
        })
    }

    pub(crate) fn buffer(&self) -> &Buffer {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Color = Color::rgb(255, 255, 255);

    fn block(fonts: &mut Fonts, text: &str, width: Option<f32>) -> TextBlock {
        let mut block = TextBlock::new(fonts, 16.0, 24.0);
        block.set_text(fonts, text, TextStyle::sans(INK), &Families::default());
        block.set_width(fonts, width);
        block
    }

    #[test]
    fn should_map_generic_and_named_families_when_resolving_settings() {
        assert_eq!(family("sans-serif"), Family::SansSerif);
        assert_eq!(family("monospace"), Family::Monospace);
        assert_eq!(family("Inter"), Family::Name("Inter"));
    }

    #[test]
    fn should_have_one_line_height_when_text_is_empty() {
        let mut fonts = Fonts::new();
        assert_eq!(block(&mut fonts, "", Some(200.0)).height(), 24.0);
    }

    #[test]
    fn should_wrap_onto_more_lines_when_width_shrinks() {
        let mut fonts = Fonts::new();
        let text = "the quick brown fox jumps over the lazy dog ".repeat(4);
        let wide = block(&mut fonts, &text, Some(2000.0));
        let narrow = block(&mut fonts, &text, Some(120.0));
        assert_eq!(wide.line_count(), 1);
        assert!(narrow.line_count() > 3, "{}", narrow.line_count());
        assert!(narrow.height() > wide.height());
    }

    #[test]
    fn should_use_one_line_height_per_line_when_text_has_hard_breaks() {
        let mut fonts = Fonts::new();
        let three = block(&mut fonts, "a\nb\nc", Some(500.0));
        assert_eq!(three.line_count(), 3);
        assert_eq!(three.height(), 72.0);
    }

    #[test]
    fn should_place_glyphs_left_to_right_when_text_is_ltr() {
        let mut fonts = Fonts::new();
        let text = block(&mut fonts, "abc", Some(500.0));
        let xs: Vec<f32> = text.glyphs().map(|glyph| glyph.x).collect();
        assert_eq!(xs.len(), 3);
        assert!(xs.windows(2).all(|pair| pair[0] < pair[1]), "{xs:?}");
    }

    #[test]
    fn should_not_panic_when_metrics_are_degenerate() {
        let mut fonts = Fonts::new();
        let mut text = TextBlock::new(&mut fonts, 0.0, -3.0);
        text.set_text(&mut fonts, "x", TextStyle::sans(INK), &Families::default());
        text.set_width(&mut fonts, Some(0.0));
        let _ = text.height();
    }

    #[test]
    fn should_keep_multibyte_text_whole_when_shaping_spans() {
        let mut fonts = Fonts::new();
        let families = Families::default();
        let mut text = TextBlock::new(&mut fonts, 16.0, 24.0);
        let spans = [
            Span {
                text: "caf\u{e9} ",
                style: TextStyle::sans(INK),
            },
            Span {
                text: "\u{4e16}\u{754c}",
                style: TextStyle::mono(INK).bold(),
            },
        ];
        text.set_spans(&mut fonts, &spans, TextStyle::sans(INK), &families);
        text.set_width(&mut fonts, Some(500.0));
        assert!(text.glyphs().count() >= 6);
    }
}
