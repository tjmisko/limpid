//! Colours, sizes and font families for the viewer. The values mirror the two
//! `:root` blocks in `limpid-core`'s `page.css`.

use limpid_core::{settings::Settings, transport::Scheme};

/// Straight (non-premultiplied) RGBA, one byte per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse `#rrggbb` or `#rrggbbaa`. Returns `None` for anything else.
    pub fn from_hex(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.is_ascii() || !(digits.len() == 6 || digits.len() == 8) {
            return None;
        }
        let byte = |index: usize| u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).ok();
        let alpha = if digits.len() == 8 { byte(3)? } else { 255 };
        Some(Self::rgba(byte(0)?, byte(1)?, byte(2)?, alpha))
    }

    /// The pixel value softbuffer expects: `0x00RRGGBB`, alpha dropped.
    pub fn to_pixel(self) -> u32 {
        (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }
}

/// Code token colours, keyed by the `hl-*` class families `limpid-core` emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightPalette {
    pub keyword: Color,
    pub operator: Color,
    pub function: Color,
    pub type_: Color,
    pub constant: Color,
    pub string: Color,
    pub comment: Color,
    pub parameter: Color,
    pub property: Color,
    pub number: Color,
}

impl HighlightPalette {
    /// Colour for an `hl-*` class (with or without the prefix), or `None` for
    /// classes the palette has no entry for.
    pub fn for_class(&self, class: &str) -> Option<Color> {
        let name = class.strip_prefix("hl-").unwrap_or(class);
        let family = |prefix: &str| name == prefix || name.starts_with(&format!("{prefix}-"));
        if family("variable-parameter") {
            return Some(self.parameter);
        }
        let color = match name.split('-').next()? {
            "keyword" => self.keyword,
            "operator" => self.operator,
            "function" => self.function,
            "type" | "constructor" | "attribute" => self.type_,
            "constant" => self.constant,
            "string" | "character" => self.string,
            "comment" => self.comment,
            "property" => self.property,
            "number" | "boolean" => self.number,
            _ => return None,
        };
        Some(color)
    }
}

/// Font family names. `sans-serif`, `serif` and `monospace` select the
/// generic families; anything else is a family name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Families {
    pub sans: String,
    pub mono: String,
}

impl Default for Families {
    fn default() -> Self {
        Self {
            sans: "sans-serif".into(),
            mono: "monospace".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub scheme: Scheme,
    pub bg: Color,
    pub surface: Color,
    pub raised: Color,
    pub ink: Color,
    pub muted: Color,
    pub faint: Color,
    pub teal: Color,
    pub blue: Color,
    pub amber: Color,
    pub purple: Color,
    pub code_bg: Color,
    pub rule: Color,
    pub focus: Color,
    /// Tint of the visual-mode selection band.
    pub selection: Color,
    pub hl: HighlightPalette,
    /// CSS px at scale 1.
    pub font_size: f32,
    pub line_height: f32,
    /// Width of the page column in CSS px.
    pub content_width: f32,
    pub rail_width: f32,
    /// Smallest side gutter of the page column in CSS px.
    pub margin: f32,
    /// Fraction of the viewport moved by Ctrl-d and Ctrl-u.
    pub scroll_fraction: f32,
    pub families: Families,
}

const DEFAULT_FONT_SIZE: f32 = 17.0;
const DEFAULT_LINE_HEIGHT: f32 = 1.65;
const DEFAULT_CONTENT_WIDTH: f32 = 860.0;
const DEFAULT_RAIL_WIDTH: f32 = 220.0;
const DEFAULT_MARGIN: f32 = 24.0;
const DEFAULT_SCROLL_FRACTION: f32 = 0.5;

const fn rgb(hex: u32) -> Color {
    Color::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            scheme: Scheme::Dark,
            bg: rgb(0x0c1015),
            surface: rgb(0x12181f),
            raised: rgb(0x18212a),
            ink: rgb(0xe8eeed),
            muted: rgb(0x95a4b1),
            faint: rgb(0x627484),
            teal: rgb(0x7cdec7),
            blue: rgb(0x90bdf5),
            amber: rgb(0xf0bc72),
            purple: rgb(0xb6a3ee),
            code_bg: rgb(0x0e1419),
            rule: rgb(0x27313d),
            focus: Color::rgba(124, 222, 199, 31),
            selection: Color::rgba(124, 222, 199, 62),
            hl: HighlightPalette {
                keyword: rgb(0xcba6f7),
                operator: rgb(0x89dceb),
                function: rgb(0x89b4fa),
                type_: rgb(0xf9e2af),
                constant: rgb(0xfab387),
                string: rgb(0xa6e3a1),
                comment: rgb(0x7f849c),
                parameter: rgb(0xeba0ac),
                property: rgb(0xb4befe),
                number: rgb(0xfab387),
            },
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            content_width: DEFAULT_CONTENT_WIDTH,
            rail_width: DEFAULT_RAIL_WIDTH,
            margin: DEFAULT_MARGIN,
            scroll_fraction: DEFAULT_SCROLL_FRACTION,
            families: Families::default(),
        }
    }

