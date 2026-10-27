//! Keyboard navigation for the document page: the block cursor, search, yank,
//! visual mode and the `:` command line.
//!
//! [`Nav`] is the state machine for the Page, Search and Command states of the
//! key routing table. It reads and moves the view through [`Env`], so it runs
//! without a display. Everything that belongs to the feedback editor (replies,
//! comments, saving) is behind [`PageActions`], which the feedback layer
//! implements.

pub mod command;
pub mod cursor;
pub mod keymap;
pub mod search;
pub mod status;
pub mod yank;

use crate::{
    layout::Layout,
    paint::Rect,
    theme::{Color, Theme},
};
use command::{COMMAND_PLACEHOLDER, Command, LineEditor};
use cursor::{Align, Cursor, Env, collect_stops};
use keymap::{Action, KeyInput, Keymap, Resolved};
use limpid_core::doc::Doc;
use search::{Hit, Query, Search};
use status::{DEFAULT_FLASH_MS, Prompt, StateName, Status, StatusView};
use std::{collections::BTreeMap, ops::Range};
use yank::{Clipboard, Operator, Step, Target};

/// Settings of the navigation layer that are not part of the theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavOptions {
    /// How long a status flash stays, in milliseconds.
    pub flash_ms: u64,
    /// Each zoom step multiplies the font size by this.
    pub zoom_step: f32,
    /// Smallest and largest zoom factor.
    pub zoom_range: (f32, f32),
}

impl Default for NavOptions {
    fn default() -> Self {
        Self {
            flash_ms: DEFAULT_FLASH_MS,
            zoom_step: 1.1,
            zoom_range: (0.5, 3.0),
        }
    }
}

/// Which window a close request is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseKind {
    /// This window.
    Window,
    /// Every window of the viewer.
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomStep {
    In,
    Out,
    Reset,
}

/// Something only the page can do; [`Nav`] asks for it in its [`Outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Close(CloseKind),
    Reload,
    Zoom(ZoomStep),
    ToggleRail,
}

/// What a key press did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The picture changed and needs a repaint.
    pub redraw: bool,
    pub requests: Vec<Request>,
}

/// Where the cursor starts in a box opened by `i`, `I` or `A`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAt {
    Keep,
    Start,
    End,
}

/// Feedback actions that belong to the editor layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackAction {
    PickOption(u8),
    OpenReply,
    OpenComment,
    Edit(EditAt),
    DeleteComment,
    Review,
    FirstUnanswered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    /// It worked; the message is flashed.
    Done(String),
    /// There was nothing to do; the message says why.
    Nothing(String),
    Failed(String),
    /// This page has no such feature.
    Unsupported,
}

/// What the page can do about feedback. The defaults are those of a page with
/// no editor.
pub trait PageActions {
    /// `:w`
    fn write(&mut self) -> ActionResult {
        ActionResult::Nothing("nothing to save".into())
    }

    /// `:wq`. On [`ActionResult::Done`] the window closes.
    fn write_and_send(&mut self) -> ActionResult {
        ActionResult::Nothing("nothing to save".into())
    }

    /// Whether `:q` must refuse.
    fn has_unsaved(&self) -> bool {
        false
    }

    /// Whether `1`-`9` pick an option (rather than start a count) on the stop
    /// at `stop`.
    fn digit_selects_option(&self, _stop: &Range<usize>) -> bool {
        false
    }

    fn feedback(&mut self, _action: FeedbackAction, _stop: Option<&Range<usize>>) -> ActionResult {
        ActionResult::Unsupported
    }
}

/// A page with no editor.
pub struct NoActions;

impl PageActions for NoActions {}

/// The collaborators a key press may need.
pub struct Io<'a> {
    pub actions: &'a mut dyn PageActions,
    pub clipboard: &'a mut dyn Clipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Page,
    Search,
    Command,
}

/// Rectangles to paint behind the text, in document coordinates.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Bands {
    pub focus: Option<Rect>,
    pub selection: Option<Rect>,
}

pub struct Nav {
    pub options: NavOptions,
    keymap: Keymap,
    cursor: Cursor,
    search: Search,
    /// Whether the committed search is drawn (Esc hides it, `n` shows it).
    show_search: bool,
    status: Status,
    mode: Mode,
    line: LineEditor,
    visual_anchor: Option<Range<usize>>,
    operator: Option<Operator>,
}

impl Nav {
    pub fn new(
        key_overrides: &BTreeMap<String, String>,
        options: NavOptions,
    ) -> Result<Self, String> {
        Ok(Self::with_keymap(Keymap::new(key_overrides)?, options))
    }

