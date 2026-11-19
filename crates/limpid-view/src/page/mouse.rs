//! Pointer gestures use the same cursor and visual selection as the keys.

use super::*;
use std::ops::Range;
use winit::{event::MouseButton, window::CursorIcon};

const DOUBLE_CLICK_MS: u64 = 500;
const DRAG_DISTANCE: f32 = 5.0;
const RESIZE_REACH: f32 = 6.0;
const MIN_RAIL_WIDTH: f32 = 100.0;

#[derive(Default)]
pub(super) struct Mouse {
    position: Option<(f32, f32)>,
    gesture: Option<Gesture>,
    last_click: Option<Click>,
}

enum Gesture {
    Selecting {
        anchor: Range<usize>,
        origin: (f32, f32),
        dragging: bool,
    },
    Resizing {
        offset: f32,
    },
}

struct Click {
    src: Range<usize>,
    position: (f32, f32),
    time_ms: u64,
}

impl Mouse {
    pub(super) fn interrupt(&mut self) {
        self.gesture = None;
        self.last_click = None;
    }
}

impl DocPage {
    pub(super) fn keyboard_mode(&mut self) -> bool {
        let switched = self.input_mode != InputMode::Keyboard;
        self.input_mode = InputMode::Keyboard;
        self.mouse.interrupt();
        switched
    }

    fn mouse_blocked(&self) -> bool {
        self.box_open() || self.review_open() || self.nav.in_prompt()
    }

    fn on_divider(&self, x: f32, y: f32) -> bool {
        self.layout
            .as_ref()
            .and_then(|layout| layout.rail.as_ref())
            .is_some_and(|rail| {
                y >= 0.0
                    && y < self.viewport_height
                    && (x - rail.width).abs() <= RESIZE_REACH * self.scale
            })
    }

    fn rail_section(&self, x: f32, y: f32) -> Option<usize> {
        if y >= self.viewport_height {
            return None;
        }
        self.layout.as_ref()?.rail.as_ref()?.section_at(x, y)
    }

