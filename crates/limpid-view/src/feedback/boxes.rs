//! The reply and comment boxes: how tall a box is, the line to the editor
//! process, and how a box is painted.

use super::fields::BoxPlan;
use crate::{
    editor::{
        BoxKind, Editor, EditorConfig, EditorError, EditorEvent, Grid,
        paint::{GRID_LINE_HEIGHT, grid_size_for, paint_grid},
    },
    layout::field::box_inner,
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::Theme,
};

/// Limits on how tall a box grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxLimits {
    /// A box is never shorter than this many rows.
    pub min_rows: u16,
    /// A box stops growing at this many rows; the editor scrolls beyond.
    pub max_rows: u16,
}

impl Default for BoxLimits {
    fn default() -> Self {
        Self {
            min_rows: 3,
            max_rows: 14,
        }
    }
}

impl BoxLimits {
    /// Rows for a buffer that fills `content_rows`, with one spare row to
    /// type into. Always between the limits, whatever order they were given.
    pub fn rows_for(&self, content_rows: u16) -> u16 {
        let low = self.min_rows.max(1).min(self.max_rows.max(1));
        let high = self.max_rows.max(self.min_rows).max(1);
        content_rows.saturating_add(1).clamp(low, high)
    }
}

/// How many rows of `grid` hold something: up to the last row with a
/// non-blank cell.
pub fn content_rows(grid: &Grid) -> u16 {
    let cols = usize::from(grid.cols);
    if cols == 0 {
        return 0;
    }
    let last_filled = grid
        .cells
        .chunks(cols)
        .rposition(|row| row.iter().any(|cell| cell.ch != ' ' && cell.ch != '\0'));
    let by_text = last_filled.map_or(0, |row| row as u16 + 1);
    // The cursor sits on a blank row after Enter; that row counts too.
    by_text.max(grid.cursor.0.saturating_add(1))
}

/// The nvim a box talks to. [`Editor`] is the real one; tests substitute a
/// scripted one.
pub trait EditorPort {
    fn open(&mut self, text: &str, kind: BoxKind) -> Result<(), EditorError>;
    fn input(&mut self, keys: &str);
    fn resize(&mut self, cols: u16, rows: u16);
    fn poll(&mut self) -> Vec<EditorEvent>;
    fn grid(&self) -> &Grid;
    fn is_running(&mut self) -> bool;
}

impl EditorPort for Editor {
    fn open(&mut self, text: &str, kind: BoxKind) -> Result<(), EditorError> {
        Editor::open(self, text, kind)
    }

    fn input(&mut self, keys: &str) {
        Editor::input(self, keys)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        Editor::resize(self, cols, rows)
    }

    fn poll(&mut self) -> Vec<EditorEvent> {
        Editor::poll(self)
    }

    fn grid(&self) -> &Grid {
        Editor::grid(self)
    }

    fn is_running(&mut self) -> bool {
        Editor::is_running(self)
    }
}

/// Starts the editor process; replaceable so tests need no nvim.
pub type EditorSpawner = Box<dyn FnMut(&EditorConfig) -> Result<Box<dyn EditorPort>, EditorError>>;

/// The spawner that starts a real `nvim --embed`.
pub fn nvim_spawner() -> EditorSpawner {
    Box::new(|config| Ok(Box::new(Editor::spawn(config)?) as Box<dyn EditorPort>))
}

/// A box that is open: what it edits and what nvim last said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenBox {
    pub plan: BoxPlan,
    pub mode: String,
    /// The `:` line nvim is showing, with its leading character.
    pub cmdline: Option<String>,
    /// The last message nvim wanted shown.
    pub message: Option<String>,
}

impl OpenBox {
    pub fn new(plan: BoxPlan) -> Self {
        Self {
            plan,
            mode: String::new(),
            cmdline: None,
            message: None,
        }
    }

    /// Whether nvim is in a mode that types text.
    pub fn is_inserting(&self) -> bool {
        self.mode.starts_with('i') || self.mode.starts_with('R')
    }

    /// A short name for the mode line of the box.
    pub fn mode_label(&self) -> &'static str {
        match self.mode.chars().next() {
            Some('i') => "INSERT",
            Some('R') => "REPLACE",
            Some('v' | 'V' | '\u{16}') => "VISUAL",
            Some('c') => "COMMAND",
            _ => "NORMAL",
        }
    }
}

/// Pixel size of one grid cell for a window at `scale`.
pub fn scaled_theme(theme: &Theme, scale: f32) -> Theme {
    let mut scaled = theme.clone();
    scaled.font_size = theme.font_size * scale;
    scaled
}

/// The (columns, rows) the grid should have to fill the box at `outer`.
pub fn grid_size(outer: Rect, theme: &Theme, scale: f32, fonts: &mut Fonts) -> (u16, u16) {
    grid_size_for(box_inner(outer, scale), fonts, &scaled_theme(theme, scale))
}

/// Paint the box: a focus-coloured frame, the editor grid and the mode line.
/// `outer` is the block's rectangle in window coordinates.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    theme: &Theme,
    scale: f32,
    outer: Rect,
    grid: &Grid,
    open: &OpenBox,
) {
    let border = scale.round().max(1.0);
    canvas.fill_rect(outer, theme.code_bg);
    canvas.stroke_rect(outer, border, theme.teal);
    let scaled = scaled_theme(theme, scale);
    paint_grid(canvas, grid, box_inner(outer, scale), &scaled, fonts);

    let size = (theme.font_size * 0.7 * scale).round().max(8.0);
    let mut label = TextBlock::new(fonts, size, size * 1.3);
    label.set_text(
        fonts,
        open.mode_label(),
        TextStyle::mono(theme.muted).bold(),
        &theme.families,
    );
    label.set_width(fonts, None);
    let width = label
        .glyphs()
        .map(|glyph| glyph.x + glyph.width)
        .fold(0.0, f32::max);
    canvas.draw_text(
        fonts,
        &label,
        outer.right() - width - 8.0 * scale,
        outer.y + 3.0 * scale,
        theme.muted,
    );
}