    pub fn with_keymap(keymap: Keymap, options: NavOptions) -> Self {
        Self {
            options,
            keymap,
            cursor: Cursor::default(),
            search: Search::default(),
            show_search: false,
            status: Status::default(),
            mode: Mode::Page,
            line: LineEditor::default(),
            visual_anchor: None,
            operator: None,
        }
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    pub fn search(&self) -> &Search {
        &self.search
    }

    pub fn flash(&mut self, text: impl Into<String>, now_ms: u64) {
        self.status.flash(text, now_ms, self.options.flash_ms);
    }

    /// Put the cursor on its starting stop if it has none yet.
    pub fn init(&mut self, env: &mut Env) -> bool {
        self.cursor.init(env)
    }

    /// Scroll just far enough to show the focused stop, laying the document
    /// out as far as it takes to find it. Returns whether the view moved.
    pub fn reveal_cursor(&mut self, env: &mut Env) -> bool {
        loop {
            let stops = collect_stops(env.layout);
            let exact = self
                .cursor
                .src
                .as_ref()
                .and_then(|src| stops.iter().find(|stop| &stop.src == src));
            if let Some(stop) = exact {
                return env.reveal(stop.rect.y, stop.rect.h);
            }
            if !env.lay_out_more() {
                return false;
            }
        }
    }

    /// Point the cursor at `src` (the source range of a stop in the document
    /// about to be shown). `None` leaves it to find its own place.
    pub fn retarget_cursor(&mut self, src: Option<Range<usize>>) {
        if src.is_some() {
            self.cursor.src = src;
        }
    }

    /// The document was replaced: redo the search and drop the selection.
    pub fn document_changed(&mut self, doc: &Doc) {
        self.visual_anchor = None;
        let query = self.search.query.clone();
        self.search.run(doc, query);
    }

    pub fn state(&self) -> StateName {
        match self.mode {
            Mode::Search => StateName::Search,
            Mode::Command => StateName::Command,
            Mode::Page if self.visual_anchor.is_some() => StateName::Visual,
            Mode::Page => StateName::Page,
        }
    }

    /// Whether the status bar is showing a prompt that owns the keys.
    pub fn in_prompt(&self) -> bool {
        self.mode != Mode::Page
    }

    pub fn status_view(&self, now_ms: u64) -> StatusView {
        let prompt = match self.mode {
            Mode::Page => None,
            Mode::Search => Some(Prompt {
                sigil: '/',
                text: self.line.text().to_owned(),
                placeholder: "",
            }),
            Mode::Command => Some(Prompt {
                sigil: ':',
                text: self.line.text().to_owned(),
                placeholder: COMMAND_PLACEHOLDER,
            }),
        };
        let pending = match &self.operator {
            Some(operator) => operator.pending_text(),
            None => self.keymap.pending_text(),
        };
        let right = if !pending.is_empty() {
            pending
        } else {
            self.search_summary()
        };
        StatusView {
            state: self.state(),
            message: self.status.current(now_ms).unwrap_or("").to_owned(),
            prompt,
            right,
        }
    }

    fn search_summary(&self) -> String {
        let (true, Some(query)) = (self.show_search, &self.search.query) else {
            return String::new();
        };
        match self.search.position() {
            Some((at, total)) => format!("[{at}/{total}] /{}", query.text),
            None => format!("/{}", query.text),
        }
    }

    /// The query to highlight and the match to emphasise, if any.
    pub fn highlight(&self) -> Option<(Query, Option<Hit>)> {
        if self.mode == Mode::Search {
            return Query::smartcase(self.line.text()).map(|query| (query, None));
        }
        if !self.show_search {
            return None;
        }
        let query = self.search.query.clone()?;
        Some((query, self.search.current_hit()))
    }

    /// Where the focus band and the visual selection go.
    pub fn bands(&self, layout: &Layout) -> Bands {
        let stops = collect_stops(layout);
        let Some(at) = self.cursor.locate(&stops) else {
            return Bands::default();
        };
        let focus = stops[at].rect;
        let selection = self.visual_anchor.as_ref().and_then(|anchor| {
            let from = stops.iter().position(|stop| &stop.src == anchor)?;
            let (low, high) = (from.min(at), from.max(at));
            let top = stops[low..=high]
                .iter()
                .map(|s| s.rect.y)
                .fold(f32::MAX, f32::min);
            let bottom = stops[low..=high]
                .iter()
                .map(|s| s.rect.bottom())
                .fold(f32::MIN, f32::max);
            Some(Rect::new(focus.x, top, focus.w, bottom - top))
        });
        Bands {
            focus: Some(focus),
            selection,
        }
    }

    /// Handle one key press.
    pub fn handle(&mut self, input: &KeyInput, now_ms: u64, env: &mut Env, io: &mut Io) -> Outcome {
        let mut out = Outcome::default();
        self.cursor.init(env);
        match self.mode {
            Mode::Page => self.page_key(input, now_ms, env, io, &mut out),
            Mode::Search | Mode::Command => self.line_key(input, now_ms, env, io, &mut out),
        }
        out
    }

    fn page_key(
        &mut self,
        input: &KeyInput,
        now_ms: u64,
        env: &mut Env,
        io: &mut Io,
        out: &mut Outcome,
    ) {
        if let Some(operator) = &mut self.operator {
            if operator.expired(now_ms) {
                self.operator = None;
            } else {
                let step = operator.feed(&input.chord);
                out.redraw = true;
                match step {
                    Step::Pending => {}
                    Step::Cancel => self.operator = None,
                    Step::Run(target) => {
                        self.operator = None;
                        self.yank_target(target, now_ms, env, io);
                    }
                }
                return;
            }
        }
        let had_pending = self.keymap.has_pending();
        let digits_are_counts = !self.digit_picks_option(io);
        match self.keymap.feed(&input.chord, now_ms, digits_are_counts) {
            Resolved::Pending => out.redraw = true,
            Resolved::Unbound => out.redraw = had_pending,
            Resolved::Action { action, count } => {
                out.redraw = true;
                self.run(action, count, now_ms, env, io, out);
            }
        }
    }

    fn digit_picks_option(&self, io: &Io) -> bool {
        self.cursor
            .src
            .as_ref()
            .is_some_and(|src| io.actions.digit_selects_option(src))
    }

    fn run(
        &mut self,
        action: Action,
        count: Option<u32>,
        now_ms: u64,
        env: &mut Env,
        io: &mut Io,
        out: &mut Outcome,
    ) {
        let steps = i64::from(count.unwrap_or(1).max(1));
        let nth = count.map(|count| count as usize);
        let fraction = env.layout.theme().scroll_fraction;
        match action {
            Action::NextBlock => out.redraw = self.cursor.move_by(env, steps),
            Action::PrevBlock => out.redraw = self.cursor.move_by(env, -steps),
            Action::HalfPageDown => out.redraw = self.cursor.half_page(env, steps, fraction),
            Action::HalfPageUp => out.redraw = self.cursor.half_page(env, -steps, fraction),
            Action::SectionNext => out.redraw = self.cursor.section(env, steps),
            Action::SectionPrev => out.redraw = self.cursor.section(env, -steps),
            Action::Top => out.redraw = self.cursor.to_top(env, nth),
            Action::Bottom => out.redraw = self.cursor.to_bottom(env, nth),
            Action::AlignTop => out.redraw = self.cursor.align(env, Align::Top),
            Action::AlignCenter => out.redraw = self.cursor.align(env, Align::Center),
            Action::AlignBottom => out.redraw = self.cursor.align(env, Align::Bottom),
            Action::ScrollLeft => out.redraw = self.scroll_code(env, -1.0),
            Action::ScrollRight => out.redraw = self.scroll_code(env, 1.0),
            Action::ToggleRail => out.requests.push(Request::ToggleRail),
            Action::ZoomIn => out.requests.push(Request::Zoom(ZoomStep::In)),
            Action::ZoomOut => out.requests.push(Request::Zoom(ZoomStep::Out)),
            Action::ZoomReset => out.requests.push(Request::Zoom(ZoomStep::Reset)),
            Action::Reload => out.requests.push(Request::Reload),
            Action::Quit => self.quit(now_ms, io, out),
            Action::SearchOpen => self.open_prompt(Mode::Search),
            Action::SearchNext => self.search_step(true, now_ms, env),
            Action::SearchPrev => self.search_step(false, now_ms, env),
            Action::CommandOpen => self.open_prompt(Mode::Command),
            Action::Visual => {
                self.visual_anchor = match self.visual_anchor {
                    Some(_) => None,
                    None => self.cursor.src.clone(),
                };
            }
            Action::Yank if self.visual_anchor.is_some() => self.yank_selection(now_ms, env, io),
            Action::Yank => self.operator = Some(Operator::start_with_count(now_ms, count)),
            Action::YankPage => {
                let text = yank::page_text(env.doc);
                self.copy(&text, "page", now_ms, io);
            }
            Action::Clear => out.redraw = self.clear(),
            Action::OpenReply => self.feedback(FeedbackAction::OpenReply, now_ms, io),
            Action::OpenComment => self.feedback(FeedbackAction::OpenComment, now_ms, io),
            Action::Insert => self.feedback(FeedbackAction::Edit(EditAt::Keep), now_ms, io),
            Action::InsertStart => self.feedback(FeedbackAction::Edit(EditAt::Start), now_ms, io),
            Action::InsertEnd => self.feedback(FeedbackAction::Edit(EditAt::End), now_ms, io),
            Action::DeleteComment => self.feedback(FeedbackAction::DeleteComment, now_ms, io),
            Action::Review => self.feedback(FeedbackAction::Review, now_ms, io),
            Action::FirstUnanswered => self.feedback(FeedbackAction::FirstUnanswered, now_ms, io),
            Action::PickOption(number) => {
                self.feedback(FeedbackAction::PickOption(number), now_ms, io)
            }
        }
    }

    fn feedback(&mut self, action: FeedbackAction, now_ms: u64, io: &mut Io) {
        let result = io.actions.feedback(action, self.cursor.src.as_ref());
        self.report(result, now_ms, "not available in this viewer yet");
    }

    fn report(&mut self, result: ActionResult, now_ms: u64, unsupported: &str) {
        match result {
            ActionResult::Done(message)
            | ActionResult::Nothing(message)
            | ActionResult::Failed(message) => self.flash(message, now_ms),
            ActionResult::Unsupported => self.flash(unsupported, now_ms),
        }
    }

    fn clear(&mut self) -> bool {
        let changed = self.visual_anchor.is_some()
            || self.show_search
            || self.keymap.has_pending()
            || self.operator.is_some();
        self.visual_anchor = None;
        self.show_search = false;
        self.operator = None;
        self.keymap.reset();
        changed
    }

    /// `h` and `l`: scroll the focused code block sideways, else the first
    /// wide one in view.
    fn scroll_code(&mut self, env: &mut Env, direction: f32) -> bool {
        let theme = env.layout.theme();
        let line = theme.font_size * theme.line_height * env.layout.scale();
        let delta = direction * line * 3.0;
        if let Some(stop) = self.cursor.stop(env.layout) {
            let focused_code = env
                .layout
                .blocks
                .get_mut(stop.top)
                .filter(|block| block.src == stop.src);
            if let Some(block) = focused_code {
                if block.scroll_code_x(delta) {
                    return true;
                }
                if matches!(block.content, crate::layout::Content::Code { .. }) {
                    return false;
                }
            }
        }
        let (top, bottom) = (*env.scroll, *env.scroll + env.viewport);
        env.layout
            .code_block_in(top, bottom)
            .is_some_and(|block| block.scroll_code_x(delta))
    }

    fn quit(&mut self, now_ms: u64, io: &mut Io, out: &mut Outcome) {
        if io.actions.has_unsaved() {
            self.flash(
                "unsaved changes — :wq to save and send, :q! to discard",
                now_ms,
            );
            return;
        }
        out.requests.push(Request::Close(CloseKind::Window));
    }

    fn open_prompt(&mut self, mode: Mode) {
        self.mode = mode;
        self.line.clear();
        self.keymap.reset();
    }

    fn close_prompt(&mut self) {
        self.mode = Mode::Page;
        self.line.clear();
    }

    fn line_key(
        &mut self,
        input: &KeyInput,
        now_ms: u64,
        env: &mut Env,
        io: &mut Io,
        out: &mut Outcome,
    ) {
        out.redraw = true;
        let chord = &input.chord;
        match chord.key.as_str() {
            "escape" => return self.close_prompt(),
            "c" if chord.ctrl => return self.close_prompt(),
            "enter" if !chord.ctrl => return self.submit(now_ms, env, io, out),
            "backspace" => {
                if !self.line.backspace() {
                    self.close_prompt();
                }
                return;
            }
            "u" if chord.ctrl => return self.line.clear(),
            "w" if chord.ctrl => return self.line.delete_word(),
            _ => {}
        }
        if chord.ctrl || chord.alt {
            return;
        }
        if let Some(text) = &input.text {
            self.line.insert(text);
        }
    }

    fn submit(&mut self, now_ms: u64, env: &mut Env, io: &mut Io, out: &mut Outcome) {
        let mode = self.mode;
        let text = self.line.text().to_owned();
        self.close_prompt();
        match mode {
            Mode::Search => self.search_submit(&text, now_ms, env),
            Mode::Command => self.run_command(&text, now_ms, io, out),
            Mode::Page => {}
        }
    }

    fn run_command(&mut self, line: &str, now_ms: u64, io: &mut Io, out: &mut Outcome) {
        match command::parse(line) {
            Command::Empty => {}
            Command::Quit => self.quit(now_ms, io, out),
            Command::QuitForce => out.requests.push(Request::Close(CloseKind::Window)),
            Command::QuitAll => out.requests.push(Request::Close(CloseKind::All)),
            Command::Write => {
                let result = io.actions.write();
                self.report(result, now_ms, "nothing to save");
            }
            Command::WriteQuit => {
                let result = io.actions.write_and_send();
                // With no feedback there is nothing to send, so it just closes.
                let done = matches!(result, ActionResult::Done(_) | ActionResult::Nothing(_));
                self.report(result, now_ms, "nothing to save");
                if done {
                    out.requests.push(Request::Close(CloseKind::Window));
                }
            }
            Command::Unknown(name) => self.flash(format!("Not a command: :{name}"), now_ms),
        }
    }

    fn cursor_block(&self, env: &Env) -> usize {
        let stops = collect_stops(env.layout);
        self.cursor.locate(&stops).map_or(0, |at| stops[at].top)
    }

    fn search_submit(&mut self, text: &str, now_ms: u64, env: &mut Env) {
        let query = if text.is_empty() {
            // An empty pattern repeats the last one, as in vim.
            match self.search.query.clone() {
                Some(query) => Some(query),
                None => return,
            }
        } else {
            Query::smartcase(text)
        };
        self.search.run(env.doc, query);
        self.show_search = true;
        let before = self.matches_before_cursor(env);
        match self.search.start_at(before) {
            Some((hit, wrapped)) => self.land(hit, wrapped, true, now_ms, env),
            None => {
                let pattern = self
                    .search
                    .query
                    .as_ref()
                    .map_or("", |query| query.text.as_str());
                let message = format!("Pattern not found: {pattern}");
                self.flash(message, now_ms);
            }
        }
    }

    fn search_step(&mut self, forward: bool, now_ms: u64, env: &mut Env) {
        if self.search.query.is_none() {
            self.flash("No previous search", now_ms);
            return;
        }
        self.show_search = true;
        let stepped = if self.search.position().is_some() {
            self.search.step(forward, 0)
        } else {
            let before = self.matches_before_cursor(env);
            self.search.step_from_cursor(forward, before)
        };
        match stepped {
            Some((hit, wrapped)) => self.land(hit, wrapped, forward, now_ms, env),
            None => {
                let pattern = self
                    .search
                    .query
                    .as_ref()
                    .map_or("", |query| query.text.as_str());
                let message = format!("Pattern not found: {pattern}");
                self.flash(message, now_ms);
            }
        }
    }

    /// How many matches lie before the focused stop: those in earlier
    /// top-level blocks plus those on rows above the stop inside its block.
    fn matches_before_cursor(&self, env: &mut Env) -> usize {
        let Some(query) = self.search.query.clone() else {
            return 0;
        };
        let stops = collect_stops(env.layout);
        let Some(stop) = self.cursor.locate(&stops).map(|at| &stops[at]) else {
            return 0;
        };
        let earlier = self.search.matches_before(stop.top);
        let above = search::block_match_baselines(env.layout, &query, stop.top)
            .into_iter()
            .filter(|baseline| *baseline < stop.rect.y - 0.5)
            .count();
        earlier + above
    }

    /// Show a match: centre it and put the cursor on the stop that holds it.
    /// `forward` is the direction of travel, which names the wrap flash.
    fn land(&mut self, hit: Hit, wrapped: bool, forward: bool, now_ms: u64, env: &mut Env) {
        if let Some(query) = self.search.query.clone() {
            search::centre_on(env, &query, hit);
            match search::locate_baseline(env.layout, &query, hit) {
                Some(y) => self.cursor.go_to_row(env, hit.block, y),
                None => self.cursor.go_to_block(env, hit.block),
            };
        } else {
            self.cursor.go_to_block(env, hit.block);
        }
        if wrapped {
            let message = if forward {
                "search hit BOTTOM, continuing at TOP"
            } else {
                "search hit TOP, continuing at BOTTOM"
            };
            self.flash(message, now_ms);
        }
    }

    fn yank_target(&mut self, target: Target, now_ms: u64, env: &mut Env, io: &mut Io) {
        let Some(src) = self.cursor.src.clone() else {
            return self.flash("nothing to yank", now_ms);
        };
        match target {
            Target::Section { inner } => {
                let top = self.cursor_block(env);
                let text = yank::section_text(env.doc, top, inner);
                self.copy(&text, "section", now_ms, io);
            }
            Target::Block => {
                let ranges = [src];
                let text = yank::blocks_text(&env.doc.source, &ranges);
                self.copy_blocks(&text, &ranges, env, now_ms, io);
            }
            Target::Motion(delta) => {
                let ranges = self.stops_from_cursor(env, delta);
                let text = yank::blocks_text(&env.doc.source, &ranges);
                self.copy_blocks(&text, &ranges, env, now_ms, io);
            }
            Target::ToEnd => {
                env.lay_out_all();
                let ranges = self.stops_from_cursor(env, i64::MAX / 2);
                let text = yank::blocks_text(&env.doc.source, &ranges);
                self.copy_blocks(&text, &ranges, env, now_ms, io);
            }
        }
    }

    /// The source ranges of the cursor's stop and the `delta` stops after it
    /// (or before, when negative), laying out as far as needed.
    fn stops_from_cursor(&self, env: &mut Env, delta: i64) -> Vec<Range<usize>> {
        loop {
            let stops = collect_stops(env.layout);
            let Some(at) = self.cursor.locate(&stops) else {
                return Vec::new();
            };
            let other = at as i64 + delta;
            if other >= stops.len() as i64 && env.lay_out_more() {
                continue;
            }
            let other = other.clamp(0, stops.len() as i64 - 1) as usize;
            let (low, high) = (at.min(other), at.max(other));
            return stops[low..=high]
                .iter()
                .map(|stop| stop.src.clone())
                .collect();
        }
    }

    fn yank_selection(&mut self, now_ms: u64, env: &mut Env, io: &mut Io) {
        let anchor = self.visual_anchor.take();
        let Some(anchor) = anchor else {
            return;
        };
        let stops = collect_stops(env.layout);
        let (Some(from), Some(to)) = (
            stops.iter().position(|stop| stop.src == anchor),
            self.cursor.locate(&stops),
        ) else {
            return self.flash("nothing to yank", now_ms);
        };
        let (low, high) = (from.min(to), from.max(to));
        let ranges: Vec<Range<usize>> = stops[low..=high].iter().map(|s| s.src.clone()).collect();
        let text = yank::blocks_text(&env.doc.source, &ranges);
        self.copy_blocks(&text, &ranges, env, now_ms, io);
    }

    fn copy_blocks(
        &mut self,
        text: &str,
        ranges: &[Range<usize>],
        env: &Env,
        now_ms: u64,
        io: &mut Io,
    ) {
        let code = ranges.len() == 1
            && env
                .doc
                .source
                .get(ranges[0].clone())
                .and_then(yank::fence_body)
                .is_some();
        let label = yank::describe(ranges.len(), code);
        self.copy(text, &label, now_ms, io);
    }

    fn copy(&mut self, text: &str, label: &str, now_ms: u64, io: &mut Io) {
        if text.trim().is_empty() {
            return self.flash("nothing to yank", now_ms);
        }
        match io.clipboard.copy(text) {
            Ok(()) => self.flash(format!("Yanked {label}"), now_ms),
            Err(error) => {
                let reason: String = error
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                self.flash(format!("Yank failed: {reason}"), now_ms);
            }
        }
    }
}

/// The tint of the visual selection: the focus colour, stronger.
pub fn selection_tint(theme: &Theme) -> Color {
    theme.selection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::lay_out, text::Fonts};
    use limpid_core::{
        doc::{Mode as DocMode, parse},
        settings::Settings,
    };

