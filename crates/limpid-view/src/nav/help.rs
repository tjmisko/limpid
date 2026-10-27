//! The `?` key reference: a dialog listing the live keymap, so remapped keys
//! show as the user set them.

use super::keymap::{Action, Chord, Keymap};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::{Color, Theme},
};

/// What the reference shows: the rows from `scroll` on, and the footer line.
pub struct View<'a> {
    pub rows: &'a [Row],
    pub scroll: usize,
    pub hint: &'a str,
}

/// One line of the reference: the keys and what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub keys: String,
    pub what: &'static str,
}

/// Rows moved by `Ctrl-d` and `Ctrl-u`.
const PAGE_ROWS: usize = 8;

const HEADING: &str = "KEYS";

/// The footer line, naming whatever keys currently toggle the reference.
pub fn hint(close: &[String]) -> String {
    if close.is_empty() {
        return "Esc to close".to_owned();
    }
    let keys: Vec<String> = close
        .iter()
        .map(|binding| pretty_binding(binding))
        .collect();
    format!("{} or Esc to close", keys.join(" or "))
}

fn describe(action: Action) -> &'static str {
    match action {
        Action::NextBlock => "Next block (e.g. 3j)",
        Action::PrevBlock => "Previous block",
        Action::HalfPageDown => "Half a page down",
        Action::HalfPageUp => "Half a page up",
        Action::SectionNext => "Next section (e.g. 2})",
        Action::SectionPrev => "Previous section",
        Action::Top => "First block",
        Action::Bottom => "Last block",
        Action::AlignTop => "Block to top",
        Action::AlignCenter => "Block to center",
        Action::AlignBottom => "Block to bottom",
        Action::ScrollLeft => "Scroll the code block left",
        Action::ScrollRight => "Scroll the code block right",
        Action::OpenReply => "Edit the focused reply or comment",
        Action::OpenComment => "Comment on the focused block",
        Action::Insert => "Edit at the cursor",
        Action::InsertStart => "Edit at the start",
        Action::InsertEnd => "Edit at the end",
        Action::DeleteComment => "Delete the focused comment",
        Action::Review => "Review the composed reply",
        Action::FirstUnanswered => "First unanswered question",
        Action::ToggleRail => "Toggle the rail",
        Action::SearchOpen => "Search",
        Action::SearchNext => "Next match",
        Action::SearchPrev => "Previous match",
        Action::CommandOpen => "Command line",
        Action::Visual => "Visual selection",
        Action::Yank => "Yank: yy block, yis section, y j / k blocks",
        Action::YankPage => "Yank the whole page",
        Action::ZoomIn => "Zoom in",
        Action::ZoomOut => "Zoom out",
        Action::ZoomReset => "Reset zoom",
        Action::Reload => "Reload",
        Action::Quit => "Quit",
        Action::Help => "Toggle this keymap",
        Action::Clear => "Clear selection and search highlight",
        Action::PickOption(_) => "Pick a question option",
    }
}

/// A key as a person would say it: `Ctrl-d`, `G`, `PageDown`.
fn pretty_chord(chord: &Chord) -> String {
    let mut text = String::new();
    if chord.ctrl {
        text.push_str("Ctrl-");
    }
    if chord.alt {
        text.push_str("Alt-");
    }
    let mut chars = chord.key.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    };
    match single {
        Some(only) if chord.shift && only.is_alphabetic() => text.extend(only.to_uppercase()),
        Some(only) => {
            if chord.shift {
                text.push_str("Shift-");
            }
            text.push(only);
        }
        None => {
            if chord.shift {
                text.push_str("Shift-");
            }
            text.push_str(&named(&chord.key));
        }
    }
    text
}