    pub fn light() -> Self {
        Self {
            scheme: Scheme::Light,
            bg: rgb(0xfbfaf7),
            surface: rgb(0xf0eee8),
            raised: rgb(0xe8e6df),
            ink: rgb(0x20252a),
            muted: rgb(0x53616c),
            faint: rgb(0x71818d),
            teal: rgb(0x126d5d),
            blue: rgb(0x245b9b),
            amber: rgb(0x966024),
            purple: rgb(0x6955a5),
            code_bg: rgb(0xf3f2ee),
            rule: rgb(0xd4d1c8),
            focus: Color::rgba(18, 109, 93, 26),
            selection: Color::rgba(18, 109, 93, 52),
            hl: HighlightPalette {
                keyword: rgb(0x6955a5),
                operator: rgb(0x6955a5),
                function: rgb(0x245b9b),
                type_: rgb(0x966024),
                constant: rgb(0x966024),
                string: rgb(0x126d5d),
                comment: rgb(0x53616c),
                parameter: rgb(0x966024),
                property: rgb(0x966024),
                number: rgb(0x6955a5),
            },
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            content_width: DEFAULT_CONTENT_WIDTH,
            rail_width: DEFAULT_RAIL_WIDTH,
            margin: DEFAULT_MARGIN,
            scroll_fraction: DEFAULT_SCROLL_FRACTION,
            families: Families::default(),
        }
    }

    pub fn for_scheme(scheme: Scheme) -> Self {
        match scheme {
            Scheme::Dark => Self::dark(),
            Scheme::Light => Self::light(),
        }
    }

