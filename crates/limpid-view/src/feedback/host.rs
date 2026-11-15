//! Embedded editor processes shared by the windows of a view process.
//!
//! Each active box gets its own process. Released editors remain available
//! for reuse until their individual idle deadlines, so sequential edits do
//! not pay for another Neovim startup.

use super::boxes::{EditorPort, EditorSpawner, nvim_spawner};
use crate::editor::{BoxKind, EditorConfig, EditorError, EditorEvent, Grid};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

/// The editor pool held by every window of a process.
pub type SharedEditor = Rc<RefCell<EditorHost>>;

/// Names a window to the host.
pub type OwnerId = u64;

#[derive(Debug)]
pub enum BeginError {
    Spawn(EditorError),
    Open(EditorError),
}

pub struct Polled {
    pub events: Vec<EditorEvent>,
    pub running: bool,
}

struct Session {
    port: Box<dyn EditorPort>,
    owner: OwnerId,
    /// None while a box is active; otherwise when the box was released.
    idle_since: Option<Instant>,
    sized: Option<(u16, u16)>,
}

pub struct EditorHost {
    spawner: EditorSpawner,
    sessions: Vec<Session>,
    next_owner: OwnerId,
}

impl EditorHost {
    pub fn new(spawner: EditorSpawner) -> Self {
        Self {
            spawner,
            sessions: Vec::new(),
            next_owner: 1,
        }
    }

    /// A pool that starts real `nvim --embed` processes on demand.
    pub fn shared_nvim() -> SharedEditor {
        Self::shared(nvim_spawner())
    }

    pub fn shared(spawner: EditorSpawner) -> SharedEditor {
        Rc::new(RefCell::new(Self::new(spawner)))
    }

    pub fn register(&mut self) -> OwnerId {
        let id = self.next_owner;
        self.next_owner += 1;
        id
    }

    pub fn has_editor(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Open a private box, reusing an idle process or starting another when
    /// every existing process belongs to an active box.
    pub fn begin(
        &mut self,
        id: OwnerId,
        config: &EditorConfig,
        text: &str,
        kind: BoxKind,
    ) -> Result<(), BeginError> {
        // Prefer this window's previous process, then any idle process.
        let reusable = self
            .sessions
            .iter()
            .position(|s| s.owner == id)
            .or_else(|| self.sessions.iter().position(|s| s.idle_since.is_some()));
        let mut session = if let Some(index) = reusable {
            self.sessions.swap_remove(index)
        } else {
            Session {
                port: (self.spawner)(config).map_err(BeginError::Spawn)?,
                owner: id,
                idle_since: None,
                sized: None,
            }
        };
        if !session.port.is_running() {
            session.port = (self.spawner)(config).map_err(BeginError::Spawn)?;
            session.sized = None;
        }
        // Idle redraws or mode changes belong to the previous box.
        if session.idle_since.is_some() {
            session.port.poll();
        }
        session.port.open(text, kind).map_err(BeginError::Open)?;
        session.owner = id;
        session.idle_since = None;
        self.sessions.push(session);
        Ok(())
    }

    pub fn release(&mut self, id: OwnerId, now: Instant) {
        if let Some(session) = self.session_mut(id)
            && session.idle_since.is_none()
        {
            session.idle_since = Some(now);
        }
    }

    /// Discard only this window's process, including any abandoned buffer.
    pub fn discard(&mut self, id: OwnerId) {
        self.sessions.retain(|session| session.owner != id);
    }

    fn session_mut(&mut self, id: OwnerId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.owner == id)
    }

    /// Keys never wait for an answer and only reach this window's active box.
    pub fn input(&mut self, id: OwnerId, keys: &str) {
        if let Some(session) = self.session_mut(id)
            && session.idle_since.is_none()
        {
            session.port.input(keys);
        }
    }

    pub fn resize(&mut self, id: OwnerId, cols: u16, rows: u16) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        if session.idle_since.is_some() || session.sized == Some((cols, rows)) {
            return;
        }
        session.sized = Some((cols, rows));
        session.port.resize(cols, rows);
    }

    /// Poll only this window's session; another box's events stay queued.
    pub fn poll(&mut self, id: OwnerId) -> Option<Polled> {
        let session = self.session_mut(id)?;
        let events = session.port.poll();
        let running = session.port.is_running();
        Some(Polled { events, running })
    }

    /// Expire idle processes independently, preserving all active boxes.
    pub fn stop_if_idle(&mut self, now: Instant, idle: Duration) {
        self.sessions
            .retain(|session| session.idle_since.is_none_or(|since| now < since + idle));
    }

    /// The earliest idle deadline, even if another box is still active.
    pub fn idle_deadline(&self, idle: Duration) -> Option<Instant> {
        self.sessions
            .iter()
            .filter_map(|session| session.idle_since.map(|since| since + idle))
            .min()
    }

    pub fn grid(&self, id: OwnerId) -> Option<&Grid> {
        self.sessions
            .iter()
            .find(|session| session.owner == id)
            .map(|session| session.port.grid())
    }
}
