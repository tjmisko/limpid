//! Key chords, the action table, overrides from `[view.keys]`, multi-key
//! sequences (`gg`, `zt`) and counts (`3j`).
//!
//! A binding is written the way a user would write it in `config.toml`:
//! `"ctrl+d"`, `"shift+g"`, `"g g"` for a sequence, and `"j, down"` for
//! alternatives. Letters are lower case with an explicit `shift+`; symbols are
//! written bare (`"{"`, `":"`). Overriding an action replaces all of its
//! default bindings.

use std::collections::BTreeMap;
use std::fmt;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// How long a half-typed sequence waits for its next key. Matches the host's
/// `GG_TIMEOUT_MS`.
pub const SEQUENCE_TIMEOUT_MS: u64 = 1000;

/// Largest count a user can type; keeps `99999999j` from overflowing.
const MAX_COUNT: u32 = 99_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NextBlock,
    PrevBlock,
    HalfPageDown,
    HalfPageUp,
    SectionNext,
    SectionPrev,
    Top,
    Bottom,
    AlignTop,
    AlignCenter,
    AlignBottom,
    ScrollLeft,
    ScrollRight,
    OpenReply,
    OpenComment,
    Insert,
    InsertStart,
    InsertEnd,
    DeleteComment,
    Review,
    FirstUnanswered,
    ToggleRail,
    SearchOpen,
    SearchNext,
    SearchPrev,
    CommandOpen,
    Visual,
    Yank,
    YankPage,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Reload,
    Quit,
    Clear,
    /// A digit on a focused question. Not remappable: digits are counts
    /// everywhere else.
    PickOption(u8),
}

/// Every action a user can bind, with its config name and default keys.
const TABLE: &[(Action, &str, &str)] = &[
    (Action::NextBlock, "next_block", "j, down"),
    (Action::PrevBlock, "prev_block", "k, up"),
    (Action::HalfPageDown, "half_page_down", "ctrl+d, pagedown"),
    (Action::HalfPageUp, "half_page_up", "ctrl+u, pageup"),
    (Action::SectionNext, "section_next", "}, ctrl+down"),
    (Action::SectionPrev, "section_prev", "{, ctrl+up"),
    (Action::Top, "top", "g g"),
    (Action::Bottom, "bottom", "shift+g"),
    (Action::AlignTop, "align_top", "z t"),
    (Action::AlignCenter, "align_center", "z z"),
    (Action::AlignBottom, "align_bottom", "z b"),
    (Action::ScrollLeft, "scroll_left", "h"),
    (Action::ScrollRight, "scroll_right", "l"),
    (Action::OpenReply, "open_reply", "enter"),
    (Action::Insert, "insert", "i"),
    (Action::InsertStart, "insert_start", "shift+i"),
    (Action::InsertEnd, "insert_end", "shift+a"),
    (Action::OpenComment, "open_comment", "c"),
    (Action::DeleteComment, "delete_comment", "x"),
    (Action::Review, "review", "shift+s"),
    (Action::FirstUnanswered, "first_unanswered", "shift+q"),
    (Action::ToggleRail, "toggle_rail", "t, f1"),
    (Action::SearchOpen, "search_open", "/"),
    (Action::SearchNext, "search_next", "n"),
    (Action::SearchPrev, "search_prev", "shift+n"),
    (Action::CommandOpen, "command_open", ":"),
    (Action::Visual, "visual", "v"),
    (Action::Yank, "yank", "y"),
    (Action::YankPage, "yank_page", "shift+y"),
    (Action::ZoomIn, "zoom_in", "+, ="),
    (Action::ZoomOut, "zoom_out", "-"),
    (Action::ZoomReset, "zoom_reset", "0"),
    (Action::Reload, "reload", "r"),
    (Action::Quit, "quit", "q, ctrl+q, ctrl+w"),
    (Action::Clear, "clear", "escape"),
];

