//! The document page: parses Markdown into a [`Doc`], lays it out lazily and
//! paints the visible part. Owns the scroll position and re-lays out on
//! resize and scale changes, keeping the block at the top of the window.

use crate::{
    layout::{Layout, LayoutOptions, lay_out_with},
    paint::Canvas,
    text::Fonts,
    theme::Theme,
    window::Page,
};
use limpid_core::{
    doc::{Doc, Mode, parse},
    settings::{Settings, ViewTheme},
    transport::Scheme,
};
use std::{cell::RefCell, path::Path, rc::Rc};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

pub type SharedFonts = Rc<RefCell<Fonts>>;

/// The theme for `scheme` with the `[view]` and `[style]` settings applied.
/// `view.theme` other than `auto` overrides `scheme`.
pub fn theme_for(settings: &Settings, scheme: Scheme) -> Theme {
    let view = &settings.view;
    let scheme = match view.theme {
        ViewTheme::Auto => scheme,
        ViewTheme::Dark => Scheme::Dark,
        ViewTheme::Light => Scheme::Light,
    };
    let mut theme = Theme::from_settings(settings, scheme);
    // `style.font_size` predates `view.font_size`; the newer key wins only
    // when it was moved off its default.
    if view.font_size > 0 && view.font_size != limpid_core::settings::View::default().font_size {
        theme.font_size = view.font_size as f32;
    }
    if view.line_height.is_finite() && view.line_height > 0.0 {
        theme.line_height = view.line_height;
    }
    if view.content_width > 0 {
        theme.content_width = view.content_width as f32;
    }
    if view.rail_width > 0 {
        theme.rail_width = view.rail_width as f32;
    }
    if view.scroll_fraction.is_finite() && view.scroll_fraction > 0.0 {
        theme.scroll_fraction = view.scroll_fraction.min(1.0);
    }
    if !view.font.trim().is_empty() {
        theme.families.sans = view.font.trim().to_owned();
    }
    if !view.mono_font.trim().is_empty() {
        theme.families.mono = view.mono_font.trim().to_owned();
    }
    theme
}

pub fn options_for(settings: &Settings) -> LayoutOptions {
    LayoutOptions {
        code_wrap: settings.view.code_wrap,
        rail: settings.view.rail,
    }
}

/// Inputs the layout was built for; a change throws it away.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutKey {
    width: f32,
    scale: f32,
}

pub struct DocPage {
    title: String,
    doc: Doc,
    theme: Theme,
    options: LayoutOptions,
    fonts: SharedFonts,
    layout: Option<Layout>,
    laid_for: Option<LayoutKey>,
    scale: f32,
    scroll: f32,
    viewport_height: f32,
    modifiers: ModifiersState,
}