    pub(super) fn mouse_cursor(&self) -> CursorIcon {
        if self.input_mode != InputMode::Mouse || self.mouse_blocked() {
            return CursorIcon::Default;
        }
        if matches!(self.mouse.gesture, Some(Gesture::Resizing { .. })) {
            return CursorIcon::ColResize;
        }
        let Some((x, y)) = self.mouse.position else {
            return CursorIcon::Default;
        };
        if self.on_divider(x, y) {
            CursorIcon::ColResize
        } else if self.rail_section(x, y).is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        }
    }

    /// Exact hits include the full painted band; nested stops win over their
    /// containers. Hover only changes the input mode, never the block cursor.
    fn block_at_pointer(&self, x: f32, y: f32) -> Option<Range<usize>> {
        if y < 0.0 || y >= self.viewport_height {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let reach = cursor::band_reach(layout.gutter, self.scale);
        if x < layout.column_x - reach || x >= layout.column_x + layout.column_width + reach {
            return None;
        }
        let y = self.scroll + y;
        cursor::collect_stops(layout)
            .into_iter()
            .rev()
            .find(|stop| stop.rect.y <= y && y < stop.rect.bottom())
            .map(|stop| stop.src)
    }

    pub(super) fn pointer_at(&mut self, event: PointerEvent, now_ms: u64) -> bool {
        if event == PointerEvent::Cancel {
            self.mouse.interrupt();
            self.mouse.position = None;
            return false;
        }
        let switched = self.input_mode != InputMode::Mouse;
        self.input_mode = InputMode::Mouse;
        if let PointerEvent::Moved { x, y } = event {
            self.mouse.position = Some((x, y));
        }
        if self.mouse_blocked() {
            self.mouse.interrupt();
            return switched;
        }
        let changed = match event {
            PointerEvent::Moved { .. } => self.move_mouse(),
            PointerEvent::Wheel { lines } => {
                self.mouse.last_click = None;
                let changed = self.wheel(lines);
                self.move_mouse() || changed
            }
            PointerEvent::Scroll { dy } => {
                self.mouse.last_click = None;
                let changed = self.scroll_by(dy);
                self.move_mouse() || changed
            }
            PointerEvent::Button {
                button: MouseButton::Left,
                state: ElementState::Pressed,
            } => self.press_mouse(now_ms),
            PointerEvent::Button {
                button: MouseButton::Left,
                state: ElementState::Released,
            } => {
                let changed = self.move_mouse();
                if let Some(Gesture::Selecting {
                    anchor,
                    origin,
                    dragging: false,
                }) = self.mouse.gesture.take()
                {
                    self.mouse.last_click = Some(Click {
                        src: anchor,
                        position: origin,
                        time_ms: now_ms,
                    });
                }
                changed
            }
            PointerEvent::Button { .. } => {
                self.mouse.last_click = None;
                false
            }
            PointerEvent::Cancel => unreachable!(),
        };
        changed || switched
    }

    fn press_mouse(&mut self, now_ms: u64) -> bool {
        self.mouse.gesture = None;
        let previous = self.mouse.last_click.take();
        let Some((x, y)) = self.mouse.position else {
            return false;
        };
        if self.on_divider(x, y) {
            let rail = self.layout.as_ref().unwrap().rail.as_ref().unwrap();
            self.mouse.gesture = Some(Gesture::Resizing {
                offset: x - rail.width,
            });
            return false;
        }
        if let Some(block) = self.rail_section(x, y) {
            return self
                .with_nav(|nav, env, _| nav.jump_section(env, block))
                .unwrap_or(false);
        }
        let Some(src) = self.block_at_pointer(x, y) else {
            return false;
        };
        let copied = previous.is_some_and(|click| {
            click.src == src
                && now_ms.saturating_sub(click.time_ms) <= DOUBLE_CLICK_MS
                && (x - click.position.0).hypot(y - click.position.1) <= DRAG_DISTANCE * self.scale
        });
        let changed = self.nav.focus_source(src.clone(), None);
        if copied {
            self.copy_mouse(now_ms);
            return true;
        }
        self.mouse.gesture = Some(Gesture::Selecting {
            anchor: src,
            origin: (x, y),
            dragging: false,
        });
        changed
    }

    fn copy_mouse(&mut self, now_ms: u64) {
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        let mut fonts = self.fonts.borrow_mut();
        let mut env = Env {
            doc: &self.doc,
            layout,
            fonts: &mut fonts,
            scroll: &mut self.scroll,
            viewport: self.viewport_height,
        };
        self.nav
            .copy_focused(now_ms, &mut env, &mut *self.mouse_clipboard);
    }

    pub(super) fn move_mouse(&mut self) -> bool {
        let Some((x, y)) = self.mouse.position else {
            return false;
        };
        let Some(gesture) = self.mouse.gesture.as_mut() else {
            return false;
        };
        match gesture {
            Gesture::Resizing { offset } => {
                let max_width = self.viewport_width / 2.0;
                let width = (x - *offset)
                    .clamp((MIN_RAIL_WIDTH * self.scale).min(max_width), max_width)
                    / self.scale
                    / crate::layout::rail::zoom_of(&self.theme);
                if (self.theme.rail_width - width).abs() < 0.5 {
                    return false;
                }
                // An explicitly resized rail stays visible even if its new
                // width crosses the automatic layout's threshold.
                self.options.rail = RailMode::On;
                self.theme.rail_width = width;
                self.laid_for = None;
                true
            }
            Gesture::Selecting {
                anchor,
                origin,
                dragging,
            } => {
                *dragging |= (x - origin.0).hypot(y - origin.1) >= DRAG_DISTANCE * self.scale;
                if !*dragging {
                    return false;
                }
                self.mouse.last_click = None;
                let Some(layout) = self.layout.as_ref() else {
                    return false;
                };
                let y = self.scroll + y.clamp(0.0, (self.viewport_height - 1.0).max(0.0));
                let stops = cursor::collect_stops(layout);
                let hit = stops
                    .iter()
                    .rev()
                    .find(|stop| stop.rect.y <= y && y < stop.rect.bottom())
                    .or_else(|| {
                        stops.iter().min_by(|a, b| {
                            let distance = |stop: &cursor::Stop| {
                                (stop.rect.y - y).max(y - stop.rect.bottom()).max(0.0)
                            };
                            distance(a).total_cmp(&distance(b))
                        })
                    });
                hit.is_some_and(|stop| {
                    self.nav
                        .focus_source(stop.src.clone(), Some(anchor.clone()))
                })
            }
        }
    }

    pub(super) fn drag_scroll_delta(&self) -> f32 {
        if !matches!(
            self.mouse.gesture,
            Some(Gesture::Selecting { dragging: true, .. })
        ) {
            return 0.0;
        }
        let Some((_, y)) = self.mouse.position else {
            return 0.0;
        };
        let edge = (24.0 * self.scale).min(self.viewport_height / 4.0);
        if y < edge && self.scroll > 0.0 {
            -18.0 * self.scale
        } else if y > self.viewport_height - edge
            && !self
                .layout
                .as_ref()
                .and_then(|layout| layout.max_scroll())
                .is_some_and(|max| self.scroll >= max)
        {
            18.0 * self.scale
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::rail::zoom_of, nav::status::StateName, snapshot::render};

    struct Copies(Rc<RefCell<Vec<String>>>);
    impl Clipboard for Copies {
        fn copy(&mut self, text: &str) -> Result<(), String> {
            self.0.borrow_mut().push(text.into());
            Ok(())
        }
    }

    fn page(source: &str) -> DocPage {
        let mut page = DocPage::from_source(
            source,
            "mouse".into(),
            &Settings::default(),
            Scheme::Dark,
            Rc::new(RefCell::new(Fonts::new())),
        )
        .with_chrome(true);
        paint(&mut page, 1400, 600, 1.0);
        page
    }

    fn paint(page: &mut DocPage, width: u32, height: u32, scale: f32) {
        render(page, width, height, scale).unwrap();
    }

    /// Where the rail's right edge is painted, in device pixels.
    fn rail_edge(page: &DocPage) -> f32 {
        page.layout.as_ref().unwrap().rail.as_ref().unwrap().width
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-3,
            "{actual} is not {expected}"
        );
    }

    fn point(page: &DocPage, block: usize) -> (f32, f32) {
        let layout = page.layout.as_ref().unwrap();
        let rect = layout.blocks[block].rect;
        (layout.column_x + 10.0, rect.y + rect.h / 2.0 - page.scroll)
    }

    fn move_to(page: &mut DocPage, (x, y): (f32, f32), time: u64) -> bool {
        page.pointer_at(PointerEvent::Moved { x, y }, time)
    }

    fn button(page: &mut DocPage, state: ElementState, time: u64) -> bool {
        page.pointer_at(
            PointerEvent::Button {
                button: MouseButton::Left,
                state,
            },
            time,
        )
    }

    fn click(page: &mut DocPage, position: (f32, f32), time: u64) {
        move_to(page, position, time);
        button(page, ElementState::Pressed, time);
        button(page, ElementState::Released, time + 10);
    }

    fn press(page: &mut DocPage, name: &str) {
        page.handle_input(&KeyInput::named(name));
    }

    fn focused(page: &DocPage) -> String {
        let src = page.nav.cursor().src.clone().unwrap();
        page.doc.source[src].trim().to_owned()
    }

    #[test]
    fn clicks_focus_blocks_and_keyboard_navigation_continues_from_them() {
        let mut page = page("one\n\ntwo\n\nthree\n");
        let second = point(&page, 1);
        move_to(&mut page, second, 0);
        assert_eq!(focused(&page), "one");
        assert_eq!(page.input_mode(), InputMode::Mouse);
        click(&mut page, second, 10);
        assert_eq!(focused(&page), "two");
        press(&mut page, "j");
        assert_eq!(page.input_mode(), InputMode::Keyboard);
        assert_eq!(focused(&page), "three");
        move_to(&mut page, second, 30);
        assert_eq!(page.input_mode(), InputMode::Mouse);
        assert_eq!(focused(&page), "three");
    }

    #[test]
    fn drags_select_ranges_in_both_directions_and_keyboard_y_copies_them() {
        for (from, to) in [(0, 2), (2, 0)] {
            let mut page = page("one\n\ntwo\n\nthree\n\nfour\n");
            let copied = Rc::new(RefCell::new(Vec::new()));
            page.clipboard = Box::new(Copies(copied.clone()));
            let start = point(&page, from);
            let end = point(&page, to);
            move_to(&mut page, start, 0);
            button(&mut page, ElementState::Pressed, 10);
            move_to(&mut page, end, 20);
            button(&mut page, ElementState::Released, 30);
            assert_eq!(page.nav.state(), StateName::Visual);
            assert_eq!(page.input_mode(), InputMode::Mouse);
            assert!(
                page.nav
                    .bands(page.layout.as_ref().unwrap())
                    .selection
                    .is_some()
            );
            assert!(page.mouse.gesture.is_none());
            assert!(
                copied.borrow().is_empty(),
                "dragging does not copy automatically"
            );
            press(&mut page, "y");
            assert_eq!(*copied.borrow(), vec!["one\n\ntwo\n\nthree\n"]);
            assert_eq!(page.nav.state(), StateName::Page);
            assert_eq!(page.input_mode(), InputMode::Keyboard);
        }
    }

    #[test]
    fn double_click_copies_code_to_the_desktop_clipboard_without_sending_feedback() {
        let mut page = page("one\n\n```rust\nlet n = 1;\n```\n");
        let desktop = Rc::new(RefCell::new(Vec::new()));
        let replies = Rc::new(RefCell::new(Vec::new()));
        page.mouse_clipboard = Box::new(Copies(desktop.clone()));
        page.clipboard = Box::new(Copies(replies.clone()));
        let code = point(&page, 1);
        click(&mut page, code, 0);
        assert!(desktop.borrow().is_empty());
        click(&mut page, code, 100);
        assert_eq!(*desktop.borrow(), vec!["let n = 1;\n"]);
        assert!(replies.borrow().is_empty());
        assert_eq!(page.input_mode(), InputMode::Mouse);
        assert_eq!(page.nav.status_view(110).message, "Yanked code");
        click(&mut page, code, 200);
        assert_eq!(
            desktop.borrow().len(),
            1,
            "a third click does not copy again"
        );
        press(&mut page, "y");
        press(&mut page, "y");
        assert_eq!(*replies.borrow(), vec!["let n = 1;\n"]);
    }

    #[test]
    fn slow_clicks_and_clicks_on_different_blocks_do_not_copy() {
        let mut page = page("one\n\ntwo\n");
        let copied = Rc::new(RefCell::new(Vec::new()));
        page.mouse_clipboard = Box::new(Copies(copied.clone()));
        let first = point(&page, 0);
        let second = point(&page, 1);
        click(&mut page, first, 0);
        click(&mut page, first, 1000);
        click(&mut page, second, 1100);
        assert!(copied.borrow().is_empty());
    }

    #[test]
    fn right_clicks_and_status_bar_clicks_do_not_retarget_the_block_cursor() {
        let mut page = page("one\n\ntwo\n");
        let second = point(&page, 1);
        move_to(&mut page, second, 0);
        page.pointer_at(
            PointerEvent::Button {
                button: MouseButton::Right,
                state: ElementState::Pressed,
            },
            10,
        );
        assert_eq!(focused(&page), "one");
        let status = (second.0, page.viewport_height + 1.0);
        click(&mut page, status, 100);
        assert_eq!(focused(&page), "one");
    }

    #[test]
    fn keyboard_input_interrupts_dragging_and_clears_the_double_click_history() {
        let mut page = page("one\n\ntwo\n\nthree\n");
        let copied = Rc::new(RefCell::new(Vec::new()));
        page.mouse_clipboard = Box::new(Copies(copied.clone()));
        let first = point(&page, 0);
        let second = point(&page, 1);
        click(&mut page, first, 0);
        press(&mut page, "escape");
        click(&mut page, first, 100);
        assert!(copied.borrow().is_empty());
        button(&mut page, ElementState::Pressed, 200);
        press(&mut page, "j");
        move_to(&mut page, first, 210);
        assert_eq!(focused(&page), "two");
        assert_eq!(page.nav.state(), StateName::Page);
        move_to(&mut page, second, 220);
        assert_eq!(focused(&page), "two");
    }

    #[test]
    fn clicking_cancels_an_incomplete_keyboard_yank() {
        let mut page = page("one\n\ntwo\n");
        let copied = Rc::new(RefCell::new(Vec::new()));
        page.clipboard = Box::new(Copies(copied.clone()));
        press(&mut page, "y");
        let second = point(&page, 1);
        click(&mut page, second, 0);
        press(&mut page, "y");
        assert!(copied.borrow().is_empty());
        press(&mut page, "y");
        assert_eq!(*copied.borrow(), vec!["two\n"]);
    }

    #[test]
    fn prompts_and_help_keep_mouse_actions_out_of_the_document() {
        for prompt in [":", "?", "/"] {
            let mut page = page("one\n\ntwo\n");
            let second = point(&page, 1);
            press(&mut page, prompt);
            let state = page.nav.state();
            click(&mut page, second, 0);
            click(&mut page, second, 100);
            assert_eq!(focused(&page), "one");
            assert_eq!(page.nav.state(), state);
            assert_eq!(page.input_mode(), InputMode::Mouse);
            assert!(page.mouse.gesture.is_none());
            press(&mut page, "escape");
            assert_eq!(page.input_mode(), InputMode::Keyboard);
        }
    }

    fn sections(count: usize) -> String {
        (0..count)
            .map(|n| format!("## Section {n}\n\n{}", "body\n\n".repeat(30)))
            .collect()
    }

    #[test]
    fn sidebar_clicks_jump_to_lazy_sections_and_focus_their_headings() {
        let mut page = page(&sections(10));
        let rail = page.layout.as_ref().unwrap().rail.as_ref().unwrap();
        let entry = &rail.entries[5];
        let block = entry.block_index.unwrap();
        let position = (20.0, entry.top + entry.height / 2.0);
        assert!(
            page.layout.as_ref().unwrap().blocks.len() <= block,
            "target starts outside the layout"
        );
        click(&mut page, position, 0);
        assert_eq!(focused(&page), "## Section 5");
        let expected = page.layout.as_ref().unwrap().blocks[block].rect.y - 24.0;
        assert!((page.scroll - expected).abs() < 0.5);
        paint(&mut page, 1400, 600, 1.0);
        assert_eq!(page.input_mode(), InputMode::Mouse);
        assert_eq!(
            page.layout
                .as_ref()
                .unwrap()
                .rail_spy(page.scroll, page.viewport_height),
            Some(5)
        );
        press(&mut page, "j");
        assert_eq!(focused(&page), "body");
    }

    #[test]
    fn sidebar_hit_testing_uses_the_offset_of_the_painted_long_outline() {
        let mut page = page(&sections(40));
        press(&mut page, "G");
        paint(&mut page, 1400, 600, 1.0);
        let rail = page.layout.as_ref().unwrap().rail.as_ref().unwrap();
        let last = rail.entries.last().unwrap();
        let offset = (last.top + last.height - (600.0 - 40.0)).max(0.0);
        let entry = &rail.entries[37];
        let position = (20.0, entry.top - offset + entry.height / 2.0);
        assert!(position.1 > 0.0 && position.1 < page.viewport_height);
        move_to(&mut page, position, 0);
        assert_eq!(page.cursor_icon(), CursorIcon::Pointer);
        click(&mut page, position, 10);
        assert_eq!(focused(&page), "## Section 37");
    }

    #[test]
    fn sidebar_resizing_scales_clamps_and_stays_visible_past_the_auto_threshold() {
        for scale in [1.0, 2.0] {
            let mut page = page(&sections(3));
            let width = 1400;
            let height = 600;
            paint(&mut page, width, height, scale);
            let initial_focus = page.nav.cursor().src.clone();
            let zoom = zoom_of(&page.theme);
            let edge = rail_edge(&page);
            assert_close(edge, page.theme.rail_width * zoom * scale);
            move_to(&mut page, (edge + 3.0 * scale, 200.0 * scale), 0);
            assert_eq!(page.cursor_icon(), CursorIcon::ColResize);
            button(&mut page, ElementState::Pressed, 10);
            let dragged = edge + 200.0 * scale;
            move_to(&mut page, (dragged + 3.0 * scale, 200.0 * scale), 20);
            paint(&mut page, width, height, scale);
            assert_close(rail_edge(&page), dragged);
            assert_close(page.theme.rail_width, dragged / scale / zoom);
            assert_eq!(page.options.rail, RailMode::On);
            assert_eq!(page.nav.cursor().src, initial_focus);
            move_to(&mut page, (-100.0, 200.0 * scale), 30);
            assert_close(page.theme.rail_width * zoom, MIN_RAIL_WIDTH);
            let widest = width as f32 / 2.0 / zoom;
            move_to(&mut page, (5000.0, 200.0 * scale), 40);
            assert_close(page.theme.rail_width, widest);
            button(&mut page, ElementState::Released, 50);
            move_to(&mut page, (300.0, 200.0 * scale), 60);
            assert_eq!(page.theme.rail_width, widest, "release stops resizing");
        }
    }

    #[test]
    fn resizing_preserves_the_viewport_anchor_and_keyboard_input_stops_the_resize() {
        let mut page = page(&sections(3));
        page.scroll_by(700.0);
        paint(&mut page, 1400, 600, 1.0);
        let layout = page.layout.as_ref().unwrap();
        let block = layout.block_at(page.scroll).unwrap();
        let offset = page.scroll - layout.blocks[block].rect.y;
        let focus = page.nav.cursor().src.clone();
        let edge = rail_edge(&page);
        move_to(&mut page, (edge, 200.0), 0);
        button(&mut page, ElementState::Pressed, 10);
        move_to(&mut page, (edge + 100.0, 200.0), 20);
        paint(&mut page, 1400, 600, 1.0);
        let layout = page.layout.as_ref().unwrap();
        assert!((page.scroll - layout.blocks[block].rect.y - offset).abs() < 0.5);
        assert_eq!(page.nav.cursor().src, focus);
        press(&mut page, "escape");
        let resized = (edge + 100.0) / zoom_of(&page.theme);
        move_to(&mut page, (edge + 280.0, 200.0), 30);
        assert_close(page.theme.rail_width, resized);
        assert_eq!(page.input_mode(), InputMode::Mouse);
        assert!(page.mouse.gesture.is_none());
    }

    #[test]
    fn unbound_keys_and_ime_commits_switch_back_to_keyboard_mode() {
        let mut page = page("one\n");
        move_to(&mut page, (0.0, 0.0), 0);
        assert!(
            page.handle_input(&KeyInput::named("f12")),
            "mode change needs a repaint"
        );
        assert_eq!(page.input_mode(), InputMode::Keyboard);
        move_to(&mut page, (0.0, 0.0), 10);
        assert!(page.ime(&Ime::Commit("a".into())));
        assert_eq!(page.input_mode(), InputMode::Keyboard);
    }

    #[test]
    fn clipboard_errors_are_reported_on_the_page() {
        let mut page = page("one\n");
        let first = point(&page, 0);
        click(&mut page, first, 0);
        click(&mut page, first, 100);
        assert!(
            page.nav
                .status_view(110)
                .message
                .starts_with("Yank failed:")
        );
        assert_eq!(focused(&page), "one");
    }

    #[test]
    fn dragging_near_an_edge_scrolls_and_stops_when_the_gesture_ends() {
        let mut page = page(&"paragraph\n\n".repeat(200));
        let start = point(&page, 0);
        move_to(&mut page, start, 0);
        button(&mut page, ElementState::Pressed, 10);
        let bottom = (start.0, page.viewport_height - 2.0);
        move_to(&mut page, bottom, 20);
        assert!(page.wants_ticks());
        page.tick();
        assert!(page.scroll > 0.0);
        assert_eq!(page.nav.state(), StateName::Visual);
        button(&mut page, ElementState::Released, 30);
        assert!(!page.wants_ticks());
        let scroll = page.scroll;
        page.tick();
        assert_eq!(page.scroll, scroll);
    }

    #[test]
    fn leaving_the_window_cancels_gestures_and_document_reload_cancels_click_history() {
        let mut page = page("one\n\ntwo\n");
        let first = point(&page, 0);
        let second = point(&page, 1);
        move_to(&mut page, first, 0);
        button(&mut page, ElementState::Pressed, 10);
        page.pointer(PointerEvent::Cancel);
        move_to(&mut page, second, 20);
        assert_eq!(focused(&page), "one");
        assert_eq!(page.nav.state(), StateName::Page);
        click(&mut page, first, 30);
        assert!(page.mouse.last_click.is_some());
        page.replace_source("new\n");
        assert!(page.mouse.last_click.is_none());
        assert!(page.mouse.gesture.is_none());
    }
}