impl Action {
    /// The name used in `[view.keys]`.
    pub fn name(self) -> &'static str {
        TABLE
            .iter()
            .find(|(action, _, _)| *action == self)
            .map_or("pick_option", |(_, name, _)| name)
    }

    pub fn from_name(name: &str) -> Option<Action> {
        TABLE
            .iter()
            .find(|(_, candidate, _)| *candidate == name)
            .map(|(action, _, _)| *action)
    }

    /// Every remappable action with its default binding text.
    pub fn all() -> impl Iterator<Item = (Action, &'static str, &'static str)> {
        TABLE.iter().copied()
    }
}

/// One key press, normalised: lower-case base key plus modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// `"j"`, `"{"`, `"down"`, `"escape"`, `"f1"`, ...
    pub key: String,
}

impl Chord {
    pub fn plain(key: &str) -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            key: key.to_owned(),
        }
    }

    pub fn with_ctrl(key: &str) -> Self {
        Self {
            ctrl: true,
            ..Self::plain(key)
        }
    }

    /// Parse `"ctrl+d"`, `"shift+g"`, `"G"`, `"down"`, `"+"`.
    pub fn parse(text: &str) -> Result<Chord, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty key".into());
        }
        let (modifiers, base) = split_modifiers(text);
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => ctrl = true,
                "alt" | "meta" | "a" | "m" => alt = true,
                "shift" | "s" => shift = true,
                other => return Err(format!("unknown modifier '{other}' in '{text}'")),
            }
        }
        let key = normalise_key_name(base).ok_or_else(|| format!("unknown key '{base}'"))?;
        let mut chars = key.chars();
        if let (Some(only), None) = (chars.next(), chars.next())
            && only.is_alphabetic()
            && only.is_uppercase()
        {
            shift = true;
        }
        let key = if key.chars().count() == 1 {
            key.to_lowercase()
        } else {
            key
        };
        Ok(Chord {
            ctrl,
            alt,
            shift,
            key,
        })
    }

    /// The chord for a winit key and modifier state. `None` for keys the
    /// keymap has no name for (modifiers alone, dead keys, IME strings) and for
    /// anything held with the Super key.
    pub fn from_winit(key: &Key, modifiers: ModifiersState) -> Option<Chord> {
        if modifiers.super_key() {
            return None;
        }
        let (name, shift_from_key) = match key {
            Key::Character(text) => {
                let mut chars = text.chars();
                let (Some(only), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                if only.is_control() {
                    return None;
                }
                // Shift is part of a symbol's identity (`{`), but a letter keeps
                // it explicit so `G` and `g` stay distinct.
                let upper = only.is_alphabetic() && only.is_uppercase();
                let lower: String = only.to_lowercase().collect();
                (lower, upper)
            }
            Key::Named(named) => (named_key_name(*named)?.to_owned(), false),
            _ => return None,
        };
        let is_letter = name.chars().count() == 1 && name.chars().all(char::is_alphabetic);
        let is_symbol = name.chars().count() == 1 && !is_letter;
        let shift = if is_symbol {
            false
        } else {
            modifiers.shift_key() || shift_from_key
        };
        Some(Chord {
            ctrl: modifiers.control_key(),
            alt: modifiers.alt_key(),
            shift,
            key: name,
        })
    }

    /// The digit this chord types, when it is a bare `0`-`9`.
    pub fn digit(&self) -> Option<u8> {
        if self.ctrl || self.alt || self.shift {
            return None;
        }
        let mut chars = self.key.chars();
        let (Some(only), None) = (chars.next(), chars.next()) else {
            return None;
        };
        only.to_digit(10).map(|digit| digit as u8)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.alt {
            f.write_str("alt+")?;
        }
        if self.shift {
            f.write_str("shift+")?;
        }
        f.write_str(&self.key)
    }
}

/// Separate leading modifiers from the base key. A trailing `+` is the key.
fn split_modifiers(text: &str) -> (Vec<&str>, &str) {
    if text == "+" {
        return (Vec::new(), "+");
    }
    if let Some(prefix) = text.strip_suffix('+') {
        // "ctrl++" is ctrl and the plus key.
        let modifiers = prefix
            .split('+')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        return (modifiers, "+");
    }
    let mut parts: Vec<&str> = text.split('+').collect();
    let base = parts.pop().unwrap_or("");
    (parts, base)
}

