//! The document page: parses Markdown into a [`Doc`], lays it out lazily and
//! paints the visible part. Owns the scroll position and re-lays out on
//! resize and scale changes, keeping the block at the top of the window. Keys
//! go to [`Nav`], which moves the cursor, searches, yanks and runs commands.

use self::load::{Input, Loader, Waker};
use crate::{
    editor::keys::translate_key,
    feedback::{
        Feedback, FeedbackHandle, Focus,
        review::{self, ReviewKey},
    },
    layout::{Layout, LayoutOptions, lay_out_with, rail::RailFollow},
    nav::{
        Bands, CloseKind, Io, Nav, NavOptions, NoActions, PageActions, Request, ZoomStep,
        cursor::{self, Env, StopKind},
        help,
        keymap::{KeyInput, Keymap},
        search, selection_tint, status,
        yank::{Clipboard, NoClipboard, ReplyCommand},
    },
    paint::Canvas,
    text::Fonts,
    theme::Theme,
    window::{Page, TICK_INTERVAL},
};
use limpid_core::{
    doc::{Block, Doc, Mode, parse},
    settings::{RailMode, Settings, ViewTheme},
    transport::{Handoff, Scheme},
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    event::{ElementState, Ime, KeyEvent},
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
    if settings.style.line_height.is_none()
        && view.line_height.is_finite()
        && view.line_height > 0.0
    {
        theme.line_height = view.line_height;
    }
    if settings.style.content_width.is_none() && view.content_width > 0 {
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
        contents_open: false,
    }
}

/// How the page named by `handoff` is parsed. An agent page is the Markdown
/// beside a `<page>.html` and its `<page>.meta.json`: it has no source file of
/// its own, and its raw HTML is the agent's output, shown as text. Anything
/// else is a user's file.
pub fn mode_for(handoff: &Handoff) -> Mode {
    if handoff.source.is_some() {
        return Mode::File;
    }
    let is_markdown = handoff
        .path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    if is_markdown && handoff.path.with_extension("meta.json").is_file() {
        Mode::Agent
    } else {
        Mode::File
    }
}

/// The document's own title, else `fallback`.
fn title_of(doc: &Doc, fallback: &str) -> String {
    doc.title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// How often a page that waits for its parse looks for the result when
/// nothing wakes the event loop.
const LOAD_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    /// Whether this window was asked to close.
    fn closing(&self) -> bool {
        false
    }
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

/// Which kind of input moved last: a key press or the pointer. The rail
/// follows the focused block after a key and the scroll position after the
/// pointer, and the focus look quiets down while the pointer leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    Keyboard,
    Mouse,
}

pub struct DocPage {
    title: String,
    /// The window title when the document has no title of its own.
    fallback_title: String,
    mode: Mode,
    doc: Doc,
    theme: Theme,
    base_font_size: f32,
    zoom: f32,
    options: LayoutOptions,
    input_mode: InputMode,
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
    /// Replies, comments and the editor box, when the page takes feedback.
    feedback: Option<Rc<RefCell<Feedback>>>,
    /// The blocks parsed from the source, before feedback is spliced in.
    base_blocks: Vec<Block>,
    /// Scroll the cursor into view at the next paint.
    reveal_cursor: bool,
    /// Parses Markdown off the UI thread.
    loader: Loader,
    /// The first document has not arrived yet; the page shows its background.
    loading: bool,
    /// Parses made on the calling thread (the loader counts the others).
    sync_parses: usize,
}

/// The outlines of focus: code, diagrams and tables in `edge`, a question
/// in teal, and everything special inside a visual selection in teal.
fn paint_outlines(
    canvas: &mut Canvas,
    layout: &Layout,
    theme: &Theme,
    bands: &Bands,
    visual: bool,
    mouse: bool,
    scroll: f32,
) {
    let teal = theme.teal;
    if visual {
        for (kind, rect) in &bands.outlined {
            cursor::paint_outline(canvas, layout, *kind, *rect, scroll, teal);
        }
        return;
    }
    let Some(focus) = bands.focus else {
        return;
    };
    let edge = if mouse {
        theme.focus_edge_faint
    } else {
        theme.focus_edge
    };
    match bands.focus_kind {
        StopKind::Special => {
            cursor::paint_outline(canvas, layout, bands.focus_kind, focus, scroll, edge)
        }
        StopKind::Question => {
            cursor::paint_outline(canvas, layout, bands.focus_kind, focus, scroll, teal)
        }
        StopKind::Text | StopKind::Heading => {}
    }
}

impl DocPage {
    pub fn open(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
    ) -> Result<Self, String> {
        Self::open_as(path, settings, scheme, fonts, Mode::File)
    }

    /// [`DocPage::open`], parsing in `mode`.
    pub fn open_as(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
        mode: Mode,
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
        Ok(page.with_mode(mode).interactive(Some(path.to_path_buf())))
    }

    /// Open `path` without reading it here: the page starts empty, the file is
    /// read and parsed on a worker thread, and `waker` is called when the
    /// document is ready. What a window uses; [`DocPage::open`] is the
    /// synchronous twin that snapshots and tests use.
    pub fn open_async(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
        mode: Mode,
        waker: Option<Waker>,
    ) -> Result<Self, String> {
        let loader = Loader::new(mode, settings).with_waker(waker);
        Self::open_async_with(path, settings, scheme, fonts, loader)
    }

