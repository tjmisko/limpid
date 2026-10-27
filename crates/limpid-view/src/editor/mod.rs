//! An embedded nvim: spawn `nvim --embed`, attach a UI, mirror its grid,
//! forward keys, and observe saves and quits.
//!
//! The module is self-contained. The page decides when to open a box, calls
//! [`Editor::input`] with the keys of [`keys::key_to_nvim`], polls for
//! [`EditorEvent`]s every frame and paints [`Editor::grid`] with
//! [`paint::paint_grid`].

pub mod grid;
pub mod keys;
pub mod paint;
pub mod rpc;

pub use grid::{Cell, CursorShape, Grid, HlAttr};
pub use keys::key_to_nvim;
pub use paint::paint_grid;

use rmpv::Value;
use rpc::{Incoming, Rpc, RpcError};
use std::{
    io,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// limpid's init, executed inside nvim after the user's own configuration.
const INIT_LUA: &str = include_str!("init.lua");

/// How long `Drop` and `shutdown` give nvim to exit before killing it.
const EXIT_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    /// The user's configuration, then limpid's init.
    User,
    /// `--clean` plus limpid's init only.
    Clean,
}

impl From<limpid_core::settings::EditorInit> for InitMode {
    fn from(init: limpid_core::settings::EditorInit) -> Self {
        match init {
            limpid_core::settings::EditorInit::User => InitMode::User,
            limpid_core::settings::EditorInit::Clean => InitMode::Clean,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorConfig {
    /// nvim binary; a name on `PATH` or a path.
    pub command: String,
    pub init: InitMode,
    pub cols: u16,
    pub rows: u16,
    /// Extra environment for the nvim child (tests point it at a scratch
    /// `XDG_CONFIG_HOME`); empty in normal use.
    pub env: Vec<(String, String)>,
}

impl EditorConfig {
    pub fn from_settings(settings: &limpid_core::settings::Editor, cols: u16, rows: u16) -> Self {
        EditorConfig {
            command: settings.command.clone(),
            init: settings.init.into(),
            cols,
            rows,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    Reply,
    Comment,
}

impl BoxKind {
    fn as_str(self) -> &'static str {
        match self {
            BoxKind::Reply => "reply",
            BoxKind::Comment => "comment",
        }
    }
}

#[derive(Debug)]
pub enum EditorError {
    /// The binary could not be started (`io::ErrorKind::NotFound` when nvim
    /// is not installed).
    Spawn(io::Error),
    Rpc(RpcError),
    /// nvim answered with something unexpected.
    Protocol(String),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditorError::Spawn(error) => write!(f, "could not start the editor: {error}"),
            EditorError::Rpc(error) => write!(f, "editor connection: {error}"),
            EditorError::Protocol(message) => write!(f, "editor protocol: {message}"),
        }
    }
}

impl std::error::Error for EditorError {}

impl From<RpcError> for EditorError {
    fn from(error: RpcError) -> Self {
        EditorError::Rpc(error)
    }
}

/// What a refused `:q` on a changed box says in place of nvim's E37.
const UNSAVED_CHANGES: &str = "unsaved changes: :wq to save, :q! to discard";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// The grid changed and is consistent; repaint.
    Redraw,
    Mode(String),
    /// `:w` ran: the whole buffer text.
    Save(String),
    /// The box is closed (`:q`, `:wq`, `:q!`). `discard` is true when the
    /// buffer still held unsaved changes.
    Quit {
        discard: bool,
    },
    /// A message nvim wanted to show (limpid draws it in its own style).
    Message(String),
    /// The `:` line, with its leading character, as it is being typed.
    CmdlineShow(String),
    CmdlineHide,
    Error(String),
}

/// A running nvim and the connection to it.
struct Process {
    child: Child,
    rpc: Rpc,
}

impl Process {
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = self.rpc.send("nvim_command", vec![Value::from("qa!")]);
            let deadline = Instant::now() + EXIT_GRACE;
            while self.is_running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if self.is_running() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

pub struct Editor {
    config: EditorConfig,
    process: Option<Process>,
    grid: Grid,
    /// A `Quit` was reported for the current process, so its exit is
    /// expected.
    quit_reported: bool,
}

impl Editor {
    /// Start nvim and attach. Fails when the binary is missing so the caller
    /// can fall back to a plain box.
    pub fn spawn(config: &EditorConfig) -> Result<Editor, EditorError> {
        let process = start_process(config)?;
        Ok(Editor {
            config: config.clone(),
            process: Some(process),
            grid: Grid::new(config.cols, config.rows),
            quit_reported: false,
        })
    }

