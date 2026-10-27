//! Feedback on a page: answering questions, commenting on blocks, writing
//! both in an embedded editor, saving them and sending them to the agent.
//!
//! [`Feedback`] is the state behind the Editor state of the key routing table.
//! The page asks it for the blocks to show ([`Feedback::overlay`]), gives it
//! keys while a box is open, and polls it for what the editor said. The key
//! table's feedback actions reach it through [`FeedbackHandle`], the
//! [`PageActions`] implementation of the page.

pub mod boxes;
pub mod fields;
pub mod host;
pub mod review;

use crate::{
    editor::{BoxKind, EditorConfig, EditorEvent},
    layout::{LaidBlock, Layout},
    nav::{ActionResult, EditAt, FeedbackAction, PageActions},
    paint::{Canvas, Rect},
    text::Fonts,
    theme::Theme,
    window::TICK_INTERVAL,
};
use boxes::{BoxLimits, EditorSpawner, OpenBox};
use fields::{
    BoxPlan, BoxTarget, CommentEntry, DocIndex, FILE_KEY_PREFIX, Pending, QUOTE_CHARS, Synthetic,
    file_key, line_of, quote_of, range_text,
};
use host::{BeginError, EditorHost, OwnerId, SharedEditor};
use limpid_core::{
    doc::Block,
    replies::{self, AgentReplies},
    settings::Settings,
    transport::Handoff,
};
use review::Review;
use std::{
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

/// How long a window with nothing to poll sleeps while another window's box
/// has the editor, or when no idle deadline is running.
const BYSTANDER_PERIOD: Duration = Duration::from_secs(1);

/// Settings of the feedback layer that are not part of the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackOptions {
    pub limits: BoxLimits,
    /// Longest quote of a block in a comment, in characters.
    pub quote_chars: usize,
    /// The editor process is stopped after this long with no box open.
    pub idle: Duration,
}

impl FeedbackOptions {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            limits: BoxLimits::default(),
            quote_chars: QUOTE_CHARS,
            idle: Duration::from_secs(settings.editor.idle_seconds),
        }
    }
}

/// The laid-out block at `src`, looked for inside list items too.
fn find_block<'a>(blocks: &'a [LaidBlock], src: &Range<usize>) -> Option<&'a LaidBlock> {
    blocks.iter().find_map(|block| {
        if &block.src == src {
            return Some(block);
        }
        find_block(block.children(), src)
    })
}

/// Text from an input method as nvim key notation: `<` is the only
/// character `nvim_input` reads as the start of a key name.
pub fn ime_keys(text: &str) -> String {
    text.replace('<', "<lt>")
}

/// What a cursor stop is, as far as feedback goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopRef {
    Field(String),
    Reply(String),
    Comment(String),
    Box,
    Plain(Range<usize>),
}

/// Where the cursor should go after the page changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Box,
    Comment(String),
    Real(Range<usize>),
}

pub struct Feedback {
    options: FeedbackOptions,
    settings: Settings,
    handoff: Handoff,
    title: String,
    file_backed: bool,
    index: DocIndex,
    source: String,
    pending: Pending,
    synthetic: HashMap<Range<usize>, Synthetic>,
    host: SharedEditor,
    owner: OwnerId,
    open: Option<OpenBox>,
    /// Columns and rows of the last grid the editor was sized to.
    grid_size: (u16, u16),
    /// The most rows a box may have in the window it was last painted in.
    viewport_rows: Option<u16>,
    review: Option<Review>,
    stale: bool,
    reload: bool,
    focus: Option<Focus>,
    flashes: Vec<String>,
}

impl Drop for Feedback {
    /// A window closed with its box open: the editor must not stay claimed.
    /// The process goes with the box (its float and buffer are abandoned, and
    /// a late event from it must not reach the next owner).
    fn drop(&mut self) {
        if self.open.is_none() {
            return;
        }
        if let Ok(mut host) = self.host.try_borrow_mut() {
            host.discard(self.owner);
        }
    }
}