fn normalise_key_name(name: &str) -> Option<String> {
    if name.chars().count() == 1 {
        return Some(name.to_owned());
    }
    let lower = name.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "down" | "arrowdown" => "down",
        "up" | "arrowup" => "up",
        "left" | "arrowleft" => "left",
        "right" | "arrowright" => "right",
        "pagedown" | "page_down" | "pgdn" => "pagedown",
        "pageup" | "page_up" | "pgup" => "pageup",
        "home" => "home",
        "end" => "end",
        "escape" | "esc" => "escape",
        "enter" | "return" | "cr" => "enter",
        "tab" => "tab",
        "backspace" | "bs" => "backspace",
        "delete" | "del" => "delete",
        "space" => "space",
        "plus" => "+",
        "minus" => "-",
        "equal" | "equals" => "=",
        "slash" => "/",
        "colon" => ":",
        "comma" => ",",
        "bracketleft" => "[",
        "bracketright" => "]",
        "braceleft" => "{",
        "braceright" => "}",
        function if is_function_key(function) => function,
        _ => return None,
    };
    Some(canonical.to_owned())
}

fn is_function_key(name: &str) -> bool {
    name.strip_prefix('f')
        .and_then(|digits| digits.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number))
}

fn named_key_name(key: NamedKey) -> Option<&'static str> {
    Some(match key {
        NamedKey::ArrowDown => "down",
        NamedKey::ArrowUp => "up",
        NamedKey::ArrowLeft => "left",
        NamedKey::ArrowRight => "right",
        NamedKey::PageDown => "pagedown",
        NamedKey::PageUp => "pageup",
        NamedKey::Home => "home",
        NamedKey::End => "end",
        NamedKey::Escape => "escape",
        NamedKey::Enter => "enter",
        NamedKey::Tab => "tab",
        NamedKey::Backspace => "backspace",
        NamedKey::Delete => "delete",
        NamedKey::Space => "space",
        NamedKey::F1 => "f1",
        NamedKey::F2 => "f2",
        NamedKey::F3 => "f3",
        NamedKey::F4 => "f4",
        NamedKey::F5 => "f5",
        NamedKey::F6 => "f6",
        NamedKey::F7 => "f7",
        NamedKey::F8 => "f8",
        NamedKey::F9 => "f9",
        NamedKey::F10 => "f10",
        NamedKey::F11 => "f11",
        NamedKey::F12 => "f12",
        _ => return None,
    })
}

/// A key press with the text it types, for the search and command lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    pub chord: Chord,
    /// The characters this press inserts into a text field, if any.
    pub text: Option<String>,
}

impl KeyInput {
    pub fn from_winit(key: &Key, modifiers: ModifiersState) -> Option<KeyInput> {
        let chord = Chord::from_winit(key, modifiers)?;
        let text = match key {
            Key::Character(text) if !chord.ctrl && !chord.alt => Some(text.to_string()),
            Key::Named(NamedKey::Space) if !chord.ctrl && !chord.alt => Some(" ".to_owned()),
            _ => None,
        };
        Some(KeyInput { chord, text })
    }

    /// A press of `name` as a user would write it in the config file.
    pub fn named(name: &str) -> KeyInput {
        let chord = Chord::parse(name).unwrap_or_else(|_| Chord::plain(name));
        let text = (!chord.ctrl && !chord.alt && chord.key.chars().count() == 1).then(|| {
            if chord.shift {
                chord.key.to_uppercase()
            } else {
                chord.key.clone()
            }
        });
        let text = text.or_else(|| (chord.key == "space").then(|| " ".to_owned()));
        KeyInput { chord, text }
    }
}

/// What a key press resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Action {
        action: Action,
        /// The typed count, if any. `None` means the user typed none.
        count: Option<u32>,
    },
    /// Part of a sequence or a count; waiting for more.
    Pending,
    Unbound,
}

type Sequence = Vec<Chord>;

