//! winit key events to nvim key notation (the strings `nvim_input` takes).

use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

/// Convert a key press. Releases and modifier-only presses give `None`.
pub fn key_to_nvim(event: &KeyEvent, modifiers: ModifiersState) -> Option<String> {
    if event.state != ElementState::Pressed {
        return None;
    }
    translate_key(&event.logical_key, event.text.as_deref(), modifiers)
}

/// The testable core of [`key_to_nvim`]: a logical key, the text the
/// platform produced for it (if any) and the held modifiers.
pub fn translate_key(key: &Key, text: Option<&str>, modifiers: ModifiersState) -> Option<String> {
    match key {
        Key::Named(named) => translate_named(*named, text, modifiers),
        Key::Character(characters) => translate_characters(characters, text, modifiers),
        Key::Dead(_) | Key::Unidentified(_) => None,
    }
}

fn translate_named(
    named: NamedKey,
    text: Option<&str>,
    modifiers: ModifiersState,
) -> Option<String> {
    if is_modifier_only(named) {
        return None;
    }
    if named == NamedKey::Space {
        if modifiers.is_empty() {
            return Some(" ".to_string());
        }
        return Some(wrap("Space", modifiers, true));
    }
    let Some(name) = named_key_name(named) else {
        // A named key with no notation of its own may still carry text.
        let typed = text.filter(|text| !text.is_empty() && !text.chars().any(char::is_control))?;
        return Some(escape_text(typed));
    };
    Some(wrap(name, modifiers, true))
}

fn translate_characters(
    characters: &str,
    text: Option<&str>,
    modifiers: ModifiersState,
) -> Option<String> {
    let has_command_modifier =
        modifiers.control_key() || modifiers.alt_key() || modifiers.super_key();
    if !has_command_modifier {
        let typed = text.filter(|text| !text.is_empty()).unwrap_or(characters);
        if typed.is_empty() {
            return None;
        }
        return Some(escape_text(typed));
    }
    let mut chars = characters.chars();
    let (Some(only), None) = (chars.next(), chars.next()) else {
        // A multi-character commit under a command modifier has no
        // single-key notation.
        return None;
    };
    // Shift is part of the character for printable keys, except that nvim
    // tells <C-S-a> from <C-a>.
    let keep_shift = modifiers.shift_key() && only.is_alphabetic();
    let name = if only == '<' {
        "lt".to_string()
    } else if keep_shift {
        only.to_lowercase().collect()
    } else {
        only.to_string()
    };
    Some(wrap(&name, modifiers, keep_shift))
}

/// Printable text goes through as is, with `<` as `<lt>`.
fn escape_text(text: &str) -> String {
    text.replace('<', "<lt>")
}

/// `<C-M-S-D-name>`; shift is included only when `with_shift`.
fn wrap(name: &str, modifiers: ModifiersState, with_shift: bool) -> String {
    let mut prefix = String::new();
    if modifiers.control_key() {
        prefix.push_str("C-");
    }
    if modifiers.alt_key() {
        prefix.push_str("M-");
    }
    if with_shift && modifiers.shift_key() {
        prefix.push_str("S-");
    }
    if modifiers.super_key() {
        prefix.push_str("D-");
    }
    format!("<{prefix}{name}>")
}

fn is_modifier_only(named: NamedKey) -> bool {
    matches!(
        named,
        NamedKey::Shift
            | NamedKey::Control
            | NamedKey::Alt
            | NamedKey::AltGraph
            | NamedKey::Super
            | NamedKey::Meta
            | NamedKey::Hyper
            | NamedKey::CapsLock
            | NamedKey::NumLock
            | NamedKey::ScrollLock
            | NamedKey::Fn
            | NamedKey::FnLock
            | NamedKey::Symbol
            | NamedKey::SymbolLock
    )
}

