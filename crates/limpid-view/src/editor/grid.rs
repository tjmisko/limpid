//! The character grid nvim draws, rebuilt from `redraw` notifications.
//!
//! Only the default grid (id 1) is mirrored: the editor attaches with
//! `ext_linegrid`, not `ext_multigrid`.

use rmpv::Value;
use std::collections::HashMap;

/// Marks the right half of a double-width character.
pub const CONTINUATION: char = '\0';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub hl: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', hl: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    /// A thin vertical line (nvim's "vertical").
    Bar,
    /// A line along the bottom of the cell (nvim's "horizontal").
    Underline,
}

/// Attributes of one highlight id, as far as limpid cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HlAttr {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub sp: Option<u32>,
    pub reverse: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub strikethrough: bool,
    /// The highlight group name from `ext_hlstate` (`Visual`, `Search`...).
    pub ui_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeInfo {
    pub name: String,
    pub shape: CursorShape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<Cell>,
    /// `(row, col)`.
    pub cursor: (u16, u16),
    pub cursor_shape: CursorShape,
    pub highlights: HashMap<u64, HlAttr>,
    pub default_fg: Option<u32>,
    pub default_bg: Option<u32>,
    pub mode_infos: Vec<ModeInfo>,
    pub mode: String,
}

/// What a batch of redraw events did besides changing cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedrawEffects {
    /// A `flush` arrived: the grid is consistent and worth repainting.
    pub flushed: bool,
    pub mode: Option<String>,
    /// `Some(Some(text))` shows the command line, `Some(None)` hides it.
    pub cmdline: Option<Option<String>>,
    /// `(kind, text)` per `msg_show`.
    pub messages: Vec<(String, String)>,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Grid {
        Grid {
            cols,
            rows,
            cells: vec![Cell::default(); usize::from(cols) * usize::from(rows)],
            cursor: (0, 0),
            cursor_shape: CursorShape::Block,
            highlights: HashMap::new(),
            default_fg: None,
            default_bg: None,
            mode_infos: Vec::new(),
            mode: "normal".into(),
        }
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// Show `text` as plain cells, wrapped at the grid width, with the cursor
    /// at the top. Stands in for nvim's first redraw so a box opens showing
    /// its text instead of a blank or the previous box.
    pub fn show_text(&mut self, text: &str) {
        self.cells.fill(Cell::default());
        self.cursor = (0, 0);
        let cols = usize::from(self.cols);
        if cols == 0 {
            return;
        }
        let mut row = 0;
        for line in text.lines() {
            let chars: Vec<char> = line.chars().collect();
            for chunk in chars
                .chunks(cols)
                .chain(chars.is_empty().then_some(&[][..]))
            {
                if row >= usize::from(self.rows) {
                    return;
                }
                for (col, ch) in chunk.iter().enumerate() {
                    self.cells[row * cols + col].ch = *ch;
                }
                row += 1;
            }
        }
    }

    /// The text of one row, continuation cells dropped, trailing blanks kept.
    pub fn row_text(&self, row: u16) -> String {
        (0..self.cols)
            .filter_map(|col| self.cell(row, col))
            .filter(|cell| cell.ch != CONTINUATION)
            .map(|cell| cell.ch)
            .collect()
    }

    /// Apply one `redraw` notification's params (a list of events).
    /// Malformed events are skipped, never fatal.
    pub fn apply_redraw(&mut self, events: &[Value]) -> RedrawEffects {
        let mut effects = RedrawEffects::default();
        for event in events {
            let Some(parts) = event.as_array() else {
                continue;
            };
            let Some((name, batches)) = parts.split_first() else {
                continue;
            };
            let Some(name) = name.as_str() else {
                continue;
            };
            if batches.is_empty() {
                self.apply_event(name, &[], &mut effects);
            }
            for args in batches {
                let Some(args) = args.as_array() else {
                    continue;
                };
                self.apply_event(name, args, &mut effects);
            }
        }
        effects
    }

    fn apply_event(&mut self, name: &str, args: &[Value], effects: &mut RedrawEffects) {
        match name {
            "grid_resize" => self.on_resize(args),
            "grid_line" => self.on_line(args),
            "grid_clear" => self.on_clear(args),
            "grid_cursor_goto" => self.on_cursor_goto(args),
            "grid_scroll" => self.on_scroll(args),
            "hl_attr_define" => self.on_hl_attr_define(args),
            "default_colors_set" => self.on_default_colors(args),
            "mode_info_set" => self.on_mode_info_set(args),
            "mode_change" => self.on_mode_change(args, effects),
            "cmdline_show" => on_cmdline_show(args, effects),
            "cmdline_hide" => effects.cmdline = Some(None),
            "msg_show" => on_msg_show(args, effects),
            "flush" => effects.flushed = true,
            _ => {}
        }
    }

    fn on_resize(&mut self, args: &[Value]) {
        if !is_default_grid(args) {
            return;
        }
        let (Some(cols), Some(rows)) = (arg_u16(args, 1), arg_u16(args, 2)) else {
            return;
        };
        let mut resized = Grid::new(cols, rows).cells;
        for row in 0..self.rows.min(rows) {
            for col in 0..self.cols.min(cols) {
                resized[usize::from(row) * usize::from(cols) + usize::from(col)] =
                    self.cells[usize::from(row) * usize::from(self.cols) + usize::from(col)];
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = resized;
        self.cursor.0 = self.cursor.0.min(rows.saturating_sub(1));
        self.cursor.1 = self.cursor.1.min(cols.saturating_sub(1));
    }

    fn on_line(&mut self, args: &[Value]) {
        if !is_default_grid(args) {
            return;
        }
        let (Some(row), Some(mut col)) = (arg_u16(args, 1), arg_u16(args, 2)) else {
            return;
        };
        let Some(cells) = args.get(3).and_then(Value::as_array) else {
            return;
        };
        if row >= self.rows {
            return;
        }
        let mut hl = 0;
        for cell in cells {
            let Some(parts) = cell.as_array() else {
                continue;
            };
            let Some(text) = parts.first().and_then(Value::as_str) else {
                continue;
            };
            if let Some(next_hl) = parts.get(1).and_then(Value::as_u64) {
                hl = next_hl;
            }
            let repeat = parts.get(2).and_then(Value::as_u64).unwrap_or(1);
            // An empty string is the right half of a double-width cell.
            let ch = text.chars().next().unwrap_or(CONTINUATION);
            for _ in 0..repeat {
                if col >= self.cols {
                    return;
                }
                let index = usize::from(row) * usize::from(self.cols) + usize::from(col);
                self.cells[index] = Cell { ch, hl };
                col += 1;
            }
        }
    }

    fn on_clear(&mut self, args: &[Value]) {
        if !is_default_grid(args) {
            return;
        }
        self.cells.fill(Cell::default());
    }

    fn on_cursor_goto(&mut self, args: &[Value]) {
        if !is_default_grid(args) {
            return;
        }
        let (Some(row), Some(col)) = (arg_u16(args, 1), arg_u16(args, 2)) else {
            return;
        };
        self.cursor = (
            row.min(self.rows.saturating_sub(1)),
            col.min(self.cols.saturating_sub(1)),
        );
    }

    /// `grid_scroll`: rows `top..bot`, columns `left..right`; positive
    /// `rows` moves the content up.
    fn on_scroll(&mut self, args: &[Value]) {
        if !is_default_grid(args) {
            return;
        }
        let (Some(top), Some(bottom), Some(left), Some(right)) = (
            arg_u16(args, 1),
            arg_u16(args, 2),
            arg_u16(args, 3),
            arg_u16(args, 4),
        ) else {
            return;
        };
        let Some(shift) = args.get(5).and_then(Value::as_i64) else {
            return;
        };
        let bottom = bottom.min(self.rows);
        let right = right.min(self.cols);
        if shift == 0 || top >= bottom || left >= right {
            return;
        }
        let height = i64::from(bottom - top);
        if shift.abs() >= height {
            return;
        }
        let rows_in_order: Vec<i64> = if shift > 0 {
            (i64::from(top)..i64::from(bottom) - shift).collect()
        } else {
            (i64::from(top) - shift..i64::from(bottom)).rev().collect()
        };
        for destination in rows_in_order {
            let source = destination + shift;
            for col in left..right {
                let from = source as usize * usize::from(self.cols) + usize::from(col);
                let to = destination as usize * usize::from(self.cols) + usize::from(col);
                self.cells[to] = self.cells[from];
            }
        }
    }

    fn on_hl_attr_define(&mut self, args: &[Value]) {
        let Some(id) = args.first().and_then(Value::as_u64) else {
            return;
        };
        let mut attr = HlAttr::default();
        if let Some(Value::Map(entries)) = args.get(1) {
            for (key, value) in entries {
                let Some(key) = key.as_str() else {
                    continue;
                };
                match key {
                    "foreground" => attr.fg = value.as_u64().map(|v| v as u32),
                    "background" => attr.bg = value.as_u64().map(|v| v as u32),
                    "special" => attr.sp = value.as_u64().map(|v| v as u32),
                    "reverse" => attr.reverse = value.as_bool().unwrap_or(false),
                    "bold" => attr.bold = value.as_bool().unwrap_or(false),
                    "italic" => attr.italic = value.as_bool().unwrap_or(false),
                    "underline" => attr.underline = value.as_bool().unwrap_or(false),
                    "undercurl" => attr.undercurl = value.as_bool().unwrap_or(false),
                    "strikethrough" => attr.strikethrough = value.as_bool().unwrap_or(false),
                    _ => {}
                }
            }
        }
        attr.ui_name = args
            .get(3)
            .and_then(Value::as_array)
            .and_then(|info| info.last())
            .and_then(|entry| map_get(entry, "hi_name"))
            .and_then(|name| name.as_str().map(str::to_string));
        self.highlights.insert(id, attr);
    }

    fn on_default_colors(&mut self, args: &[Value]) {
        self.default_fg = args.first().and_then(Value::as_u64).map(|v| v as u32);
        self.default_bg = args.get(1).and_then(Value::as_u64).map(|v| v as u32);
    }

    fn on_mode_info_set(&mut self, args: &[Value]) {
        let Some(modes) = args.get(1).and_then(Value::as_array) else {
            return;
        };
        self.mode_infos = modes
            .iter()
            .map(|mode| ModeInfo {
                name: map_get(mode, "name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                shape: match map_get(mode, "cursor_shape").and_then(Value::as_str) {
                    Some("vertical") => CursorShape::Bar,
                    Some("horizontal") => CursorShape::Underline,
                    _ => CursorShape::Block,
                },
            })
            .collect();
    }

    fn on_mode_change(&mut self, args: &[Value], effects: &mut RedrawEffects) {
        let Some(name) = args.first().and_then(Value::as_str) else {
            return;
        };
        self.mode = name.to_string();
        let by_index = args
            .get(1)
            .and_then(Value::as_u64)
            .and_then(|index| self.mode_infos.get(index as usize));
        let by_name = self.mode_infos.iter().find(|info| info.name == name);
        self.cursor_shape = by_index.or(by_name).map_or(CursorShape::Block, |m| m.shape);
        effects.mode = Some(name.to_string());
    }
}

fn on_cmdline_show(args: &[Value], effects: &mut RedrawEffects) {
    let Some(content) = args.first().and_then(Value::as_array) else {
        return;
    };
    let typed: String = content
        .iter()
        .filter_map(|chunk| chunk.as_array())
        .filter_map(|chunk| chunk.get(1).and_then(Value::as_str))
        .collect();
    let first_character = args.get(2).and_then(Value::as_str).unwrap_or_default();
    effects.cmdline = Some(Some(format!("{first_character}{typed}")));
}

fn on_msg_show(args: &[Value], effects: &mut RedrawEffects) {
    let kind = args
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(content) = args.get(1).and_then(Value::as_array) else {
        return;
    };
    let text: String = content
        .iter()
        .filter_map(|chunk| chunk.as_array())
        .filter_map(|chunk| chunk.get(1).and_then(Value::as_str))
        .collect();
    if text.is_empty() {
        return;
    }
    effects.messages.push((kind, text));
}

/// Look up a string key in a msgpack map.
fn map_get<'a>(map: &'a Value, key: &str) -> Option<&'a Value> {
    map.as_map()?
        .iter()
        .find(|(candidate, _)| candidate.as_str() == Some(key))
        .map(|(_, value)| value)
}