/// The resolved key table plus the state of a half-typed sequence or count.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(Sequence, Action)>,
    pub timeout_ms: u64,
    pending: Sequence,
    pending_since: u64,
    count: Option<u32>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        Keymap::new(&BTreeMap::new()).expect("the default key table is consistent")
    }

    /// The default table with `overrides` (action name to binding text) laid
    /// over it. Errors name the offending action or key.
    pub fn new(overrides: &BTreeMap<String, String>) -> Result<Keymap, String> {
        for name in overrides.keys() {
            if Action::from_name(name).is_none() {
                let known: Vec<&str> = Action::all().map(|(_, name, _)| name).collect();
                return Err(format!(
                    "unknown action '{name}' in [view.keys]; known actions: {}",
                    known.join(", ")
                ));
            }
        }
        let mut bindings: Vec<(Sequence, Action)> = Vec::new();
        for (action, name, default) in Action::all() {
            let text = overrides.get(name).map_or(default, String::as_str);
            for sequence in parse_alternatives(text)
                .map_err(|error| format!("[view.keys] {name} = \"{text}\": {error}"))?
            {
                bindings.push((sequence, action));
            }
        }
        check_conflicts(&bindings)?;
        Ok(Keymap {
            bindings,
            timeout_ms: SEQUENCE_TIMEOUT_MS,
            pending: Vec::new(),
            pending_since: 0,
            count: None,
        })
    }

    /// The bindings of `action` as text, for help and tests.
    pub fn bindings_of(&self, action: Action) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, bound)| *bound == action)
            .map(|(sequence, _)| {
                sequence
                    .iter()
                    .map(Chord::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// Forget a half-typed sequence and count.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = None;
    }

    /// The keys typed so far but not yet resolved, as the status line shows
    /// them (`3g`).
    pub fn pending_text(&self) -> String {
        let mut text = self
            .count
            .map(|count| count.to_string())
            .unwrap_or_default();
        for chord in &self.pending {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&chord.to_string());
        }
        text
    }

    pub fn has_pending(&self) -> bool {
        self.count.is_some() || !self.pending.is_empty()
    }

    /// Feed one key. With `digits_are_counts` false, `1`-`9` resolve to
    /// [`Action::PickOption`] instead of starting a count.
    pub fn feed(&mut self, chord: &Chord, now_ms: u64, digits_are_counts: bool) -> Resolved {
        if !self.pending.is_empty() && now_ms.saturating_sub(self.pending_since) > self.timeout_ms {
            self.reset();
        }
        if self.pending.is_empty()
            && let Some(digit) = chord.digit()
        {
            if digits_are_counts && (digit != 0 || self.count.is_some()) {
                let next = self.count.unwrap_or(0) * 10 + u32::from(digit);
                self.count = Some(next.min(MAX_COUNT));
                return Resolved::Pending;
            }
            if !digits_are_counts && digit != 0 {
                let count = self.count.take();
                return Resolved::Action {
                    action: Action::PickOption(digit),
                    count,
                };
            }
        }
        let resolved = self.resolve(chord, now_ms);
        if resolved == Resolved::Unbound {
            self.reset();
        }
        resolved
    }

    fn resolve(&mut self, chord: &Chord, now_ms: u64) -> Resolved {
        let mut candidate = self.pending.clone();
        candidate.push(chord.clone());
        match self.lookup(&candidate) {
            Lookup::Exact(action) => {
                self.pending.clear();
                Resolved::Action {
                    action,
                    count: self.count.take(),
                }
            }
            Lookup::Prefix => {
                self.pending = candidate;
                self.pending_since = now_ms;
                Resolved::Pending
            }
            Lookup::None if !self.pending.is_empty() => {
                // The sequence broke; the key starts a new one.
                self.pending.clear();
                self.resolve(chord, now_ms)
            }
            Lookup::None => Resolved::Unbound,
        }
    }

    fn lookup(&self, candidate: &[Chord]) -> Lookup {
        let mut prefix = false;
        for (sequence, action) in &self.bindings {
            if sequence.as_slice() == candidate {
                return Lookup::Exact(*action);
            }
            if sequence.len() > candidate.len() && sequence.starts_with(candidate) {
                prefix = true;
            }
        }
        if prefix { Lookup::Prefix } else { Lookup::None }
    }
}