impl DocPage {
    pub fn open(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
    ) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "limpid".to_owned());
        Ok(Self::from_source(
            &String::from_utf8_lossy(&bytes),
            stem,
            settings,
            scheme,
            fonts,
        ))
    }

    /// `fallback_title` names the window when the document has no title.
    pub fn from_source(
        source: &str,
        fallback_title: String,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
    ) -> Self {
        Self::with_theme(
            source,
            fallback_title,
            settings,
            theme_for(settings, scheme),
            options_for(settings),
            fonts,
        )
    }

    pub fn with_theme(
        source: &str,
        fallback_title: String,
        settings: &Settings,
        theme: Theme,
        options: LayoutOptions,
        fonts: SharedFonts,
    ) -> Self {
        let doc = parse(source, Mode::File, settings);
        let title = doc
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(fallback_title);
        Self {
            title,
            doc,
            theme,
            options,
            fonts,
            layout: None,
            laid_for: None,
            scale: 1.0,
            scroll: 0.0,
            viewport_height: 0.0,
            modifiers: ModifiersState::empty(),
        }
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn layout(&self) -> Option<&Layout> {
        self.layout.as_ref()
    }

    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// How many top-level blocks have been laid out so far.
    pub fn laid_out_blocks(&self) -> usize {
        self.layout.as_ref().map_or(0, |layout| layout.blocks.len())
    }

    fn line(&self) -> f32 {
        self.theme.font_size * self.theme.line_height * self.scale
    }

    fn max_scroll(&self) -> Option<f32> {
        let layout = self.layout.as_ref().filter(|layout| layout.is_complete())?;
        Some((layout.height - self.viewport_height).max(0.0))
    }

    fn ensure_to(&mut self, y: f32) {
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        layout.ensure_to(y, &self.doc, &mut self.fonts.borrow_mut());
    }

    fn scroll_to(&mut self, target: f32) {
        let mut target = if target.is_finite() {
            target.max(0.0)
        } else {
            0.0
        };
        if let Some(max) = self.max_scroll() {
            target = target.min(max);
        }
        self.scroll = target;
    }

    /// Lay the document out for a window `width` device pixels wide, keeping
    /// the block at the top of the viewport in place.
    fn relayout(&mut self, key: LayoutKey, old_scale: f32) {
        let anchor = self.layout.as_ref().and_then(|layout| {
            let index = layout.block_at(self.scroll)?;
            let rect = layout.blocks[index].rect;
            Some((index, (self.scroll - rect.y) / old_scale.max(f32::EPSILON)))
        });
        let mut fonts = self.fonts.borrow_mut();
        let mut layout = lay_out_with(
            &self.doc,
            key.width,
            &self.theme,
            key.scale,
            &mut fonts,
            self.options,
        );
        if let Some((index, offset)) = anchor {
            layout.ensure_block(index, &self.doc, &mut fonts);
            if let Some(block) = layout.blocks.get(index) {
                self.scroll = block.rect.y + offset * key.scale;
            }
        }
        drop(fonts);
        self.layout = Some(layout);
        self.laid_for = Some(key);
    }

    /// Handle one key press. `ctrl` is the Control modifier state.
    pub fn handle_key(&mut self, key: &Key, ctrl: bool) -> bool {
        let line = self.line();
        let half = self.viewport_height * self.theme.scroll_fraction;
        let delta = match key {
            Key::Character(text) if ctrl => match text.as_str() {
                "d" => half,
                "u" => -half,
                _ => return false,
            },
            Key::Character(text) => match text.as_str() {
                "j" => line,
                "k" => -line,
                "h" => return self.scroll_code(-line * 3.0),
                "l" => return self.scroll_code(line * 3.0),
                _ => return false,
            },
            Key::Named(NamedKey::ArrowDown) => line,
            Key::Named(NamedKey::ArrowUp) => -line,
            Key::Named(NamedKey::PageDown) => self.viewport_height - line,
            Key::Named(NamedKey::PageUp) => -(self.viewport_height - line),
            _ => return false,
        };
        let target = self.scroll + delta;
        // Lay out ahead of the move so the clamp knows whether the end is near.
        self.ensure_to(target + self.viewport_height * 2.0);
        self.scroll_to(target);
        true
    }

    /// Scroll the first wide code block in view sideways. Until the cursor
    /// exists, "focused" means topmost on screen.
    fn scroll_code(&mut self, delta: f32) -> bool {
        let (top, bottom) = (self.scroll, self.scroll + self.viewport_height);
        let Some(layout) = self.layout.as_mut() else {
            return false;
        };
        layout
            .code_block_in(top, bottom)
            .is_some_and(|block| block.scroll_code_x(delta))
    }
}

impl Page for DocPage {
    fn title(&self) -> String {
        self.title.clone()
    }

    fn paint(&mut self, canvas: &mut Canvas, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.viewport_height = canvas.height() as f32;
        let key = LayoutKey {
            width: canvas.width() as f32,
            scale,
        };
        if self.laid_for != Some(key) {
            self.relayout(key, self.scale);
        }
        self.scale = scale;
        self.ensure_to(self.scroll + self.viewport_height * 2.0);
        self.scroll_to(self.scroll);

        canvas.clear(self.theme.bg);
        let scroll = self.scroll;
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        layout.paint(canvas, &mut self.fonts.borrow_mut(), scroll);
    }