    struct World {
        doc: Doc,
        layout: Layout,
        fonts: Fonts,
        scroll: f32,
        viewport: f32,
        nav: Nav,
        actions: Fake,
        clipboard: Recorder,
        now: u64,
    }

    #[derive(Default)]
    struct Fake {
        unsaved: bool,
        fields: bool,
        picked: Vec<FeedbackAction>,
        write_result: Option<ActionResult>,
    }

    impl PageActions for Fake {
        fn has_unsaved(&self) -> bool {
            self.unsaved
        }
        fn digit_selects_option(&self, _stop: &Range<usize>) -> bool {
            self.fields
        }
        fn feedback(
            &mut self,
            action: FeedbackAction,
            _stop: Option<&Range<usize>>,
        ) -> ActionResult {
            self.picked.push(action);
            ActionResult::Done("ok".into())
        }
        fn write(&mut self) -> ActionResult {
            self.write_result
                .clone()
                .unwrap_or(ActionResult::Nothing("nothing to save".into()))
        }
        fn write_and_send(&mut self) -> ActionResult {
            self.write_result
                .clone()
                .unwrap_or(ActionResult::Nothing("nothing to save".into()))
        }
    }

    #[derive(Default)]
    struct Recorder {
        copied: Vec<String>,
        fail: bool,
    }