fn named(key: &str) -> String {
    match key {
        "pagedown" => "PageDown".into(),
        "pageup" => "PageUp".into(),
        "escape" => "Esc".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

/// A binding as `bindings_of` prints it (`"g g"`, `"ctrl+d"`) in prose.
fn pretty_binding(binding: &str) -> String {
    let parts: Vec<String> = binding
        .split_whitespace()
        .map(|part| match Chord::parse(part) {
            Ok(chord) => pretty_chord(&chord),
            Err(_) => part.to_owned(),
        })
        .collect();
    // `g g` reads as `gg`; longer names keep their space.
    if parts.iter().all(|part| part.chars().count() == 1) {
        parts.concat()
    } else {
        parts.join(" ")
    }
}

/// The reference for `keymap`: one row per action with every key bound to it,
/// then the keys that cannot be remapped.
pub fn rows(keymap: &Keymap) -> Vec<Row> {
    let mut rows = Vec::new();
    for (action, _, _) in Action::all() {
        let bindings = keymap.bindings_of(action);
        if bindings.is_empty() {
            continue;
        }
        let keys = bindings
            .iter()
            .map(|binding| pretty_binding(binding))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(Row {
            keys,
            what: describe(action),
        });
    }
    rows.push(Row {
        keys: "1-9".into(),
        what: describe(Action::PickOption(1)),
    });
    rows.push(Row {
        keys: ":w  :wq  :q!  :qa".into(),
        what: "Save / save and send / discard / close all",
    });
    rows
}

/// What a key did to the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpKey {
    Ignored,
    Moved,
    Close,
}

/// Handle a key while the dialog is open. `close` is the text of the keys
/// bound to [`Action::Help`]. Every other key is swallowed.
pub fn handle(chord: &Chord, scroll: &mut usize, total: usize, close: &[String]) -> HelpKey {
    let last = total.saturating_sub(1);
    let before = *scroll;
    let typed = chord.to_string();
    if chord.key == "escape" || (chord.key == "c" && chord.ctrl) || close.contains(&typed) {
        return HelpKey::Close;
    }
    match (chord.key.as_str(), chord.ctrl) {
        ("j" | "down", false) => *scroll = (*scroll + 1).min(last),
        ("k" | "up", false) => *scroll = scroll.saturating_sub(1),
        ("d", true) => *scroll = (*scroll + PAGE_ROWS).min(last),
        ("u", true) => *scroll = scroll.saturating_sub(PAGE_ROWS),
        _ => {}
    }
    if *scroll == before {
        HelpKey::Ignored
    } else {
        HelpKey::Moved
    }
}

fn shape(fonts: &mut Fonts, theme: &Theme, size: f32, text: &str, style: TextStyle) -> TextBlock {
    let mut block = TextBlock::new(fonts, size, size * 1.4);
    block.set_text(fonts, text, style, &theme.families);
    block.set_width(fonts, None);
    block
}

/// Paint the dialog centred over a `canvas` whose bottom `reserved` pixels
/// belong to the status bar, dimming the page behind it.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    theme: &Theme,
    scale: f32,
    reserved: f32,
    view: &View,
) {
    let (width, total) = (canvas.width() as f32, canvas.height() as f32 - reserved);
    if width < 80.0 || total < 80.0 {
        return;
    }
    canvas.fill_rect(
        Rect::new(0.0, 0.0, width, total),
        Color::rgba(0, 0, 0, 0x99),
    );
    let (pad_x, pad_y) = (26.0 * scale, 22.0 * scale);
    let panel_width = (560.0 * scale).min(width - 32.0 * scale);
    let key_size = (12.5 * scale).round().max(8.0);
    let what_size = (14.0 * scale).round().max(8.0);
    let small = (12.0 * scale).round().max(8.0);
    let row_height = what_size * 1.4 + 7.0 * scale;
    let head_height = small * 1.4 + 14.0 * scale;
    let foot_height = small * 1.4 + 16.0 * scale;
    let room = 0.8 * total - 2.0 * pad_y - head_height - foot_height;
    let capacity = ((room / row_height).floor() as usize).max(1);
    let View { rows, scroll, hint } = *view;
    let shown: Vec<&Row> = rows.iter().skip(scroll).take(capacity).collect();
    let panel_height = 2.0 * pad_y + head_height + foot_height + shown.len() as f32 * row_height;
    let panel = Rect::new(
        (width - panel_width) / 2.0,
        ((total - panel_height) / 2.0).max(0.0),
        panel_width,
        panel_height,
    );
    let radius = 12.0 * scale;
    canvas.fill_rounded_rect(panel, radius, theme.surface);
    canvas.stroke_rounded_rect(panel, radius, scale.round().max(1.0), theme.rule);

    let heading = shape(fonts, theme, small, HEADING, TextStyle::sans(theme.faint));
    canvas.draw_text(
        fonts,
        &heading,
        panel.x + pad_x,
        panel.y + pad_y,
        theme.faint,
    );

    let keys: Vec<TextBlock> = shown
        .iter()
        .map(|row| {
            shape(
                fonts,
                theme,
                key_size,
                &row.keys,
                TextStyle::mono(theme.teal),
            )
        })
        .collect();
    let key_column = keys
        .iter()
        .map(|block| {
            block
                .glyphs()
                .map(|glyph| glyph.x + glyph.width)
                .fold(0.0, f32::max)
        })
        .fold(0.0, f32::max)
        .min(panel_width * 0.5);
    let what_x = panel.x + pad_x + key_column + 20.0 * scale;
    let mut y = panel.y + pad_y + head_height;
    for (row, key_block) in shown.iter().zip(&keys) {
        canvas.draw_text(fonts, key_block, panel.x + pad_x, y + scale, theme.teal);
        let what = shape(
            fonts,
            theme,
            what_size,
            row.what,
            TextStyle::sans(theme.ink),
        );
        canvas.draw_text(fonts, &what, what_x, y, theme.ink);
        y += row_height;
    }
    let hint = shape(fonts, theme, small, hint, TextStyle::sans(theme.faint));
    canvas.draw_text(fonts, &hint, panel.x + pad_x, y + 16.0 * scale, theme.faint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn keys_of(rows: &[Row], what: &str) -> String {
        rows.iter()
            .find(|row| row.what == what)
            .map(|row| row.keys.clone())
            .unwrap_or_default()
    }

    #[test]
    fn should_name_the_remapped_key_when_building_the_hint() {
        assert_eq!(hint(&["?".to_owned()]), "? or Esc to close");
        assert_eq!(hint(&["F2".to_owned()]), "F2 or Esc to close");
        assert_eq!(hint(&[]), "Esc to close");
    }

    #[test]
    fn should_list_every_action_with_its_default_keys_when_building_rows() {
        let rows = rows(&Keymap::defaults());
        assert_eq!(keys_of(&rows, "Next block (e.g. 3j)"), "j, Down");
        assert_eq!(keys_of(&rows, "First block"), "gg");
        assert_eq!(keys_of(&rows, "Last block"), "G");
        assert_eq!(keys_of(&rows, "Half a page down"), "Ctrl-d, PageDown");
        assert_eq!(keys_of(&rows, "Toggle this keymap"), "?");
        assert_eq!(keys_of(&rows, "Quit"), "q, Ctrl-q, Ctrl-w");
    }

    #[test]
    fn should_show_the_remapped_keys_when_the_keymap_has_overrides() {
        let mut keys = BTreeMap::new();
        keys.insert("next_block".to_owned(), "ctrl+n".to_owned());
        keys.insert("top".to_owned(), "home".to_owned());
        let rows = rows(&Keymap::new(&keys).unwrap());
        assert_eq!(keys_of(&rows, "Next block (e.g. 3j)"), "Ctrl-n");
        assert_eq!(keys_of(&rows, "First block"), "Home");
    }

    #[test]
    fn should_close_on_escape_and_the_help_key_when_open() {
        let close = vec!["?".to_owned()];
        for key in ["escape", "?"] {
            let chord = Chord::parse(key).unwrap();
            assert_eq!(handle(&chord, &mut 0, 10, &close), HelpKey::Close, "{key}");
        }
    }

    #[test]
    fn should_swallow_other_keys_and_scroll_within_the_rows_when_open() {
        let close = vec!["?".to_owned()];
        let mut scroll = 0;
        let x = Chord::parse("x").unwrap();
        assert_eq!(handle(&x, &mut scroll, 3, &close), HelpKey::Ignored);
        let j = Chord::parse("j").unwrap();
        assert_eq!(handle(&j, &mut scroll, 3, &close), HelpKey::Moved);
        handle(&j, &mut scroll, 3, &close);
        assert_eq!(handle(&j, &mut scroll, 3, &close), HelpKey::Ignored);
        assert_eq!(scroll, 2, "never past the last row");
    }

    #[test]
    fn should_draw_inside_the_window_when_painting_small_and_large_canvases() {
        let theme = Theme::dark();
        let mut fonts = Fonts::new();
        let rows = rows(&Keymap::defaults());
        for (w, h) in [(1u32, 1u32), (50, 50), (300, 200), (900, 700)] {
            let mut pixels = vec![0u32; (w * h) as usize];
            let mut canvas = Canvas::new(&mut pixels, w, h);
            paint(
                &mut canvas,
                &mut fonts,
                &theme,
                1.0,
                20.0,
                &View {
                    rows: &rows,
                    scroll: 0,
                    hint: "? or Esc to close",
                },
            );
        }
        let mut pixels = vec![0u32; 900 * 700];
        let mut canvas = Canvas::new(&mut pixels, 900, 700);
        canvas.clear(theme.bg);
        paint(
            &mut canvas,
            &mut fonts,
            &theme,
            1.0,
            20.0,
            &View {
                rows: &rows,
                scroll: 0,
                hint: "? or Esc to close",
            },
        );
        assert_ne!(canvas.pixel(450, 350), Some(theme.bg.to_pixel()));
        assert_ne!(
            canvas.pixel(5, 5),
            Some(theme.bg.to_pixel()),
            "page is dimmed"
        );
    }
}