impl Feedback {
    /// `handoff` names the page; with both `source` and `source_hash` set,
    /// feedback is written into the Markdown file, else into the page's
    /// sidecar.
    pub fn new(handoff: Handoff, title: String, settings: &Settings) -> Self {
        let mut handoff = handoff;
        if let (Some(source), None) = (&handoff.source, &handoff.source_hash)
            && let Ok(bytes) = std::fs::read(source)
        {
            handoff.source_hash = Some(replies::source_hash(&bytes));
        }
        let file_backed = handoff.source.is_some() && handoff.source_hash.is_some();
        let mut flashes = Vec::new();
        let pending = if file_backed {
            Pending::default()
        } else {
            match replies::read_annotations_file(&handoff.path) {
                Ok(saved) => Pending::from_saved(&saved),
                Err(error) => {
                    flashes.push(format!("could not read saved feedback: {error}"));
                    Pending::default()
                }
            }
        };
        let host = EditorHost::shared_nvim();
        let owner = host.borrow_mut().register();
        Self {
            options: FeedbackOptions::from_settings(settings),
            settings: settings.clone(),
            handoff,
            title,
            file_backed,
            index: DocIndex::default(),
            source: String::new(),
            pending,
            synthetic: HashMap::new(),
            host,
            owner,
            open: None,
            grid_size: (80, 6),
            viewport_rows: None,
            review: None,
            stale: false,
            reload: false,
            focus: None,
            flashes,
        }
    }

    /// A private editor host that starts editors with `spawner`.
    pub fn with_spawner(self, spawner: EditorSpawner) -> Self {
        self.with_host(EditorHost::shared(spawner))
    }

    /// Share `host` (one per view process) with the other windows.
    pub fn with_host(mut self, host: SharedEditor) -> Self {
        self.owner = host.borrow_mut().register();
        self.host = host;
        self
    }

    pub fn with_options(mut self, options: FeedbackOptions) -> Self {
        self.options = options;
        self
    }

    pub fn options(&self) -> FeedbackOptions {
        self.options
    }

    pub fn handoff(&self) -> &Handoff {
        &self.handoff
    }

    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    pub fn is_file_backed(&self) -> bool {
        self.file_backed
    }

    // ---- the document ----

    /// Remember the parsed blocks (before the overlay) and their source.
    pub fn index_document(&mut self, blocks: &[Block], source: &str) {
        self.index = DocIndex::from_blocks(blocks, source);
        self.source = source.to_owned();
    }

    /// `base` with the pending replies and comments and the open box spliced
    /// in. Remembers which added block is which.
    pub fn overlay(&mut self, base: &[Block]) -> Vec<Block> {
        let plan = self.open.as_ref().map(|open| &open.plan);
        let overlay = fields::overlay(base, &self.index, &self.pending, plan, &self.source);
        self.synthetic = overlay.synthetic;
        self.stale = false;
        overlay.blocks
    }