fn named_key_name(named: NamedKey) -> Option<&'static str> {
    Some(match named {
        NamedKey::Enter => "CR",
        NamedKey::Escape => "Esc",
        NamedKey::Backspace => "BS",
        NamedKey::Delete => "Del",
        NamedKey::Tab => "Tab",
        NamedKey::ArrowUp => "Up",
        NamedKey::ArrowDown => "Down",
        NamedKey::ArrowLeft => "Left",
        NamedKey::ArrowRight => "Right",
        NamedKey::Home => "Home",
        NamedKey::End => "End",
        NamedKey::PageUp => "PageUp",
        NamedKey::PageDown => "PageDown",
        NamedKey::Insert => "Insert",
        NamedKey::Help => "Help",
        NamedKey::F1 => "F1",
        NamedKey::F2 => "F2",
        NamedKey::F3 => "F3",
        NamedKey::F4 => "F4",
        NamedKey::F5 => "F5",
        NamedKey::F6 => "F6",
        NamedKey::F7 => "F7",
        NamedKey::F8 => "F8",
        NamedKey::F9 => "F9",
        NamedKey::F10 => "F10",
        NamedKey::F11 => "F11",
        NamedKey::F12 => "F12",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: ModifiersState = ModifiersState::empty();

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn press(key: Key, modifiers: ModifiersState) -> Option<String> {
        translate_key(&key, None, modifiers)
    }

    #[test]
    fn should_send_printable_text_as_is_when_no_modifier_is_held() {
        assert_eq!(press(character("a"), NONE).as_deref(), Some("a"));
        assert_eq!(
            press(character("Z"), ModifiersState::SHIFT).as_deref(),
            Some("Z")
        );
        assert_eq!(press(character("\u{e9}"), NONE).as_deref(), Some("\u{e9}"));
    }

    #[test]
    fn should_prefer_platform_text_when_the_key_produced_it() {
        let key = character("a");
        assert_eq!(translate_key(&key, Some("A"), NONE).as_deref(), Some("A"));
    }

    #[test]
    fn should_escape_less_than_when_text_contains_it() {
        assert_eq!(press(character("<"), NONE).as_deref(), Some("<lt>"));
        assert_eq!(
            translate_key(&character("x"), Some("a<b"), NONE).as_deref(),
            Some("a<lt>b")
        );
    }

    #[test]
    fn should_name_special_keys_when_pressed_plain() {
        let cases = [
            (NamedKey::Enter, "<CR>"),
            (NamedKey::Escape, "<Esc>"),
            (NamedKey::Backspace, "<BS>"),
            (NamedKey::Delete, "<Del>"),
            (NamedKey::Tab, "<Tab>"),
            (NamedKey::ArrowUp, "<Up>"),
            (NamedKey::ArrowDown, "<Down>"),
            (NamedKey::ArrowLeft, "<Left>"),
            (NamedKey::ArrowRight, "<Right>"),
            (NamedKey::Home, "<Home>"),
            (NamedKey::End, "<End>"),
            (NamedKey::PageUp, "<PageUp>"),
            (NamedKey::PageDown, "<PageDown>"),
            (NamedKey::Insert, "<Insert>"),
            (NamedKey::F1, "<F1>"),
            (NamedKey::F12, "<F12>"),
        ];
        for (named, expected) in cases {
            assert_eq!(press(Key::Named(named), NONE).as_deref(), Some(expected));
        }
    }

    #[test]
    fn should_send_a_literal_space_when_space_is_plain() {
        assert_eq!(
            press(Key::Named(NamedKey::Space), NONE).as_deref(),
            Some(" ")
        );
    }

    #[test]
    fn should_name_space_when_a_modifier_is_held() {
        assert_eq!(
            press(Key::Named(NamedKey::Space), ModifiersState::CONTROL).as_deref(),
            Some("<C-Space>")
        );
        assert_eq!(
            press(Key::Named(NamedKey::Space), ModifiersState::SHIFT).as_deref(),
            Some("<S-Space>")
        );
    }

    #[test]
    fn should_prefix_modifiers_when_a_special_key_is_modified() {
        assert_eq!(
            press(Key::Named(NamedKey::Tab), ModifiersState::SHIFT).as_deref(),
            Some("<S-Tab>")
        );
        assert_eq!(
            press(Key::Named(NamedKey::Enter), ModifiersState::CONTROL).as_deref(),
            Some("<C-CR>")
        );
        assert_eq!(
            press(
                Key::Named(NamedKey::ArrowLeft),
                ModifiersState::CONTROL | ModifiersState::ALT
            )
            .as_deref(),
            Some("<C-M-Left>")
        );
        assert_eq!(
            press(Key::Named(NamedKey::F5), ModifiersState::SUPER).as_deref(),
            Some("<D-F5>")
        );
    }

    #[test]
    fn should_use_control_notation_when_control_is_held() {
        assert_eq!(
            press(character("x"), ModifiersState::CONTROL).as_deref(),
            Some("<C-x>")
        );
        assert_eq!(
            press(character("["), ModifiersState::CONTROL).as_deref(),
            Some("<C-[>")
        );
    }

    #[test]
    fn should_use_meta_notation_when_alt_is_held() {
        assert_eq!(
            press(character("b"), ModifiersState::ALT).as_deref(),
            Some("<M-b>")
        );
    }

    #[test]
    fn should_keep_shift_when_control_and_a_letter_are_combined() {
        assert_eq!(
            press(
                character("A"),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            )
            .as_deref(),
            Some("<C-S-a>")
        );
    }

    #[test]
    fn should_drop_shift_when_control_meets_a_non_letter() {
        assert_eq!(
            press(
                character("+"),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            )
            .as_deref(),
            Some("<C-+>")
        );
    }

    #[test]
    fn should_escape_less_than_when_control_is_held() {
        assert_eq!(
            press(character("<"), ModifiersState::CONTROL).as_deref(),
            Some("<C-lt>")
        );
    }

    #[test]
    fn should_drop_modifier_only_presses() {
        for named in [
            NamedKey::Shift,
            NamedKey::Control,
            NamedKey::Alt,
            NamedKey::AltGraph,
            NamedKey::Super,
            NamedKey::Meta,
            NamedKey::CapsLock,
            NamedKey::NumLock,
            NamedKey::Fn,
        ] {
            assert_eq!(
                press(Key::Named(named), ModifiersState::SHIFT),
                None,
                "{named:?}"
            );
        }
    }

    #[test]
    fn should_drop_dead_and_unidentified_keys() {
        assert_eq!(press(Key::Dead(Some('`')), NONE), None);
        assert_eq!(press(Key::Dead(None), NONE), None);
    }

    #[test]
    fn should_drop_empty_character_keys() {
        assert_eq!(press(character(""), NONE), None);
    }

    #[test]
    fn should_drop_multi_character_commits_when_a_command_modifier_is_held() {
        assert_eq!(press(character("ab"), ModifiersState::CONTROL), None);
    }

    #[test]
    fn should_pass_ime_commits_through_when_no_command_modifier_is_held() {
        assert_eq!(
            press(character("\u{3053}\u{3093}"), NONE).as_deref(),
            Some("\u{3053}\u{3093}")
        );
    }

    #[test]
    fn should_ignore_named_keys_without_notation_or_text() {
        assert_eq!(press(Key::Named(NamedKey::MediaPlayPause), NONE), None);
    }
}
