//! The one editor process of a view process, shared by every window's
//! [`Feedback`](super::Feedback).
//!
//! nvim is expensive, so a view process starts at most one and lends it to
//! one box at a time. A window claims the editor when its box opens and
//! releases it when the box closes; a second window that asks meanwhile is
//! told the editor is in use. The idle timer is the host's: it runs from the
//! last release, whichever window made it.

use super::boxes::{EditorPort, EditorSpawner, nvim_spawner};
use crate::editor::{BoxKind, EditorConfig, EditorError, EditorEvent, Grid};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

/// What every window of a process holds.
pub type SharedEditor = Rc<RefCell<EditorHost>>;

/// Names a window to the host.
pub type OwnerId = u64;

#[derive(Debug)]
pub enum BeginError {
    /// Another window's box has the editor.
    InUse,
    /// The editor could not be started.
    Spawn(EditorError),
    /// The editor started but would not open the box.
    Open(EditorError),
}

/// What [`EditorHost::poll`] found.
pub struct Polled {
    pub events: Vec<EditorEvent>,
    pub running: bool,
}

pub struct EditorHost {
    spawner: EditorSpawner,
    port: Option<Box<dyn EditorPort>>,
    owner: Option<OwnerId>,
    idle_since: Option<Instant>,
    next_owner: OwnerId,
    /// The size the editor's grid was last set to, whichever window set it.
    sized: Option<(u16, u16)>,
}

impl EditorHost {
    pub fn new(spawner: EditorSpawner) -> Self {
        Self {
            spawner,
            port: None,
            owner: None,
            idle_since: None,
            next_owner: 1,
            sized: None,
        }
    }

    /// A host that starts a real `nvim --embed` when first asked.
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

    /// Whether an editor process exists (running a box or idling).
    pub fn has_editor(&self) -> bool {
        self.port.is_some()
    }

    /// Whether a box of some other window has the editor.
    pub fn in_use_by_other(&self, id: OwnerId) -> bool {
        self.owner.is_some_and(|owner| owner != id)
    }

    /// Give `id` the editor and open `text` in it, starting the process if
    /// there is none or it died.
    pub fn begin(
        &mut self,
        id: OwnerId,
        config: &EditorConfig,
        text: &str,
        kind: BoxKind,
    ) -> Result<(), BeginError> {
        if self.in_use_by_other(id) {
            return Err(BeginError::InUse);
        }
        let alive = self.port.as_mut().is_some_and(|port| port.is_running());
        if !alive {
            self.port = None;
            self.port = Some((self.spawner)(config).map_err(BeginError::Spawn)?);
            self.sized = None;
        }
        let Some(port) = self.port.as_mut() else {
            return Err(BeginError::Open(EditorError::Protocol(
                "editor not running".into(),
            )));
        };
        if let Err(error) = port.open(text, kind) {
            self.port = None;
            return Err(BeginError::Open(error));
        }
        self.owner = Some(id);
        self.idle_since = None;
        Ok(())
    }

    /// `id`'s box closed: the idle timer starts.
    pub fn release(&mut self, id: OwnerId, now: Instant) {
        if self.owner != Some(id) {
            return;
        }
        self.owner = None;
        self.idle_since = Some(now);
    }

    /// Stop the process now (it died, or its box was abandoned with it).
    pub fn discard(&mut self, id: OwnerId) {
        if self.in_use_by_other(id) {
            return;
        }
        self.port = None;
        self.sized = None;
        self.owner = None;
        self.idle_since = None;
    }

    /// Keys from the owner. Never waits for an answer. Dropped when `id`
    /// does not own the editor.
    pub fn input(&mut self, id: OwnerId, keys: &str) {
        if self.owner != Some(id) {
            return;
        }
        if let Some(port) = self.port.as_mut() {
            port.input(keys);
        }
    }

    /// Size the editor's grid for the owner's box. The host remembers the
    /// last size it sent, so a window whose box has the size it had last
    /// time still resizes an editor another window left at a different one.
    pub fn resize(&mut self, id: OwnerId, cols: u16, rows: u16) {
        if self.owner != Some(id) || self.sized == Some((cols, rows)) {
            return;
        }
        self.sized = Some((cols, rows));
        if let Some(port) = self.port.as_mut() {
            port.resize(cols, rows);
        }
    }

    /// What the editor reported since the last call. `None` when there is no
    /// editor or another window's box has it (its events are not ours).
    pub fn poll(&mut self, id: OwnerId) -> Option<Polled> {
        if self.in_use_by_other(id) {
            return None;
        }
        let port = self.port.as_mut()?;
        let events = port.poll();
        let running = port.is_running();
        Some(Polled { events, running })
    }

    /// Stop the process once it has had no box for `idle`.
    pub fn stop_if_idle(&mut self, now: Instant, idle: Duration) {
        let Some(deadline) = self.idle_deadline(idle) else {
            return;
        };
        if now < deadline {
            return;
        }
        self.port = None;
        self.idle_since = None;
    }

    /// When the idle timer will stop the process; `None` while a box has it
    /// or when there is no process.
    pub fn idle_deadline(&self, idle: Duration) -> Option<Instant> {
        if self.owner.is_some() || self.port.is_none() {
            return None;
        }
        self.idle_since.map(|since| since + idle)
    }

    /// The editor's grid, for painting the owner's box.
    pub fn grid(&self) -> Option<&Grid> {
        self.port.as_ref().map(|port| port.grid())
    }
}