    /// The page must rebuild its blocks with [`Feedback::overlay`].
    pub fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// The Markdown file changed on disk (a save); read it again.
    pub fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.reload)
    }

    /// The title replies are subjected with.
    #[cfg(test)]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The document's title changed; replies name it in their subject.
    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    /// The file was read again; keep the revision check in step with it.
    pub fn source_reloaded(&mut self, bytes: &[u8]) {
        if self.file_backed {
            self.handoff.source_hash = Some(replies::source_hash(bytes));
        }
    }

    pub fn take_focus(&mut self) -> Option<Focus> {
        self.focus.take()
    }

    /// Messages to show on the status line, oldest first.
    pub fn take_flashes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.flashes)
    }

    /// The source range of the stop `focus` names, in the current overlay.
    pub fn resolve_focus(&self, focus: &Focus) -> Option<Range<usize>> {
        match focus {
            Focus::Box => self.box_src(),
            Focus::Real(range) => Some(range.clone()),
            Focus::Comment(key) => {
                let synthetic = self.synthetic.iter().find_map(|(range, what)| {
                    (*what == Synthetic::Comment(key.clone())).then(|| range.clone())
                });
                synthetic.or_else(|| {
                    self.index
                        .notes
                        .iter()
                        .find(|note| file_key(&note.src) == *key)
                        .map(|note| note.src.clone())
                })
            }
        }
    }

    /// What the stop at `src` is.
    pub fn classify(&self, src: &Range<usize>) -> StopRef {
        if let Some(what) = self.synthetic.get(src) {
            return match what {
                Synthetic::Reply(id) => StopRef::Reply(id.clone()),
                Synthetic::Comment(key) => StopRef::Comment(key.clone()),
                Synthetic::Box => StopRef::Box,
            };
        }
        if let Some(field) = self.index.field_at(src) {
            return StopRef::Field(field.id.clone());
        }
        if self.index.saved_comment(src).is_some() {
            return StopRef::Comment(file_key(src));
        }
        StopRef::Plain(src.clone())
    }

    /// An identity for the stop at `src` that survives a rebuild of the
    /// overlay.
    pub fn identify(&self, src: &Range<usize>) -> Focus {
        match self.classify(src) {
            StopRef::Box => Focus::Box,
            StopRef::Comment(key) => Focus::Comment(key),
            _ => Focus::Real(src.clone()),
        }
    }

    // ---- the editor box ----

    pub fn box_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open_box(&self) -> Option<&OpenBox> {
        self.open.as_ref()
    }

    /// The source range of the block that holds the editor box.
    pub fn box_src(&self) -> Option<Range<usize>> {
        self.synthetic
            .iter()
            .find_map(|(range, what)| (*what == Synthetic::Box).then(|| range.clone()))
    }

    /// Whether the window should call [`Feedback::tick`]: an editor is
    /// running or its idle timer is.
    pub fn wants_ticks(&self) -> bool {
        self.open.is_some() || self.host.borrow().has_editor()
    }

    /// How long the window may sleep before the next [`Feedback::tick`]: the
    /// frame interval while this window's box is open, else the time until
    /// the idle timer stops the editor (a long sleep when there is none).
    pub fn tick_period(&self, now: Instant) -> Duration {
        if self.open.is_some() {
            return TICK_INTERVAL;
        }
        let host = self.host.borrow();
        if host.in_use_by_other(self.owner) {
            return BYSTANDER_PERIOD;
        }
        let Some(deadline) = host.idle_deadline(self.options.idle) else {
            return BYSTANDER_PERIOD;
        };
        deadline
            .saturating_duration_since(now)
            .max(Duration::from_millis(1))
    }

    /// The row limits, with the top set by the window's height once known.
    fn limits(&self) -> BoxLimits {
        let mut limits = self.options.limits;
        if let Some(rows) = self.viewport_rows {
            limits.max_rows = rows.max(limits.min_rows);
        }
        limits
    }

    fn editor_config(&self) -> EditorConfig {
        EditorConfig::from_settings(&self.settings.editor, self.grid_size.0, self.grid_size.1)
    }

    fn begin_box(&mut self, target: BoxTarget, text: &str, kind: BoxKind) -> ActionResult {
        if self.open.is_some() {
            return ActionResult::Nothing("editor open: :wq or :q!".into());
        }
        let config = self.editor_config();
        let begun = self
            .host
            .borrow_mut()
            .begin(self.owner, &config, text, kind);
        match begun {
            Ok(()) => {}
            Err(BeginError::InUse) => {
                return ActionResult::Nothing("editor in use in another window".into());
            }
            Err(BeginError::Spawn(error)) => {
                return ActionResult::Failed(format!(
                    "{error} (set [editor] command to your nvim)"
                ));
            }
            Err(BeginError::Open(error)) => return ActionResult::Failed(error.to_string()),
        }
        let rows = self.limits().rows_for(text.lines().count() as u16);
        self.open = Some(OpenBox::new(BoxPlan { target, rows }));
        self.stale = true;
        self.focus = Some(Focus::Box);
        ActionResult::Done(String::new())
    }

    /// Keys for the editor, in nvim notation. Returns at once; the editor's
    /// answer arrives through [`Feedback::tick`].
    pub fn input(&mut self, keys: &str) {
        if keys.is_empty() || self.open.is_none() {
            return;
        }
        self.host.borrow_mut().input(self.owner, keys);
    }

    /// Text committed by an input method: typed into the box literally.
    pub fn ime_commit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.input(&ime_keys(text));
    }

    /// The input method's uncommitted text, shown in the box's status line;
    /// `None` or empty clears it. Returns whether the display changed.
    pub fn ime_preedit(&mut self, text: Option<&str>) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        let text = text.filter(|text| !text.is_empty()).map(str::to_owned);
        if open.preedit == text {
            return false;
        }
        open.preedit = text;
        true
    }

    /// `Ctrl-Shift-q`: close the box and throw its buffer away.
    pub fn force_close(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let leaves = open.plan.clone();
        self.host
            .borrow_mut()
            .input(self.owner, "<C-\\><C-n>:q!<CR>");
        self.finish_box(leaves);
        self.flashes.push("editor closed, buffer discarded".into());
    }

    fn finish_box(&mut self, plan: BoxPlan) {
        self.open = None;
        self.host.borrow_mut().release(self.owner, Instant::now());
        self.stale = true;
        self.focus = Some(match &plan.target {
            BoxTarget::Reply { field } => self
                .index
                .field(field)
                .map(|field| Focus::Real(field.src.clone()))
                .unwrap_or(Focus::Box),
            BoxTarget::Comment { key, target } => {
                if self.pending.comments.contains_key(key) {
                    Focus::Comment(key.clone())
                } else {
                    Focus::Real(target.clone())
                }
            }
        });
    }

    /// Handle what the editor reported. Returns whether anything did.
    fn drain_editor(&mut self) -> bool {
        let Some(polled) = self.host.borrow_mut().poll(self.owner) else {
            return false;
        };
        let crashed = self.open.is_some() && !polled.running;
        let handled = !polled.events.is_empty();
        for event in polled.events {
            self.handle_event(event);
        }
        if crashed && let Some(open) = self.open.as_ref() {
            let plan = open.plan.clone();
            self.finish_box(plan);
            self.host.borrow_mut().discard(self.owner);
            self.flashes.push("the editor exited".into());
            return true;
        }
        handled
    }

    fn handle_event(&mut self, event: EditorEvent) {
        match event {
            EditorEvent::Redraw => self.fit_box_to_content(),
            EditorEvent::Mode(mode) => {
                if let Some(open) = self.open.as_mut() {
                    open.mode = mode;
                }
            }
            EditorEvent::Save(text) => self.apply_buffer(&text),
            EditorEvent::Quit { .. } => {
                if let Some(open) = self.open.as_ref() {
                    let plan = open.plan.clone();
                    self.finish_box(plan);
                }
            }
            EditorEvent::Message(message) => {
                if let Some(open) = self.open.as_mut() {
                    open.message = Some(message.clone());
                }
                self.flashes.push(message);
            }
            EditorEvent::CmdlineShow(line) => {
                if let Some(open) = self.open.as_mut() {
                    open.cmdline = Some(line);
                }
            }
            EditorEvent::CmdlineHide => {
                if let Some(open) = self.open.as_mut() {
                    open.cmdline = None;
                }
            }
            EditorEvent::Error(message) => self.flashes.push(format!("editor: {message}")),
        }
    }

    /// Grow or shrink the box to the text in it.
    fn fit_box_to_content(&mut self) {
        let limits = self.limits();
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let content = self.host.borrow().grid().map(boxes::content_rows);
        let Some(content) = content else {
            return;
        };
        let rows = limits.rows_for(content);
        if open.plan.rows != rows {
            open.plan.rows = rows;
            self.stale = true;
        }
    }

    /// The text of the buffer arrived (`:w`): it is the new reply or comment.
    fn apply_buffer(&mut self, text: &str) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        match open.plan.target.clone() {
            BoxTarget::Reply { field } => {
                let has_saved = self.index.saved_reply(&field).is_some();
                self.pending.set_reply(&field, text, has_saved);
            }
            BoxTarget::Comment { key, target } => {
                let turn = self
                    .pending
                    .comments
                    .get(&key)
                    .map(|entry| entry.turn.clone())
                    .unwrap_or_default();
                let quoted = self
                    .source
                    .get(target.clone())
                    .map(|slice| quote_of(slice, self.options.quote_chars))
                    .unwrap_or_default();
                let last = target.end.saturating_sub(1).max(target.start);
                self.pending.set_comment(CommentEntry {
                    key,
                    turn,
                    text: text.to_owned(),
                    quote: quoted,
                    start_line: line_of(&self.source, target.start),
                    end_line: line_of(&self.source, last),
                    target,
                });
            }
        }
        self.stale = true;
    }

    /// Poll the editor and stop it when it has been idle long enough.
    /// Returns whether the page needs a repaint.
    pub fn tick(&mut self, now: Instant) -> bool {
        let handled = self.drain_editor();
        if self.open.is_none() {
            self.host.borrow_mut().stop_if_idle(now, self.options.idle);
        }
        handled || self.stale
    }

    /// Paint the open box over its placeholder block. `scroll` is the
    /// document offset of the window's top edge.
    pub fn paint_box(
        &mut self,
        canvas: &mut Canvas,
        layout: &Layout,
        scroll: f32,
        theme: &Theme,
        fonts: &mut Fonts,
    ) {
        if self.open.is_none() {
            return;
        }
        let max_rows = boxes::rows_in_viewport(canvas.height() as f32, theme, layout.scale());
        if self.viewport_rows != Some(max_rows) {
            self.viewport_rows = Some(max_rows);
            self.fit_box_to_content();
        }
        let (Some(open), Some(src)) = (self.open.as_ref(), self.box_src()) else {
            return;
        };
        let Some(block) = find_block(&layout.blocks, &src) else {
            return;
        };
        let outer = Rect::new(
            block.rect.x + layout.column_x,
            block.rect.y - scroll,
            block.rect.w,
            block.rect.h,
        );
        if outer.bottom() < 0.0 || outer.y > canvas.height() as f32 {
            return;
        }
        let scale = layout.scale();
        let size = boxes::grid_size(outer, theme, scale, fonts);
        self.grid_size = size;
        self.host.borrow_mut().resize(self.owner, size.0, size.1);
        let host = self.host.borrow();
        let Some(grid) = host.grid() else {
            return;
        };
        boxes::paint(canvas, fonts, theme, scale, outer, grid, open);
    }

    // ---- the review overlay ----

    pub fn review(&self) -> Option<&Review> {
        self.review.as_ref()
    }

    pub fn review_mut(&mut self) -> Option<&mut Review> {
        self.review.as_mut()
    }

    pub fn close_review(&mut self) {
        self.review = None;
    }

    fn open_review(&mut self) -> ActionResult {
        let saved = match replies::saved_replies(&self.handoff) {
            Ok(saved) => saved,
            Err(error) => return ActionResult::Failed(format!("review: {error}")),
        };
        let merged = self.pending.merged_into(saved);
        if !merged.has_feedback() {
            return ActionResult::Nothing("no feedback yet".into());
        }
        match replies::compose_for_page(&self.handoff, &self.title, &merged) {
            Ok(message) => {
                self.review = Some(Review::new(message));
                ActionResult::Done(String::new())
            }
            Err(error) => ActionResult::Failed(format!("review: {error}")),
        }
    }

    // ---- saving and sending ----

    fn save(&mut self) -> Result<(), String> {
        let value = self.pending.to_value();
        let file_backed =
            replies::save_pending(&mut self.handoff, &value).map_err(|error| error.to_string())?;
        self.pending.saved(file_backed);
        if file_backed {
            self.reload = true;
        }
        self.stale = true;
        Ok(())
    }

    fn send(&mut self) -> ActionResult {
        let feedback = match replies::has_feedback(&self.handoff) {
            Ok(feedback) => feedback,
            Err(error) => return ActionResult::Failed(error.to_string()),
        };
        if !feedback {
            return ActionResult::Nothing("no feedback to send".into());
        }
        let message = match replies::compose_review(&self.handoff, &self.title) {
            Ok(message) => message,
            Err(error) => return ActionResult::Failed(error.to_string()),
        };
        match replies::deliver_or_wake_with(&self.handoff, &message, &self.settings.reply.command) {
            Ok(()) => ActionResult::Done("Sent".into()),
            Err(error) => ActionResult::Failed(format!("not sent (saved): {error}")),
        }
    }

    // ---- the feedback actions of the key table ----

    fn saved_reply_text(&self, field: &str) -> String {
        self.index
            .saved_reply(field)
            .map(|note| note.text.trim().to_owned())
            .unwrap_or_default()
    }

    fn open_reply(&mut self, stop: Option<&Range<usize>>) -> ActionResult {
        let field = match stop.map(|stop| self.classify(stop)) {
            Some(StopRef::Field(id) | StopRef::Reply(id)) => id,
            Some(StopRef::Comment(_)) => return self.open_comment(stop),
            _ => return ActionResult::Nothing("no question here".into()),
        };
        let text = match self.pending.reply(&field) {
            Some(text) => text.to_owned(),
            None => self.saved_reply_text(&field),
        };
        self.begin_box(BoxTarget::Reply { field }, &text, BoxKind::Reply)
    }

    /// `i`, `I` and `A`: edit the question's reply, or comment on the block,
    /// with the cursor where `at` says.
    fn edit_at(&mut self, at: EditAt, stop: Option<&Range<usize>>) -> ActionResult {
        let on_question = matches!(
            stop.map(|stop| self.classify(stop)),
            Some(StopRef::Field(_) | StopRef::Reply(_))
        );
        let result = if on_question {
            self.open_reply(stop)
        } else {
            self.open_comment(stop)
        };
        if matches!(result, ActionResult::Done(_)) {
            match at {
                EditAt::Keep if on_question => {}
                EditAt::Keep | EditAt::Start => self.input("<C-\\><C-n>ggI"),
                EditAt::End => self.input("<C-\\><C-n>GA"),
            }
        }
        result
    }

    fn open_comment(&mut self, stop: Option<&Range<usize>>) -> ActionResult {
        let Some(stop) = stop else {
            return ActionResult::Nothing("no block here".into());
        };
        let (key, target, text) = match self.classify(stop) {
            StopRef::Box => return ActionResult::Nothing("editor open: :wq or :q!".into()),
            StopRef::Comment(key) => {
                let note = self
                    .index
                    .notes
                    .iter()
                    .find(|note| file_key(&note.src) == key);
                let entry = self.pending.comments.get(&key);
                let target = entry
                    .map(|entry| entry.target.clone())
                    .or_else(|| note.map(|note| note.target.clone()));
                let text = entry
                    .map(|entry| entry.text.clone())
                    .or_else(|| note.map(|note| note.text.trim().to_owned()))
                    .unwrap_or_default();
                match target {
                    Some(target) => (key, target, text),
                    None => return ActionResult::Nothing("no block here".into()),
                }
            }
            StopRef::Field(_) | StopRef::Reply(_) => return self.open_reply(Some(stop)),
            StopRef::Plain(_) => self.comment_on(stop.clone()),
        };
        self.begin_box(BoxTarget::Comment { key, target }, &text, BoxKind::Comment)
    }

    /// The key, target and starting text for a comment on the block at `range`.
    fn comment_on(&self, range: Range<usize>) -> (String, Range<usize>, String) {
        let key = range_text(&range);
        let text = self
            .pending
            .comments
            .get(&key)
            .map(|entry| entry.text.clone())
            .unwrap_or_default();
        (key, range, text)
    }

    fn delete_comment(&mut self, stop: Option<&Range<usize>>) -> ActionResult {
        let Some(StopRef::Comment(key)) = stop.map(|stop| self.classify(stop)) else {
            return ActionResult::Nothing("no comment here".into());
        };
        let target = self
            .pending
            .comments
            .get(&key)
            .map(|entry| entry.target.clone())
            .or_else(|| {
                self.index
                    .notes
                    .iter()
                    .find(|note| file_key(&note.src) == key)
                    .map(|note| note.target.clone())
            });
        self.pending.remove_comment(&key);
        self.stale = true;
        if let Some(target) = target {
            self.focus = Some(Focus::Real(target));
        }
        ActionResult::Done("Comment removed, :w to save".into())
    }

    fn pick_option(&mut self, number: u8, stop: Option<&Range<usize>>) -> ActionResult {
        let Some(StopRef::Field(id)) = stop.map(|stop| self.classify(stop)) else {
            return ActionResult::Nothing("no question here".into());
        };
        let Some(field) = self.index.field(&id) else {
            return ActionResult::Nothing("no question here".into());
        };
        let Some(option) = usize::from(number)
            .checked_sub(1)
            .and_then(|at| field.options.get(at))
            .cloned()
        else {
            return ActionResult::Nothing(format!("{id} has no option {number}"));
        };
        let src = field.src.clone();
        let has_saved = self.index.saved_reply(&id).is_some();
        self.pending.set_reply(&id, &option, has_saved);
        self.stale = true;
        self.focus = Some(Focus::Real(src));
        ActionResult::Done(format!("{id}: {option}"))
    }

    fn first_unanswered(&mut self) -> ActionResult {
        let answered = |field: &fields::FieldInfo| {
            let pending = self.pending.reply(&field.id);
            let text = match pending {
                Some(text) => text.to_owned(),
                None => self.saved_reply_text(&field.id),
            };
            !text.trim().is_empty()
        };
        let unanswered: Vec<&fields::FieldInfo> = self
            .index
            .fields
            .iter()
            .filter(|field| !answered(field))
            .collect();
        let Some(first) = unanswered.first() else {
            return ActionResult::Nothing("all questions answered".into());
        };
        let message = format!("{} unanswered, first {}", unanswered.len(), first.id);
        self.focus = Some(Focus::Real(first.src.clone()));
        ActionResult::Done(message)
    }

    /// Whether `1`-`9` pick an option on the stop at `stop`.
    pub fn digit_picks(&self, stop: &Range<usize>) -> bool {
        match self.classify(stop) {
            StopRef::Field(id) => self
                .index
                .field(&id)
                .is_some_and(|field| !field.options.is_empty()),
            _ => false,
        }
    }

    pub fn write(&mut self) -> ActionResult {
        if !self.pending.has_unsaved() {
            return ActionResult::Nothing("nothing to save".into());
        }
        match self.save() {
            Ok(()) => ActionResult::Done("Saved".into()),
            Err(error) => ActionResult::Failed(format!("not saved: {error}")),
        }
    }

    pub fn write_and_send(&mut self) -> ActionResult {
        if self.pending.has_unsaved()
            && let Err(error) = self.save()
        {
            return ActionResult::Failed(format!("not saved: {error}"));
        }
        self.send()
    }

    pub fn has_unsaved(&self) -> bool {
        self.pending.has_unsaved() || self.open.is_some()
    }

    pub fn act(&mut self, action: FeedbackAction, stop: Option<&Range<usize>>) -> ActionResult {
        match action {
            FeedbackAction::PickOption(number) => self.pick_option(number, stop),
            FeedbackAction::OpenReply => self.open_reply(stop),
            FeedbackAction::OpenComment => self.open_comment(stop),
            FeedbackAction::Edit(at) => self.edit_at(at, stop),
            FeedbackAction::DeleteComment => self.delete_comment(stop),
            FeedbackAction::Review => self.open_review(),
            FeedbackAction::FirstUnanswered => self.first_unanswered(),
        }
    }

    /// The saved replies and comments as `limpid-core` has them on disk.
    pub fn saved(&self) -> Result<AgentReplies, String> {
        replies::saved_replies(&self.handoff).map_err(|error| error.to_string())
    }

    /// The key of a comment on the block at `range`; exposed for tests and
    /// for pages that show feedback some other way.
    pub fn comment_key(range: &Range<usize>) -> String {
        range_text(range)
    }

    /// Whether `key` names a comment already in the Markdown file.
    pub fn is_file_key(key: &str) -> bool {
        key.starts_with(FILE_KEY_PREFIX)
    }
}

/// The page's [`PageActions`]: the key table's feedback actions, `:w` and
/// `:wq`, forwarded to the shared [`Feedback`].
#[derive(Clone)]
pub struct FeedbackHandle(pub Rc<RefCell<Feedback>>);

impl PageActions for FeedbackHandle {
    fn write(&mut self) -> ActionResult {
        self.0.borrow_mut().write()
    }

    fn write_and_send(&mut self) -> ActionResult {
        self.0.borrow_mut().write_and_send()
    }

    fn has_unsaved(&self) -> bool {
        self.0.borrow().has_unsaved()
    }

    fn digit_selects_option(&self, stop: &Range<usize>) -> bool {
        self.0.borrow().digit_picks(stop)
    }

    fn feedback(&mut self, action: FeedbackAction, stop: Option<&Range<usize>>) -> ActionResult {
        self.0.borrow_mut().act(action, stop)
    }
}

#[cfg(test)]
mod tests;
