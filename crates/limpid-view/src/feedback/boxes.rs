//! The reply and comment boxes: how tall a box is, the line to the editor
//! process, and how a box is painted.

use super::fields::BoxPlan;
use crate::{
    editor::{
        BoxKind, Editor, EditorConfig, EditorError, EditorEvent, Grid,
        paint::{EDITOR_FONT_SIZE, GRID_LINE_HEIGHT, grid_size_for, paint_grid},
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
    /// Text an input method is composing, not yet committed.
    pub preedit: Option<String>,
}

impl OpenBox {
    pub fn new(plan: BoxPlan) -> Self {
        Self {
            plan,
            mode: String::new(),
            cmdline: None,
            message: None,
            preedit: None,
        }
    }

    /// Whether nvim is in a mode that types text.
    pub fn is_inserting(&self) -> bool {
        self.mode.starts_with('i') || self.mode.starts_with('R')
    }

    /// What the box's status line says: the mode, and the composition in
    /// progress when an input method has one.
    pub fn status_label(&self) -> String {
        match &self.preedit {
            Some(preedit) => format!("{preedit} | {}", self.mode_label()),
            None => self.mode_label().to_owned(),
        }
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
    scaled.font_size = EDITOR_FONT_SIZE * scale;
    scaled
}

/// The (columns, rows) the grid should have to fill the box at `outer`.
pub fn grid_size(outer: Rect, theme: &Theme, scale: f32, fonts: &mut Fonts) -> (u16, u16) {
    grid_size_for(box_inner(outer, scale), fonts, &scaled_theme(theme, scale))
}

/// Corner radius of the box in logical px.
const BOX_RADIUS: f32 = 10.0;

/// Font size of the command line in logical px.
const CMDLINE_FONT_SIZE: f32 = 13.0;

/// The most rows that keep a box within 70 % of a viewport `height` px tall.
pub fn rows_in_viewport(height: f32, theme: &Theme, scale: f32) -> u16 {
    let row = box_height(2, theme, scale) - box_height(1, theme, scale);
    let frame = box_height(1, theme, scale) - row;
    let rows = ((height * 0.7 - frame) / row.max(1.0)).floor();
    rows.clamp(1.0, f32::from(u16::MAX)) as u16
}

fn rounded_path(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut path = tiny_skia::PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.quad_to(x + w, y, x + w, y + r);
    path.line_to(x + w, y + h - r);
    path.quad_to(x + w, y + h, x + w - r, y + h);
    path.line_to(x + r, y + h);
    path.quad_to(x, y + h, x, y + h - r);
    path.line_to(x, y + r);
    path.quad_to(x, y, x + r, y);
    path.close();
    path.finish()
}

const SHADOW_REACH: f32 = 10.0;
const SHADOW_DROP: f32 = 6.0;
const SHADOW_LAYERS: u32 = 10;
const SHADOW_LAYER_ALPHA: u8 = 6;

/// The surface fill with rounded corners, a teal border, a teal ring and
/// the soft drop shadow.
fn paint_frame(canvas: &mut Canvas, theme: &Theme, scale: f32, outer: Rect) {
    let border = scale.round().max(1.0);
    let room = border.ceil() + 1.0 + SHADOW_REACH * scale;
    let width = (outer.w + 2.0 * room).ceil();
    let height = (outer.h + 2.0 * room).ceil();
    if width < 1.0 || height < 1.0 {
        return;
    }
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width as u32, height as u32) else {
        return;
    };
    let inset = Rect::new(room, room, outer.w, outer.h);
    let Some(path) = rounded_path(inset, BOX_RADIUS * scale) else {
        return;
    };
    let mut paint = tiny_skia::Paint {
        anti_alias: true,
        ..tiny_skia::Paint::default()
    };
    for layer in 0..SHADOW_LAYERS {
        let grow = SHADOW_REACH * scale * (1.0 - layer as f32 / SHADOW_LAYERS as f32);
        let rect = Rect::new(
            inset.x - grow,
            inset.y + SHADOW_DROP * scale - grow,
            inset.w + 2.0 * grow,
            inset.h + 2.0 * grow,
        );
        if let Some(shadow) = rounded_path(rect, BOX_RADIUS * scale + grow) {
            paint.set_color_rgba8(0, 0, 0, SHADOW_LAYER_ALPHA);
            pixmap.fill_path(
                &shadow,
                &paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }
    let fill = theme.surface;
    paint.set_color_rgba8(fill.r, fill.g, fill.b, 255);
    pixmap.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
    let teal = theme.teal;
    paint.set_color_rgba8(teal.r, teal.g, teal.b, 255);
    let stroke = tiny_skia::Stroke {
        width: border * 2.0,
        ..tiny_skia::Stroke::default()
    };
    pixmap.stroke_path(
        &path,
        &paint,
        &stroke,
        tiny_skia::Transform::identity(),
        None,
    );
    canvas.draw_pixmap(&pixmap, outer.x - room, outer.y - room);
}

/// The `:` or `/` line nvim is typing, in a dark strip along the bottom of
/// the box.
fn paint_cmdline(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    theme: &Theme,
    scale: f32,
    outer: Rect,
    line: &str,
) {
    let size = (CMDLINE_FONT_SIZE * scale).round().max(8.0);
    let pad = 14.0 * scale;
    let height = (size * 1.5 + 12.0 * scale).round();
    let border = scale.round().max(1.0);
    let strip = Rect::new(
        outer.x + border,
        outer.bottom() - border - height,
        (outer.w - 2.0 * border).max(0.0),
        height,
    );
    canvas.fill_rect(strip, theme.code_bg);
    canvas.fill_rect(Rect::new(strip.x, strip.y, strip.w, border), theme.rule);
    let mut text = TextBlock::new(fonts, size, size * 1.5);
    text.set_text(fonts, line, TextStyle::mono(theme.ink), &theme.families);
    text.set_width(fonts, None);
    let end = text
        .glyphs()
        .map(|glyph| glyph.x + glyph.width)
        .fold(0.0, f32::max);
    let top = strip.y + 6.0 * scale;
    canvas.draw_text(fonts, &text, strip.x + pad, top, theme.ink);
    canvas.fill_rect(
        Rect::new(strip.x + pad + end, top, 2.0 * scale.max(1.0), size * 1.5),
        theme.teal,
    );
}

/// Paint the box: a rounded teal-bordered frame, the editor grid, the mode
/// line and, while one is being typed, the command line. `outer` is the
/// block's rectangle in window coordinates.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    theme: &Theme,
    scale: f32,
    outer: Rect,
    grid: &Grid,
    open: &OpenBox,
) {
    // The block underneath is square; clear its corners first.
    canvas.fill_rect(outer, theme.bg);
    paint_frame(canvas, theme, scale, outer);
    let scaled = scaled_theme(theme, scale);
    paint_grid(canvas, grid, box_inner(outer, scale), &scaled, fonts);

    let size = (theme.font_size * 0.7 * scale).round().max(8.0);
    let mut label = TextBlock::new(fonts, size, size * 1.3);
    label.set_text(
        fonts,
        &open.status_label(),
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
    if let Some(line) = open.cmdline.as_deref() {
        paint_cmdline(canvas, fonts, theme, scale, outer, line);
    }
}

/// Height in device pixels of a box of `rows` rows at `scale`, frame included.
pub fn box_height(rows: u16, _theme: &Theme, scale: f32) -> f32 {
    let row = (EDITOR_FONT_SIZE * scale * GRID_LINE_HEIGHT)
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
        let row = (EDITOR_FONT_SIZE * GRID_LINE_HEIGHT).round();
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
            assert!(grid_row_height(EDITOR_FONT_SIZE * scale) > 0.0);
        }
    }

    fn scene(width: u32, height: u32) -> Vec<u32> {
        vec![Theme::dark().bg.to_pixel(); (width * height) as usize]
    }

    #[test]
    fn should_round_the_corners_and_fill_the_surface_when_painting_the_frame() {
        let theme = Theme::dark();
        let mut pixels = scene(200, 120);
        let mut canvas = Canvas::new(&mut pixels, 200, 120);
        let outer = Rect::new(20.0, 20.0, 160.0, 80.0);
        canvas.fill_rect(outer, theme.rule);
        paint_frame(&mut canvas, &theme, 1.0, outer);
        assert_eq!(canvas.pixel(100, 60), Some(theme.surface.to_pixel()));
        assert_eq!(canvas.pixel(100, 20), Some(theme.teal.to_pixel()));
        // The very corner lies outside a radius of 10, so only the shadow touches it.
        assert_ne!(canvas.pixel(20, 20), Some(theme.surface.to_pixel()));
        assert_ne!(canvas.pixel(20, 20), Some(theme.teal.to_pixel()));
    }

    #[test]
    fn should_darken_the_page_below_the_box_when_painting_the_frame_shadow() {
        let theme = Theme::dark();
        let mut pixels = scene(200, 160);
        let mut canvas = Canvas::new(&mut pixels, 200, 160);
        let outer = Rect::new(20.0, 20.0, 160.0, 80.0);
        canvas.fill_rect(Rect::new(0.0, 0.0, 200.0, 160.0), theme.bg);
        paint_frame(&mut canvas, &theme, 1.0, outer);
        let below = canvas.pixel(100, 105).unwrap();
        assert_ne!(below, theme.bg.to_pixel());
        assert_eq!(canvas.pixel(100, 150), Some(theme.bg.to_pixel()));
    }

    #[test]
    fn should_paint_a_dark_strip_when_a_command_line_is_open() {
        let theme = Theme::dark();
        let mut fonts = Fonts::new();
        let mut pixels = scene(300, 160);
        let mut canvas = Canvas::new(&mut pixels, 300, 160);
        let outer = Rect::new(10.0, 10.0, 280.0, 140.0);
        let mut open = OpenBox::new(BoxPlan {
            target: super::super::fields::BoxTarget::Reply { field: "q1".into() },
            rows: 3,
        });
        open.cmdline = Some(":wq".into());
        paint(
            &mut canvas,
            &mut fonts,
            &theme,
            1.0,
            outer,
            &Grid::new(10, 3),
            &open,
        );
        assert_eq!(canvas.pixel(30, 140), Some(theme.code_bg.to_pixel()));
    }

    #[test]
    fn should_limit_a_box_to_seventy_percent_of_the_viewport_when_asking_for_rows() {
        let theme = Theme::dark();
        for scale in [1.0, 2.0] {
            let rows = rows_in_viewport(1000.0 * scale, &theme, scale);
            assert!(box_height(rows, &theme, scale) <= 700.0 * scale);
            assert!(box_height(rows + 1, &theme, scale) > 700.0 * scale);
        }
        assert_eq!(rows_in_viewport(0.0, &theme, 1.0), 1);
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
