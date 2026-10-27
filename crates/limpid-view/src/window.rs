//! Window lifecycle: one winit window and one softbuffer surface per page,
//! redraw coalescing, scale and resize handling.

use crate::paint::Canvas;
use limpid_core::transport::Handoff;
use softbuffer::{Context, Surface};
use std::{collections::HashMap, num::NonZeroU32, rc::Rc, time::Duration};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, OwnedDisplayHandle},
    keyboard::ModifiersState,
    platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11},
    window::{Window, WindowId},
};

/// What a window shows.
pub trait Page {
    fn title(&self) -> String;
    /// Paint the whole viewport. `canvas` is in device pixels.
    fn paint(&mut self, canvas: &mut Canvas, scale: f32);
    /// Handle a key event. Returns true when the page changed and needs a
    /// repaint.
    fn key(&mut self, event: &KeyEvent) -> bool;
    fn modifiers_changed(&mut self, _modifiers: ModifiersState) {}
    /// Whether the page needs [`Page::tick`] calls (an embedded editor is
    /// running); the event loop then wakes up every [`TICK_INTERVAL`].
    fn wants_ticks(&self) -> bool {
        false
    }
    /// Called while [`Page::wants_ticks`] is true. Returns true when the page
    /// changed and needs a repaint.
    fn tick(&mut self) -> bool {
        false
    }
    /// How long the loop may sleep before the next [`Page::tick`] while
    /// [`Page::wants_ticks`] is true. A page that only polls slowly should
    /// say so, so an idle window does not wake sixty times a second.
    fn tick_period(&self) -> Duration {
        TICK_INTERVAL
    }
}

/// How often a page that wants ticks is woken.
pub const TICK_INTERVAL: Duration = Duration::from_millis(16);

pub type PageFactory = Box<dyn FnMut(&Handoff) -> Result<Box<dyn Page>, String>>;

/// Sent to the event loop from the socket thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    Open(Handoff),
    CloseAll,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    /// Wayland app id and X11 class; compositor rules match on it.
    pub class: String,
    /// Initial inner size in logical pixels.
    pub width: f64,
    pub height: f64,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            class: "limpid".into(),
            width: 1000.0,
            height: 800.0,
        }
    }
}

struct OpenWindow {
    window: Rc<Window>,
    surface: Surface<OwnedDisplayHandle, Rc<Window>>,
    page: Box<dyn Page>,
    redraw_queued: bool,
}

impl OpenWindow {
    fn request_redraw(&mut self) {
        if self.redraw_queued {
            return;
        }
        self.redraw_queued = true;
        self.window.request_redraw();
    }

    fn redraw(&mut self) {
        self.redraw_queued = false;
        let size = self.window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if let Err(error) = self.surface.resize(width, height) {
            eprintln!("limpid-view: could not resize the surface: {error}");
            return;
        }
        let mut buffer = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                eprintln!("limpid-view: could not map the surface: {error}");
                return;
            }
        };
        let mut canvas = Canvas::new(&mut buffer, width.get(), height.get());
        self.page
            .paint(&mut canvas, self.window.scale_factor() as f32);
        if let Err(error) = buffer.present() {
            eprintln!("limpid-view: could not present: {error}");
        }
    }
}

pub struct App {
    factory: PageFactory,
    config: WindowConfig,
    windows: HashMap<WindowId, OpenWindow>,
    context: Option<Context<OwnedDisplayHandle>>,
    ever_opened: bool,
    exit_on_failed_open: bool,
}

impl App {
    /// With `exit_on_failed_open`, a request that cannot be opened ends the
    /// process when no window exists (used when there is no socket to wait on).
    pub fn new(factory: PageFactory, config: WindowConfig, exit_on_failed_open: bool) -> Self {
        Self {
            factory,
            config,
            windows: HashMap::new(),
            context: None,
            ever_opened: false,
            exit_on_failed_open,
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, request: &Handoff) -> Result<(), String> {
        let page = (self.factory)(request)?;
        let class = self.config.class.clone();
        let attributes = Window::default_attributes()
            .with_title(page.title())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));
        let attributes = WindowAttributesExtWayland::with_name(attributes, &class, &class);
        let attributes = WindowAttributesExtX11::with_name(attributes, &class, &class);
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| error.to_string())?,
        );
        let context = match self.context.take() {
            Some(context) => context,
            None => Context::new(event_loop.owned_display_handle())
                .map_err(|error| error.to_string())?,
        };
        let surface = Surface::new(&context, window.clone()).map_err(|error| error.to_string());
        self.context = Some(context);
        let mut open = OpenWindow {
            window: window.clone(),
            surface: surface?,
            page,
            redraw_queued: false,
        };
        open.request_redraw();
        self.windows.insert(window.id(), open);
        self.ever_opened = true;
        Ok(())
    }

    fn exit_if_finished(&self, event_loop: &ActiveEventLoop) {
        if self.ever_opened && self.windows.is_empty() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Open(request) => {
                if let Err(error) = self.open(event_loop, &request) {
                    eprintln!("limpid-view: {error}");
                    if self.exit_on_failed_open && self.windows.is_empty() {
                        event_loop.exit();
                    }
                }
            }
            UserEvent::CloseAll => {
                self.windows.clear();
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut next_wake: Option<Duration> = None;
        for open in self.windows.values_mut() {
            if !open.page.wants_ticks() {
                continue;
            }
            if open.page.tick() {
                open.request_redraw();
            }
            let period = open.page.tick_period();
            next_wake = Some(next_wake.map_or(period, |soonest| soonest.min(period)));
        }
        let flow = match next_wake {
            Some(period) => ControlFlow::WaitUntil(std::time::Instant::now() + period),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(open) = self.windows.get_mut(&id) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                self.windows.remove(&id);
                self.exit_if_finished(event_loop);
            }
            WindowEvent::RedrawRequested => open.redraw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                open.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                open.page.modifiers_changed(modifiers.state());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let changed = open.page.key(&event);
                if changed {
                    open.request_redraw();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Still;

    impl Page for Still {
        fn title(&self) -> String {
            "still".into()
        }
        fn paint(&mut self, _canvas: &mut Canvas, _scale: f32) {}
        fn key(&mut self, _event: &KeyEvent) -> bool {
            false
        }
    }

    #[test]
    fn should_not_ask_for_ticks_when_a_page_has_no_editor() {
        let mut page = Still;
        assert!(!page.wants_ticks());
        assert!(!page.tick());
        assert_eq!(page.tick_period(), TICK_INTERVAL);
        assert!(TICK_INTERVAL < Duration::from_millis(100));
    }

    #[test]
    fn should_default_to_the_limpid_class_when_no_class_is_configured() {
        let config = WindowConfig::default();
        assert_eq!(config.class, "limpid");
        assert!(config.width > 0.0 && config.height > 0.0);
    }
}
