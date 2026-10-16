//! Display-free key interpretation, ported from htmlview's `keys.py`.
//!
//! Maps a GDK key name, modifiers and context to an action. The host performs
//! the action when the page leaves the key unhandled. No GTK import, so it is
//! unit-testable anywhere.

use std::cell::Cell;

pub const GG_TIMEOUT_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Reload,
    ReloadBypassCache,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Inspector,
    Back,
    Forward,
    ScrollDown,
    ScrollUp,
    HalfPageDown,
    HalfPageUp,
    SectionNext,
    SectionPrev,
    Top,
    Bottom,
    SearchOpen,
    SearchClose,
    SearchNext,
    SearchPrev,
    ClearSearch,
}

#[derive(Default)]
pub struct KeyDispatcher {
    g_pending_since: Cell<Option<u64>>,
}

impl KeyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the action for a key, or `None` to leave it unhandled.
    pub fn dispatch(
        &self,
        key: Option<&str>,
        ctrl: bool,
        shift: bool,
        now_ms: u64,
        editable_focus: bool,
        search_focused: bool,
    ) -> Option<Action> {
        use Action::*;
        let pending_since = self.g_pending_since.take();
        let key = key?;
        if ctrl {
            if shift && key.eq_ignore_ascii_case("i") {
                return Some(Inspector);
            }
            if editable_focus || search_focused {
                if matches!(key, "Down" | "Up") {
                    return None;
                }
                return match key {
                    "q" | "w" => Some(Quit),
                    "r" => Some(ReloadBypassCache),
                    "plus" | "equal" => Some(ZoomIn),
                    "minus" => Some(ZoomOut),
                    "0" => Some(ZoomReset),
                    _ => None,
                };
            }
            return match key {
                "Down" => Some(SectionNext),
                "Up" => Some(SectionPrev),
                "q" | "w" => Some(Quit),
                "r" => Some(ReloadBypassCache),
                "plus" | "equal" => Some(ZoomIn),
                "minus" => Some(ZoomOut),
                "0" => Some(ZoomReset),
                _ => None,
            };
        }
        if key == "F5" {
            return Some(Reload);
        }
        if key == "F12" {
            return Some(Inspector);
        }
        if search_focused {
            return if key == "Escape" {
                Some(SearchClose)
            } else {
                None
            };
        }
        if editable_focus {
            return None;
        }
        if key == "g" {
            if pending_since.is_some_and(|since| now_ms.saturating_sub(since) <= GG_TIMEOUT_MS) {
                return Some(Top);
            }
            self.g_pending_since.set(Some(now_ms));
            return None;
        }
        match key {
            "q" => Some(Quit),
            "r" => Some(Reload),
            "plus" | "equal" => Some(ZoomIn),
            "minus" => Some(ZoomOut),
            "0" => Some(ZoomReset),
            "bracketleft" => Some(Back),
            "bracketright" => Some(Forward),
            "j" => Some(ScrollDown),
            "k" => Some(ScrollUp),
            "G" => Some(Bottom),
            "Page_Down" => Some(HalfPageDown),
            "Page_Up" => Some(HalfPageUp),
            "slash" => Some(SearchOpen),
            "n" => Some(SearchNext),
            "N" => Some(SearchPrev),
            "Escape" => Some(ClearSearch),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Action::*, GG_TIMEOUT_MS, KeyDispatcher};

    fn press(
        dispatcher: &KeyDispatcher,
        key: &str,
        now: u64,
        ctrl: bool,
        shift: bool,
        editable: bool,
        search: bool,
    ) -> Option<super::Action> {
        dispatcher.dispatch(Some(key), ctrl, shift, now, editable, search)
    }

    fn plain(key: &str) -> Option<super::Action> {
        KeyDispatcher::new().dispatch(Some(key), false, false, 0, false, false)
    }

    #[test]
    fn scroll_and_page_keys() {
        assert_eq!(plain("j"), Some(ScrollDown));
        assert_eq!(plain("k"), Some(ScrollUp));
        assert_eq!(plain("Page_Down"), Some(HalfPageDown));
        assert_eq!(plain("Page_Up"), Some(HalfPageUp));
    }

    #[test]
    fn page_keys_are_ignored_while_typing() {
        assert_eq!(
            press(
                &KeyDispatcher::new(),
                "Page_Down",
                0,
                false,
                false,
                true,
                false
            ),
            None
        );
        assert_eq!(
            press(
                &KeyDispatcher::new(),
                "Page_Up",
                0,
                false,
                false,
                false,
                true
            ),
            None
        );
    }

    #[test]
    fn ctrl_arrows_jump_sections_unless_typing() {
        assert_eq!(
            press(&KeyDispatcher::new(), "Down", 0, true, false, false, false),
            Some(SectionNext)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "Up", 0, true, false, false, false),
            Some(SectionPrev)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "Down", 0, true, false, true, false),
            None
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "Up", 0, true, false, false, true),
            None
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "Down", 0, false, false, false, false),
            None
        );
    }

    #[test]
    fn g_and_gg() {
        assert_eq!(
            press(&KeyDispatcher::new(), "G", 0, false, true, false, false),
            Some(Bottom)
        );
        let dispatcher = KeyDispatcher::new();
        assert_eq!(
            press(&dispatcher, "g", 1000, false, false, false, false),
            None
        );
        assert_eq!(
            press(
                &dispatcher,
                "g",
                1000 + GG_TIMEOUT_MS,
                false,
                false,
                false,
                false
            ),
            Some(Top)
        );
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 1000, false, false, false, false);
        assert_ne!(
            press(
                &dispatcher,
                "g",
                1000 + GG_TIMEOUT_MS + 1,
                false,
                false,
                false,
                false
            ),
            Some(Top)
        );
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 0, false, false, false, false);
        press(&dispatcher, "g", 5000, false, false, false, false);
        assert_eq!(
            press(&dispatcher, "g", 5100, false, false, false, false),
            Some(Top)
        );
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 0, false, false, false, false);
        assert_eq!(
            press(&dispatcher, "j", 100, false, false, false, false),
            Some(ScrollDown)
        );
        assert_ne!(
            press(&dispatcher, "g", 200, false, false, false, false),
            Some(Top)
        );
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 0, false, false, false, false);
        press(&dispatcher, "g", 10, false, false, false, false);
        assert_ne!(
            press(&dispatcher, "g", 20, false, false, false, false),
            Some(Top)
        );
    }

    #[test]
    fn editable_and_search_suppress_plain_letters() {
        for key in "jkgGnNqr".chars().map(String::from).chain(["slash".into()]) {
            assert_eq!(
                press(&KeyDispatcher::new(), &key, 0, false, false, true, false),
                None,
                "{key}"
            );
            assert_eq!(
                press(&KeyDispatcher::new(), &key, 0, false, false, false, true),
                None,
                "{key}"
            );
        }
        assert_eq!(
            press(
                &KeyDispatcher::new(),
                "Escape",
                0,
                false,
                false,
                true,
                false
            ),
            None
        );
    }

    #[test]
    fn escape_closes_search_and_clears_it_otherwise() {
        assert_eq!(
            press(
                &KeyDispatcher::new(),
                "Escape",
                0,
                false,
                false,
                false,
                true
            ),
            Some(SearchClose)
        );
        assert_eq!(plain("Escape"), Some(ClearSearch));
    }

    #[test]
    fn g_is_not_armed_while_typing_or_with_ctrl() {
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 0, false, false, true, false);
        assert_ne!(
            press(&dispatcher, "g", 10, false, false, false, false),
            Some(Top)
        );
        let dispatcher = KeyDispatcher::new();
        press(&dispatcher, "g", 0, true, false, false, false);
        assert_ne!(
            press(&dispatcher, "g", 10, false, false, false, false),
            Some(Top)
        );
    }

    #[test]
    fn search_and_navigation_bindings() {
        assert_eq!(plain("slash"), Some(SearchOpen));
        assert_eq!(plain("n"), Some(SearchNext));
        assert_eq!(
            press(&KeyDispatcher::new(), "N", 0, false, true, false, false),
            Some(SearchPrev)
        );
    }

    #[test]
    fn plain_bindings() {
        for (key, action) in [
            ("q", Quit),
            ("r", Reload),
            ("plus", ZoomIn),
            ("equal", ZoomIn),
            ("minus", ZoomOut),
            ("0", ZoomReset),
            ("bracketleft", Back),
            ("bracketright", Forward),
        ] {
            assert_eq!(plain(key), Some(action), "{key}");
        }
    }

    #[test]
    fn always_on_and_ctrl_bindings_work_while_typing() {
        for (editable, search) in [(true, false), (false, true)] {
            assert_eq!(
                press(
                    &KeyDispatcher::new(),
                    "F5",
                    0,
                    false,
                    false,
                    editable,
                    search
                ),
                Some(Reload)
            );
            assert_eq!(
                press(&KeyDispatcher::new(), "q", 0, true, false, editable, search),
                Some(Quit)
            );
        }
        assert_eq!(
            press(&KeyDispatcher::new(), "q", 0, true, false, false, false),
            Some(Quit)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "w", 0, true, false, false, false),
            Some(Quit)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "r", 0, true, false, false, false),
            Some(ReloadBypassCache)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "plus", 0, true, false, false, false),
            Some(ZoomIn)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "minus", 0, true, false, false, false),
            Some(ZoomOut)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "0", 0, true, false, false, false),
            Some(ZoomReset)
        );
        assert_eq!(
            press(&KeyDispatcher::new(), "I", 0, true, true, false, false),
            Some(Inspector)
        );
        assert_eq!(plain("F12"), Some(Inspector));
    }

    #[test]
    fn unbound_keys_return_none() {
        assert_eq!(plain("x"), None);
        assert_eq!(
            KeyDispatcher::new().dispatch(None, false, false, 0, false, false),
            None
        );
    }
}
