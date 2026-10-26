//! The document page: parses Markdown into a [`Doc`], lays it out lazily and
//! paints the visible part. Owns the scroll position and re-lays out on
//! resize and scale changes, keeping the block at the top of the window. Keys
//! go to [`Nav`], which moves the cursor, searches, yanks and runs commands.

use crate::{
    layout::{Layout, LayoutOptions, lay_out_with},
    nav::{
        CloseKind, Io, Nav, NavOptions, NoActions, PageActions, Request, ZoomStep,
        cursor::{self, Env},
        keymap::{KeyInput, Keymap},
        search, selection_tint, status,
        yank::{Clipboard, NoClipboard, ReplyCommand},
    },
    paint::Canvas,
    text::Fonts,
    theme::Theme,
    window::Page,
};
use limpid_core::{
    doc::{Doc, Mode, parse},
    settings::{RailMode, Settings, ViewTheme},
    transport::Scheme,
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState},
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

/// Closes windows on the page's behalf. The window owner implements it; a page
/// without one cannot close itself.
pub trait Closer {
    /// Returns false when the request cannot be honoured.
    fn close(&self, kind: CloseKind) -> bool;
}

/// Where a reload keeps the top of the viewport: the block that was there,
/// found again by its text, and how far below its top the viewport sat (CSS
/// px).
#[derive(Debug, Clone, PartialEq)]
struct SourceAnchor {
    index: usize,
    text: String,
    offset_css: f32,
}

/// The trimmed source of top-level block `index`.
fn block_text(doc: &Doc, index: usize) -> Option<&str> {
    let block = doc.blocks.get(index)?;
    Some(doc.source.get(block.src.clone())?.trim())
}

/// The block of `doc` with the same text as `text`, nearest to `near`; else
/// `near` itself (clamped), for a block that was edited.
fn find_block_like(doc: &Doc, text: &str, near: usize) -> Option<usize> {
    let last = doc.blocks.len().checked_sub(1)?;
    let near = near.min(last);
    for distance in 0..=last {
        let below = near.checked_sub(distance);
        let above = near + distance;
        for candidate in [below, (above <= last).then_some(above)]
            .into_iter()
            .flatten()
        {
            if block_text(doc, candidate) == Some(text) {
                return Some(candidate);
            }
        }
    }
    Some(near)
}

pub struct DocPage {
    title: String,
    doc: Doc,
    theme: Theme,
    base_font_size: f32,
    zoom: f32,
    options: LayoutOptions,
    settings: Settings,
    path: Option<PathBuf>,
    fonts: SharedFonts,
    layout: Option<Layout>,
    laid_for: Option<LayoutKey>,
    scale: f32,
    scroll: f32,
    /// Height of the area the document shows in, device pixels (the canvas
    /// less the status bar).
    viewport_height: f32,
    modifiers: ModifiersState,
    nav: Nav,
    actions: Box<dyn PageActions>,
    clipboard: Box<dyn Clipboard>,
    closer: Option<Box<dyn Closer>>,
    /// Paint the focus band, search highlights and status bar.
    chrome: bool,
    pending_anchor: Option<SourceAnchor>,
    clock: Instant,
}