/// Height in device pixels of a box of `rows` rows at `scale`, frame included.
pub fn box_height(rows: u16, theme: &Theme, scale: f32) -> f32 {
    let row = (theme.font_size * scale * GRID_LINE_HEIGHT)
        .round()
        .max(1.0);
    let frame = 2.0 * (12.0 * scale + scale.round().max(1.0));
    f32::from(rows.max(1)) * row + frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{BoxKind, Cell};

    fn grid_with(rows: &[&str], cursor: (u16, u16)) -> Grid {
        let cols = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(1) as u16;
        let mut grid = Grid::new(cols.max(1), rows.len() as u16);
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                grid.cells[y * usize::from(grid.cols) + x] = Cell { ch, hl: 0 };
            }
        }
        grid.cursor = cursor;
        grid
    }

    #[test]
    fn should_grow_with_content_when_asking_for_rows() {
        let limits = BoxLimits::default();
        assert_eq!(limits.rows_for(0), 3);
        assert_eq!(limits.rows_for(2), 3);
        assert_eq!(limits.rows_for(5), 6);
        assert_eq!(limits.rows_for(200), 14);
        assert_eq!(limits.rows_for(u16::MAX), 14);
    }

    #[test]
    fn should_stay_sane_when_the_limits_are_inverted_or_zero() {
        let inverted = BoxLimits {
            min_rows: 10,
            max_rows: 4,
        };
        let rows = inverted.rows_for(1);
        assert!((1..=10).contains(&rows), "{rows}");
        let zero = BoxLimits {
            min_rows: 0,
            max_rows: 0,
        };
        assert_eq!(zero.rows_for(0), 1);
        assert_eq!(zero.rows_for(9), 1);
    }

    #[test]
    fn should_count_up_to_the_last_filled_row_when_measuring_content() {
        let grid = grid_with(&["hello", "     ", "world", "     ", "     "], (0, 0));
        assert_eq!(content_rows(&grid), 3);
    }

    #[test]
    fn should_count_the_cursor_row_when_the_buffer_ends_in_a_blank_line() {
        let grid = grid_with(&["hello", "     ", "     "], (1, 0));
        assert_eq!(content_rows(&grid), 2);
    }

    #[test]
    fn should_count_zero_when_the_grid_is_empty() {
        assert_eq!(content_rows(&Grid::new(0, 0)), 0);
        assert_eq!(content_rows(&grid_with(&["   "], (0, 0))), 1);
    }

    #[test]
    fn should_name_the_mode_when_labelling_the_box() {
        let mut open = OpenBox::new(BoxPlan {
            target: super::super::fields::BoxTarget::Reply { field: "q1".into() },
            rows: 3,
        });
        assert_eq!(open.mode_label(), "NORMAL");
        open.mode = "insert".into();
        assert_eq!(open.mode_label(), "INSERT");
        assert!(open.is_inserting());
        open.mode = "visual".into();
        assert_eq!(open.mode_label(), "VISUAL");
        assert!(!open.is_inserting());
    }

    #[test]
    fn should_size_a_box_by_rows_and_frame_when_computing_height() {
        let theme = Theme::dark();
        let three = box_height(3, &theme, 1.0);
        let four = box_height(4, &theme, 1.0);
        let row = (theme.font_size * GRID_LINE_HEIGHT).round();
        assert!((four - three - row).abs() < 0.01);
        assert!(box_height(3, &theme, 2.0) > 1.9 * three);
        assert_eq!(box_height(0, &theme, 1.0), box_height(1, &theme, 1.0));
    }

    #[test]
    fn should_agree_with_the_layout_when_reserving_a_box() {
        use crate::layout::field::{box_text, grid_row_height};
        use limpid_core::doc::{AnnotationKind, Block, BlockKind, Mode, parse};
        let theme = Theme::dark();
        let mut doc = parse(
            "para\n",
            Mode::File,
            &limpid_core::settings::Settings::default(),
        );
        let end = doc.source.len();
        doc.blocks.push(Block {
            kind: BlockKind::Annotation {
                target: 0..end,
                kind: AnnotationKind::Comment,
                text: box_text(5),
                field: None,
            },
            src: end..end,
            id: None,
        });
        for scale in [1.0, 1.5, 2.0] {
            let layout = crate::layout::lay_out(&doc, 1000.0, &theme, scale, &mut Fonts::new());
            let laid = layout.blocks[1].rect.h;
            let predicted = box_height(5, &theme, scale);
            assert!(
                (laid - predicted).abs() < 1.0,
                "scale {scale}: {laid} vs {predicted}"
            );
            assert!(grid_row_height(theme.font_size * scale) > 0.0);
        }
    }

    #[test]
    fn should_report_nothing_running_when_the_port_is_a_stub() {
        struct Stub(Grid);
        impl EditorPort for Stub {
            fn open(&mut self, _: &str, _: BoxKind) -> Result<(), EditorError> {
                Ok(())
            }
            fn input(&mut self, _: &str) {}
            fn resize(&mut self, _: u16, _: u16) {}
            fn poll(&mut self) -> Vec<EditorEvent> {
                Vec::new()
            }
            fn grid(&self) -> &Grid {
                &self.0
            }
            fn is_running(&mut self) -> bool {
                false
            }
        }
        let mut stub = Stub(Grid::new(2, 2));
        assert!(!stub.is_running());
        assert!(stub.poll().is_empty());
        assert_eq!(stub.grid().cols, 2);
    }
}