    /// Put `text` in a fresh `acwrite` buffer. Empty text starts in insert
    /// mode. Restarts nvim first if the previous box ended it.
    pub fn open(&mut self, text: &str, kind: BoxKind) -> Result<(), EditorError> {
        self.ensure_running()?;
        let Some(process) = self.process.as_mut() else {
            return Err(EditorError::Rpc(RpcError::Closed));
        };
        self.quit_reported = false;
        process.rpc.request(
            "nvim_exec_lua",
            vec![
                Value::from("return _G.limpid.open(...)"),
                Value::Array(vec![Value::from(text), Value::from(kind.as_str())]),
            ],
        )?;
        Ok(())
    }

    /// Send keys in nvim notation (`<Esc>`, `<C-x>`, `<lt>`). Dropped when
    /// no editor process is running.
    pub fn input(&mut self, keys: &str) {
        if keys.is_empty() {
            return;
        }
        let Some(process) = self.process.as_mut() else {
            return;
        };
        let _ = process.rpc.send("nvim_input", vec![Value::from(keys)]);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.config.cols = cols;
        self.config.rows = rows;
        let Some(process) = self.process.as_mut() else {
            return;
        };
        let _ = process.rpc.send(
            "nvim_ui_try_resize",
            vec![Value::from(cols), Value::from(rows)],
        );
    }