fn is_default_grid(args: &[Value]) -> bool {
    args.first().and_then(Value::as_u64) == Some(1)
}

fn arg_u16(args: &[Value], index: usize) -> Option<u16> {
    args.get(index)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(value: impl Into<Value>) -> Value {
        value.into()
    }

    fn array(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    fn event(name: &str, batches: Vec<Vec<Value>>) -> Value {
        let mut parts = vec![v(name)];
        parts.extend(batches.into_iter().map(array));
        array(parts)
    }

    fn line(row: u64, col: u64, cells: Vec<Value>) -> Vec<Value> {
        vec![v(1), v(row), v(col), array(cells), v(false)]
    }

    fn cell(text: &str, hl: Option<u64>, repeat: Option<u64>) -> Value {
        let mut parts = vec![v(text)];
        if let Some(hl) = hl {
            parts.push(v(hl));
        }
        if let Some(repeat) = repeat {
            parts.push(v(repeat));
        }
        array(parts)
    }

    fn resized(cols: u16, rows: u16) -> Grid {
        let mut grid = Grid::new(0, 0);
        grid.apply_redraw(&[event("grid_resize", vec![vec![v(1), v(cols), v(rows)]])]);
        grid
    }

    #[test]
    fn should_wrap_and_clip_text_when_showing_it_before_nvim_draws() {
        let mut grid = Grid::new(4, 3);
        grid.cursor = (2, 2);
        grid.show_text("abcdef\n\nxyz\nlost");
        assert_eq!(grid.row_text(0), "abcd");
        assert_eq!(grid.row_text(1), "ef  ");
        assert_eq!(grid.row_text(2), "    ", "an empty line takes a row");
        assert_eq!(grid.cursor, (0, 0));
        grid.show_text("");
        assert!((0..3).all(|row| grid.row_text(row) == "    "));
    }

    #[test]
    fn should_size_the_grid_when_grid_resize_arrives() {
        let grid = resized(10, 3);
        assert_eq!((grid.cols, grid.rows), (10, 3));
        assert_eq!(grid.cells.len(), 30);
    }

    #[test]
    fn should_write_text_when_grid_line_arrives() {
        let mut grid = resized(10, 2);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(
                0,
                2,
                vec![cell("h", Some(0), None), cell("i", None, None)],
            )],
        )]);
        assert_eq!(grid.row_text(0), "  hi      ");
    }

    #[test]
    fn should_repeat_and_inherit_highlight_when_cells_omit_them() {
        let mut grid = resized(6, 1);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(
                0,
                0,
                vec![
                    cell("a", Some(7), Some(2)),
                    cell("b", None, None),
                    cell("c", Some(3), None),
                ],
            )],
        )]);
        let hls: Vec<u64> = grid.cells[..4].iter().map(|c| c.hl).collect();
        assert_eq!(hls, vec![7, 7, 7, 3]);
        assert_eq!(grid.row_text(0).trim_end(), "aabc");
    }

    #[test]
    fn should_mark_continuation_when_a_wide_character_has_an_empty_second_cell() {
        let mut grid = resized(4, 1);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(
                0,
                0,
                vec![cell("\u{4e2d}", Some(0), None), cell("", None, None)],
            )],
        )]);
        assert_eq!(grid.cell(0, 1).unwrap().ch, CONTINUATION);
        assert_eq!(grid.row_text(0).trim_end(), "\u{4e2d}");
    }

    #[test]
    fn should_clip_without_panicking_when_a_line_overruns_the_grid() {
        let mut grid = resized(3, 1);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![
                line(0, 1, vec![cell("x", Some(0), Some(50))]),
                line(9, 0, vec![cell("y", Some(0), None)]),
                line(0, 99, vec![cell("z", Some(0), None)]),
            ],
        )]);
        assert_eq!(grid.row_text(0), " xx");
    }

    #[test]
    fn should_blank_every_cell_when_grid_clear_arrives() {
        let mut grid = resized(3, 1);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(0, 0, vec![cell("a", Some(2), Some(3))])],
        )]);
        grid.apply_redraw(&[event("grid_clear", vec![vec![v(1)]])]);
        assert_eq!(grid.row_text(0), "   ");
        assert!(grid.cells.iter().all(|c| c.hl == 0));
    }

    #[test]
    fn should_move_rows_up_when_grid_scroll_is_positive() {
        let mut grid = resized(2, 3);
        for (row, ch) in ["a", "b", "c"].iter().enumerate() {
            grid.apply_redraw(&[event(
                "grid_line",
                vec![line(row as u64, 0, vec![cell(ch, Some(0), Some(2))])],
            )]);
        }
        grid.apply_redraw(&[event(
            "grid_scroll",
            vec![vec![v(1), v(0), v(3), v(0), v(2), v(1), v(0)]],
        )]);
        assert_eq!(grid.row_text(0), "bb");
        assert_eq!(grid.row_text(1), "cc");
    }

    #[test]
    fn should_move_rows_down_when_grid_scroll_is_negative() {
        let mut grid = resized(2, 3);
        for (row, ch) in ["a", "b", "c"].iter().enumerate() {
            grid.apply_redraw(&[event(
                "grid_line",
                vec![line(row as u64, 0, vec![cell(ch, Some(0), Some(2))])],
            )]);
        }
        grid.apply_redraw(&[event(
            "grid_scroll",
            vec![vec![v(1), v(0), v(3), v(0), v(2), v(-1), v(0)]],
        )]);
        assert_eq!(grid.row_text(1), "aa");
        assert_eq!(grid.row_text(2), "bb");
    }

    #[test]
    fn should_ignore_a_scroll_when_it_exceeds_the_region() {
        let mut grid = resized(2, 2);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(0, 0, vec![cell("a", Some(0), Some(2))])],
        )]);
        grid.apply_redraw(&[event(
            "grid_scroll",
            vec![vec![v(1), v(0), v(2), v(0), v(2), v(5), v(0)]],
        )]);
        assert_eq!(grid.row_text(0), "aa");
    }

    #[test]
    fn should_clamp_the_cursor_when_it_lands_outside() {
        let mut grid = resized(4, 2);
        grid.apply_redraw(&[event("grid_cursor_goto", vec![vec![v(1), v(9), v(9)]])]);
        assert_eq!(grid.cursor, (1, 3));
    }

    #[test]
    fn should_keep_overlapping_content_when_the_grid_shrinks() {
        let mut grid = resized(4, 2);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![line(0, 0, vec![cell("w", Some(0), Some(4))])],
        )]);
        grid.apply_redraw(&[event("grid_resize", vec![vec![v(1), v(2), v(1)]])]);
        assert_eq!(grid.row_text(0), "ww");
        assert_eq!(grid.cells.len(), 2);
    }

    #[test]
    fn should_ignore_other_grids_when_events_name_a_different_id() {
        let mut grid = resized(3, 1);
        grid.apply_redraw(&[event(
            "grid_line",
            vec![vec![
                v(2),
                v(0),
                v(0),
                array(vec![cell("q", Some(0), None)]),
                v(false),
            ]],
        )]);
        assert_eq!(grid.row_text(0), "   ");
    }

    #[test]
    fn should_skip_malformed_events_when_arguments_are_missing() {
        let mut grid = resized(3, 1);
        let effects = grid.apply_redraw(&[
            v("not an event"),
            array(vec![]),
            event("grid_line", vec![vec![v(1)]]),
            event("grid_resize", vec![vec![v(1), v("wide"), v(1)]]),
            event(
                "grid_line",
                vec![vec![v(1), v(0), v(0), v("cells"), v(false)]],
            ),
            event("unknown_event", vec![vec![v(1)]]),
        ]);
        assert_eq!(effects, RedrawEffects::default());
        assert_eq!((grid.cols, grid.rows), (3, 1));
    }

    #[test]
    fn should_follow_the_mode_shape_when_mode_change_arrives() {
        let mut grid = resized(3, 1);
        let modes = array(vec![
            Value::Map(vec![
                (v("name"), v("normal")),
                (v("cursor_shape"), v("block")),
            ]),
            Value::Map(vec![
                (v("name"), v("insert")),
                (v("cursor_shape"), v("vertical")),
            ]),
            Value::Map(vec![
                (v("name"), v("replace")),
                (v("cursor_shape"), v("horizontal")),
            ]),
        ]);
        grid.apply_redraw(&[event("mode_info_set", vec![vec![v(true), modes]])]);
        let effects = grid.apply_redraw(&[event("mode_change", vec![vec![v("insert"), v(1)]])]);
        assert_eq!(effects.mode.as_deref(), Some("insert"));
        assert_eq!(grid.cursor_shape, CursorShape::Bar);
        grid.apply_redraw(&[event("mode_change", vec![vec![v("replace"), v(2)]])]);
        assert_eq!(grid.cursor_shape, CursorShape::Underline);
        grid.apply_redraw(&[event("mode_change", vec![vec![v("normal"), v(0)]])]);
        assert_eq!(grid.cursor_shape, CursorShape::Block);
    }

    #[test]
    fn should_record_attributes_and_group_name_when_hl_attr_define_arrives() {
        let mut grid = resized(3, 1);
        let rgb = Value::Map(vec![
            (v("background"), v(0x112233u64)),
            (v("bold"), v(true)),
        ]);
        let info = array(vec![Value::Map(vec![
            (v("kind"), v("ui")),
            (v("ui_name"), v("Visual")),
            (v("hi_name"), v("Visual")),
            (v("id"), v(5)),
        ])]);
        grid.apply_redraw(&[event(
            "hl_attr_define",
            vec![vec![v(5), rgb, Value::Map(vec![]), info]],
        )]);
        let attr = grid.highlights.get(&5).unwrap();
        assert_eq!(attr.bg, Some(0x112233));
        assert!(attr.bold);
        assert_eq!(attr.ui_name.as_deref(), Some("Visual"));
    }

    #[test]
    fn should_report_command_line_text_when_cmdline_show_and_hide_arrive() {
        let mut grid = resized(3, 1);
        let content = array(vec![array(vec![Value::Map(vec![]), v("wq")])]);
        let shown = grid.apply_redraw(&[event(
            "cmdline_show",
            vec![vec![content, v(2), v(":"), v(""), v(0), v(1)]],
        )]);
        assert_eq!(shown.cmdline, Some(Some(":wq".to_string())));
        let hidden = grid.apply_redraw(&[event("cmdline_hide", vec![vec![v(1)]])]);
        assert_eq!(hidden.cmdline, Some(None));
    }

    #[test]
    fn should_collect_message_text_when_msg_show_arrives() {
        let mut grid = resized(3, 1);
        let content = array(vec![
            array(vec![v(0), v("E37: ")]),
            array(vec![v(0), v("No write since last change")]),
        ]);
        let effects = grid.apply_redraw(&[event(
            "msg_show",
            vec![
                vec![v("emsg"), content, v(false)],
                vec![v("echo"), array(vec![]), v(false)],
            ],
        )]);
        assert_eq!(
            effects.messages,
            vec![(
                "emsg".to_string(),
                "E37: No write since last change".to_string()
            )]
        );
    }

    #[test]
    fn should_mark_the_batch_flushed_when_flush_arrives() {
        let mut grid = resized(3, 1);
        assert!(
            !grid
                .apply_redraw(&[event("grid_clear", vec![vec![v(1)]])])
                .flushed
        );
        assert!(grid.apply_redraw(&[event("flush", vec![vec![]])]).flushed);
    }
}
