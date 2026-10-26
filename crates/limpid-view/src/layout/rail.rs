//! The contents rail: where it fits, what it lists, and how it is drawn.

use super::{
    Geometry,
    inline::{self, Flat, RichText},
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
    theme::{Color, Theme},
};
use limpid_core::{
    doc::{BlockKind, Doc},
    settings::RailMode,
    transport::Scheme,
};
use std::collections::HashMap;

/// Padding around the rail's content, CSS px.
const PADDING: f32 = 14.0;

/// Whether the rail fits at this window width (CSS px).
pub fn fits(window_css_width: f32, theme: &Theme) -> bool {
    window_css_width >= theme.content_width + theme.rail_width + 2.0 * theme.margin
}

/// Place the rail and the text column in a window `width` device pixels wide.
pub fn geometry(width: f32, theme: &Theme, scale: f32, mode: RailMode, has_toc: bool) -> Geometry {
    let css_width = width / scale;
    let wanted = has_toc
        && match mode {
            RailMode::Off => false,
            RailMode::On => true,
            RailMode::Auto => fits(css_width, theme),
        };
    // A forced rail never takes more than half of a narrow window.
    let rail_width = if wanted {
        (theme.rail_width * scale).min(width / 2.0).max(1.0)
    } else {
        0.0
    };
    let available = (width - rail_width).max(1.0);
    let side = if css_width <= 880.0 {
        theme.margin
    } else {
        (css_width * 0.05).clamp(theme.margin, 64.0_f32.max(theme.margin))
    } * scale;
    let page_width = (theme.content_width * scale).min(available);
    let column_width = (page_width - 2.0 * side).max(1.0);
    let page_x = rail_width + (available - page_width) / 2.0;
    // When the page is too narrow for its gutters the column takes what is left.
    let column_x = page_x + side.min((page_width - column_width).max(0.0) / 2.0);
    Geometry {
        rail_width,
        column_x,
        column_width,
    }
}

pub fn rail_bg(theme: &Theme) -> Color {
    match theme.scheme {
        Scheme::Dark => Color::rgb(0x0a, 0x0e, 0x12),
        Scheme::Light => Color::rgb(0xf3, 0xf2, 0xed),
    }
}

pub struct RailEntry {
    /// Shaped on first paint; an outline of thousands of headings stays cheap.
    pub text: Option<RichText>,
    /// Device-pixel font size of the entry.
    font_size: f32,
    pub label: String,
    pub level: u8,
    /// Top-level block index of the heading, when it has one.
    pub block_index: Option<usize>,
    /// Top of the entry, relative to the rail.
    pub top: f32,
    pub height: f32,
    /// Space left of the text inside the entry.
    pub indent: f32,
    text_width: f32,
}

pub struct RailLayout {
    pub width: f32,
    pub title: RichText,
    pub entries: Vec<RailEntry>,
    active_text: Option<(usize, RichText)>,
}

pub fn build(doc: &Doc, width: f32, theme: &Theme, scale: f32, fonts: &mut Fonts) -> RailLayout {
    let ratio = theme.font_size / 17.0;
    let px = |css: f32| css * scale;
    let font = |css: f32| css * ratio * scale;
    let title_size = font(10.0);
    let title_style = TextStyle::sans(theme.faint).bold();
    let title = inline::build(
        fonts,
        &Flat::plain("CONTENTS", title_style),
        title_style,
        theme,
        title_size,
        title_size * 1.4,
        None,
    );

    let headings: HashMap<&str, usize> = doc
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match &block.kind {
            BlockKind::Heading { id, .. } => Some((id.as_str(), index)),
            _ => None,
        })
        .collect();

    let mut top = px(20.0) + title.height() + px(10.0);
    let mut entries = Vec::new();
    for entry in &doc.toc {
        let deep = entry.level >= 3;
        let size = font(if deep { 12.0 } else { 13.0 });
        let indent = px(if deep { 28.0 } else { 14.0 });
        let text_width = (width - 2.0 * px(PADDING) - indent - px(8.0)).max(1.0);
        // Every entry is one line tall so positions need no shaping.
        let height = size * 1.4 + px(10.0);
        entries.push(RailEntry {
            text: None,
            font_size: size,
            label: entry.text.clone(),
            level: entry.level,
            block_index: headings.get(entry.id.as_str()).copied(),
            top,
            height,
            indent,
            text_width,
        });
        top += height;
    }
    RailLayout {
        width,
        title,
        entries,
        active_text: None,
    }
}