enum Lookup {
    Exact(Action),
    Prefix,
    None,
}

fn parse_alternatives(text: &str) -> Result<Vec<Sequence>, String> {
    let mut sequences = Vec::new();
    for alternative in text.split(',') {
        let alternative = alternative.trim();
        if alternative.is_empty() {
            return Err("empty binding (write the comma key as 'comma')".into());
        }
        let sequence = alternative
            .split_whitespace()
            .map(Chord::parse)
            .collect::<Result<Sequence, _>>()?;
        sequences.push(sequence);
    }
    Ok(sequences)
}

fn check_conflicts(bindings: &[(Sequence, Action)]) -> Result<(), String> {
    for (index, (sequence, action)) in bindings.iter().enumerate() {
        for (other, other_action) in &bindings[index + 1..] {
            let show = |sequence: &Sequence| {
                sequence
                    .iter()
                    .map(Chord::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            if sequence == other && action != other_action {
                return Err(format!(
                    "key '{}' is bound to both {} and {}",
                    show(sequence),
                    action.name(),
                    other_action.name()
                ));
            }
            let shorter_first = sequence.len() < other.len() && other.starts_with(sequence);
            let longer_first = other.len() < sequence.len() && sequence.starts_with(other);
            if shorter_first || longer_first {
                let (short, long) = if shorter_first {
                    (sequence, other)
                } else {
                    (other, sequence)
                };
                return Err(format!(
                    "key '{}' ({}) is also the start of '{}' ({}); a sequence cannot share its first keys with another binding",
                    show(short),
                    if shorter_first { action } else { other_action }.name(),
                    show(long),
                    if shorter_first { other_action } else { action }.name(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Keymap {
        Keymap::defaults()
    }

    fn press(map: &mut Keymap, key: &str, now: u64) -> Resolved {
        let chord = Chord::parse(key).unwrap_or_else(|_| Chord::plain(key));
        map.feed(&chord, now, true)
    }

    fn action(action: Action) -> Resolved {
        Resolved::Action {
            action,
            count: None,
        }
    }

    #[test]
    fn should_bind_i_and_capital_i_and_a_to_the_insert_actions_when_defaults_are_used() {
        let mut map = map();
        assert_eq!(press(&mut map, "i", 0), action(Action::Insert));
        assert_eq!(press(&mut map, "shift+i", 0), action(Action::InsertStart));
        assert_eq!(press(&mut map, "shift+a", 0), action(Action::InsertEnd));
        assert_eq!(press(&mut map, "enter", 0), action(Action::OpenReply));
    }

    #[test]
    fn should_parse_modifiers_and_aliases_when_reading_a_chord() {
        assert_eq!(Chord::parse("ctrl+d").unwrap(), Chord::with_ctrl("d"));
        let upper = Chord::parse("G").unwrap();
        assert_eq!((upper.shift, upper.key.as_str()), (true, "g"));
        assert_eq!(Chord::parse("shift+g").unwrap(), upper);
        assert_eq!(Chord::parse("Page_Down").unwrap().key, "pagedown");
        assert_eq!(Chord::parse("esc").unwrap().key, "escape");
        assert_eq!(Chord::parse("+").unwrap().key, "+");
        let ctrl_plus = Chord::parse("ctrl++").unwrap();
        assert!(ctrl_plus.ctrl && ctrl_plus.key == "+");
        assert_eq!(Chord::parse("{").unwrap().key, "{");
        assert_eq!(Chord::parse("braceleft").unwrap().key, "{");
    }

    #[test]
    fn should_reject_unknown_modifiers_and_keys_when_parsing_a_chord() {
        assert!(Chord::parse("hyper+x").unwrap_err().contains("hyper"));
        assert!(Chord::parse("flarb").unwrap_err().contains("flarb"));
        assert!(Chord::parse("").is_err());
        assert!(Chord::parse("   ").is_err());
        assert!(Chord::parse("f99").is_err());
        assert!(Chord::parse("f12").is_ok());
    }

    #[test]
    fn should_normalise_winit_keys_when_reading_events() {
        let none = ModifiersState::empty();
        let shift = ModifiersState::SHIFT;
        let ctrl = ModifiersState::CONTROL;
        let chord = |key: Key, mods| Chord::from_winit(&key, mods);
        assert_eq!(
            chord(Key::Character("j".into()), none),
            Some(Chord::plain("j"))
        );
        // Shift with a letter stays explicit, upper or lower case on the key.
        let expected = Chord::parse("shift+g").unwrap();
        assert_eq!(
            chord(Key::Character("G".into()), shift),
            Some(expected.clone())
        );
        assert_eq!(chord(Key::Character("G".into()), none), Some(expected));
        // Shift with a symbol is part of the symbol.
        assert_eq!(
            chord(Key::Character("{".into()), shift),
            Some(Chord::plain("{"))
        );
        assert_eq!(
            chord(Key::Character("d".into()), ctrl),
            Some(Chord::with_ctrl("d"))
        );
        assert_eq!(
            chord(Key::Named(NamedKey::PageDown), none),
            Some(Chord::plain("pagedown"))
        );
        assert_eq!(chord(Key::Named(NamedKey::Shift), none), None);
        assert_eq!(chord(Key::Character("ab".into()), none), None);
        assert_eq!(chord(Key::Character("\u{4}".into()), ctrl), None);
        assert_eq!(
            chord(Key::Character("j".into()), ModifiersState::SUPER),
            None
        );
    }

    #[test]
    fn should_carry_typed_text_when_the_key_inserts_characters() {
        let none = ModifiersState::empty();
        let typed = KeyInput::from_winit(&Key::Character("a".into()), none).unwrap();
        assert_eq!(typed.text.as_deref(), Some("a"));
        let space = KeyInput::from_winit(&Key::Named(NamedKey::Space), none).unwrap();
        assert_eq!(space.text.as_deref(), Some(" "));
        let ctrl =
            KeyInput::from_winit(&Key::Character("a".into()), ModifiersState::CONTROL).unwrap();
        assert_eq!(ctrl.text, None);
        let arrow = KeyInput::from_winit(&Key::Named(NamedKey::ArrowDown), none).unwrap();
        assert_eq!(arrow.text, None);
    }

    #[test]
    fn should_resolve_single_keys_and_alternatives_when_using_the_defaults() {
        let mut map = map();
        assert_eq!(press(&mut map, "j", 0), action(Action::NextBlock));
        assert_eq!(press(&mut map, "down", 0), action(Action::NextBlock));
        assert_eq!(press(&mut map, "ctrl+d", 0), action(Action::HalfPageDown));
        assert_eq!(press(&mut map, "pagedown", 0), action(Action::HalfPageDown));
        assert_eq!(press(&mut map, "shift+g", 0), action(Action::Bottom));
        assert_eq!(press(&mut map, "ctrl+down", 0), action(Action::SectionNext));
        assert_eq!(press(&mut map, "}", 0), action(Action::SectionNext));
        assert_eq!(press(&mut map, "f1", 0), action(Action::ToggleRail));
        assert_eq!(press(&mut map, "0", 0), action(Action::ZoomReset));
        assert_eq!(press(&mut map, "nonsense", 0), Resolved::Unbound);
    }

    #[test]
    fn should_resolve_a_sequence_when_its_keys_arrive_in_time() {
        let mut map = map();
        assert_eq!(press(&mut map, "g", 1000), Resolved::Pending);
        assert_eq!(map.pending_text(), "g");
        assert_eq!(
            press(&mut map, "g", 1000 + SEQUENCE_TIMEOUT_MS),
            action(Action::Top)
        );
        assert!(!map.has_pending());
        assert_eq!(press(&mut map, "z", 0), Resolved::Pending);
        assert_eq!(press(&mut map, "t", 1), action(Action::AlignTop));
        assert_eq!(press(&mut map, "z", 2), Resolved::Pending);
        assert_eq!(press(&mut map, "z", 3), action(Action::AlignCenter));
        assert_eq!(press(&mut map, "z", 4), Resolved::Pending);
        assert_eq!(press(&mut map, "b", 5), action(Action::AlignBottom));
    }

    #[test]
    fn should_drop_a_sequence_when_it_times_out() {
        let mut map = map();
        press(&mut map, "g", 1000);
        let late = press(&mut map, "g", 1000 + SEQUENCE_TIMEOUT_MS + 1);
        assert_eq!(late, Resolved::Pending, "the late g starts a new sequence");
        assert_eq!(
            press(&mut map, "g", 1000 + SEQUENCE_TIMEOUT_MS + 2),
            action(Action::Top)
        );
    }

    #[test]
    fn should_restart_with_the_new_key_when_a_sequence_breaks() {
        let mut map = map();
        press(&mut map, "g", 0);
        assert_eq!(press(&mut map, "r", 10), action(Action::Reload));
        press(&mut map, "g", 20);
        assert_eq!(press(&mut map, "x", 30), action(Action::DeleteComment));
        // "g g g": the third g begins a fresh sequence.
        press(&mut map, "g", 40);
        assert_eq!(press(&mut map, "g", 41), action(Action::Top));
        assert_eq!(press(&mut map, "g", 42), Resolved::Pending);
    }

    #[test]
    fn should_accumulate_a_count_when_digits_precede_an_action() {
        let mut map = map();
        assert_eq!(press(&mut map, "3", 0), Resolved::Pending);
        assert_eq!(map.pending_text(), "3");
        assert_eq!(
            press(&mut map, "j", 0),
            Resolved::Action {
                action: Action::NextBlock,
                count: Some(3)
            }
        );
        press(&mut map, "1", 0);
        press(&mut map, "0", 0);
        assert_eq!(
            press(&mut map, "k", 0),
            Resolved::Action {
                action: Action::PrevBlock,
                count: Some(10)
            }
        );
        // The count was consumed.
        assert_eq!(press(&mut map, "j", 0), action(Action::NextBlock));
    }

    #[test]
    fn should_treat_a_leading_zero_as_a_key_when_no_count_is_started() {
        let mut map = map();
        assert_eq!(press(&mut map, "0", 0), action(Action::ZoomReset));
    }

    #[test]
    fn should_apply_the_count_to_the_end_of_a_sequence_when_counting_gg() {
        let mut map = map();
        press(&mut map, "5", 0);
        press(&mut map, "g", 1);
        assert_eq!(
            press(&mut map, "g", 2),
            Resolved::Action {
                action: Action::Top,
                count: Some(5)
            }
        );
    }

    #[test]
    fn should_clamp_the_count_when_the_user_types_a_huge_number() {
        let mut map = map();
        for _ in 0..30 {
            press(&mut map, "9", 0);
        }
        let Resolved::Action { count, .. } = press(&mut map, "j", 0) else {
            panic!("no action");
        };
        assert_eq!(count, Some(MAX_COUNT));
    }

    #[test]
    fn should_forget_the_count_when_an_unbound_key_follows() {
        let mut map = map();
        press(&mut map, "4", 0);
        assert_eq!(press(&mut map, "nonsense", 1), Resolved::Unbound);
        assert_eq!(press(&mut map, "j", 2), action(Action::NextBlock));
    }

    #[test]
    fn should_pick_an_option_when_digits_are_not_counts() {
        let mut map = map();
        let resolved = map.feed(&Chord::parse("2").unwrap(), 0, false);
        assert_eq!(
            resolved,
            Resolved::Action {
                action: Action::PickOption(2),
                count: None
            }
        );
        // Zero is never an option; it is the zoom reset.
        let zero = map.feed(&Chord::parse("0").unwrap(), 0, false);
        assert_eq!(zero, action(Action::ZoomReset));
    }

    #[test]
    fn should_replace_all_default_bindings_when_an_action_is_overridden() {
        let overrides = BTreeMap::from([("next_block".to_owned(), "n".to_owned())]);
        let mut map = Keymap::new(&overrides);
        // `n` is also search_next by default, so this conflicts.
        assert!(map.as_ref().unwrap_err().contains("both"), "{map:?}");
        let overrides = BTreeMap::from([
            ("next_block".to_owned(), "ctrl+n".to_owned()),
            ("search_next".to_owned(), "n".to_owned()),
        ]);
        map = Keymap::new(&overrides);
        let mut map = map.unwrap();
        assert_eq!(press(&mut map, "ctrl+n", 0), action(Action::NextBlock));
        assert_eq!(press(&mut map, "j", 0), Resolved::Unbound);
        assert_eq!(press(&mut map, "down", 0), Resolved::Unbound);
    }

    #[test]
    fn should_accept_sequences_and_alternatives_when_overriding() {
        let overrides = BTreeMap::from([
            ("top".to_owned(), "space t, home".to_owned()),
            ("align_top".to_owned(), "z h".to_owned()),
        ]);
        let mut map = Keymap::new(&overrides).unwrap();
        assert_eq!(press(&mut map, "home", 0), action(Action::Top));
        press(&mut map, "space", 1);
        assert_eq!(press(&mut map, "t", 2), action(Action::Top));
        press(&mut map, "z", 3);
        assert_eq!(press(&mut map, "h", 4), action(Action::AlignTop));
        // The old `gg` is gone.
        assert_eq!(press(&mut map, "g", 5), Resolved::Unbound);
    }

    #[test]
    fn should_name_the_action_when_an_override_is_unknown() {
        let overrides = BTreeMap::from([("jump_around".to_owned(), "j".to_owned())]);
        let error = Keymap::new(&overrides).unwrap_err();
        assert!(error.contains("jump_around"), "{error}");
        assert!(
            error.contains("next_block"),
            "known actions are listed: {error}"
        );
    }

    #[test]
    fn should_name_the_action_and_text_when_a_binding_does_not_parse() {
        let overrides = BTreeMap::from([("next_block".to_owned(), "hyper+j".to_owned())]);
        let error = Keymap::new(&overrides).unwrap_err();
        assert!(
            error.contains("next_block") && error.contains("hyper"),
            "{error}"
        );
        let empty = BTreeMap::from([("next_block".to_owned(), "".to_owned())]);
        assert!(Keymap::new(&empty).is_err());
        let trailing = BTreeMap::from([("next_block".to_owned(), "j,".to_owned())]);
        assert!(Keymap::new(&trailing).is_err());
    }

    #[test]
    fn should_reject_a_prefix_conflict_when_a_key_starts_a_sequence() {
        let overrides = BTreeMap::from([("reload".to_owned(), "g".to_owned())]);
        let error = Keymap::new(&overrides).unwrap_err();
        assert!(error.contains("reload") && error.contains("top"), "{error}");
    }

    #[test]
    fn should_not_let_digits_be_remapped_when_naming_pick_option() {
        assert_eq!(Action::from_name("pick_option"), None);
        assert_eq!(Action::PickOption(3).name(), "pick_option");
    }

    #[test]
    fn should_round_trip_every_action_name_when_listing_the_table() {
        for (action, name, default) in Action::all() {
            assert_eq!(Action::from_name(name), Some(action));
            assert_eq!(action.name(), name);
            assert!(parse_alternatives(default).is_ok(), "{name}: {default}");
        }
    }

    #[test]
    fn should_list_the_default_bindings_when_asking_for_an_action() {
        assert_eq!(map().bindings_of(Action::NextBlock), vec!["j", "down"]);
        assert_eq!(map().bindings_of(Action::Top), vec!["g g"]);
        assert_eq!(map().bindings_of(Action::Bottom), vec!["shift+g"]);
    }

    #[test]
    fn should_forget_everything_when_reset() {
        let mut map = map();
        press(&mut map, "3", 0);
        press(&mut map, "g", 1);
        assert_eq!(map.pending_text(), "3 g");
        map.reset();
        assert!(!map.has_pending());
        assert_eq!(map.pending_text(), "");
    }
}