impl DocPage {
    pub fn open(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
    ) -> Result<Self, String> {
        Keymap::new(&settings.view.keys)?;
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "limpid".to_owned());
        let page = Self::from_source(
            &String::from_utf8_lossy(&bytes),
            stem,
            settings,
            scheme,
            fonts,
        );
        Ok(page.interactive(Some(path.to_path_buf())))
    }

    /// `fallback_title` names the window when the document has no title. The
    /// page has no status bar, focus band or clipboard until
    /// [`DocPage::interactive`] adds them.
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
        let (nav, key_error) = match Nav::new(&settings.view.keys, NavOptions::default()) {
            Ok(nav) => (nav, None),
            Err(error) => (
                Nav::with_keymap(Keymap::defaults(), NavOptions::default()),
                Some(error),
            ),
        };
        let mut page = Self {
            title,
            doc,
            base_font_size: theme.font_size,
            zoom: 1.0,
            theme,
            options,
            settings: settings.clone(),
            path: None,
            fonts,
            layout: None,
            laid_for: None,
            scale: 1.0,
            scroll: 0.0,
            viewport_height: 0.0,
            modifiers: ModifiersState::empty(),
            nav,
            actions: Box::new(NoActions),
            clipboard: Box::new(NoClipboard),
            closer: None,
            chrome: false,
            pending_anchor: None,
            clock: Instant::now(),
        };
        if let Some(error) = key_error {
            eprintln!("limpid-view: {error}");
            page.flash(format!("keys: {error}"));
        }
        page
    }

    /// Turn the page into what a window shows: the status bar, focus band and
    /// search highlights, and yanks that go through `reply.command`. `path` is
    /// the file `r` reloads.
    pub fn interactive(mut self, path: Option<PathBuf>) -> Self {
        self.chrome = true;
        self.clipboard = Box::new(ReplyCommand {
            command: self.settings.reply.command.clone(),
            source: path.clone(),
        });
        self.path = path;
        self
    }

    pub fn with_chrome(mut self, chrome: bool) -> Self {
        self.chrome = chrome;
        self
    }

    pub fn with_actions(mut self, actions: Box<dyn PageActions>) -> Self {
        self.actions = actions;
        self
    }

    pub fn with_clipboard(mut self, clipboard: Box<dyn Clipboard>) -> Self {
        self.clipboard = clipboard;
        self
    }

    pub fn with_closer(mut self, closer: Box<dyn Closer>) -> Self {
        self.closer = Some(closer);
        self
    }

    pub fn with_nav_options(mut self, options: NavOptions) -> Self {
        self.nav.options = options;
        self
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
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

    /// The font size after zoom, CSS px.
    pub fn font_size(&self) -> f32 {
        self.theme.font_size
    }

    /// How many top-level blocks have been laid out so far.
    pub fn laid_out_blocks(&self) -> usize {
        self.layout.as_ref().map_or(0, |layout| layout.blocks.len())
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn flash(&mut self, text: impl Into<String>) {
        let now = self.now_ms();
        self.nav.flash(text, now);
    }

    /// Run `body` with the navigation state and the view it works on. `None`
    /// before the first paint, when there is no layout yet.
    fn with_nav<R>(&mut self, body: impl FnOnce(&mut Nav, &mut Env, &mut Io) -> R) -> Option<R> {
        let layout = self.layout.as_mut()?;
        let mut fonts = self.fonts.borrow_mut();
        let mut env = Env {
            doc: &self.doc,
            layout,
            fonts: &mut fonts,
            scroll: &mut self.scroll,
            viewport: self.viewport_height,
        };
        let mut io = Io {
            actions: &mut *self.actions,
            clipboard: &mut *self.clipboard,
        };
        Some(body(&mut self.nav, &mut env, &mut io))
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
        let max = self
            .layout
            .as_ref()
            .filter(|layout| layout.is_complete())
            .map(|layout| (layout.height - self.viewport_height).max(0.0));
        if let Some(max) = max {
            target = target.min(max);
        }
        self.scroll = target;
    }

    /// Lay the document out for a window `width` device pixels wide, keeping
    /// the block at the top of the viewport in place.
    fn relayout(&mut self, key: LayoutKey, old_scale: f32) {
        let from_layout = self.layout.as_ref().and_then(|layout| {
            let index = layout.block_at(self.scroll)?;
            let rect = layout.blocks[index].rect;
            Some((index, (self.scroll - rect.y) / old_scale.max(f32::EPSILON)))
        });
        let from_source = self.pending_anchor.take().and_then(|anchor| {
            let index = find_block_like(&self.doc, &anchor.text, anchor.index)?;
            Some((index, anchor.offset_css))
        });
        let anchor = from_source.or(from_layout);
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

    /// Replace the document with `source`, keeping the viewport on the same
    /// part of the text. The cursor and search follow by source position.
    pub fn replace_source(&mut self, source: &str) {
        self.pending_anchor = self.layout.as_ref().and_then(|layout| {
            let index = layout.block_at(self.scroll)?;
            let block = layout.blocks.get(index)?;
            Some(SourceAnchor {
                index,
                text: block_text(&self.doc, index)?.to_owned(),
                offset_css: (self.scroll - block.rect.y) / self.scale.max(f32::EPSILON),
            })
        });
        // The cursor follows its block by text, since offsets shift when text
        // above it changes.
        let cursor_block = self.layout.as_ref().and_then(|layout| {
            let top = self.nav.cursor().stop(layout)?.top;
            Some((top, block_text(&self.doc, top)?.to_owned()))
        });
        self.doc = parse(source, Mode::File, &self.settings);
        let retargeted = cursor_block.and_then(|(top, text)| {
            let index = find_block_like(&self.doc, &text, top)?;
            Some(self.doc.blocks.get(index)?.src.clone())
        });
        self.nav.retarget_cursor(retargeted);
        self.nav.document_changed(&self.doc);
        self.layout = None;
        self.laid_for = None;
    }

    fn reload(&mut self) {
        let Some(path) = self.path.clone() else {
            self.flash("nothing to reload");
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                self.replace_source(&String::from_utf8_lossy(&bytes));
                self.flash("Reloaded");
            }
            Err(error) => self.flash(format!("reload failed: {error}")),
        }
    }

    fn zoom_by(&mut self, step: ZoomStep) {
        let options = self.nav.options;
        let (low, high) = options.zoom_range;
        let step_factor = if options.zoom_step.is_finite() && options.zoom_step > 1.0 {
            options.zoom_step
        } else {
            1.1
        };
        self.zoom = match step {
            ZoomStep::In => self.zoom * step_factor,
            ZoomStep::Out => self.zoom / step_factor,
            ZoomStep::Reset => 1.0,
        }
        .clamp(low.min(high), high.max(low));
        self.theme.font_size = self.base_font_size * self.zoom;
        self.laid_for = None;
        self.flash(format!("Zoom {}%", (self.zoom * 100.0).round()));
    }

    fn toggle_rail(&mut self) {
        let showing = self
            .layout
            .as_ref()
            .is_some_and(|layout| layout.rail.is_some());
        self.options.rail = if showing { RailMode::Off } else { RailMode::On };
        self.laid_for = None;
    }

    fn perform(&mut self, request: Request) {
        match request {
            Request::Close(kind) => {
                let closed = self
                    .closer
                    .as_ref()
                    .is_some_and(|closer| closer.close(kind));
                if !closed {
                    let message = match (&self.closer, kind) {
                        (Some(_), CloseKind::Window) => {
                            "other windows are open: close this one from the window manager, or :qa"
                        }
                        _ => "this page cannot close itself",
                    };
                    self.flash(message);
                }
            }
            Request::Reload => self.reload(),
            Request::Zoom(step) => self.zoom_by(step),
            Request::ToggleRail => self.toggle_rail(),
        }
    }

    /// Handle one key press. Returns true when the page changed and needs a
    /// repaint.
    pub fn handle_input(&mut self, input: &KeyInput) -> bool {
        let now = self.now_ms();
        let Some(outcome) = self.with_nav(|nav, env, io| nav.handle(input, now, env, io)) else {
            return false;
        };
        let mut changed = outcome.redraw;
        for request in outcome.requests {
            self.perform(request);
            changed = true;
        }
        changed
    }

    /// [`DocPage::handle_input`] for a winit key and whether Control is held.
    pub fn handle_key(&mut self, key: &Key, ctrl: bool) -> bool {
        let modifiers = if ctrl {
            ModifiersState::CONTROL
        } else {
            ModifiersState::empty()
        };
        KeyInput::from_winit(key, modifiers).is_some_and(|input| self.handle_input(&input))
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
        let total_height = canvas.height() as f32;
        let bar = if self.chrome {
            status::height(&self.theme, scale)
        } else {
            0.0
        };
        let bar = if bar < total_height { bar } else { 0.0 };
        self.viewport_height = total_height - bar;
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
        self.with_nav(|nav, env, _| nav.init(env));

        canvas.clear(self.theme.bg);
        let scroll = self.scroll;
        let now = self.now_ms();
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        if self.chrome {
            let bands = self.nav.bands(layout);
            if let Some(selection) = bands.selection {
                let tint = selection_tint(&self.theme);
                cursor::paint_band(canvas, layout, &self.theme, selection, scroll, tint, false);
            }
            if let Some(focus) = bands.focus {
                let tint = self.theme.focus;
                cursor::paint_band(canvas, layout, &self.theme, focus, scroll, tint, true);
            }
        }
        layout.paint(canvas, &mut self.fonts.borrow_mut(), scroll);
        if !self.chrome {
            return;
        }
        if let Some((query, current)) = self.nav.highlight() {
            search::paint_highlights(canvas, layout, &query, current, scroll);
        }
        let view = self.nav.status_view(now);
        status::paint(
            canvas,
            &mut self.fonts.borrow_mut(),
            &self.theme,
            scale,
            &view,
        );
    }

    fn key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        KeyInput::from_winit(&event.logical_key, self.modifiers)
            .is_some_and(|input| self.handle_input(&input))
    }

    fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::{ActionResult, FeedbackAction, keymap::Chord};
    use std::{cell::Cell, ops::Range};
    use winit::keyboard::NamedKey;

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

    fn press(page: &mut DocPage, key: &str) -> bool {
        page.handle_input(&KeyInput::named(key))
    }

    fn type_line(page: &mut DocPage, text: &str) {
        for character in text.chars() {
            let name = if character == ' ' {
                "space".to_owned()
            } else {
                character.to_string()
            };
            press(page, &name);
        }
    }

    fn focused(page: &DocPage) -> String {
        let stop = page.nav().cursor().stop(page.layout().unwrap()).unwrap();
        page.doc().source[stop.src].trim().to_owned()
    }

    #[test]
    fn should_paint_background_in_the_corner_and_ink_somewhere_when_painting() {
        let mut page = page("# Hello\n\nsome body text\n");
        let pixels = paint(&mut page, 400, 300, 1.0);
        assert_eq!(pixels[0], Theme::dark().bg.to_pixel());
        assert!(pixels.iter().any(|&p| p != Theme::dark().bg.to_pixel()));
    }

    #[test]
    fn should_move_the_cursor_not_the_scroll_when_pressing_j_and_k_on_screen() {
        let mut page = page(&numbered(200));
        paint(&mut page, 400, 300, 1.0);
        assert!(page.handle_key(&Key::Character("j".into()), false));
        assert_eq!(focused(&page), "line 1");
        assert_eq!(page.scroll(), 0.0, "the block is still on screen");
        page.handle_key(&Key::Character("k".into()), false);
        assert_eq!(focused(&page), "line 0");
        assert!(!page.handle_key(&Key::Character("k".into()), false));
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_scroll_to_follow_the_cursor_when_it_leaves_the_window() {
        let mut page = page(&numbered(200));
        paint(&mut page, 400, 300, 1.0);
        for _ in 0..40 {
            page.handle_key(&Key::Character("j".into()), false);
        }
        assert!(page.scroll() > 0.0);
        paint(&mut page, 400, 300, 1.0);
        assert_eq!(focused(&page), "line 40");
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
    fn should_use_the_configured_fraction_when_scrolling_a_half_page() {
        let settings = Settings::parse("[view]\nscroll_fraction = 0.25\n").unwrap();
        let mut page = DocPage::from_source(
            &numbered(200),
            "t".into(),
            &settings,
            Scheme::Dark,
            Rc::new(RefCell::new(Fonts::new())),
        );
        paint(&mut page, 400, 400, 1.0);
        page.handle_key(&Key::Character("d".into()), true);
        assert_eq!(page.scroll(), 100.0);
    }

    #[test]
    fn should_ignore_keys_when_they_are_not_bound() {
        let mut page = page("text");
        paint(&mut page, 200, 100, 1.0);
        assert!(!page.handle_key(&Key::Character("q".into()), false));
        assert!(!page.handle_key(&Key::Character("d".into()), false));
        assert!(!page.handle_key(&Key::Named(NamedKey::Escape), false));
        assert!(!page.handle_key(&Key::Named(NamedKey::Shift), false));
    }

    #[test]
    fn should_ignore_keys_when_nothing_has_been_painted_yet() {
        let mut page = page("text");
        assert!(!page.handle_key(&Key::Character("j".into()), false));
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
            page.handle_key(&Key::Named(NamedKey::PageDown), false);
        }
        paint(&mut page, 400, 300, 1.0);
        let layout = page.layout().unwrap();
        let max = layout.height - 300.0;
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
        let mut chrome = self::page("text\n\nmore").with_chrome(true);
        paint(&mut chrome, 0, 0, 1.0);
        paint(&mut chrome, 10, 10, 1.0);
        paint(&mut chrome, 300, 20, 2.0);
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
    fn should_refuse_to_open_when_a_key_override_is_wrong() {
        let dir = std::env::temp_dir().join(format!("limpid-view-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        std::fs::write(&path, "text").unwrap();
        let settings = Settings::parse("[view.keys]\nnext_blok = \"j\"\n").unwrap();
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let error = DocPage::open(&path, &settings, Scheme::Dark, fonts)
            .err()
            .unwrap();
        assert!(error.contains("next_blok"), "{error}");
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
        for _ in 0..100 {
            page.handle_key(&Key::Character("l".into()), false);
        }
        assert!(
            !page.handle_key(&Key::Character("l".into()), false),
            "already at the right edge"
        );
    }

    // Chrome: the band, highlights and status bar.

    fn chrome_page(source: &str) -> DocPage {
        page(source).with_chrome(true)
    }

    #[test]
    fn should_paint_the_focus_band_behind_the_focused_block_only() {
        let mut page = chrome_page("one\n\ntwo\n\nthree\n\nfour\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "j");
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let layout = page.layout().unwrap();
        let gutter = (layout.column_x - 5.0) as usize;
        let at = |block: usize| {
            let rect = layout.blocks[block].rect;
            pixels[(rect.y + rect.h / 2.0) as usize * 800 + gutter]
        };
        let bg = Theme::dark().bg.to_pixel();
        assert_eq!(at(1), bg, "second block is not focused");
        assert_ne!(at(2), bg, "third block carries the band");
        assert_eq!(at(3), bg);
    }

    #[test]
    fn should_leave_the_page_bare_when_chrome_is_off() {
        let mut bare = page("one\n\ntwo\n");
        let pixels = paint(&mut bare, 800, 600, 1.0);
        let bg = Theme::dark().bg.to_pixel();
        assert!(
            pixels[800 * 599..].iter().all(|&p| p == bg),
            "no status bar"
        );
        let layout = bare.layout().unwrap();
        let gutter = (layout.column_x - 5.0) as usize;
        let y = (layout.blocks[0].rect.y + 4.0) as usize;
        assert_eq!(pixels[y * 800 + gutter], bg, "no focus band");
    }

    #[test]
    fn should_paint_the_status_bar_with_the_state_name_when_chrome_is_on() {
        let mut page = chrome_page("one\n\ntwo\n");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        assert_eq!(pixels[800 * 599 + 2], theme.surface.to_pixel());
        let bar = status::height(&theme, 1.0) as usize;
        let text_pixels = pixels[800 * (600 - bar)..]
            .iter()
            .filter(|&&p| p != theme.surface.to_pixel())
            .count();
        assert!(text_pixels > 50, "the label is drawn");
        assert_eq!(page.nav().state(), crate::nav::status::StateName::Page);
    }

    #[test]
    fn should_keep_the_document_above_the_status_bar_when_scrolling_to_the_end() {
        let mut page = chrome_page(&numbered(40));
        paint(&mut page, 600, 400, 1.0);
        press(&mut page, "shift+g");
        paint(&mut page, 600, 400, 1.0);
        let bar = status::height(&Theme::dark(), 1.0);
        let layout = page.layout().unwrap();
        assert_eq!(page.scroll(), layout.height - (400.0 - bar));
    }

    #[test]
    fn should_highlight_matches_and_show_the_prompt_when_searching() {
        let mut page = chrome_page("alpha needle beta\n\nplain\n");
        let before = paint(&mut page, 800, 400, 1.0);
        press(&mut page, "/");
        type_line(&mut page, "needle");
        let typing = paint(&mut page, 800, 400, 1.0);
        assert_ne!(before, typing, "highlight and prompt appear while typing");
        press(&mut page, "enter");
        let after = paint(&mut page, 800, 400, 1.0);
        assert_ne!(typing, after);
        assert_eq!(page.nav().search().position(), Some((1, 1)));
        press(&mut page, "escape");
        let cleared = paint(&mut page, 800, 400, 1.0);
        assert_ne!(after, cleared);
    }

    #[test]
    fn should_draw_the_selection_band_taller_than_the_focus_band_when_in_visual_mode() {
        let mut page = chrome_page("one\n\ntwo\n\nthree\n");
        paint(&mut page, 800, 400, 1.0);
        let plain = paint(&mut page, 800, 400, 1.0);
        press(&mut page, "v");
        press(&mut page, "j");
        let visual = paint(&mut page, 800, 400, 1.0);
        let changed = plain.iter().zip(&visual).filter(|(a, b)| a != b).count();
        assert!(changed > 1000, "{changed}");
    }

    // Zoom, rail, reload.

    #[test]
    fn should_change_the_font_size_and_relayout_when_zooming() {
        let mut page = chrome_page("some text here\n");
        paint(&mut page, 600, 400, 1.0);
        let base = page.font_size();
        let height = page.layout().unwrap().height;
        press(&mut page, "+");
        paint(&mut page, 600, 400, 1.0);
        assert!(page.font_size() > base);
        assert!(page.layout().unwrap().height > height);
        press(&mut page, "-");
        press(&mut page, "-");
        paint(&mut page, 600, 400, 1.0);
        assert!(page.font_size() < base);
        press(&mut page, "0");
        assert_eq!(page.font_size(), base);
    }

    #[test]
    fn should_stop_zooming_at_the_configured_limits() {
        let mut page = chrome_page("text\n");
        paint(&mut page, 600, 400, 1.0);
        let base = page.font_size();
        for _ in 0..200 {
            press(&mut page, "+");
        }
        assert!((page.font_size() / base - 3.0).abs() < 0.01);
        for _ in 0..400 {
            press(&mut page, "-");
        }
        assert!((page.font_size() / base - 0.5).abs() < 0.01);
    }

    #[test]
    fn should_flash_the_zoom_level_when_zooming() {
        let mut page = chrome_page("text\n");
        paint(&mut page, 600, 400, 1.0);
        press(&mut page, "+");
        assert_eq!(page.nav().status_view(page.now_ms()).message, "Zoom 110%");
    }

    #[test]
    fn should_toggle_the_rail_when_pressing_t() {
        let mut page = chrome_page("# One\n\ntext\n\n## Two\n\ntext\n");
        paint(&mut page, 1400, 600, 1.0);
        assert!(page.layout().unwrap().rail.is_some());
        press(&mut page, "t");
        paint(&mut page, 1400, 600, 1.0);
        assert!(page.layout().unwrap().rail.is_none());
        press(&mut page, "f1");
        paint(&mut page, 1400, 600, 1.0);
        assert!(page.layout().unwrap().rail.is_some());
    }

    #[test]
    fn should_reread_the_file_and_keep_the_place_when_pressing_r() {
        let dir = std::env::temp_dir().join(format!("limpid-view-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, numbered(100)).unwrap();
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let mut page = DocPage::open(&path, &Settings::default(), Scheme::Dark, fonts).unwrap();
        paint(&mut page, 500, 300, 1.0);
        for _ in 0..30 {
            press(&mut page, "j");
        }
        paint(&mut page, 500, 300, 1.0);
        let top_before = {
            let layout = page.layout().unwrap();
            let index = layout.block_at(page.scroll()).unwrap();
            page.doc().source[page.doc().blocks[index].src.clone()]
                .trim()
                .to_owned()
        };
        // New text above shifts everything down; the viewport stays on the
        // same lines.
        let mut changed = "A new first paragraph.\n\n".to_owned();
        changed.push_str(&numbered(100));
        std::fs::write(&path, changed).unwrap();
        assert!(press(&mut page, "r"));
        paint(&mut page, 500, 300, 1.0);
        assert_eq!(page.doc().blocks.len(), 101);
        let layout = page.layout().unwrap();
        let index = layout.block_at(page.scroll()).unwrap();
        let top_after = page.doc().source[page.doc().blocks[index].src.clone()]
            .trim()
            .to_owned();
        assert_eq!(top_after, top_before);
        assert_eq!(focused(&page), "line 30");
        assert_eq!(page.nav().status_view(page.now_ms()).message, "Reloaded");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_say_so_when_reloading_a_page_without_a_file() {
        let mut page = chrome_page("text\n");
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "r");
        assert_eq!(
            page.nav().status_view(page.now_ms()).message,
            "nothing to reload"
        );
    }

    #[test]
    fn should_flash_when_the_file_vanished_before_reloading() {
        let dir = std::env::temp_dir().join(format!("limpid-view-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "text\n").unwrap();
        let fonts = Rc::new(RefCell::new(Fonts::new()));
        let mut page = DocPage::open(&path, &Settings::default(), Scheme::Dark, fonts).unwrap();
        paint(&mut page, 500, 300, 1.0);
        std::fs::remove_dir_all(&dir).unwrap();
        press(&mut page, "r");
        let message = page.nav().status_view(page.now_ms()).message;
        assert!(message.starts_with("reload failed"), "{message}");
        assert_eq!(page.doc().blocks.len(), 1, "the old text stays");
    }

    // Closing.

    struct Closed(Rc<Cell<Option<CloseKind>>>, bool);

    impl Closer for Closed {
        fn close(&self, kind: CloseKind) -> bool {
            self.0.set(Some(kind));
            self.1
        }
    }

    #[test]
    fn should_ask_the_closer_when_running_q_and_qa() {
        let seen = Rc::new(Cell::new(None));
        let mut page = chrome_page("text\n").with_closer(Box::new(Closed(seen.clone(), true)));
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, ":");
        type_line(&mut page, "q");
        press(&mut page, "enter");
        assert_eq!(seen.get(), Some(CloseKind::Window));
        press(&mut page, ":");
        type_line(&mut page, "qa");
        press(&mut page, "enter");
        assert_eq!(seen.get(), Some(CloseKind::All));
    }

    #[test]
    fn should_flash_when_the_window_cannot_be_closed() {
        let seen = Rc::new(Cell::new(None));
        let mut page = chrome_page("text\n").with_closer(Box::new(Closed(seen, false)));
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "ctrl+q");
        let message = page.nav().status_view(page.now_ms()).message;
        assert!(message.contains(":qa"), "{message}");
        let mut loose = chrome_page("text\n");
        paint(&mut loose, 500, 300, 1.0);
        press(&mut loose, "ctrl+q");
        assert_eq!(
            loose.nav().status_view(loose.now_ms()).message,
            "this page cannot close itself"
        );
    }

    // Yank and actions through the page.

    struct Copies(Rc<RefCell<Vec<String>>>);

    impl Clipboard for Copies {
        fn copy(&mut self, text: &str) -> Result<(), String> {
            self.0.borrow_mut().push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn should_hand_yanked_text_to_the_clipboard_when_pressing_yy() {
        let copies = Rc::new(RefCell::new(Vec::new()));
        let mut page = chrome_page("first\n\n```\ncode\n```\n")
            .with_clipboard(Box::new(Copies(copies.clone())));
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "y");
        press(&mut page, "y");
        press(&mut page, "j");
        press(&mut page, "y");
        press(&mut page, "y");
        assert_eq!(*copies.borrow(), vec!["first\n", "code\n"]);
    }

    #[test]
    fn should_not_run_any_command_when_a_bare_page_yanks() {
        let mut page = page("first\n");
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "y");
        press(&mut page, "y");
        let message = page.nav().status_view(page.now_ms()).message;
        assert!(message.starts_with("Yank failed"), "{message}");
    }

    struct Recording(Rc<RefCell<Vec<FeedbackAction>>>);

    impl PageActions for Recording {
        fn feedback(&mut self, action: FeedbackAction, _: Option<&Range<usize>>) -> ActionResult {
            self.0.borrow_mut().push(action);
            ActionResult::Done("ok".into())
        }
        fn digit_selects_option(&self, _: &Range<usize>) -> bool {
            true
        }
    }

    #[test]
    fn should_route_digits_to_the_page_actions_when_a_question_is_focused() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut page = chrome_page("text\n").with_actions(Box::new(Recording(log.clone())));
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "2");
        press(&mut page, "c");
        assert_eq!(
            *log.borrow(),
            vec![FeedbackAction::PickOption(2), FeedbackAction::OpenComment]
        );
    }

    #[test]
    fn should_use_remapped_keys_when_the_settings_have_overrides() {
        let settings = Settings::parse("[view.keys]\nnext_block = \"ctrl+n\"\n").unwrap();
        let mut page = DocPage::from_source(
            "one\n\ntwo\n",
            "t".into(),
            &settings,
            Scheme::Dark,
            Rc::new(RefCell::new(Fonts::new())),
        );
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "j");
        assert_eq!(focused(&page), "one");
        press(&mut page, "ctrl+n");
        assert_eq!(focused(&page), "two");
    }

    #[test]
    fn should_fall_back_to_the_defaults_and_say_why_when_a_bare_page_has_bad_keys() {
        let settings = Settings::parse("[view.keys]\nnope = \"j\"\n").unwrap();
        let mut page = DocPage::from_source(
            "one\n\ntwo\n",
            "t".into(),
            &settings,
            Scheme::Dark,
            Rc::new(RefCell::new(Fonts::new())),
        );
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, "j");
        assert_eq!(focused(&page), "two");
        let message = page.nav().status_view(page.now_ms()).message;
        assert!(message.contains("nope"), "{message}");
    }

    #[test]
    fn should_type_into_the_prompt_instead_of_moving_when_the_command_line_is_open() {
        let mut page = chrome_page("one\n\ntwo\n");
        paint(&mut page, 500, 300, 1.0);
        press(&mut page, ":");
        type_line(&mut page, "jj");
        assert_eq!(focused(&page), "one");
        press(&mut page, "escape");
        press(&mut page, "j");
        assert_eq!(focused(&page), "two");
        let chord = Chord::parse("j").unwrap();
        assert_eq!(chord.key, "j");
    }
}