    impl Clipboard for Recorder {
        fn copy(&mut self, text: &str) -> Result<(), String> {
            if self.fail {
                return Err("wl-copy: not found\nsecond line".into());
            }
            self.copied.push(text.to_owned());
            Ok(())
        }
    }

    impl World {
        fn new(markdown: &str, viewport: f32) -> Self {
            Self::with_keys(markdown, viewport, &BTreeMap::new())
        }

        fn with_keys(markdown: &str, viewport: f32, keys: &BTreeMap<String, String>) -> Self {
            let doc = parse(markdown, DocMode::File, &Settings::default());
            let mut fonts = Fonts::new();
            let layout = lay_out(&doc, 900.0, &Theme::dark(), 1.0, &mut fonts);
            World {
                doc,
                layout,
                fonts,
                scroll: 0.0,
                viewport,
                nav: Nav::new(keys, NavOptions::default()).unwrap(),
                actions: Fake::default(),
                clipboard: Recorder::default(),
                now: 0,
            }
        }

        fn press(&mut self, key: &str) -> Outcome {
            self.now += 10;
            let input = KeyInput::named(key);
            let mut env = Env {
                doc: &self.doc,
                layout: &mut self.layout,
                fonts: &mut self.fonts,
                scroll: &mut self.scroll,
                viewport: self.viewport,
            };
            let mut io = Io {
                actions: &mut self.actions,
                clipboard: &mut self.clipboard,
            };
            self.nav.handle(&input, self.now, &mut env, &mut io)
        }