    fn key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        self.handle_key(&event.logical_key, self.modifiers.control_key())
    }

    fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(source: &str) -> DocPage {
        DocPage::from_source(
            source,
            "t".into(),
            &Settings::default(),
            Scheme::Dark,
            Rc::new(RefCell::new(Fonts::new())),
        )
    }

    fn paint(page: &mut DocPage, width: u32, height: u32, scale: f32) -> Vec<u32> {
        let mut pixels = vec![0; (width * height) as usize];
        page.paint(&mut Canvas::new(&mut pixels, width, height), scale);
        pixels
    }

    fn numbered(count: usize) -> String {
        (0..count).map(|n| format!("line {n}\n\n")).collect()
    }

    #[test]
    fn should_paint_background_in_the_corner_and_ink_somewhere_when_painting() {
        let mut page = page("# Hello\n\nsome body text\n");
        let pixels = paint(&mut page, 400, 300, 1.0);
        assert_eq!(pixels[0], Theme::dark().bg.to_pixel());
        assert!(pixels.iter().any(|&p| p != Theme::dark().bg.to_pixel()));
    }

    #[test]
    fn should_scroll_by_lines_and_clamp_at_the_top_when_pressing_j_and_k() {
        let mut page = page(&numbered(200));
        paint(&mut page, 400, 300, 1.0);
        assert!(page.handle_key(&Key::Character("j".into()), false));
        assert!(page.scroll() > 0.0);
        page.handle_key(&Key::Character("k".into()), false);
        page.handle_key(&Key::Character("k".into()), false);
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_scroll_half_a_viewport_when_pressing_ctrl_d_and_ctrl_u() {
        let mut page = page(&numbered(200));
        paint(&mut page, 400, 300, 1.0);
        assert!(page.handle_key(&Key::Character("d".into()), true));
        assert_eq!(page.scroll(), 150.0);
        assert!(page.handle_key(&Key::Character("u".into()), true));
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_ignore_keys_when_they_are_not_bound() {
        let mut page = page("text");
        paint(&mut page, 200, 100, 1.0);
        assert!(!page.handle_key(&Key::Character("q".into()), false));
        assert!(!page.handle_key(&Key::Character("d".into()), false));
        assert!(!page.handle_key(&Key::Named(NamedKey::Escape), false));
    }

    #[test]
    fn should_not_scroll_past_the_end_when_the_document_is_short() {
        let mut page = page("short\n");
        paint(&mut page, 400, 300, 1.0);
        page.handle_key(&Key::Character("d".into()), true);
        page.handle_key(&Key::Named(NamedKey::PageDown), false);
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_stop_at_the_bottom_when_scrolling_far_past_a_long_document() {
        let mut page = page(&numbered(60));
        paint(&mut page, 400, 300, 1.0);
        for _ in 0..500 {
            page.handle_key(&Key::Character("j".into()), false);
        }
        paint(&mut page, 400, 300, 1.0);
        let max = page.max_scroll().expect("whole document is laid out");
        assert_eq!(page.scroll(), max);
        assert!(max > 0.0);
    }

    #[test]
    fn should_lay_out_lazily_when_the_document_is_large() {
        let mut page = page(&"paragraph of text\n\n".repeat(20_000));
        paint(&mut page, 800, 600, 1.0);
        assert_eq!(page.doc().blocks.len(), 20_000);
        assert!(page.laid_out_blocks() < 200, "{}", page.laid_out_blocks());
    }

    #[test]
    fn should_relayout_when_the_scale_changes() {
        let mut page = page("# Title\n\nbody\n");
        paint(&mut page, 400, 300, 1.0);
        let one = page.layout().unwrap().height;
        paint(&mut page, 800, 600, 2.0);
        assert!(page.layout().unwrap().height > one * 1.5);
    }

    #[test]
    fn should_keep_the_top_block_in_place_when_the_window_is_resized() {
        let mut page = page(&numbered(300));
        paint(&mut page, 600, 400, 1.0);
        page.handle_key(&Key::Character("d".into()), true);
        page.handle_key(&Key::Character("d".into()), true);
        paint(&mut page, 600, 400, 1.0);
        let before = {
            let layout = page.layout().unwrap();
            layout.block_at(page.scroll()).unwrap()
        };
        paint(&mut page, 900, 400, 1.0);
        let layout = page.layout().unwrap();
        let after = layout.block_at(page.scroll()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn should_survive_a_zero_sized_canvas_and_a_bad_scale() {
        let mut page = page("text\n\nmore");
        paint(&mut page, 0, 0, 1.0);
        paint(&mut page, 10, 10, f32::NAN);
        paint(&mut page, 10, 10, -2.0);
    }

    #[test]
    fn should_use_the_document_title_when_it_has_one_else_the_fallback() {
        assert_eq!(page("# My Doc\n\nbody").title(), "My Doc");
        assert_eq!(page("no heading here").title(), "t");
        assert_eq!(page("").title(), "t");
    }

    #[test]
    fn should_report_the_file_stem_when_opening_a_path() {
        let dir = std::env::temp_dir().join(format!("limpid-view-page-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, b"plain\n\xff\xfe bad utf8\n").unwrap();
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let page = DocPage::open(&path, &Settings::default(), Scheme::Dark, fonts.clone()).unwrap();
        assert_eq!(page.title(), "notes");
        assert_eq!(page.doc().blocks.len(), 1);
        assert!(
            DocPage::open(
                &dir.join("missing.md"),
                &Settings::default(),
                Scheme::Dark,
                fonts
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_apply_view_settings_when_building_the_theme() {
        let settings = Settings::parse(
            "[view]\nfont = \"Inter\"\nmono_font = \"Hack\"\nfont_size = 20\nline_height = 1.4\n\
             content_width = 700\nrail_width = 180\nscroll_fraction = 0.25\ntheme = \"light\"\n",
        )
        .unwrap();
        let theme = theme_for(&settings, Scheme::Dark);
        assert_eq!(theme.scheme, Scheme::Light);
        assert_eq!(theme.font_size, 20.0);
        assert_eq!(theme.line_height, 1.4);
        assert_eq!(theme.content_width, 700.0);
        assert_eq!(theme.rail_width, 180.0);
        assert_eq!(theme.scroll_fraction, 0.25);
        assert_eq!(theme.families.sans, "Inter");
        assert_eq!(theme.families.mono, "Hack");
    }

    #[test]
    fn should_follow_the_cli_scheme_when_view_theme_is_auto() {
        let theme = theme_for(&Settings::default(), Scheme::Light);
        assert_eq!(theme.scheme, Scheme::Light);
    }

    #[test]
    fn should_keep_the_style_font_size_when_view_font_size_is_untouched() {
        let settings = Settings::parse("[style]\nfont_size = 22\n").unwrap();
        assert_eq!(theme_for(&settings, Scheme::Dark).font_size, 22.0);
    }

    #[test]
    fn should_ignore_nonsense_view_values_when_building_the_theme() {
        let settings = Settings::parse(
            "[view]\nline_height = -1.0\nscroll_fraction = 0.0\nfont = \"  \"\ncontent_width = 0\n",
        )
        .unwrap();
        let theme = theme_for(&settings, Scheme::Dark);
        assert_eq!(theme.line_height, 1.65);
        assert_eq!(theme.scroll_fraction, 0.5);
        assert_eq!(theme.families.sans, "sans-serif");
        assert_eq!(theme.content_width, 860.0);
    }

    #[test]
    fn should_pass_view_layout_settings_through_when_building_options() {
        let settings = Settings::parse("[view]\ncode_wrap = true\nrail = \"off\"\n").unwrap();
        let options = options_for(&settings);
        assert!(options.code_wrap);
        assert_eq!(options.rail, limpid_core::settings::RailMode::Off);
    }

    #[test]
    fn should_scroll_a_wide_code_block_sideways_when_pressing_l_and_h() {
        let source = format!("```\n{}\n```\n", "w".repeat(500));
        let mut page = page(&source);
        paint(&mut page, 600, 400, 1.0);
        assert!(
            !page.handle_key(&Key::Character("h".into()), false),
            "already at the left edge"
        );
        assert!(page.handle_key(&Key::Character("l".into()), false));
        assert!(page.handle_key(&Key::Character("h".into()), false));
        assert!(page.scroll_code(1e6));
        assert!(
            !page.handle_key(&Key::Character("l".into()), false),
            "already at the right edge"
        );
    }
}