impl RailEntry {
    /// Shape the label in `color` on one line, cutting it with an ellipsis
    /// when it is too long for the rail.
    fn shape(&self, color: Color, theme: &Theme, fonts: &mut Fonts) -> RichText {
        let style = TextStyle::sans(color);
        let line = self.font_size * 1.4;
        let characters: Vec<char> = self.label.chars().collect();
        let mut keep = characters.len();
        loop {
            let label: String = if keep == characters.len() {
                self.label.clone()
            } else {
                let head: String = characters[..keep].iter().collect();
                format!("{}\u{2026}", head.trim_end())
            };
            let text = inline::build(
                fonts,
                &Flat::plain(&label, style),
                style,
                theme,
                self.font_size,
                line,
                Some(self.text_width),
            );
            if text.height() <= line * 1.5 || keep <= 1 {
                return text;
            }
            keep = (keep * 3 / 4).min(keep - 1).max(1);
        }
    }
}

impl RailLayout {
    /// The entry that follows top-level block `block_index`: the last
    /// heading at or above it.
    pub fn active_for(&self, block_index: usize) -> Option<usize> {
        self.entries.iter().rposition(|entry| {
            entry
                .block_index
                .is_some_and(|heading| heading <= block_index)
        })
    }

    pub fn paint(
        &mut self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        theme: &Theme,
        scale: f32,
        active: Option<usize>,
    ) {
        let viewport = canvas.height() as f32;
        let border = scale.round().max(1.0);
        let area = Rect::new(0.0, 0.0, self.width, viewport);
        canvas.fill_rect(area, rail_bg(theme));
        canvas.fill_rect(
            Rect::new(self.width - border, 0.0, border, viewport),
            theme.rule,
        );

        // Keep the active entry on screen when the outline is taller than the window.
        let reach = viewport - 40.0 * scale;
        let offset = active
            .and_then(|index| self.entries.get(index))
            .map_or(0.0, |entry| (entry.top + entry.height - reach).max(0.0));

        let active = active.filter(|&index| index < self.entries.len());
        if let Some(index) = active {
            self.ensure_active_text(index, theme, fonts);
        }
        for entry in &mut self.entries {
            let top = entry.top - offset;
            if top + entry.height < 0.0 {
                continue;
            }
            if top > viewport {
                break;
            }
            if entry.text.is_none() {
                entry.text = Some(entry.shape(theme.muted, theme, fonts));
            }
        }
        let pad = PADDING * scale;
        inline::paint(
            canvas,
            fonts,
            &self.title,
            pad + 8.0 * scale,
            20.0 * scale - offset,
            theme,
            scale,
        );
        for (index, entry) in self.entries.iter().enumerate() {
            let top = entry.top - offset;
            if top + entry.height < 0.0 {
                continue;
            }
            if top > viewport {
                break;
            }
            let is_active = active == Some(index);
            if is_active {
                canvas.fill_rect(
                    Rect::new(pad, top, self.width - 2.0 * pad - border, entry.height),
                    theme.surface,
                );
                canvas.fill_rect(
                    Rect::new(pad, top, 2.0 * scale.round().max(1.0), entry.height),
                    theme.teal,
                );
            }
            let text = match &self.active_text {
                Some((cached, text)) if is_active && *cached == index => text,
                _ => match &entry.text {
                    Some(text) => text,
                    None => continue,
                },
            };
            inline::paint(
                canvas,
                fonts,
                text,
                pad + entry.indent,
                top + 5.0 * scale,
                theme,
                scale,
            );
        }
    }