    /// Everything nvim has reported since the last call; never blocks.
    pub fn poll(&mut self) -> Vec<EditorEvent> {
        let Some(process) = self.process.as_mut() else {
            return Vec::new();
        };
        let incoming = process.rpc.drain();
        let mut events = Vec::new();
        for item in incoming {
            match item {
                Incoming::Notification { method, params } => {
                    self.handle_notification(&method, &params, &mut events);
                }
                Incoming::Closed => self.handle_closed(&mut events),
            }
        }
        events
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Whether an nvim process is currently alive.
    pub fn is_running(&mut self) -> bool {
        self.process.as_mut().is_some_and(Process::is_running)
    }

    /// Ask nvim to exit, then make sure it did.
    pub fn shutdown(self) {
        drop(self);
    }

    fn ensure_running(&mut self) -> Result<(), EditorError> {
        if self.is_running() {
            return Ok(());
        }
        self.process = None;
        self.process = Some(start_process(&self.config)?);
        self.grid = Grid::new(self.config.cols, self.config.rows);
        Ok(())
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: &[Value],
        events: &mut Vec<EditorEvent>,
    ) {
        match method {
            "redraw" => self.handle_redraw(params, events),
            "limpid" => self.handle_limpid(params, events),
            _ => {}
        }
    }

    fn handle_redraw(&mut self, params: &[Value], events: &mut Vec<EditorEvent>) {
        let effects = self.grid.apply_redraw(params);
        if let Some(mode) = effects.mode {
            events.push(EditorEvent::Mode(mode));
        }
        match effects.cmdline {
            Some(Some(text)) => events.push(EditorEvent::CmdlineShow(text)),
            Some(None) => events.push(EditorEvent::CmdlineHide),
            None => {}
        }
        for (kind, text) in effects.messages {
            if text.starts_with("E37") || text.starts_with("E162") {
                events.push(EditorEvent::Message(UNSAVED_CHANGES.to_string()));
                continue;
            }
            let is_error = matches!(
                kind.as_str(),
                "emsg" | "echoerr" | "lua_error" | "rpc_error"
            );
            events.push(if is_error {
                EditorEvent::Error(text)
            } else {
                EditorEvent::Message(text)
            });
        }
        if effects.flushed {
            events.push(EditorEvent::Redraw);
        }
    }

    fn handle_limpid(&mut self, params: &[Value], events: &mut Vec<EditorEvent>) {
        let Some(kind) = params.first().and_then(Value::as_str) else {
            return;
        };
        match (kind, params.get(1)) {
            ("save", Some(Value::String(text))) => {
                events.push(EditorEvent::Save(
                    text.as_str().unwrap_or_default().to_string(),
                ));
            }
            ("quit", discard) => {
                let discard = discard.and_then(Value::as_bool).unwrap_or(false);
                self.quit_reported = true;
                events.push(EditorEvent::Quit { discard });
            }
            _ => {}
        }
    }

    fn handle_closed(&mut self, events: &mut Vec<EditorEvent>) {
        self.process = None;
        if self.quit_reported {
            return;
        }
        events.push(EditorEvent::Error("the editor process exited".into()));
    }
}

fn start_process(config: &EditorConfig) -> Result<Process, EditorError> {
    let mut command = Command::new(&config.command);
    command.arg("--embed").arg("-n");
    command.envs(config.env.iter().map(|(k, v)| (k, v)));
    if config.init == InitMode::Clean {
        command.arg("--clean");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(EditorError::Spawn)?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(EditorError::Protocol("the editor has no stdio".into()));
    };
    let mut process = Process {
        child,
        rpc: Rpc::new(stdout, stdin),
    };
    attach(&mut process.rpc, config)?;
    Ok(process)
}

/// Handshake: learn the channel id, attach the UI, run limpid's init.
fn attach(rpc: &mut Rpc, config: &EditorConfig) -> Result<(), EditorError> {
    let info = rpc.request("nvim_get_api_info", vec![])?;
    let channel = info
        .as_array()
        .and_then(|parts| parts.first())
        .and_then(Value::as_u64)
        .ok_or_else(|| EditorError::Protocol("nvim_get_api_info returned no channel".into()))?;
    let options = Value::Map(
        [
            "ext_linegrid",
            "ext_cmdline",
            "ext_messages",
            "ext_hlstate",
            "rgb",
        ]
        .into_iter()
        .map(|name| (Value::from(name), Value::from(true)))
        .collect(),
    );
    rpc.request(
        "nvim_ui_attach",
        vec![
            Value::from(config.cols.max(1)),
            Value::from(config.rows.max(1)),
            options,
        ],
    )?;
    rpc.request(
        "nvim_exec_lua",
        vec![
            Value::from(INIT_LUA),
            Value::Array(vec![Value::from(channel)]),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvim_available() -> bool {
        Command::new("nvim")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn clean_config() -> EditorConfig {
        EditorConfig {
            command: "nvim".into(),
            init: InitMode::Clean,
            cols: 40,
            rows: 8,
            env: Vec::new(),
        }
    }

    /// Poll until `found` accepts the events seen so far, or time runs out.
    fn collect_until(
        editor: &mut Editor,
        found: impl Fn(&Editor, &[EditorEvent]) -> bool,
    ) -> Vec<EditorEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(editor.poll());
            if found(editor, &seen) {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out; saw {seen:?}");
    }

    fn started() -> Option<Editor> {
        if !nvim_available() {
            eprintln!("skipping: nvim is not installed");
            return None;
        }
        Some(Editor::spawn(&clean_config()).expect("spawn"))
    }

    #[test]
    fn should_report_save_then_quit_when_text_is_edited_and_written() {
        let Some(mut editor) = started() else { return };
        editor.open("hello", BoxKind::Reply).unwrap();
        editor.input("A world<Esc>:w<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
        });
        let save = events
            .iter()
            .position(|e| *e == EditorEvent::Save("hello world".into()));
        let quit = events
            .iter()
            .position(|e| *e == EditorEvent::Quit { discard: false });
        assert!(save.is_some() && quit.is_some() && save < quit);
    }

    #[test]
    fn should_start_in_insert_mode_when_the_text_is_empty() {
        let Some(mut editor) = started() else { return };
        editor.open("", BoxKind::Comment).unwrap();
        editor.input("typed<Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("typed".into()))
        });
    }

    #[test]
    fn should_keep_the_mode_normal_when_the_text_is_not_empty() {
        let Some(mut editor) = started() else { return };
        editor.open("abc", BoxKind::Reply).unwrap();
        editor.input("x:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("ab".into()))
        });
    }

    #[test]
    fn should_roundtrip_multiple_lines_and_less_than_when_saving() {
        let Some(mut editor) = started() else { return };
        editor.open("one\ntwo <b>", BoxKind::Reply).unwrap();
        editor.input("Gotail<lt><Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("one\ntwo <b>\ntail<".into()))
        });
    }