        fn keys(&mut self, keys: &[&str]) -> Outcome {
            let mut last = Outcome::default();
            for key in keys {
                last = self.press(key);
            }
            last
        }

        fn typed(&mut self, text: &str) {
            for character in text.chars() {
                let name = if character == ' ' {
                    "space".to_owned()
                } else {
                    character.to_string()
                };
                self.press(&name);
            }
        }

        fn focused(&self) -> String {
            let stop = self.nav.cursor().stop(&self.layout).unwrap();
            self.doc.source[stop.src].trim().to_owned()
        }

        fn message(&self) -> String {
            self.nav.status_view(self.now).message
        }
    }

    fn paragraphs(count: usize) -> String {
        (0..count).map(|n| format!("paragraph {n}\n\n")).collect()
    }

    #[test]
    fn should_move_the_cursor_when_pressing_j_and_k() {
        let mut world = World::new("one\n\ntwo\n\nthree\n", 600.0);
        assert!(world.press("j").redraw);
        assert_eq!(world.focused(), "two");
        world.press("down");
        assert_eq!(world.focused(), "three");
        world.press("k");
        assert_eq!(world.focused(), "two");
        world.press("up");
        assert_eq!(world.focused(), "one");
    }

    #[test]
    fn should_move_by_the_count_when_a_number_precedes_j() {
        let mut world = World::new(&paragraphs(20), 600.0);
        world.keys(&["3", "j"]);
        assert_eq!(world.focused(), "paragraph 3");
        world.keys(&["1", "0", "j"]);
        assert_eq!(world.focused(), "paragraph 13");
        world.keys(&["2", "k"]);
        assert_eq!(world.focused(), "paragraph 11");
        assert_eq!(world.nav.keymap().pending_text(), "");
    }

    #[test]
    fn should_jump_with_gg_capital_g_and_counts() {
        let mut world = World::new(&paragraphs(30), 300.0);
        world.press("shift+g");
        assert_eq!(world.focused(), "paragraph 29");
        world.keys(&["g", "g"]);
        assert_eq!(world.focused(), "paragraph 0");
        world.keys(&["7", "shift+g"]);
        assert_eq!(world.focused(), "paragraph 6");
    }