    /// The active entry's text in teal, shaped on first use and kept until
    /// another entry becomes active.
    fn ensure_active_text(&mut self, index: usize, theme: &Theme, fonts: &mut Fonts) {
        let cached = matches!(&self.active_text, Some((cached, _)) if *cached == index);
        if !cached {
            let text = self.entries[index].shape(theme.teal, theme, fonts);
            self.active_text = Some((index, text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutOptions, lay_out_with};
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    const DOC: &str = "# One\n\ntext\n\n## Two\n\ntext\n\n### Three\n\ntext\n";

    fn layout(markdown: &str, width: f32, mode: RailMode) -> crate::layout::Layout {
        let doc = parse(markdown, Mode::File, &Settings::default());
        lay_out_with(
            &doc,
            width,
            &Theme::dark(),
            1.0,
            &mut Fonts::new(),
            LayoutOptions {
                rail: mode,
                ..LayoutOptions::default()
            },
        )
    }

    #[test]
    fn should_show_the_rail_when_the_window_reaches_the_threshold() {
        let theme = Theme::dark();
        let threshold = theme.content_width + theme.rail_width + 2.0 * theme.margin;
        assert!(fits(threshold, &theme));
        assert!(!fits(threshold - 1.0, &theme));
        assert!(layout(DOC, threshold, RailMode::Auto).rail.is_some());
        assert!(layout(DOC, threshold - 1.0, RailMode::Auto).rail.is_none());
    }

    #[test]
    fn should_force_the_rail_on_or_off_when_the_setting_says_so() {
        assert!(layout(DOC, 500.0, RailMode::On).rail.is_some());
        assert!(layout(DOC, 2000.0, RailMode::Off).rail.is_none());
    }

    #[test]
    fn should_skip_the_rail_when_the_document_has_no_headings() {
        assert!(layout("just text\n", 2000.0, RailMode::On).rail.is_none());
    }

    #[test]
    fn should_centre_the_column_in_what_the_rail_leaves() {
        let theme = Theme::dark();
        let with_rail = geometry(1600.0, &theme, 1.0, RailMode::Auto, true);
        assert_eq!(with_rail.rail_width, 220.0);
        let page_x = 220.0 + (1380.0 - 860.0) / 2.0;
        assert!(with_rail.column_x > page_x && with_rail.column_x < page_x + 70.0);
        assert!(with_rail.column_width < 860.0);
        let without = geometry(1600.0, &theme, 1.0, RailMode::Off, true);
        assert_eq!(without.rail_width, 0.0);
        assert!(without.column_x > with_rail.column_x - 220.0 - 1.0);
    }

    #[test]
    fn should_shrink_the_column_to_the_window_when_it_is_narrow() {
        let geometry = geometry(400.0, &Theme::dark(), 1.0, RailMode::Auto, false);
        assert_eq!(geometry.column_x, 24.0);
        assert_eq!(geometry.column_width, 352.0);
    }

    #[test]
    fn should_not_collapse_when_the_window_is_absurdly_small() {
        for width in [1.0, 10.0, 30.0] {
            let geometry = geometry(width, &Theme::dark(), 1.0, RailMode::On, true);
            assert!(geometry.column_width >= 1.0);
            assert!(geometry.column_x >= 0.0);
        }
    }

    #[test]
    fn should_scale_geometry_with_the_device_scale() {
        let theme = Theme::dark();
        let one = geometry(1600.0, &theme, 1.0, RailMode::Auto, true);
        let two = geometry(3200.0, &theme, 2.0, RailMode::Auto, true);
        assert!((two.column_x - 2.0 * one.column_x).abs() < 1.0);
        assert!((two.column_width - 2.0 * one.column_width).abs() < 1.0);
    }

    #[test]
    fn should_list_one_entry_per_heading_and_map_them_to_blocks() {
        let layout = layout(DOC, 1600.0, RailMode::Auto);
        let rail = layout.rail.as_ref().unwrap();
        assert_eq!(rail.entries.len(), 3);
        let blocks: Vec<_> = rail.entries.iter().map(|e| e.block_index).collect();
        assert_eq!(blocks, vec![Some(0), Some(2), Some(4)]);
        assert!(
            rail.entries
                .windows(2)
                .all(|p| p[1].top >= p[0].top + p[0].height)
        );
        assert!(rail.entries[2].indent > rail.entries[1].indent);
    }

    #[test]
    fn should_follow_the_block_at_the_top_when_choosing_the_active_entry() {
        let layout = layout(DOC, 1600.0, RailMode::Auto);
        let second_text = layout.blocks[3].rect.y;
        assert_eq!(layout.rail_active(0.0), Some(0));
        assert_eq!(layout.rail_active(second_text), Some(1));
        assert_eq!(layout.rail_active(1e9), Some(2));
    }

    #[test]
    fn should_leave_no_entry_active_when_the_top_precedes_every_heading() {
        let layout = layout("intro\n\n## Later\n", 1600.0, RailMode::Auto);
        assert_eq!(layout.rail_active(0.0), None);
        assert_eq!(layout.rail_active(1e9), Some(0));
    }

    #[test]
    fn should_list_every_heading_and_follow_the_last_when_there_are_many() {
        let count = 700;
        let source: String = (0..count).map(|n| format!("## H{n}\n\ntext\n\n")).collect();
        let doc = parse(&source, Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let mut layout = lay_out_with(
            &doc,
            1600.0,
            &Theme::dark(),
            1.0,
            &mut fonts,
            LayoutOptions::default(),
        );
        assert_eq!(layout.rail.as_ref().unwrap().entries.len(), count);
        layout.ensure_block(2 * count, &doc, &mut fonts);
        assert_eq!(layout.rail_active(1e9), Some(count - 1));
        let middle = layout.blocks[2 * 500].rect.y;
        assert_eq!(layout.rail_active(middle), Some(500));
    }

    #[test]
    fn should_keep_each_entry_one_line_when_a_heading_is_very_long() {
        let source = format!("## {}\n\ntext\n\n## Short\n", "word ".repeat(80));
        let theme = Theme::dark();
        let doc = parse(&source, Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let mut layout = lay_out_with(
            &doc,
            1600.0,
            &theme,
            1.0,
            &mut fonts,
            LayoutOptions::default(),
        );
        let mut pixels = vec![theme.bg.to_pixel(); 1600 * 400];
        layout.paint(&mut Canvas::new(&mut pixels, 1600, 400), &mut fonts, 0.0);
        let rail = layout.rail.as_ref().unwrap();
        let text = rail.entries[0]
            .text
            .as_ref()
            .expect("visible entry is shaped");
        assert!(text.height() <= rail.entries[0].height);
    }

    #[test]
    fn should_paint_the_rail_background_and_highlight_the_active_entry() {
        let doc = parse(DOC, Mode::File, &Settings::default());
        let theme = Theme::dark();
        let mut fonts = Fonts::new();
        let mut layout = lay_out_with(
            &doc,
            1600.0,
            &theme,
            1.0,
            &mut fonts,
            LayoutOptions::default(),
        );
        let mut pixels = vec![theme.bg.to_pixel(); 1600 * 400];
        layout.paint(&mut Canvas::new(&mut pixels, 1600, 400), &mut fonts, 0.0);
        assert_eq!(pixels[300 * 1600 + 5], rail_bg(&theme).to_pixel());
        assert_eq!(pixels[300 * 1600 + 1000], theme.bg.to_pixel());
        let entry = &layout.rail.as_ref().unwrap().entries[0];
        let y = (entry.top + 2.0) as usize;
        assert_eq!(pixels[y * 1600 + 14], theme.teal.to_pixel(), "active bar");
    }
}