    #[test]
    fn should_report_discard_when_quitting_with_unsaved_changes() {
        let Some(mut editor) = started() else { return };
        editor.open("keep", BoxKind::Reply).unwrap();
        editor.input("Ax<Esc>:q!<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
        });
        assert!(events.contains(&EditorEvent::Quit { discard: true }));
        assert!(!events.iter().any(|e| matches!(e, EditorEvent::Save(_))));
    }

    #[test]
    fn should_save_and_quit_when_control_enter_is_sent() {
        let Some(mut editor) = started() else { return };
        editor.open("", BoxKind::Reply).unwrap();
        editor.input("done<C-CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
        });
        let save = events
            .iter()
            .position(|e| *e == EditorEvent::Save("done".into()));
        let quit = events
            .iter()
            .position(|e| *e == EditorEvent::Quit { discard: false });
        assert!(
            save.is_some() && quit.is_some() && save < quit,
            "{events:?}"
        );
    }

    #[test]
    fn should_refuse_to_quit_silently_when_the_buffer_is_modified() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input("Ay<Esc>:q<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Message(_)))
        });
        assert!(!events.iter().any(|e| matches!(e, EditorEvent::Quit { .. })));
        assert!(editor.is_running());
    }

    #[test]
    fn should_explain_the_refusal_in_the_box_when_quitting_a_changed_box() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input("Ay<Esc>:q<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Message(_)))
        });
        assert!(events.contains(&EditorEvent::Message(UNSAVED_CHANGES.to_string())));
        assert!(!events.iter().any(|e| matches!(e, EditorEvent::Error(_))));
    }

    #[test]
    fn should_save_and_close_when_w_runs_on_a_changed_box() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input("Ay<Esc>:w<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
        });
        assert!(events.contains(&EditorEvent::Save("xy".into())));
        assert!(events.contains(&EditorEvent::Quit { discard: false }));
    }

    #[test]
    fn should_explain_the_refusal_when_qa_runs_on_a_changed_box() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input("Ay<Esc>:qa<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Message(_)))
        });
        assert!(events.contains(&EditorEvent::Message(UNSAVED_CHANGES.to_string())));
        assert!(!events.iter().any(|e| matches!(e, EditorEvent::Error(_))));
    }

    #[test]
    fn should_save_and_close_when_wq_or_x_runs_on_a_changed_box() {
        for command in [":wq<CR>", ":x<CR>"] {
            let Some(mut editor) = started() else { return };
            editor.open("x", BoxKind::Reply).unwrap();
            editor.input(&format!("Ay<Esc>{command}"));
            let events = collect_until(&mut editor, |_, seen| {
                seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
            });
            assert!(
                events.contains(&EditorEvent::Save("xy".into())),
                "{command}"
            );
            assert!(events.contains(&EditorEvent::Quit { discard: false }));
        }
    }

    #[test]
    fn should_mirror_the_buffer_text_and_cursor_shape_when_redraws_arrive() {
        let Some(mut editor) = started() else { return };
        editor.open("", BoxKind::Reply).unwrap();
        editor.input("grid text");
        collect_until(&mut editor, |editor, _| {
            editor_row_contains(editor, "grid text")
        });
        assert_eq!(editor.grid().cursor_shape, CursorShape::Bar);
        editor.input("<Esc>");
        collect_until(&mut editor, |editor, _| {
            editor.grid().cursor_shape == CursorShape::Block
        });
    }

    fn editor_row_contains(editor: &Editor, needle: &str) -> bool {
        let grid = editor.grid();
        (0..grid.rows).any(|row| grid.row_text(row).contains(needle))
    }

    #[test]
    fn should_resize_the_grid_when_resize_is_called() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.resize(60, 12);
        collect_until(&mut editor, |editor, _| {
            (editor.grid().cols, editor.grid().rows) == (60, 12)
        });
    }

    #[test]
    fn should_show_the_command_line_when_a_colon_is_typed() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input(":set");
        collect_until(&mut editor, |_, seen| {
            seen.iter()
                .any(|e| matches!(e, EditorEvent::CmdlineShow(text) if text == ":set"))
        });
        editor.input("<Esc>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::CmdlineHide)
        });
    }

    #[test]
    fn should_keep_the_same_process_when_opening_after_a_quit() {
        let Some(mut editor) = started() else { return };
        editor.open("first", BoxKind::Reply).unwrap();
        let pid = editor.process.as_ref().unwrap().child.id();
        editor.input(":q<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Quit { discard: false })
        });
        assert!(editor.is_running(), "quitting a box must not end nvim");
        editor.open("second", BoxKind::Comment).unwrap();
        assert_eq!(editor.process.as_ref().unwrap().child.id(), pid);
        editor.input("A!<Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("second!".into()))
        });
    }

    #[test]
    fn should_report_discard_and_stay_alive_when_quitting_with_bang() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input("Ay<Esc>:q!<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Quit { discard: true })
        });
        assert!(editor.is_running());
    }

    #[test]
    fn should_report_an_error_when_the_editor_process_dies() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.process.as_mut().unwrap().child.kill().unwrap();
        collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Error(_)))
        });
        assert!(!editor.is_running());
        editor.input("ignored");
        assert!(editor.poll().is_empty());
    }

    #[test]
    fn should_fail_with_a_spawn_error_when_the_binary_is_missing() {
        let config = EditorConfig {
            command: "/nonexistent/limpid-no-such-nvim".into(),
            ..clean_config()
        };
        match Editor::spawn(&config) {
            Err(EditorError::Spawn(error)) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
            Err(other) => panic!("unexpected error {other}"),
            Ok(_) => panic!("spawned a nonexistent binary"),
        }
    }

    #[test]
    fn should_ignore_empty_input_and_input_before_open() {
        let Some(mut editor) = started() else { return };
        editor.input("");
        editor.input("<Esc>");
        editor.open("still fine", BoxKind::Reply).unwrap();
        assert!(editor.is_running());
    }

    #[test]
    fn should_stop_the_process_when_shut_down() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        let pid = editor.process.as_ref().unwrap().child.id();
        editor.shutdown();
        let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
        assert!(
            !alive || !cfg!(target_os = "linux"),
            "nvim {pid} still alive"
        );
    }

    #[test]
    fn should_map_settings_into_a_config_when_built_from_settings() {
        let settings = limpid_core::settings::Editor {
            command: "/opt/nvim".into(),
            init: limpid_core::settings::EditorInit::Clean,
            idle_seconds: 1,
        };
        let config = EditorConfig::from_settings(&settings, 30, 5);
        assert_eq!(config.command, "/opt/nvim");
        assert_eq!(config.init, InitMode::Clean);
        assert_eq!((config.cols, config.rows), (30, 5));
    }

    #[test]
    fn should_report_quit_when_the_whole_editor_is_quit_with_qa() {
        let Some(mut editor) = started() else { return };
        editor.open("x", BoxKind::Reply).unwrap();
        editor.input(":qa<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Quit { .. }))
        });
        assert!(events.contains(&EditorEvent::Quit { discard: false }));
        assert!(
            !events.iter().any(|e| matches!(e, EditorEvent::Error(_))),
            "a reported quit must suppress the exit error: {events:?}"
        );
    }

    #[test]
    fn should_still_save_the_box_when_focus_tries_to_leave_with_control_w() {
        let Some(mut editor) = started() else { return };
        editor.open("box", BoxKind::Reply).unwrap();
        editor.input("<C-w>w");
        editor.input("A!<Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("box!".into()))
        });
    }

    #[test]
    fn should_still_save_the_box_when_enew_replaces_its_buffer() {
        let Some(mut editor) = started() else { return };
        editor.open("box", BoxKind::Reply).unwrap();
        editor.input(":enew<CR>");
        editor.input("A?<Esc>:w<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Save(_)))
        });
        assert!(
            events.contains(&EditorEvent::Save("box?".into())),
            "{events:?}"
        );
    }

    #[test]
    fn should_keep_the_box_buffer_when_enew_and_edit_are_refused() {
        let Some(mut editor) = started() else { return };
        editor.open("box", BoxKind::Reply).unwrap();
        editor.input(":enew<CR>");
        editor.input(":edit /nonexistent/limpid-other<CR>");
        editor.input("A?<Esc>:w<CR>");
        let events = collect_until(&mut editor, |_, seen| {
            seen.iter().any(|e| matches!(e, EditorEvent::Save(_)))
        });
        assert!(
            events.contains(&EditorEvent::Save("box?".into())),
            "{events:?}"
        );
    }

    #[test]
    fn should_never_write_a_file_when_w_names_a_path() {
        let Some(mut editor) = started() else { return };
        editor.open("box", BoxKind::Reply).unwrap();
        let target = std::env::temp_dir().join(format!("limpid-lock-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&target);
        editor.input(&format!(":w {}<CR>", target.display()));
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("box".into()))
        });
        assert!(!target.exists(), "a file was written outside the box");
    }

    #[test]
    fn should_never_write_a_file_when_a_range_write_or_append_names_a_path() {
        let Some(mut editor) = started() else { return };
        editor.open("box", BoxKind::Reply).unwrap();
        let id = std::process::id();
        let part = std::env::temp_dir().join(format!("limpid-range-{id}.txt"));
        let appended = std::env::temp_dir().join(format!("limpid-append-{id}.txt"));
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&appended);
        editor.input(&format!(":1w {}<CR>", part.display()));
        editor.input(&format!(":1w >> {}<CR>", appended.display()));
        editor.input(":w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("box".into()))
        });
        assert!(!part.exists(), "a range write created a file");
        assert!(!appended.exists(), "a range append created a file");
    }

    #[test]
    fn should_type_committed_ime_text_literally_when_sent_through_nvim_input() {
        let Some(mut editor) = started() else { return };
        editor.open("", BoxKind::Comment).unwrap();
        editor.input(&crate::feedback::ime_keys("a<Esc>\u{3042}"));
        editor.input("<Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("a<Esc>\u{3042}".into()))
        });
    }

    fn user_mode_with_border(name: &str) -> Option<Editor> {
        if !nvim_available() {
            eprintln!("skipping: nvim is not installed");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("limpid-editor-{name}-{}", std::process::id()));
        let nvim_dir = dir.join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(nvim_dir.join("init.lua"), "vim.opt.winborder = 'rounded'\n").unwrap();
        let mut config = clean_config();
        config.init = InitMode::User;
        config.cols = 20;
        config.rows = 6;
        config.env = vec![
            ("XDG_CONFIG_HOME".into(), dir.to_string_lossy().into_owned()),
            ("XDG_DATA_HOME".into(), dir.to_string_lossy().into_owned()),
            ("XDG_STATE_HOME".into(), dir.to_string_lossy().into_owned()),
        ];
        Some(Editor::spawn(&config).expect("spawn"))
    }

    #[test]
    fn should_draw_no_border_when_the_user_config_sets_winborder() {
        let Some(mut editor) = user_mode_with_border("border") else {
            return;
        };
        editor.open("hello", BoxKind::Reply).unwrap();
        collect_until(&mut editor, |editor, _| {
            editor.grid().row_text(0).starts_with("hello")
        });
        let top = editor.grid().row_text(0);
        assert!(!top.contains('\u{256d}'), "border leaked: {top:?}");
        assert!(!editor.grid().row_text(1).starts_with('\u{2502}'));
    }

    #[test]
    fn should_stay_in_normal_mode_when_opening_text_in_user_mode() {
        let Some(mut editor) = user_mode_with_border("mode") else {
            return;
        };
        editor.open("hello", BoxKind::Reply).unwrap();
        editor.input("A world<Esc>:w<CR>");
        collect_until(&mut editor, |_, seen| {
            seen.contains(&EditorEvent::Save("hello world".into()))
        });
    }
}