    /// The scheme's palette with the `[style]` settings applied. The `[view]`
    /// keys are applied by the caller.
    pub fn from_settings(settings: &Settings, scheme: Scheme) -> Self {
        let mut theme = Self::for_scheme(scheme);
        let style = &settings.style;
        if style.font_size > 0 {
            theme.font_size = style.font_size as f32;
        }
        if let Some(value) = style.line_height {
            theme.line_height = value;
        }
        if let Some(value) = style.content_width {
            theme.content_width = value as f32;
        }
        let slots: [(&Option<String>, &mut Color); 10] = [
            (&style.bg, &mut theme.bg),
            (&style.surface, &mut theme.surface),
            (&style.raised, &mut theme.raised),
            (&style.rule, &mut theme.rule),
            (&style.ink, &mut theme.ink),
            (&style.muted, &mut theme.muted),
            (&style.faint, &mut theme.faint),
            (&style.teal, &mut theme.teal),
            (&style.blue, &mut theme.blue),
            (&style.amber, &mut theme.amber),
        ];
        for (value, slot) in slots {
            if let Some(color) = value.as_deref().and_then(Color::from_hex) {
                *slot = color;
            }
        }
        // The bands are the teal token at a low alpha.
        let teal = theme.teal;
        let alpha = |fraction: f32| (fraction * 255.0).round() as u8;
        let focus_alpha = style.focus_alpha.map_or(theme.focus.a, alpha);
        let selection_alpha = style.selection_alpha.map_or(theme.selection.a, alpha);
        theme.focus = Color::rgba(teal.r, teal.g, teal.b, focus_alpha);
        theme.selection = Color::rgba(teal.r, teal.g, teal.b, selection_alpha);
        theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_page_css_values_when_built_with_defaults() {
        let theme = Theme::from_settings(&Settings::default(), Scheme::Dark);
        assert_eq!(theme.bg, Color::rgb(0x0c, 0x10, 0x15));
        assert_eq!(theme.font_size, 17.0);
        assert_eq!(theme.line_height, 1.65);
        assert_eq!(theme.content_width, 860.0);
        assert_eq!(theme.rail_width, 220.0);
        assert_eq!(theme.hl.keyword, Color::rgb(0xcb, 0xa6, 0xf7));
    }

    #[test]
    fn should_use_the_light_palette_when_scheme_is_light() {
        let theme = Theme::from_settings(&Settings::default(), Scheme::Light);
        assert_eq!(theme.scheme, Scheme::Light);
        assert_eq!(theme.bg, Color::rgb(0xfb, 0xfa, 0xf7));
        assert_eq!(theme.hl.string, Color::rgb(0x12, 0x6d, 0x5d));
    }

    #[test]
    fn should_override_font_size_when_settings_set_it() {
        let settings = Settings::parse("[style]\nfont_size = 22\n").unwrap();
        assert_eq!(
            Theme::from_settings(&settings, Scheme::Dark).font_size,
            22.0
        );
    }

    #[test]
    fn should_keep_default_font_size_when_setting_is_zero() {
        let settings = Settings::parse("[style]\nfont_size = 0\n").unwrap();
        assert_eq!(
            Theme::from_settings(&settings, Scheme::Dark).font_size,
            17.0
        );
    }

    #[test]
    fn should_apply_style_tokens_when_settings_set_them() {
        let settings = Settings::parse(
            "[style]\nline_height = 2.0\ncontent_width = 700\nbg = '#010203'\nraised = '#040506'\nteal = '#0a0b0c'\nfocus_alpha = 0.5\nselection_alpha = 1.0\n",
        )
        .unwrap();
        let theme = Theme::from_settings(&settings, Scheme::Dark);
        assert_eq!(theme.line_height, 2.0);
        assert_eq!(theme.content_width, 700.0);
        assert_eq!(theme.bg, Color::rgb(1, 2, 3));
        assert_eq!(theme.raised, Color::rgb(4, 5, 6));
        assert_eq!(theme.focus, Color::rgba(10, 11, 12, 128));
        assert_eq!(theme.selection, Color::rgba(10, 11, 12, 255));
        assert_eq!(theme.surface, Theme::dark().surface);
    }

    #[test]
    fn should_keep_the_default_bands_when_no_alpha_is_set() {
        let theme = Theme::from_settings(&Settings::default(), Scheme::Dark);
        assert_eq!(theme.focus, Theme::dark().focus);
        assert_eq!(theme.selection, Theme::dark().selection);
    }

    #[test]
    fn should_parse_hex_colours_when_well_formed() {
        assert_eq!(Color::from_hex("#7cdec7"), Some(Color::rgb(124, 222, 199)));
        assert_eq!(
            Color::from_hex("#7cdec780"),
            Some(Color::rgba(124, 222, 199, 128))
        );
    }

    #[test]
    fn should_reject_hex_colours_when_malformed() {
        for text in [
            "",
            "#",
            "7cdec7",
            "#7cdec",
            "#7cdecg",
            "#7cdec7801",
            "#é7cdec",
        ] {
            assert_eq!(Color::from_hex(text), None, "{text:?}");
        }
    }

    #[test]
    fn should_drop_alpha_when_converting_to_a_pixel() {
        assert_eq!(Color::rgba(1, 2, 3, 9).to_pixel(), 0x0001_0203);
    }

    #[test]
    fn should_map_highlight_classes_to_palette_entries_when_known() {
        let palette = Theme::dark().hl;
        assert_eq!(palette.for_class("hl-keyword"), Some(palette.keyword));
        assert_eq!(
            palette.for_class("hl-keyword-control"),
            Some(palette.keyword)
        );
        assert_eq!(
            palette.for_class("hl-variable-parameter"),
            Some(palette.parameter)
        );
        assert_eq!(palette.for_class("boolean"), Some(palette.number));
        assert_eq!(palette.for_class("hl-constructor"), Some(palette.type_));
    }

    #[test]
    fn should_return_none_when_highlight_class_is_unknown() {
        let palette = Theme::dark().hl;
        assert_eq!(palette.for_class("hl-nonsense"), None);
        assert_eq!(palette.for_class(""), None);
    }
}