    /// [`DocPage::open_async`] with a prepared [`Loader`] (its mode is the
    /// page's mode), so tests can slow the parser down.
    pub fn open_async_with(
        path: &Path,
        settings: &Settings,
        scheme: Scheme,
        fonts: SharedFonts,
        mut loader: Loader,
    ) -> Result<Self, String> {
        Keymap::new(&settings.view.keys)?;
        // A missing file is an error now, not a blank window.
        std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "limpid".to_owned());
        let mut page = Self::from_source("", stem, settings, scheme, fonts);
        page.mode = loader.mode();
        page.loading = true;
        loader.start(Input::File(path.to_path_buf()));
        page.loader = loader;
        Ok(page.interactive(Some(path.to_path_buf())))
    }

    /// Parse in `mode` from now on, and parse the current text again if that
    /// changes what it means.
    pub fn with_mode(mut self, mode: Mode) -> Self {
        if mode == self.mode {
            return self;
        }
        self.mode = mode;
        self.loader.set_mode(mode);
        self.sync_parses += 1;
        let source = std::mem::take(&mut self.doc.source);
        self.doc = parse(&source, mode, &self.settings);
        self.title = title_of(&self.doc, &self.fallback_title);
        self.adopt_document();
        self
    }

    /// The mode the page parses in.
    pub fn mode(&self) -> Mode {
        self.mode
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
        let title = title_of(&doc, &fallback_title);
        let (nav, key_error) = match Nav::new(&settings.view.keys, NavOptions::default()) {
            Ok(nav) => (nav, None),
            Err(error) => (
                Nav::with_keymap(Keymap::defaults(), NavOptions::default()),
                Some(error),
            ),
        };
        let mut page = Self {
            title,
            fallback_title,
            mode: Mode::File,
            doc,
            base_font_size: theme.font_size,
            zoom: 1.0,
            theme,
            options,
            input_mode: InputMode::default(),
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
            feedback: None,
            base_blocks: Vec::new(),
            reveal_cursor: false,
            loader: Loader::new(Mode::File, settings),
            loading: false,
            sync_parses: 1,
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

    /// Let the page take feedback: replies to questions and comments on
    /// blocks, written in the embedded editor and saved through `feedback`.
    /// Replies are subjects named after the document, so feedback follows the
    /// title as the heading arrives or changes.
    fn sync_feedback_title(&self) {
        if let Some(feedback) = &self.feedback {
            feedback.borrow_mut().set_title(self.title.clone());
        }
    }

    pub fn with_feedback(mut self, feedback: Feedback) -> Self {
        let shared = Rc::new(RefCell::new(feedback));
        self.actions = Box::new(FeedbackHandle(shared.clone()));
        self.feedback = Some(shared);
        self.sync_feedback_title();
        self.adopt_document();
        self
    }

    pub fn feedback(&self) -> Option<std::cell::Ref<'_, Feedback>> {
        self.feedback.as_ref().map(|feedback| feedback.borrow())
    }

    pub fn with_nav_options(mut self, options: NavOptions) -> Self {
        self.nav.options = options;
        self
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    /// Whether the keyboard or the mouse moved last.
    pub fn input_mode(&self) -> InputMode {
        self.input_mode
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

    pub fn set_input_mode(&mut self, mode: InputMode) {
        self.input_mode = mode;
    }

    /// Open or close the compact contents block shown in narrow windows.
    pub fn toggle_contents(&mut self) {
        self.options.contents_open = !self.options.contents_open;
        self.laid_for = None;
    }

    /// The font size after zoom, CSS px.
    pub fn font_size(&self) -> f32 {
        self.theme.font_size
    }

    /// How many top-level blocks have been laid out so far.
    pub fn laid_out_blocks(&self) -> usize {
        self.layout.as_ref().map_or(0, |layout| layout.blocks.len())
    }

    /// Milliseconds since the page was made, the clock status flashes use.
    pub fn elapsed_ms(&self) -> u64 {
        self.now_ms()
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    pub fn flash(&mut self, text: impl Into<String>) {
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
        let max = self.layout.as_ref().and_then(|layout| layout.max_scroll());
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
        self.sync_parses += 1;
        let doc = parse(source, self.mode, &self.settings);
        self.install_doc(doc);
    }

    /// Show `new_doc` in place of the current document, keeping the viewport
    /// on the same part of the text. The window title follows the document.
    fn install_doc(&mut self, new_doc: Doc) {
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
        self.doc = new_doc;
        self.title = title_of(&self.doc, &self.fallback_title);
        self.sync_feedback_title();
        let retargeted = cursor_block.and_then(|(top, text)| {
            let index = find_block_like(&self.doc, &text, top)?;
            Some(self.doc.blocks.get(index)?.src.clone())
        });
        self.nav.retarget_cursor(retargeted);
        self.adopt_document();
        self.nav.document_changed(&self.doc);
        self.layout = None;
        self.laid_for = None;
    }

    /// The document was just parsed: hand it to the feedback layer and show
    /// it with the pending feedback spliced in.
    fn adopt_document(&mut self) {
        let Some(feedback) = self.feedback.clone() else {
            return;
        };
        self.base_blocks = self.doc.blocks.clone();
        let mut feedback = feedback.borrow_mut();
        feedback.index_document(&self.base_blocks, &self.doc.source);
        self.doc.blocks = feedback.overlay(&self.base_blocks);
        self.layout = None;
        self.laid_for = None;
    }

    /// The feedback changed (a reply was picked, a box opened): show the
    /// blocks again, keeping the viewport and the cursor where they were.
    fn rebuild_overlay(&mut self) {
        let Some(feedback) = self.feedback.clone() else {
            return;
        };
        self.pending_anchor = self.layout.as_ref().and_then(|layout| {
            let index = layout.block_at(self.scroll)?;
            let block = layout.blocks.get(index)?;
            Some(SourceAnchor {
                index,
                text: block_text(&self.doc, index)?.to_owned(),
                offset_css: (self.scroll - block.rect.y) / self.scale.max(f32::EPSILON),
            })
        });
        let mut feedback = feedback.borrow_mut();
        let cursor = self
            .nav
            .cursor()
            .src
            .as_ref()
            .map(|src| feedback.identify(src));
        self.doc.blocks = feedback.overlay(&self.base_blocks);
        if let Some(src) = cursor.and_then(|focus| feedback.resolve_focus(&focus)) {
            self.nav.retarget_cursor(Some(src));
        }
        drop(feedback);
        self.nav.document_changed(&self.doc);
        self.layout = None;
        self.laid_for = None;
    }

    /// Math or Mermaid output for `hashes` reached the cache: draw it from the
    /// first block that shows one. Returns whether the page needs a repaint.
    pub fn refresh_pending(&mut self, hashes: &[String]) -> bool {
        self.layout
            .as_mut()
            .is_some_and(|layout| layout.refresh_pending(hashes))
    }

    /// Take in what the feedback layer did since the last look: messages,
    /// a file to read again, blocks to rebuild, a place for the cursor.
    /// Returns whether anything changed on screen.
    fn sync_feedback(&mut self) -> bool {
        let Some(feedback) = self.feedback.clone() else {
            return false;
        };
        let mut changed = false;
        let flashes = feedback.borrow_mut().take_flashes();
        for message in flashes {
            if !message.is_empty() {
                self.flash(message);
            }
            changed = true;
        }
        let reload = feedback.borrow_mut().take_reload();
        if reload {
            self.reload_quietly();
            changed = true;
        }
        let stale = feedback.borrow_mut().take_stale();
        if stale {
            self.rebuild_overlay();
            changed = true;
        }
        let focus = feedback.borrow_mut().take_focus();
        if let Some(focus) = focus {
            changed |= self.focus_on(&feedback, &focus);
        }
        changed
    }

    fn focus_on(&mut self, feedback: &Rc<RefCell<Feedback>>, focus: &Focus) -> bool {
        let Some(src) = feedback.borrow().resolve_focus(focus) else {
            return false;
        };
        self.nav.retarget_cursor(Some(src));
        self.reveal_cursor = true;
        true
    }

    /// Show `bytes`, just read from the file, in place of the document. The
    /// text is parsed on the worker thread; the old document stays on screen
    /// until the new one arrives. The caller read the file, so nothing here
    /// reads it a second time.
    pub fn reload_from_bytes(&mut self, bytes: &[u8]) {
        if let Some(feedback) = &self.feedback {
            feedback.borrow_mut().source_reloaded(bytes);
        }
        self.loader
            .start(Input::Source(String::from_utf8_lossy(bytes).into_owned()));
    }

    /// Whether a parse on the worker thread is still running.
    pub fn parsing(&self) -> bool {
        self.loader.busy()
    }

    /// Whether the first document has not arrived yet.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// How many times this page has parsed Markdown, on any thread.
    pub fn parse_count(&self) -> usize {
        self.sync_parses + self.loader.started()
    }

    /// Take in a document the worker finished. Returns whether the page
    /// changed.
    fn poll_loaded(&mut self) -> bool {
        let Some(result) = self.loader.poll() else {
            return false;
        };
        self.loading = false;
        match result {
            Ok(doc) => self.install_doc(doc),
            Err(error) => {
                eprintln!("limpid-view: {error}");
                self.flash(format!("could not load: {error}"));
            }
        }
        true
    }

    /// Read the file again after a save, without announcing it.
    pub fn reload_quietly(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        if let Some(feedback) = &self.feedback {
            feedback.borrow_mut().source_reloaded(&bytes);
        }
        self.replace_source(&String::from_utf8_lossy(&bytes));
    }

    fn reload(&mut self) {
        let Some(path) = self.path.clone() else {
            self.flash("nothing to reload");
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Some(feedback) = &self.feedback {
                    feedback.borrow_mut().source_reloaded(&bytes);
                }
                self.replace_source(&String::from_utf8_lossy(&bytes));
                self.flash("Reloaded");
            }
            Err(error) => self.flash(format!("reload failed: {error}")),
        }
    }

    fn zoom_by(&mut self, step: ZoomStep) {
        let options = self.nav.options;
        let (low, high) = options.zoom_range;
        let step_size = if options.zoom_step.is_finite() && options.zoom_step > 0.0 {
            options.zoom_step
        } else {
            0.1
        };
        self.zoom = match step {
            ZoomStep::In => self.zoom + step_size,
            ZoomStep::Out => self.zoom - step_size,
            ZoomStep::Reset => 1.0,
        }
        .clamp(low.min(high), high.max(low));
        self.zoom = (self.zoom * 100.0).round() / 100.0;
        self.theme.font_size = self.base_font_size * self.zoom;
        self.laid_for = None;
        self.flash(format!("Zoom {}%", (self.zoom * 100.0).round()));
    }

    fn toggle_rail(&mut self) {
        let compact = self
            .layout
            .as_ref()
            .is_some_and(|layout| layout.compact.is_some());
        if compact {
            self.toggle_contents();
            return;
        }
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
                    self.flash("this page cannot close itself");
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
        self.input_mode = InputMode::Keyboard;
        if self.box_open() {
            // Every key belongs to the editor; they arrive through
            // [`DocPage::editor_key`].
            return false;
        }
        if self.review_open() {
            return self.review_input(input);
        }
        let now = self.now_ms();
        let Some(outcome) = self.with_nav(|nav, env, io| nav.handle(input, now, env, io)) else {
            return false;
        };
        let mut changed = outcome.redraw;
        for request in outcome.requests {
            self.perform(request);
            changed = true;
        }
        changed |= self.sync_feedback();
        changed
    }

    /// Whether a reply or comment box is open.
    pub fn box_open(&self) -> bool {
        self.feedback
            .as_ref()
            .is_some_and(|feedback| feedback.borrow().box_open())
    }

    /// Whether the review overlay is showing.
    pub fn review_open(&self) -> bool {
        self.feedback
            .as_ref()
            .is_some_and(|feedback| feedback.borrow().review().is_some())
    }

    fn review_input(&mut self, input: &KeyInput) -> bool {
        let Some(feedback) = self.feedback.clone() else {
            return false;
        };
        let outcome = feedback
            .borrow_mut()
            .review_mut()
            .map_or(ReviewKey::Ignored, |review| review.handle(input));
        match outcome {
            ReviewKey::Ignored => false,
            ReviewKey::Moved => true,
            ReviewKey::Dismiss => {
                feedback.borrow_mut().close_review();
                true
            }
            ReviewKey::Send => {
                feedback.borrow_mut().close_review();
                let result = feedback.borrow_mut().write_and_send();
                let sent = matches!(
                    result,
                    crate::nav::ActionResult::Done(_) | crate::nav::ActionResult::Nothing(_)
                );
                let message = match result {
                    crate::nav::ActionResult::Done(message)
                    | crate::nav::ActionResult::Nothing(message)
                    | crate::nav::ActionResult::Failed(message) => message,
                    crate::nav::ActionResult::Unsupported => "cannot send".to_owned(),
                };
                self.flash(message);
                if sent {
                    self.perform(Request::Close(CloseKind::Window));
                }
                self.sync_feedback();
                true
            }
        }
    }

    /// One key for the open editor box, in winit terms. `Ctrl-Shift-q`
    /// closes the box and throws its buffer away; anything else goes to nvim.
    /// Returns whether the page needs a repaint.
    pub fn editor_key(&mut self, key: &Key, text: Option<&str>, modifiers: ModifiersState) -> bool {
        let Some(feedback) = self.feedback.clone() else {
            return false;
        };
        if !feedback.borrow().box_open() {
            return false;
        }
        let force_close = modifiers.control_key()
            && modifiers.shift_key()
            && matches!(key, Key::Character(typed) if typed.eq_ignore_ascii_case("q"));
        if force_close {
            feedback.borrow_mut().force_close();
        } else if let Some(keys) = translate_key(key, text, modifiers) {
            feedback.borrow_mut().input(&keys);
        }
        self.sync_feedback();
        true
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
        if std::mem::take(&mut self.reveal_cursor) {
            self.with_nav(|nav, env, _| nav.reveal_cursor(env));
            self.ensure_to(self.scroll + self.viewport_height * 2.0);
        }

        canvas.clear(self.theme.bg);
        let scroll = self.scroll;
        let now = self.now_ms();
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        let focused = match self.input_mode {
            InputMode::Keyboard => self
                .nav
                .cursor()
                .src
                .as_ref()
                .and_then(|src| layout.block_for_src(src.start)),
            InputMode::Mouse => None,
        };
        layout.rail_follow = match focused {
            Some(block) => RailFollow::Block(block),
            None => RailFollow::Scroll {
                viewport: self.viewport_height,
            },
        };
        let bands = if self.chrome {
            self.nav.bands(layout)
        } else {
            Bands::default()
        };
        let mouse = self.input_mode == InputMode::Mouse;
        let visual = bands.selection.is_some() || !bands.outlined.is_empty();
        if let Some(selection) = bands.selection {
            let tint = selection_tint(&self.theme);
            cursor::paint_band(canvas, layout, selection, scroll, tint);
            for (_, rect) in bands
                .outlined
                .iter()
                .filter(|(kind, _)| *kind == StopKind::Special)
            {
                let block = crate::paint::Rect::new(
                    layout.column_x + rect.x,
                    rect.y - scroll,
                    rect.w,
                    rect.h,
                );
                canvas.fill_rect(block, self.theme.bg);
            }
        } else if let (Some(focus), false, StopKind::Text) = (bands.focus, visual, bands.focus_kind)
        {
            let tint = if mouse {
                self.theme.focus_faint
            } else {
                self.theme.focus_soft
            };
            cursor::paint_band(canvas, layout, focus, scroll, tint);
        }
        layout.heading_tint = bands.heading.clone();
        layout.paint(canvas, &mut self.fonts.borrow_mut(), scroll);
        if self.chrome {
            paint_outlines(canvas, layout, &self.theme, &bands, visual, mouse, scroll);
        }
        if let Some(feedback) = &self.feedback {
            feedback.borrow_mut().paint_box(
                canvas,
                layout,
                scroll,
                &self.theme,
                &mut self.fonts.borrow_mut(),
            );
        }
        if !self.chrome {
            return;
        }
        if let Some((query, current)) = self.nav.highlight() {
            search::paint_highlights(canvas, layout, &query, current, scroll);
        }
        let mut view = self.nav.status_view(now);
        if let Some(feedback) = &self.feedback {
            let feedback = feedback.borrow();
            if let Some(open_review) = feedback.review() {
                view.state = status::StateName::Review;
                review::paint(
                    canvas,
                    &mut self.fonts.borrow_mut(),
                    &self.theme,
                    scale,
                    bar,
                    open_review,
                );
            } else if let Some(open) = feedback.open_box() {
                view.state = status::StateName::Editor;
                view.right = open.mode_label().to_owned();
                if let Some(line) = &open.cmdline {
                    let mut characters = line.chars();
                    view.prompt = characters.next().map(|sigil| status::Prompt {
                        sigil,
                        text: characters.as_str().to_owned(),
                        placeholder: "",
                    });
                }
                if view.message.is_empty()
                    && let Some(message) = &open.message
                {
                    view.message = message.clone();
                }
            }
        }
        if let Some(text) = self.nav.search_box() {
            let mut fonts = self.fonts.borrow_mut();
            search::paint_box(canvas, &mut fonts, &self.theme, scale, bar, text);
        }
        if let Some((rows, scroll)) = self.nav.help_view() {
            let mut fonts = self.fonts.borrow_mut();
            let hint = self.nav.help_hint();
            let view = help::View {
                rows: &rows,
                scroll,
                hint: &hint,
            };
            help::paint(canvas, &mut fonts, &self.theme, scale, bar, &view);
        }
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
        let left_mouse_mode = self.input_mode == InputMode::Mouse;
        self.input_mode = InputMode::Keyboard;
        if self.box_open() {
            return self.editor_key(&event.logical_key, event.text.as_deref(), self.modifiers)
                || left_mouse_mode;
        }
        KeyInput::from_winit(&event.logical_key, self.modifiers)
            .is_some_and(|input| self.handle_input(&input))
            || left_mouse_mode
    }

    fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    fn pointer(&mut self) -> bool {
        let changed = self.input_mode != InputMode::Mouse;
        self.input_mode = InputMode::Mouse;
        changed
    }

    fn ime(&mut self, event: &Ime) -> bool {
        let Some(feedback) = self.feedback.clone() else {
            return false;
        };
        let mut feedback = feedback.borrow_mut();
        match event {
            Ime::Commit(text) => {
                feedback.ime_commit(text);
                feedback.ime_preedit(None);
                true
            }
            Ime::Preedit(text, _) => feedback.ime_preedit(Some(text)),
            Ime::Enabled => false,
            Ime::Disabled => feedback.ime_preedit(None),
        }
    }

    fn closing(&self) -> bool {
        self.closer.as_ref().is_some_and(|closer| closer.closing())
    }

    fn wants_ticks(&self) -> bool {
        self.loader.busy()
            || self
                .feedback
                .as_ref()
                .is_some_and(|feedback| feedback.borrow().wants_ticks())
    }

    fn tick_period(&self) -> Duration {
        let feedback_period = match self.feedback.as_ref() {
            Some(feedback) if feedback.borrow().wants_ticks() => {
                feedback.borrow().tick_period(Instant::now())
            }
            _ => TICK_INTERVAL,
        };
        if self.loader.busy() {
            // The worker wakes the loop when it finishes; this only backs
            // that up, and serves where nobody wakes the loop.
            return feedback_period.min(LOAD_POLL_INTERVAL);
        }
        feedback_period
    }

    fn tick(&mut self) -> bool {
        let mut changed = self.poll_loaded();
        let Some(feedback) = self.feedback.clone() else {
            return changed;
        };
        changed |= feedback.borrow_mut().tick(Instant::now());
        changed |= self.sync_feedback();
        changed
    }
}

pub mod load {
    //! Parsing on a worker thread, so a window can open before its text is read.
    //!
    //! [`Loader::start`] returns at once; the parsed [`Doc`] comes back through a
    //! channel that the page polls from its tick. Each request carries a
    //! generation: when a newer request exists, an older result is dropped, so a
    //! slow parse of stale text never replaces a fresher document.

    use limpid_core::{
        doc::{Doc, Mode, parse},
        settings::Settings,
    };
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
        sync::{
            Arc,
            mpsc::{Receiver, Sender, channel},
        },
        thread,
    };

    /// Called from the worker when a result is ready, to wake the event loop.
    pub type Waker = Arc<dyn Fn() + Send + Sync>;

    /// How text becomes a document; tests slow it down.
    pub type Parser = Arc<dyn Fn(&str, Mode, &Settings) -> Doc + Send + Sync>;

    /// What to parse.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Input {
        /// Read the file on the worker, then parse it.
        File(PathBuf),
        /// Text the caller already has.
        Source(String),
    }

    struct Parsed {
        generation: u64,
        result: Result<Doc, String>,
    }

    pub struct Loader {
        mode: Mode,
        settings: Settings,
        waker: Option<Waker>,
        parser: Parser,
        sender: Sender<Parsed>,
        receiver: Receiver<Parsed>,
        generation: u64,
        in_flight: usize,
        started: usize,
    }

    impl Loader {
        pub fn new(mode: Mode, settings: &Settings) -> Self {
            let (sender, receiver) = channel();
            Self {
                mode,
                settings: settings.clone(),
                waker: None,
                parser: Arc::new(parse),
                sender,
                receiver,
                generation: 0,
                in_flight: 0,
                started: 0,
            }
        }

        pub fn with_waker(mut self, waker: Option<Waker>) -> Self {
            self.waker = waker;
            self
        }

        pub fn with_parser(mut self, parser: Parser) -> Self {
            self.parser = parser;
            self
        }

        pub fn mode(&self) -> Mode {
            self.mode
        }

        /// Parse later requests in `mode`. Requests already running keep theirs.
        pub fn set_mode(&mut self, mode: Mode) {
            self.mode = mode;
        }

        /// Whether a request is still running (including one whose result will be
        /// dropped as stale).
        pub fn busy(&self) -> bool {
            self.in_flight > 0
        }

        /// How many requests this loader has started.
        pub fn started(&self) -> usize {
            self.started
        }

        /// Begin parsing `input` on a new thread and return immediately.
        pub fn start(&mut self, input: Input) {
            self.generation += 1;
            self.in_flight += 1;
            self.started += 1;
            let generation = self.generation;
            let sender = self.sender.clone();
            let waker = self.waker.clone();
            let parser = self.parser.clone();
            let settings = self.settings.clone();
            let mode = self.mode;
            let spawned = thread::Builder::new()
                .name("limpid-parse".into())
                .spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let source = match input {
                            Input::Source(source) => source,
                            Input::File(path) => {
                                let bytes = std::fs::read(&path)
                                    .map_err(|error| format!("{}: {error}", path.display()))?;
                                String::from_utf8_lossy(&bytes).into_owned()
                            }
                        };
                        Ok(parser(&source, mode, &settings))
                    }))
                    .unwrap_or_else(|_| Err("the parser stopped unexpectedly".to_owned()));
                    // The page may be gone by now; nobody is left to tell.
                    let _ = sender.send(Parsed { generation, result });
                    if let Some(waker) = waker {
                        waker();
                    }
                });
            if let Err(error) = spawned {
                // No thread: report the failure through the same channel.
                let _ = self.sender.send(Parsed {
                    generation,
                    result: Err(format!("could not start the parser: {error}")),
                });
            }
        }

        /// The newest finished request, if its result has arrived. Results of
        /// requests that a later one replaced are discarded.
        pub fn poll(&mut self) -> Option<Result<Doc, String>> {
            let mut latest = None;
            while let Ok(parsed) = self.receiver.try_recv() {
                self.in_flight = self.in_flight.saturating_sub(1);
                if parsed.generation == self.generation {
                    latest = Some(parsed.result);
                }
            }
            latest
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::{Duration, Instant};

        fn wait(loader: &mut Loader) -> Result<Doc, String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(result) = loader.poll() {
                    return result;
                }
                assert!(Instant::now() < deadline, "no result");
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn slow(delay: Duration) -> Parser {
            Arc::new(move |source, mode, settings| {
                thread::sleep(delay);
                parse(source, mode, settings)
            })
        }

        #[test]
        fn should_return_at_once_when_the_parse_is_slow() {
            let mut loader = Loader::new(Mode::File, &Settings::default())
                .with_parser(slow(Duration::from_millis(400)));
            let begun = Instant::now();
            loader.start(Input::Source("# Slow\n".into()));
            assert!(begun.elapsed() < Duration::from_millis(200));
            assert!(loader.busy());
            assert!(loader.poll().is_none(), "the parse is still running");
            let doc = wait(&mut loader).unwrap();
            assert_eq!(doc.title.as_deref(), Some("Slow"));
            assert!(!loader.busy());
        }

        #[test]
        fn should_read_the_file_on_the_worker_when_given_a_path() {
            let path = std::env::temp_dir().join(format!("limpid-load-{}.md", std::process::id()));
            std::fs::write(&path, "# From disk\n").unwrap();
            let mut loader = Loader::new(Mode::File, &Settings::default());
            loader.start(Input::File(path.clone()));
            assert_eq!(
                wait(&mut loader).unwrap().title.as_deref(),
                Some("From disk")
            );
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn should_report_the_path_when_the_file_cannot_be_read() {
            let mut loader = Loader::new(Mode::File, &Settings::default());
            loader.start(Input::File("/nonexistent-dir/none.md".into()));
            let error = wait(&mut loader).unwrap_err();
            assert!(error.contains("/nonexistent-dir/none.md"), "{error}");
        }

        #[test]
        fn should_drop_the_older_result_when_a_newer_request_replaced_it() {
            let mut loader = Loader::new(Mode::File, &Settings::default()).with_parser(Arc::new(
                |source, mode, settings| {
                    if source.contains("old") {
                        thread::sleep(Duration::from_millis(150));
                    }
                    parse(source, mode, settings)
                },
            ));
            loader.start(Input::Source("# old\n".into()));
            loader.start(Input::Source("# new\n".into()));
            let doc = wait(&mut loader).unwrap();
            assert_eq!(doc.title.as_deref(), Some("new"));
            // Let the stale one land; it must neither surface nor leave the loader busy.
            let deadline = Instant::now() + Duration::from_secs(5);
            while loader.busy() {
                assert!(Instant::now() < deadline);
                assert!(loader.poll().is_none(), "a stale result surfaced");
                thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn should_report_an_error_when_the_parser_panics() {
            let mut loader = Loader::new(Mode::File, &Settings::default())
                .with_parser(Arc::new(|_, _, _| panic!("boom")));
            loader.start(Input::Source("x".into()));
            let error = wait(&mut loader).unwrap_err();
            assert!(error.contains("stopped"), "{error}");
            assert!(!loader.busy());
        }

        #[test]
        fn should_call_the_waker_when_a_result_is_ready() {
            let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = woken.clone();
            let mut loader = Loader::new(Mode::File, &Settings::default()).with_waker(Some(
                Arc::new(move || {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            ));
            loader.start(Input::Source("text".into()));
            wait(&mut loader).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while woken.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
        }

        #[test]
        fn should_parse_in_agent_mode_when_the_loader_says_so() {
            let mut loader = Loader::new(Mode::Agent, &Settings::default());
            loader.start(Input::Source("<div>raw</div>\n\ntext\n".into()));
            let doc = wait(&mut loader).unwrap();
            assert!(matches!(
                doc.blocks[0].kind,
                limpid_core::doc::BlockKind::Html(ref text) if text.contains("<div>raw</div>")
            ));
        }

        #[test]
        fn should_count_every_request_when_starting_several() {
            let mut loader = Loader::new(Mode::File, &Settings::default());
            loader.start(Input::Source("a".into()));
            loader.start(Input::Source("b".into()));
            assert_eq!(loader.started(), 2);
            wait(&mut loader).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::load::Loader;
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
        assert!((page.scroll() - 150.0).abs() < 2.0, "{}", page.scroll());
        assert!(page.handle_key(&Key::Character("u".into()), true));
        assert!(page.scroll() < 30.0, "{}", page.scroll());
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
        assert!((page.scroll() - 100.0).abs() < 2.0, "{}", page.scroll());
    }

    #[test]
    fn should_ignore_keys_when_they_are_not_bound() {
        let mut page = page("text");
        paint(&mut page, 200, 100, 1.0);
        assert!(!page.handle_key(&Key::Character("w".into()), false));
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
    fn should_not_scroll_past_the_scroll_space_when_the_document_is_short() {
        let mut page = page("short\n");
        paint(&mut page, 400, 300, 1.0);
        page.handle_key(&Key::Character("d".into()), true);
        page.handle_key(&Key::Named(NamedKey::PageDown), false);
        assert!(page.scroll() <= page.layout().unwrap().height);
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
        let max = layout.height;
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
        // `Settings::parse` rejects these; the theme must still cope with a
        // hand-built `Settings`.
        let mut settings = Settings::default();
        settings.view.line_height = -1.0;
        settings.view.scroll_fraction = 0.0;
        settings.view.font = "  ".into();
        settings.view.content_width = 0;
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
        let gutter = (layout.column_x + layout.column_width - 14.0) as usize;
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
        let gutter = (layout.column_x + layout.column_width - 14.0) as usize;
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
        let last = layout.blocks.last().unwrap().rect.bottom();
        assert!(page.scroll() + (400.0 - bar) >= last);
        assert!(page.scroll() < layout.height);
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

    // Focus look.

    fn blend(base: crate::theme::Color, over: crate::theme::Color, alpha: u32) -> u32 {
        let mix = |b: u8, o: u8| (u32::from(o) * alpha + u32::from(b) * (255 - alpha) + 127) / 255;
        (mix(base.r, over.r) << 16) | (mix(base.g, over.g) << 8) | mix(base.b, over.b)
    }

    #[test]
    fn should_paint_a_soft_rounded_band_without_a_bar_when_a_text_block_is_focused() {
        let mut page = chrome_page("one\n\ntwo\n");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let y = (rect.y + rect.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        let soft = blend(theme.bg, theme.teal, 15);
        assert_eq!(pixels[y * 800 + left], soft, "no bar on the left edge");
        let corner = (rect.y as usize) * 800 + (layout.column_x + 10.0) as usize;
        assert_eq!(pixels[corner], theme.bg.to_pixel(), "corner is rounded");
    }

    #[test]
    fn should_drop_the_band_to_faint_when_the_mouse_moves() {
        let mut page = chrome_page("one\n\ntwo\n");
        assert!(page.pointer());
        assert!(!page.pointer(), "already in mouse mode");
        assert_eq!(page.input_mode(), InputMode::Mouse);
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let y = (rect.y + rect.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        assert_eq!(pixels[y * 800 + left], blend(theme.bg, theme.teal, 5));
    }

    #[test]
    fn should_return_to_keyboard_mode_when_a_key_is_pressed() {
        let mut page = chrome_page("one\n\ntwo\n");
        page.pointer();
        press(&mut page, "j");
        assert_eq!(page.input_mode(), InputMode::Keyboard);
    }

    #[test]
    fn should_outline_a_code_block_and_leave_the_gutter_bare_when_it_is_focused() {
        let mut page = chrome_page("```\ncode\n```\n\ntext\n");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let mid = (rect.y + rect.h / 2.0) as usize;
        let gutter = (layout.column_x - 5.0) as usize;
        assert_eq!(pixels[mid * 800 + gutter], theme.bg.to_pixel(), "no band");
        let edge = blend(theme.bg, theme.teal, 128);
        let x = (layout.column_x + rect.x + rect.w / 2.0) as usize;
        let above = (rect.y - 3.0) as usize;
        assert_eq!(pixels[above * 800 + x], edge, "outline above the block");
    }

    #[test]
    fn should_paint_the_outline_fainter_when_the_mouse_leads() {
        let mut page = chrome_page("```\ncode\n```\n\ntext\n");
        page.pointer();
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let x = (layout.column_x + rect.x + rect.w / 2.0) as usize;
        let above = (rect.y - 3.0) as usize;
        assert_eq!(pixels[above * 800 + x], blend(theme.bg, theme.teal, 38));
    }

    #[test]
    fn should_paint_a_heading_teal_when_it_is_focused() {
        let mut page = chrome_page("## Title\n\ntext\n");
        let teal = Theme::dark().teal.to_pixel();
        let focused = paint(&mut page, 800, 600, 1.0);
        assert!(focused.contains(&teal), "heading text is teal");
        press(&mut page, "j");
        let moved = paint(&mut page, 800, 600, 1.0);
        assert!(!moved.contains(&teal), "heading is ink once focus leaves");
    }

    #[test]
    fn should_paint_no_band_behind_a_focused_heading() {
        let mut page = chrome_page("## Title\n\ntext\n");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let y = (rect.y + rect.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        assert_eq!(pixels[y * 800 + left], Theme::dark().bg.to_pixel());
    }

    #[test]
    fn should_keep_inline_code_blue_when_the_heading_is_focused() {
        let mut page = chrome_page("## Title `code`\n\ntext\n");
        let blue = Theme::dark().blue.to_pixel();
        let plain = {
            let mut other = chrome_page("## Title `code`\n\ntext\n");
            press(&mut other, "j");
            paint(&mut other, 800, 600, 1.0)
        };
        assert!(
            plain.contains(&blue),
            "code is blue in an unfocused heading"
        );
        let focused = paint(&mut page, 800, 600, 1.0);
        assert!(
            focused.contains(&blue),
            "code stays blue under the teal tint"
        );
    }

    #[test]
    fn should_paint_a_lone_visual_heading_as_text_colour_only() {
        let mut page = chrome_page("## Title\n\ntext\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "v");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        assert!(pixels.contains(&theme.teal.to_pixel()));
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let y = (rect.y + rect.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        assert_eq!(pixels[y * 800 + left], theme.bg.to_pixel(), "no band");
    }

    #[test]
    fn should_band_the_heading_when_visual_reaches_the_next_block() {
        let mut page = chrome_page("## Title\n\ntext\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "v");
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let layout = page.layout().unwrap();
        let rect = layout.blocks[0].rect;
        let y = (rect.y + rect.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        assert_ne!(pixels[y * 800 + left], Theme::dark().bg.to_pixel());
    }

    #[test]
    fn should_keep_the_question_ring_teal_when_in_visual_mode() {
        let mut page = chrome_page("## Questions\n\n1. Which?\n   - A\n   - B\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "j");
        press(&mut page, "v");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let layout = page.layout().unwrap();
        let field = layout
            .blocks
            .iter()
            .find(|block| matches!(block.content, crate::layout::Content::Pending(ref p) if p.kind == crate::layout::PendingKind::Field))
            .unwrap()
            .rect;
        let x = (layout.column_x + field.x + field.w / 2.0) as usize;
        let above = (field.y - 1.0) as usize;
        assert_eq!(pixels[above * 800 + x], Theme::dark().teal.to_pixel());
    }

    #[test]
    fn should_outline_a_question_in_solid_teal_when_it_is_focused() {
        let mut page = chrome_page("## Questions\n\n1. Which?\n   - A\n   - B\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let field = layout
            .blocks
            .iter()
            .find(|block| matches!(block.content, crate::layout::Content::Pending(ref p) if p.kind == crate::layout::PendingKind::Field))
            .unwrap()
            .rect;
        let x = (layout.column_x + field.x + field.w / 2.0) as usize;
        let above = (field.y - 1.0) as usize;
        assert_eq!(pixels[above * 800 + x], theme.teal.to_pixel());
        let mid = ((field.y + field.h / 2.0) as usize) * 800;
        assert_eq!(
            pixels[mid + (layout.column_x - 5.0) as usize],
            theme.bg.to_pixel(),
            "no band"
        );
    }

    #[test]
    fn should_paint_the_selection_at_the_plain_tint_when_in_visual_mode() {
        let mut page = chrome_page("one\n\ntwo\n\nthree\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "v");
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let top = layout.blocks[0].rect;
        let y = (top.y + top.h / 2.0) as usize;
        let left = (layout.column_x + layout.column_width - 14.0) as usize;
        assert_eq!(pixels[y * 800 + left], blend(theme.bg, theme.teal, 31));
        let gap = ((top.bottom() + layout.blocks[1].rect.y) / 2.0) as usize;
        assert_eq!(
            pixels[gap * 800 + left],
            blend(theme.bg, theme.teal, 31),
            "continuous"
        );
    }

    #[test]
    fn should_outline_a_code_block_in_teal_above_the_selection_when_in_visual_mode() {
        let mut page = chrome_page("text\n\n```\ncode\n```\n\nafter\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "v");
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[1].rect;
        let x = (layout.column_x + rect.x + rect.w / 2.0) as usize;
        let above = (rect.y - 3.0) as usize;
        assert_eq!(pixels[above * 800 + x], theme.teal.to_pixel());
    }

    #[test]
    fn should_keep_a_table_on_the_page_background_when_inside_a_visual_selection() {
        let mut page =
            chrome_page("text\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n\nafter\n");
        paint(&mut page, 800, 600, 1.0);
        press(&mut page, "v");
        press(&mut page, "j");
        let pixels = paint(&mut page, 800, 600, 1.0);
        let theme = Theme::dark();
        let layout = page.layout().unwrap();
        let rect = layout.blocks[1].rect;
        let tinted = blend(theme.bg, theme.teal, 31).to_owned();
        let (left, right) = (
            (layout.column_x + rect.x) as usize,
            (layout.column_x + rect.right()) as usize,
        );
        let leaked = (rect.y as usize..rect.bottom() as usize)
            .flat_map(|y| (left..right).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[y * 800 + x] == tinted)
            .count();
        assert_eq!(leaked, 0, "selection must not show through the table");
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
        assert!((page.font_size() / base - 2.0).abs() < 0.01);
        for _ in 0..400 {
            press(&mut page, "-");
        }
        assert!((page.font_size() / base - 0.5).abs() < 0.01);
    }

    #[test]
    fn should_zoom_in_steps_of_a_tenth_when_pressing_plus_and_minus() {
        let mut page = chrome_page("text\n");
        paint(&mut page, 600, 400, 1.0);
        let base = page.font_size();
        for _ in 0..3 {
            press(&mut page, "+");
        }
        assert!((page.font_size() / base - 1.3).abs() < 1e-4);
        press(&mut page, "-");
        assert!((page.font_size() / base - 1.2).abs() < 1e-4);
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
    fn should_toggle_the_compact_contents_when_pressing_t_in_a_narrow_window() {
        let mut page = chrome_page("# One\n\ntext\n\n## Two\n\ntext\n");
        let open = |page: &DocPage| page.layout().unwrap().compact.as_ref().map(|c| c.open);
        paint(&mut page, 700, 600, 1.0);
        assert_eq!(open(&page), Some(false), "closed by default");
        press(&mut page, "t");
        paint(&mut page, 700, 600, 1.0);
        assert_eq!(open(&page), Some(true));
        assert!(page.layout().unwrap().rail.is_none());
        press(&mut page, "f1");
        paint(&mut page, 700, 600, 1.0);
        assert_eq!(open(&page), Some(false));
    }

    #[test]
    fn should_toggle_the_rail_and_not_the_contents_when_pressing_t_in_a_wide_window() {
        let mut page = chrome_page("# One\n\ntext\n\n## Two\n\ntext\n");
        paint(&mut page, 1400, 600, 1.0);
        press(&mut page, "t");
        paint(&mut page, 1400, 600, 1.0);
        assert!(page.layout().unwrap().compact.is_none());
    }

    #[test]
    fn should_follow_the_focused_block_when_the_last_input_was_a_key() {
        let source: String = (0..6).map(|n| format!("## H{n}\n\ntext\n\n")).collect();
        let mut page = chrome_page(&source);
        paint(&mut page, 1400, 600, 1.0);
        assert_eq!(page.input_mode(), InputMode::Keyboard);
        press(&mut page, "G");
        paint(&mut page, 1400, 600, 1.0);
        let layout = page.layout().unwrap();
        assert_eq!(layout.rail_follow, RailFollow::Block(11));
    }

    #[test]
    fn should_follow_the_scroll_position_when_the_last_input_was_the_mouse() {
        let mut page = chrome_page("## One\n\ntext\n\n## Two\n\ntext\n");
        press(&mut page, "G");
        page.set_input_mode(InputMode::Mouse);
        paint(&mut page, 1400, 600, 1.0);
        let follow = page.layout().unwrap().rail_follow;
        assert!(matches!(follow, RailFollow::Scroll { .. }), "{follow:?}");
        press(&mut page, "k");
        assert_eq!(page.input_mode(), InputMode::Keyboard);
    }

    #[test]
    fn should_show_the_contents_block_open_when_toggled_in_a_narrow_window() {
        let mut page = chrome_page("## One\n\ntext\n\n## Two\n\ntext\n");
        paint(&mut page, 700, 600, 1.0);
        assert!(
            page.layout()
                .unwrap()
                .compact
                .as_ref()
                .is_some_and(|c| !c.open)
        );
        page.toggle_contents();
        paint(&mut page, 700, 600, 1.0);
        assert!(
            page.layout()
                .unwrap()
                .compact
                .as_ref()
                .is_some_and(|c| c.open)
        );
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
    fn should_report_no_change_when_refreshing_hashes_no_block_shows() {
        let mut page = chrome_page("plain text\n");
        assert!(
            !page.refresh_pending(&["abcd".to_owned()]),
            "nothing laid out yet"
        );
        paint(&mut page, 500, 300, 1.0);
        assert!(!page.refresh_pending(&["abcd".to_owned()]));
        assert!(!page.refresh_pending(&[]));
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
        assert_eq!(message, "this page cannot close itself");
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

    // Opening without waiting for the parse.

    fn scratch_file(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limpid-page-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        std::fs::write(&path, text).unwrap();
        path
    }

    fn slow_loader(mode: Mode, delay: Duration) -> Loader {
        Loader::new(mode, &Settings::default()).with_parser(std::sync::Arc::new(
            move |source, mode, settings| {
                std::thread::sleep(delay);
                parse(source, mode, settings)
            },
        ))
    }

    fn fonts() -> SharedFonts {
        Rc::new(RefCell::new(Fonts::new()))
    }

    fn tick_until_loaded(page: &mut DocPage) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.parsing() {
            assert!(Instant::now() < deadline, "the parse never finished");
            page.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        page.tick();
    }

    #[test]
    fn should_open_and_paint_the_background_when_the_parse_is_slow() {
        let path = scratch_file("slow", "# Heading\n\nbody\n");
        let begun = Instant::now();
        let mut page = DocPage::open_async_with(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            slow_loader(Mode::File, Duration::from_millis(500)),
        )
        .unwrap();
        let pixels = paint(&mut page, 400, 300, 1.0);
        assert!(
            begun.elapsed() < Duration::from_millis(400),
            "opening and painting must not wait for the parse"
        );
        assert!(page.loading() && page.parsing());
        assert!(
            pixels[..400 * 240]
                .iter()
                .all(|&pixel| pixel == Theme::dark().bg.to_pixel()),
            "only the background above the status bar until the text arrives"
        );
        assert!(page.wants_ticks(), "the window must keep asking for ticks");
        assert!(page.tick_period() <= LOAD_POLL_INTERVAL);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_title_the_window_with_the_file_stem_until_the_heading_arrives() {
        let path = scratch_file("stem-title", "# Real title\n\nbody\n");
        let mut page = DocPage::open_async_with(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            slow_loader(Mode::File, Duration::from_millis(100)),
        )
        .unwrap();
        assert_eq!(page.title(), "stem-title");
        tick_until_loaded(&mut page);
        assert_eq!(page.title(), "Real title");
        assert!(!page.loading());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_give_feedback_the_heading_title_when_the_document_arrives_after_it_was_attached() {
        let path = scratch_file("feedback-title", "# Fix the parser\n\nbody\n");
        let page = DocPage::open_async_with(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            slow_loader(Mode::File, Duration::from_millis(100)),
        )
        .unwrap();
        let handoff = Handoff {
            path: path.clone(),
            theme: Scheme::Dark,
            pane: None,
            source: None,
            source_hash: None,
            source_backed: false,
        };
        let feedback = Feedback::new(handoff, page.title(), &Settings::default());
        assert_eq!(feedback.title(), "feedback-title");
        let mut page = page.with_feedback(feedback);
        tick_until_loaded(&mut page);
        assert_eq!(page.feedback().unwrap().title(), "Fix the parser");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_report_a_repaint_when_the_document_arrives_and_show_its_text() {
        let path = scratch_file("arrives", "# Hi\n\nsome body text\n");
        let mut page = DocPage::open_async(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            Mode::File,
            None,
        )
        .unwrap();
        paint(&mut page, 400, 300, 1.0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !page.tick() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let pixels = paint(&mut page, 400, 300, 1.0);
        assert!(pixels.iter().any(|&p| p != Theme::dark().bg.to_pixel()));
        assert!(!page.wants_ticks(), "nothing left to wait for");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_refuse_to_open_when_the_file_does_not_exist() {
        let error = DocPage::open_async(
            Path::new("/nonexistent-dir/none.md"),
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            Mode::File,
            None,
        )
        .err()
        .expect("a missing file is an error");
        assert!(error.contains("/nonexistent-dir/none.md"), "{error}");
    }

    #[test]
    fn should_flash_and_stop_waiting_when_the_file_vanishes_before_it_is_read() {
        let path = scratch_file("vanishes", "text\n");
        let mut page = DocPage::open_async_with(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            slow_loader(Mode::File, Duration::from_millis(1)),
        )
        .unwrap();
        // Whether the worker read the file first or not, the page must settle.
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        tick_until_loaded(&mut page);
        assert!(!page.loading() && !page.wants_ticks());
    }

    #[test]
    fn should_show_raw_html_as_text_when_the_page_is_an_agent_page() {
        let source = "Before\n\n<script>alert(1)</script>\n\nAfter\n";
        let path = scratch_file("agent", source);
        let mut page = DocPage::open_async(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            Mode::Agent,
            None,
        )
        .unwrap();
        tick_until_loaded(&mut page);
        assert_eq!(page.mode(), Mode::Agent);
        let html: Vec<_> = page
            .doc()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                limpid_core::doc::BlockKind::Html(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(html.len(), 1);
        assert!(html[0].contains("<script>alert(1)</script>"), "{html:?}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_sanitise_raw_html_when_the_page_is_a_file() {
        let path = scratch_file("file-mode", "<script>alert(1)</script>\n\nAfter\n");
        let mut page = DocPage::open_async(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            Mode::File,
            None,
        )
        .unwrap();
        tick_until_loaded(&mut page);
        let shown = format!("{:?}", page.doc().blocks);
        assert!(!shown.contains("alert(1)"), "{shown}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_reparse_in_the_new_mode_when_the_mode_changes() {
        let page = page("<div>raw</div>\n\ntext\n").with_mode(Mode::Agent);
        assert_eq!(page.mode(), Mode::Agent);
        assert_eq!(page.parse_count(), 2);
        assert!(matches!(
            &page.doc().blocks[0].kind,
            limpid_core::doc::BlockKind::Html(text) if text.contains("<div>raw</div>")
        ));
        let unchanged = page.with_mode(Mode::Agent);
        assert_eq!(unchanged.parse_count(), 2, "the same mode costs nothing");
    }

    fn handoff_at(path: &Path, source: Option<&Path>) -> Handoff {
        Handoff {
            path: path.to_path_buf(),
            theme: Scheme::Dark,
            pane: None,
            source: source.map(Path::to_path_buf),
            source_hash: None,
            source_backed: true,
        }
    }

    #[test]
    fn should_choose_agent_mode_only_for_a_sidecar_markdown_beside_its_meta_file() {
        let path = scratch_file("mode-for", "x\n");
        let directory = path.parent().unwrap().to_path_buf();
        std::fs::write(directory.join("other.md"), "x").unwrap();
        std::fs::write(directory.join("mode-for.meta.json"), "{}").unwrap();
        assert_eq!(mode_for(&handoff_at(&path, None)), Mode::Agent);
        assert_eq!(
            mode_for(&handoff_at(&path, Some(&path))),
            Mode::File,
            "a file with a source of its own is the user's"
        );
        assert_eq!(
            mode_for(&handoff_at(&directory.join("other.md"), None)),
            Mode::File,
            "a stdin page has no meta file"
        );
        assert_eq!(
            mode_for(&handoff_at(&directory.join("missing.md"), None)),
            Mode::File
        );
        assert_eq!(
            mode_for(&handoff_at(&directory.join("mode-for.meta.json"), None)),
            Mode::File,
            "only Markdown pages are agent pages"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn should_keep_the_old_document_until_the_new_text_is_parsed_when_reloading_from_bytes() {
        let path = scratch_file("keeps-old", "old text\n");
        let mut page = DocPage::open_async_with(
            &path,
            &Settings::default(),
            Scheme::Dark,
            fonts(),
            slow_loader(Mode::File, Duration::from_millis(150)),
        )
        .unwrap();
        tick_until_loaded(&mut page);
        page.reload_from_bytes(b"new text\n");
        assert!(page.parsing());
        assert!(
            page.doc().source.contains("old"),
            "still showing the old text"
        );
        tick_until_loaded(&mut page);
        assert!(page.doc().source.contains("new"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