    #[test]
    fn should_not_redraw_when_a_key_is_unbound() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        let outcome = world.press("F9");
        assert!(!outcome.redraw && outcome.requests.is_empty());
        let outcome = world.press("f1");
        assert_eq!(outcome.requests, vec![Request::ToggleRail]);
    }

    #[test]
    fn should_redraw_to_show_the_pending_keys_when_a_sequence_starts() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        assert!(world.press("g").redraw);
        assert_eq!(world.nav.status_view(world.now).right, "g");
        world.press("g");
        assert_eq!(world.nav.status_view(world.now).right, "");
    }

    #[test]
    fn should_ask_the_page_for_the_things_it_owns_when_pressing_their_keys() {
        let mut world = World::new("one\n", 600.0);
        assert_eq!(world.press("r").requests, vec![Request::Reload]);
        assert_eq!(world.press("+").requests, vec![Request::Zoom(ZoomStep::In)]);
        assert_eq!(
            world.press("-").requests,
            vec![Request::Zoom(ZoomStep::Out)]
        );
        assert_eq!(
            world.press("0").requests,
            vec![Request::Zoom(ZoomStep::Reset)]
        );
        assert_eq!(world.press("t").requests, vec![Request::ToggleRail]);
    }

    #[test]
    fn should_honour_remapped_keys_when_overrides_are_given() {
        let keys = BTreeMap::from([("next_block".to_owned(), "ctrl+n".to_owned())]);
        let mut world = World::with_keys("one\n\ntwo\n", 600.0, &keys);
        world.press("j");
        assert_eq!(world.focused(), "one");
        world.press("ctrl+n");
        assert_eq!(world.focused(), "two");
    }

    #[test]
    fn should_report_the_bad_override_when_building_the_nav() {
        let keys = BTreeMap::from([("nope".to_owned(), "j".to_owned())]);
        let error = Nav::new(&keys, NavOptions::default()).err().unwrap();
        assert!(error.contains("nope"));
    }

    // Search.

    #[test]
    fn should_jump_to_the_first_match_after_the_cursor_when_submitting_a_search() {
        let mut world = World::new("alpha\n\nbeta needle\n\ngamma\n\nneedle again\n", 600.0);
        world.press("/");
        assert_eq!(world.nav.state(), StateName::Search);
        world.typed("needle");
        world.press("enter");
        assert_eq!(world.nav.state(), StateName::Page);
        assert_eq!(world.focused(), "beta needle");
        assert_eq!(world.nav.search().position(), Some((1, 2)));
        assert_eq!(world.nav.status_view(world.now).right, "[1/2] /needle");
    }

    #[test]
    fn should_step_through_matches_and_wrap_when_pressing_n_and_capital_n() {
        let mut world = World::new("a needle\n\nfiller\n\nb needle\n\nfiller\n", 600.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        assert_eq!(world.focused(), "a needle");
        world.press("n");
        assert_eq!(world.focused(), "b needle");
        world.press("n");
        assert_eq!(world.focused(), "a needle");
        assert!(world.message().contains("BOTTOM"), "{}", world.message());
        world.press("shift+n");
        assert_eq!(world.focused(), "b needle");
        assert!(world.message().contains("TOP"), "{}", world.message());
        world.press("shift+n");
        assert_eq!(world.focused(), "a needle");
    }

    const LIST: &str = "intro para\n\n- alpha\n- beta\n- gamma target\n- delta\n";

    #[test]
    fn should_focus_the_list_item_holding_the_match_when_submitting_a_search() {
        let mut world = World::new(LIST, 600.0);
        world.press("/");
        world.typed("target");
        world.press("enter");
        assert_eq!(world.focused(), "gamma target");
    }

    #[test]
    fn should_search_from_the_focused_item_when_the_cursor_is_inside_a_list() {
        let mut world = World::new(LIST, 600.0);
        world.press("/");
        world.typed("alpha");
        world.press("enter");
        assert_eq!(world.focused(), "alpha");
        for _ in 0..3 {
            world.press("j");
        }
        assert_eq!(world.focused(), "delta");
        world.press("/");
        world.typed("alpha");
        world.press("enter");
        assert_eq!(world.focused(), "alpha");
        assert!(world.message().contains("BOTTOM"), "{}", world.message());
    }

    #[test]
    fn should_report_the_bottom_wrap_when_the_only_match_is_stepped_backward_and_forward() {
        let mut world = World::new("one\n\nsole needle\n\nthree\n", 600.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        world.press("n");
        assert!(world.message().contains("BOTTOM"), "{}", world.message());
        world.press("shift+n");
        assert!(world.message().contains("TOP"), "{}", world.message());
    }

    #[test]
    fn should_centre_the_match_when_jumping_to_a_far_match() {
        let source = paragraphs(80) + "the needle is here\n";
        let mut world = World::new(&source, 300.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        assert!(world.scroll > 500.0, "{}", world.scroll);
        let query = Query::smartcase("needle").unwrap();
        let hit = world.nav.search().current_hit().unwrap();
        let y = search::locate_y(&world.layout, &query, hit).unwrap();
        assert!((y - world.scroll - 150.0).abs() < 25.0);
    }

    #[test]
    fn should_use_smartcase_when_the_pattern_has_a_capital() {
        let mut world = World::new("lower needle\n\nUpper Needle\n", 600.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        assert_eq!(world.nav.search().len(), 2);
        world.press("/");
        world.typed("Needle");
        world.press("enter");
        assert_eq!(world.nav.search().len(), 1);
        assert_eq!(world.focused(), "Upper Needle");
    }

    #[test]
    fn should_flash_when_the_pattern_is_not_found() {
        let mut world = World::new("alpha\n", 600.0);
        world.press("/");
        world.typed("zebra");
        world.press("enter");
        assert_eq!(world.message(), "Pattern not found: zebra");
        world.press("n");
        assert_eq!(world.message(), "Pattern not found: zebra");
    }

    #[test]
    fn should_flash_when_n_is_pressed_before_any_search() {
        let mut world = World::new("alpha\n", 600.0);
        world.press("n");
        assert_eq!(world.message(), "No previous search");
    }

    #[test]
    fn should_repeat_the_last_search_when_submitting_an_empty_pattern() {
        let mut world = World::new("a needle\n\nb needle\n", 600.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        world.press("/");
        world.press("enter");
        assert_eq!(world.nav.search().len(), 2);
        let mut fresh = World::new("a\n", 600.0);
        fresh.press("/");
        fresh.press("enter");
        assert_eq!(fresh.nav.state(), StateName::Page);
        assert_eq!(fresh.message(), "");
    }

    #[test]
    fn should_cancel_the_prompt_when_escape_or_backspace_on_empty_is_pressed() {
        let mut world = World::new("a\n", 600.0);
        world.press("/");
        world.typed("ab");
        world.press("escape");
        assert_eq!(world.nav.state(), StateName::Page);
        world.press("/");
        world.typed("a");
        world.press("backspace");
        assert_eq!(world.nav.state(), StateName::Search);
        world.press("backspace");
        assert_eq!(world.nav.state(), StateName::Page);
    }

    #[test]
    fn should_keep_page_keys_out_of_the_prompt_when_typing_a_pattern() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        world.press("/");
        world.typed("jjq:");
        assert_eq!(world.nav.state(), StateName::Search);
        assert_eq!(world.focused(), "one");
        world.press("ctrl+u");
        assert_eq!(world.nav.status_view(world.now).prompt.unwrap().text, "");
    }

    #[test]
    fn should_search_for_text_with_spaces_and_capitals_when_typed() {
        let mut world = World::new("hello big World\n", 600.0);
        world.press("/");
        world.typed("big World");
        world.press("enter");
        assert_eq!(world.nav.search().len(), 1);
    }

    #[test]
    fn should_hide_highlights_but_keep_the_pattern_when_pressing_escape() {
        let mut world = World::new("needle\n\nneedle\n", 600.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        assert!(world.nav.highlight().is_some());
        assert!(world.press("escape").redraw);
        assert!(world.nav.highlight().is_none());
        world.press("n");
        assert!(world.nav.highlight().is_some());
        assert!(!world.press("escape").redraw || world.nav.highlight().is_none());
    }

    #[test]
    fn should_highlight_while_typing_when_the_prompt_has_text() {
        let mut world = World::new("needle\n", 600.0);
        world.press("/");
        assert!(world.nav.highlight().is_none());
        world.typed("nee");
        let (query, hit) = world.nav.highlight().unwrap();
        assert_eq!(query.text, "nee");
        assert_eq!(hit, None);
    }

    // Commands.

    #[test]
    fn should_quit_the_window_when_running_q_with_nothing_unsaved() {
        let mut world = World::new("a\n", 600.0);
        world.press(":");
        assert_eq!(world.nav.state(), StateName::Command);
        world.typed("q");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        assert_eq!(world.nav.state(), StateName::Page);
    }

    #[test]
    fn should_refuse_q_but_not_q_bang_when_there_are_unsaved_changes() {
        let mut world = World::new("a\n", 600.0);
        world.actions.unsaved = true;
        world.press(":");
        world.typed("q");
        let outcome = world.press("enter");
        assert!(outcome.requests.is_empty());
        assert_eq!(
            world.message(),
            "unsaved changes — :wq to save and send, :q! to discard"
        );
        world.press(":");
        world.typed("q!");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
    }

    #[test]
    fn should_quit_on_q_ctrl_q_and_ctrl_w_when_nothing_is_unsaved() {
        let mut world = World::new("a\n", 600.0);
        let outcome = world.press("q");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        let outcome = world.press("ctrl+q");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        let outcome = world.press("ctrl+w");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
    }

    #[test]
    fn should_refuse_plain_q_when_there_are_unsaved_changes() {
        let mut world = World::new("a\n", 600.0);
        world.actions.unsaved = true;
        let outcome = world.press("q");
        assert!(outcome.requests.is_empty());
        assert_eq!(
            world.message(),
            "unsaved changes — :wq to save and send, :q! to discard"
        );
    }

    #[test]
    fn should_close_every_window_when_running_qa() {
        let mut world = World::new("a\n", 600.0);
        world.actions.unsaved = true;
        world.press(":");
        world.typed("qa");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::All)]);
    }

    #[test]
    fn should_say_there_is_nothing_to_save_when_no_editor_handles_w_and_close_on_wq() {
        let mut world = World::new("a\n", 600.0);
        world.press(":");
        world.typed("w");
        let outcome = world.press("enter");
        assert!(outcome.requests.is_empty());
        assert_eq!(world.message(), "nothing to save");
        world.press(":");
        world.typed("wq");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        assert_eq!(world.message(), "nothing to save");
    }

    #[test]
    fn should_close_after_wq_when_there_is_no_feedback_to_send() {
        let mut world = World::new("a\n", 600.0);
        world.actions.write_result = Some(ActionResult::Nothing("no feedback to send".into()));
        world.press(":");
        world.typed("wq");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        assert_eq!(world.message(), "no feedback to send");
    }

    #[test]
    fn should_close_after_wq_only_when_the_send_succeeded() {
        let mut world = World::new("a\n", 600.0);
        world.actions.write_result = Some(ActionResult::Done("sent".into()));
        world.press(":");
        world.typed("wq");
        let outcome = world.press("enter");
        assert_eq!(outcome.requests, vec![Request::Close(CloseKind::Window)]);
        assert_eq!(world.message(), "sent");
        world.actions.write_result = Some(ActionResult::Failed("no pane".into()));
        world.press(":");
        world.typed("wq");
        let outcome = world.press("enter");
        assert!(outcome.requests.is_empty());
        assert_eq!(world.message(), "no pane");
    }

    #[test]
    fn should_flash_the_command_when_it_is_unknown() {
        let mut world = World::new("a\n", 600.0);
        world.press(":");
        world.typed("frob");
        world.press("enter");
        assert_eq!(world.message(), "Not a command: :frob");
        world.press(":");
        world.press("enter");
        assert_eq!(world.nav.state(), StateName::Page);
    }

    #[test]
    fn should_show_the_command_placeholder_only_while_the_line_is_empty() {
        let mut world = World::new("a\n", 600.0);
        world.press(":");
        let prompt = world.nav.status_view(world.now).prompt.unwrap();
        assert_eq!(prompt.sigil, ':');
        assert!(!prompt.placeholder.is_empty());
        assert_eq!(prompt.text, "");
    }

    #[test]
    fn should_expire_flashes_after_the_configured_time() {
        let mut world = World::new("a\n", 600.0);
        world.press(":");
        world.typed("frob");
        world.press("enter");
        let shown = world.now;
        assert!(!world.nav.status_view(shown).message.is_empty());
        assert!(
            world
                .nav
                .status_view(shown + NavOptions::default().flash_ms + 1)
                .message
                .is_empty()
        );
    }

    // Feedback routing.

    #[test]
    fn should_hand_feedback_keys_to_the_page_actions_when_pressed() {
        let mut world = World::new("a\n", 600.0);
        world.keys(&["c", "x", "enter", "shift+s", "shift+q"]);
        assert_eq!(
            world.actions.picked,
            vec![
                FeedbackAction::OpenComment,
                FeedbackAction::DeleteComment,
                FeedbackAction::OpenReply,
                FeedbackAction::Review,
                FeedbackAction::FirstUnanswered,
            ]
        );
    }

    #[test]
    fn should_hand_the_insert_keys_to_the_page_actions_when_pressed() {
        let mut world = World::new("a\n", 600.0);
        world.keys(&["i", "shift+i", "shift+a"]);
        assert_eq!(
            world.actions.picked,
            vec![
                FeedbackAction::Edit(EditAt::Keep),
                FeedbackAction::Edit(EditAt::Start),
                FeedbackAction::Edit(EditAt::End),
            ]
        );
    }

    #[test]
    fn should_say_so_when_a_feedback_key_has_no_editor() {
        let mut world = World::new("a\n", 600.0);
        world.nav = Nav::new(&BTreeMap::new(), NavOptions::default()).unwrap();
        let mut none = NoActions;
        let input = KeyInput::named("c");
        let mut env = Env {
            doc: &world.doc,
            layout: &mut world.layout,
            fonts: &mut world.fonts,
            scroll: &mut world.scroll,
            viewport: 600.0,
        };
        let mut io = Io {
            actions: &mut none,
            clipboard: &mut world.clipboard,
        };
        world.nav.handle(&input, 5, &mut env, &mut io);
        assert!(world.nav.status_view(5).message.contains("not available"));
    }

    #[test]
    fn should_count_digits_unless_the_focused_stop_is_a_question() {
        let mut world = World::new(&paragraphs(10), 600.0);
        world.keys(&["3", "j"]);
        assert_eq!(world.focused(), "paragraph 3");
        world.actions.fields = true;
        world.press("2");
        assert_eq!(world.actions.picked, vec![FeedbackAction::PickOption(2)]);
        world.press("j");
        assert_eq!(world.focused(), "paragraph 4", "no count was started");
    }

    // Yank.

    #[test]
    fn should_yank_the_focused_block_when_pressing_yy() {
        let mut world = World::new("first\n\nsecond *x*\n", 600.0);
        world.press("j");
        let outcome = world.keys(&["y", "y"]);
        assert!(outcome.redraw);
        assert_eq!(world.clipboard.copied, vec!["second *x*\n"]);
        assert_eq!(world.message(), "Yanked 1 block");
    }

    #[test]
    fn should_yank_the_code_only_when_the_block_is_a_fence() {
        let mut world = World::new("```rs\nlet a = 1;\n```\n", 600.0);
        world.keys(&["y", "y"]);
        assert_eq!(world.clipboard.copied, vec!["let a = 1;\n"]);
        assert_eq!(world.message(), "Yanked code");
    }

    #[test]
    fn should_yank_a_run_of_blocks_when_giving_yj_a_count() {
        let mut world = World::new("one\n\ntwo\n\nthree\n\nfour\n", 600.0);
        world.keys(&["y", "2", "j"]);
        assert_eq!(world.clipboard.copied, vec!["one\n\ntwo\n\nthree\n"]);
        assert_eq!(world.message(), "Yanked 3 blocks");
        world.keys(&["shift+g", "y", "k"]);
        assert_eq!(world.clipboard.copied[1], "three\n\nfour\n");
        world.keys(&["g", "g", "j", "y", "shift+g"]);
        assert_eq!(world.clipboard.copied[2], "two\n\nthree\n\nfour\n");
    }

    #[test]
    fn should_yank_count_plus_one_blocks_when_a_count_comes_before_yk() {
        let mut world = World::new("one\n\ntwo\n\nthree\n\nfour\n", 600.0);
        world.keys(&["shift+g", "3", "y", "k"]);
        assert_eq!(
            world.clipboard.copied,
            vec!["one\n\ntwo\n\nthree\n\nfour\n"]
        );
        assert_eq!(world.message(), "Yanked 4 blocks");
    }

    #[test]
    fn should_yank_count_blocks_when_a_count_comes_before_yy() {
        let mut world = World::new("one\n\ntwo\n\nthree\n\nfour\n", 600.0);
        world.keys(&["3", "y", "y"]);
        assert_eq!(world.clipboard.copied, vec!["one\n\ntwo\n\nthree\n"]);
        world.keys(&["y", "3", "y"]);
        assert_eq!(world.clipboard.copied[1], "one\n\ntwo\n\nthree\n");
    }

    #[test]
    fn should_yank_the_section_when_pressing_yis_and_yas() {
        let mut world = World::new("# A\n\nalpha\n\n# B\n\nbeta\n", 600.0);
        world.keys(&["y", "a", "s"]);
        assert_eq!(world.clipboard.copied, vec!["# A\n\nalpha\n"]);
        world.keys(&["y", "i", "s"]);
        assert_eq!(world.clipboard.copied[1], "alpha\n");
        assert_eq!(world.message(), "Yanked section");
    }

    #[test]
    fn should_yank_the_whole_page_when_pressing_capital_y() {
        let mut world = World::new("# T\n\nbody\n", 600.0);
        world.press("shift+y");
        assert_eq!(world.clipboard.copied, vec!["# T\n\nbody\n"]);
        assert_eq!(world.message(), "Yanked page");
    }

    #[test]
    fn should_cancel_the_operator_when_the_motion_is_unknown() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        world.keys(&["y", "q"]);
        assert!(world.clipboard.copied.is_empty());
        world.press("j");
        assert_eq!(world.focused(), "two", "keys work normally again");
    }

    #[test]
    fn should_drop_the_operator_when_the_motion_comes_too_late() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        world.press("y");
        world.now += yank::OPERATOR_TIMEOUT_MS + 100;
        world.press("j");
        assert_eq!(world.focused(), "two", "j moved instead of yanking");
        assert!(world.clipboard.copied.is_empty());
    }

    #[test]
    fn should_select_and_yank_a_range_when_using_visual_mode() {
        let mut world = World::new("one\n\ntwo\n\nthree\n\nfour\n", 600.0);
        world.press("j");
        world.press("v");
        assert_eq!(world.nav.state(), StateName::Visual);
        world.keys(&["j", "j"]);
        let bands = world.nav.bands(&world.layout);
        let focus = bands.focus.unwrap();
        let selection = bands.selection.unwrap();
        assert!(selection.h > focus.h * 2.0);
        world.press("y");
        assert_eq!(world.clipboard.copied, vec!["two\n\nthree\n\nfour\n"]);
        assert_eq!(world.nav.state(), StateName::Page);
        assert_eq!(world.message(), "Yanked 3 blocks");
    }

    #[test]
    fn should_extend_the_selection_by_the_count_when_a_count_precedes_j_or_k() {
        let mut world = World::new("one\n\ntwo\n\nthree\n\nfour\n\nfive\n", 600.0);
        world.keys(&["v", "3", "j", "y"]);
        assert_eq!(
            world.clipboard.copied,
            vec!["one\n\ntwo\n\nthree\n\nfour\n"]
        );
        world.keys(&["shift+g", "v", "2", "k", "y"]);
        assert_eq!(world.clipboard.copied[1], "three\n\nfour\n\nfive\n");
        assert_eq!(world.message(), "Yanked 3 blocks");
    }

    #[test]
    fn should_yank_the_code_and_exit_when_visual_covers_one_code_block() {
        let mut world = World::new("```rs\nlet a = 1;\n```\n", 600.0);
        world.keys(&["v", "y"]);
        assert_eq!(world.clipboard.copied, vec!["let a = 1;\n"]);
        assert_eq!(world.message(), "Yanked code");
        assert_eq!(world.nav.state(), StateName::Page);
    }

    #[test]
    fn should_select_upwards_the_same_way_when_moving_before_the_anchor() {
        let mut world = World::new("one\n\ntwo\n\nthree\n", 600.0);
        world.keys(&["shift+g", "v", "k", "y"]);
        assert_eq!(world.clipboard.copied, vec!["two\n\nthree\n"]);
    }

    #[test]
    fn should_leave_visual_mode_when_pressing_escape_or_v_again() {
        let mut world = World::new("one\n\ntwo\n", 600.0);
        world.keys(&["v", "j"]);
        world.press("escape");
        assert_eq!(world.nav.state(), StateName::Page);
        assert!(world.nav.bands(&world.layout).selection.is_none());
        world.press("v");
        world.press("v");
        assert_eq!(world.nav.state(), StateName::Page);
        assert!(world.clipboard.copied.is_empty());
    }

    #[test]
    fn should_flash_the_failure_when_the_clipboard_command_fails() {
        let mut world = World::new("one\n", 600.0);
        world.clipboard.fail = true;
        world.keys(&["y", "y"]);
        assert_eq!(world.message(), "Yank failed: wl-copy: not found");
    }

    #[test]
    fn should_say_there_is_nothing_to_yank_when_the_document_is_empty() {
        let mut world = World::new("", 600.0);
        world.press("shift+y");
        assert_eq!(world.message(), "nothing to yank");
        world.keys(&["y", "y"]);
        assert_eq!(world.message(), "nothing to yank");
        assert!(world.clipboard.copied.is_empty());
    }

    // Code scrolling.

    #[test]
    fn should_scroll_the_focused_code_block_when_pressing_l_and_h() {
        let wide = "w".repeat(500);
        let mut world = World::new(&format!("```\n{wide}\n```\n"), 400.0);
        assert!(!world.press("h").redraw, "already at the left edge");
        assert!(world.press("l").redraw);
        assert!(world.press("h").redraw);
        for _ in 0..100 {
            world.press("l");
        }
        assert!(!world.press("l").redraw, "at the right edge");
    }

    #[test]
    fn should_not_scroll_anything_when_there_is_no_code_to_scroll() {
        let mut world = World::new("just text\n", 400.0);
        assert!(!world.press("l").redraw);
    }

    #[test]
    fn should_paint_nothing_for_bands_when_there_are_no_stops() {
        let mut world = World::new("# heading only\n", 400.0);
        world.press("j");
        assert_eq!(world.nav.bands(&world.layout), Bands::default());
    }

    #[test]
    fn should_report_the_state_in_the_status_view() {
        let mut world = World::new("a\n\nb\n", 400.0);
        assert_eq!(world.nav.status_view(0).state, StateName::Page);
        world.press("v");
        assert_eq!(world.nav.status_view(0).state, StateName::Visual);
        world.press("escape");
        world.press(":");
        assert_eq!(world.nav.status_view(0).state, StateName::Command);
        assert!(world.nav.in_prompt());
    }

    #[test]
    fn should_redo_the_search_when_the_document_changes() {
        let mut world = World::new("needle\n", 400.0);
        world.press("/");
        world.typed("needle");
        world.press("enter");
        assert_eq!(world.nav.search().len(), 1);
        let replaced = parse("needle\n\nneedle\n", DocMode::File, &Settings::default());
        world.nav.document_changed(&replaced);
        assert_eq!(world.nav.search().len(), 2);
        assert_eq!(world.nav.search().position(), None);
    }
}
