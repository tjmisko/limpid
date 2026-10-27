//! Painting an nvim grid with the viewer's own glyph pipeline.
//!
//! Cells are drawn in the mono font on whatever the box already shows. Of
//! nvim's highlighting only the cursor, visual selection, search matches and
//! spell marks are honoured; text colour comes from the theme.

use super::grid::{CONTINUATION, CursorShape, Grid, HlAttr};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::{Color, Theme},
};

/// Cell height as a multiple of the font size.
pub const GRID_LINE_HEIGHT: f32 = 1.7;

/// Font size of the editor box in logical px, whatever the page's size.
pub const EDITOR_FONT_SIZE: f32 = 15.0;

/// Thickness of the bar and underline cursors, and of spell marks, in px.
const MARK_THICKNESS: f32 = 2.0;

/// Alpha of the fallback selection and search tint.
const TINT_ALPHA: u8 = 96;

/// Highlight groups nvim may restyle: selection, search, spell.
const HONOURED_GROUPS: &[&str] = &[
    "Visual",
    "VisualNOS",
    "Search",
    "IncSearch",
    "CurSearch",
    "Substitute",
    "SpellBad",
    "SpellCap",
    "SpellRare",
    "SpellLocal",
];

/// Width and height of one cell in pixels for the theme's mono font.
pub fn cell_size(fonts: &mut Fonts, theme: &Theme) -> (f32, f32) {
    let height = (theme.font_size * GRID_LINE_HEIGHT).round().max(1.0);
    let mut block = TextBlock::new(fonts, theme.font_size, height);
    block.set_text(fonts, "M", TextStyle::mono(theme.ink), &theme.families);
    let width = block
        .glyphs()
        .next()
        .map(|glyph| glyph.width)
        .filter(|width| *width > 0.0)
        .unwrap_or(theme.font_size * 0.6);
    (width.round().max(1.0), height)
}

/// How many whole columns and rows fit in `rect`.
pub fn grid_size_for(rect: Rect, fonts: &mut Fonts, theme: &Theme) -> (u16, u16) {
    let (cell_width, cell_height) = cell_size(fonts, theme);
    let cols = (rect.w / cell_width)
        .floor()
        .clamp(1.0, f32::from(u16::MAX));
    let rows = (rect.h / cell_height)
        .floor()
        .clamp(1.0, f32::from(u16::MAX));
    (cols as u16, rows as u16)
}

/// What a cell's highlight turns into on the page.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct CellLook {
    background: Option<Color>,
    spell_mark: bool,
}

fn rgb_color(value: u32) -> Color {
    Color::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn look_for(attr: Option<&HlAttr>, theme: &Theme) -> CellLook {
    let Some(attr) = attr else {
        return CellLook::default();
    };
    let Some(group) = attr.ui_name.as_deref() else {
        return CellLook::default();
    };
    if !HONOURED_GROUPS.contains(&group) {
        return CellLook::default();
    }
    if group.starts_with("Spell") {
        return CellLook {
            background: None,
            spell_mark: true,
        };
    }
    let tint = Color::rgba(theme.focus.r, theme.focus.g, theme.focus.b, TINT_ALPHA);
    CellLook {
        background: Some(attr.bg.map_or(tint, rgb_color)),
        spell_mark: false,
    }
}

/// Paint `grid` with its top-left at `rect`'s. Anything beyond `rect` is
/// skipped, so a grid larger than its box is clipped, not overdrawn.
pub fn paint_grid(canvas: &mut Canvas, grid: &Grid, rect: Rect, theme: &Theme, fonts: &mut Fonts) {
    let (cell_width, cell_height) = cell_size(fonts, theme);
    let visible_cols = ((rect.w / cell_width).floor().max(0.0) as u16).min(grid.cols);
    let visible_rows = ((rect.h / cell_height).floor().max(0.0) as u16).min(grid.rows);

    for row in 0..visible_rows {
        let top = rect.y + f32::from(row) * cell_height;
        paint_backgrounds(
            canvas,
            grid,
            row,
            visible_cols,
            (rect.x, top),
            (cell_width, cell_height),
            theme,
        );
        paint_row_text(
            canvas,
            grid,
            row,
            visible_cols,
            (rect.x, top),
            (cell_width, cell_height),
            theme,
            fonts,
        );
    }
    paint_cursor(
        canvas,
        grid,
        rect,
        (visible_cols, visible_rows),
        (cell_width, cell_height),
        theme,
        fonts,
    );
}

fn paint_backgrounds(
    canvas: &mut Canvas,
    grid: &Grid,
    row: u16,
    visible_cols: u16,
    origin: (f32, f32),
    cell: (f32, f32),
    theme: &Theme,
) {
    for col in 0..visible_cols {
        let Some(grid_cell) = grid.cell(row, col) else {
            continue;
        };
        if grid_cell.hl == 0 {
            continue;
        }
        let look = look_for(grid.highlights.get(&grid_cell.hl), theme);
        let x = origin.0 + f32::from(col) * cell.0;
        if let Some(background) = look.background {
            canvas.fill_rect(Rect::new(x, origin.1, cell.0, cell.1), background);
        }
        if look.spell_mark {
            let y = origin.1 + cell.1 - MARK_THICKNESS;
            canvas.fill_rect(Rect::new(x, y, cell.0, MARK_THICKNESS), theme.amber);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_row_text(
    canvas: &mut Canvas,
    grid: &Grid,
    row: u16,
    visible_cols: u16,
    origin: (f32, f32),
    cell: (f32, f32),
    theme: &Theme,
    fonts: &mut Fonts,
) {
    let characters: Vec<(u16, char)> = (0..visible_cols)
        .filter_map(|col| grid.cell(row, col).map(|c| (col, c.ch)))
        .collect();
    let style = TextStyle::mono(theme.ink);
    // Every character is placed on its own cell so glyphs, backgrounds and
    // the cursor share one grid whatever the font's fractional advance is.
    for (col, ch) in characters {
        if ch == CONTINUATION || ch == ' ' {
            continue;
        }
        let x = origin.0 + f32::from(col) * cell.0;
        draw_run(
            canvas,
            &ch.to_string(),
            x,
            origin.1,
            cell.1,
            theme,
            style,
            fonts,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_run(
    canvas: &mut Canvas,
    text: &str,
    x: f32,
    y: f32,
    line_height: f32,
    theme: &Theme,
    style: TextStyle,
    fonts: &mut Fonts,
) {
    let mut block = TextBlock::new(fonts, theme.font_size, line_height);
    block.set_text(fonts, text, style, &theme.families);
    canvas.draw_text(fonts, &block, x, y, style.color);
}

fn paint_cursor(
    canvas: &mut Canvas,
    grid: &Grid,
    rect: Rect,
    visible: (u16, u16),
    cell: (f32, f32),
    theme: &Theme,
    fonts: &mut Fonts,
) {
    let (row, col) = grid.cursor;
    if row >= visible.1 || col >= visible.0 {
        return;
    }
    let x = rect.x + f32::from(col) * cell.0;
    let y = rect.y + f32::from(row) * cell.1;
    match grid.cursor_shape {
        CursorShape::Block => {
            canvas.fill_rect(Rect::new(x, y, cell.0, cell.1), theme.teal);
            let Some(under) = grid.cell(row, col) else {
                return;
            };
            if under.ch == ' ' || under.ch == CONTINUATION {
                return;
            }
            let style = TextStyle::mono(theme.bg);
            draw_run(
                canvas,
                &under.ch.to_string(),
                x,
                y,
                cell.1,
                theme,
                style,
                fonts,
            );
        }
        CursorShape::Bar => {
            canvas.fill_rect(Rect::new(x, y, MARK_THICKNESS, cell.1), theme.teal);
        }
        CursorShape::Underline => {
            canvas.fill_rect(
                Rect::new(x, y + cell.1 - MARK_THICKNESS, cell.0, MARK_THICKNESS),
                theme.teal,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::grid::Cell;

    const WIDTH: u32 = 240;
    const HEIGHT: u32 = 120;

    struct Scene {
        pixels: Vec<u32>,
        theme: Theme,
        fonts: Fonts,
    }

    impl Scene {
        fn new() -> Scene {
            let theme = Theme::dark();
            Scene {
                pixels: vec![theme.bg.to_pixel(); (WIDTH * HEIGHT) as usize],
                theme,
                fonts: Fonts::new(),
            }
        }

        fn paint(&mut self, grid: &Grid, rect: Rect) {
            let mut canvas = Canvas::new(&mut self.pixels, WIDTH, HEIGHT);
            paint_grid(&mut canvas, grid, rect, &self.theme, &mut self.fonts);
        }

        fn pixel(&self, x: u32, y: u32) -> u32 {
            self.pixels[(y * WIDTH + x) as usize]
        }

        fn differs_from_background(&self, region: Rect) -> bool {
            let background = self.theme.bg.to_pixel();
            (region.y as u32..region.bottom() as u32).any(|y| {
                (region.x as u32..region.right() as u32).any(|x| self.pixel(x, y) != background)
            })
        }
    }

    fn grid_with(text: &str) -> Grid {
        let mut grid = Grid::new(10, 3);
        for (index, ch) in text.chars().enumerate() {
            grid.cells[index] = Cell { ch, hl: 0 };
        }
        grid.cursor = (2, 9);
        grid
    }

    #[test]
    fn should_measure_a_positive_cell_when_the_mono_font_loads() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        assert!(width >= 1.0 && height >= scene.theme.font_size);
    }

    #[test]
    fn should_fit_whole_cells_when_sizing_a_grid_for_a_rect() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        let rect = Rect::new(0.0, 0.0, width * 12.5, height * 4.5);
        assert_eq!(grid_size_for(rect, &mut scene.fonts, &scene.theme), (12, 4));
        let tiny = Rect::new(0.0, 0.0, 1.0, 1.0);
        assert_eq!(grid_size_for(tiny, &mut scene.fonts, &scene.theme), (1, 1));
    }

    #[test]
    fn should_draw_glyph_pixels_when_cells_hold_text() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        scene.paint(&grid_with("HHHH"), Rect::new(10.0, 10.0, 200.0, 100.0));
        let first_row = Rect::new(10.0, 10.0, width * 4.0, height);
        assert!(scene.differs_from_background(first_row));
    }

    #[test]
    fn should_leave_blank_rows_untouched_when_cells_are_spaces() {
        let mut scene = Scene::new();
        let (_, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("HHHH");
        grid.cursor = (0, 0);
        scene.paint(&grid, Rect::new(10.0, 10.0, 200.0, 100.0));
        let second_row = Rect::new(10.0, 10.0 + height, 100.0, height);
        assert!(!scene.differs_from_background(second_row));
    }

    #[test]
    fn should_align_glyphs_with_their_cell_when_the_column_is_far_right() {
        let mut fonts = Fonts::new();
        let theme = Theme::dark();
        let (width, height) = cell_size(&mut fonts, &theme);
        let (canvas_width, canvas_height) = (1200u32, (height as u32) + 4);
        let mut pixels = vec![theme.bg.to_pixel(); (canvas_width * canvas_height) as usize];
        let mut grid = Grid::new(80, 1);
        let column = 60u16;
        grid.cells[column as usize] = Cell { ch: 'M', hl: 0 };
        grid.cursor = (0, 0);
        grid.cursor_shape = CursorShape::Underline;
        let rect = Rect::new(0.0, 0.0, canvas_width as f32, height);
        {
            let mut canvas = Canvas::new(&mut pixels, canvas_width, canvas_height);
            paint_grid(&mut canvas, &grid, rect, &theme, &mut fonts);
        }
        let background = theme.bg.to_pixel();
        let cell_left = f32::from(column) * width;
        let mut ink_columns = Vec::new();
        for x in 2 * width as u32..canvas_width {
            let has_ink = (0..height as u32 - 3)
                .any(|y| pixels[(y * canvas_width + x) as usize] != background);
            if has_ink {
                ink_columns.push(x as f32);
            }
        }
        let first = ink_columns.first().copied().expect("the glyph was drawn");
        let last = ink_columns.last().copied().unwrap();
        assert!(
            first >= cell_left - 1.0,
            "glyph starts at {first}, cell at {cell_left}"
        );
        assert!(
            last < cell_left + width + 1.0,
            "glyph ends at {last}, cell ends at {}",
            cell_left + width
        );
    }

    #[test]
    fn should_fill_the_cell_when_the_cursor_is_a_block() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (1, 2);
        grid.cursor_shape = CursorShape::Block;
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        let x = (2.0 * width + 1.0) as u32;
        let y = (height + 1.0) as u32;
        assert_eq!(scene.pixel(x, y), scene.theme.teal.to_pixel());
    }

    #[test]
    fn should_draw_a_thin_line_when_the_cursor_is_a_bar() {
        let mut scene = Scene::new();
        let (width, _) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (0, 1);
        grid.cursor_shape = CursorShape::Bar;
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        let left = width as u32;
        assert_eq!(scene.pixel(left, 2), scene.theme.teal.to_pixel());
        assert_ne!(scene.pixel(left + 4, 2), scene.theme.teal.to_pixel());
    }

    #[test]
    fn should_draw_along_the_bottom_when_the_cursor_is_an_underline() {
        let mut scene = Scene::new();
        let (_, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (0, 0);
        grid.cursor_shape = CursorShape::Underline;
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(
            scene.pixel(1, height as u32 - 1),
            scene.theme.teal.to_pixel()
        );
        assert_ne!(scene.pixel(1, 1), scene.theme.teal.to_pixel());
    }

    #[test]
    fn should_tint_cells_when_nvim_marks_them_visual() {
        let mut scene = Scene::new();
        let (width, _) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        grid.highlights.insert(
            4,
            HlAttr {
                ui_name: Some("Visual".into()),
                ..HlAttr::default()
            },
        );
        grid.cells[1] = Cell { ch: ' ', hl: 4 };
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        let tinted = scene.pixel((width * 1.0 + 2.0) as u32, 2);
        assert_ne!(tinted, scene.theme.bg.to_pixel());
        let untinted = scene.pixel((width * 3.0 + 2.0) as u32, 2);
        assert_eq!(untinted, scene.theme.bg.to_pixel());
    }

    #[test]
    fn should_ignore_nvim_colours_when_the_group_is_not_honoured() {
        let mut scene = Scene::new();
        let (width, _) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        grid.highlights.insert(
            6,
            HlAttr {
                bg: Some(0xff0000),
                ui_name: Some("Normal".into()),
                ..HlAttr::default()
            },
        );
        grid.highlights.insert(
            7,
            HlAttr {
                bg: Some(0x00ff00),
                ui_name: None,
                ..HlAttr::default()
            },
        );
        grid.cells[0] = Cell { ch: ' ', hl: 6 };
        grid.cells[1] = Cell { ch: ' ', hl: 7 };
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(scene.pixel(2, 2), scene.theme.bg.to_pixel());
        assert_eq!(
            scene.pixel((width + 2.0) as u32, 2),
            scene.theme.bg.to_pixel()
        );
    }

    #[test]
    fn should_use_nvims_background_when_a_search_match_defines_one() {
        let mut scene = Scene::new();
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        grid.highlights.insert(
            3,
            HlAttr {
                bg: Some(0x336699),
                ui_name: Some("Search".into()),
                ..HlAttr::default()
            },
        );
        grid.cells[0] = Cell { ch: ' ', hl: 3 };
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(scene.pixel(2, 2), 0x336699);
    }

    #[test]
    fn should_mark_the_cell_bottom_when_spelling_is_flagged() {
        let mut scene = Scene::new();
        let (_, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        grid.highlights.insert(
            8,
            HlAttr {
                ui_name: Some("SpellBad".into()),
                ..HlAttr::default()
            },
        );
        grid.cells[0] = Cell { ch: ' ', hl: 8 };
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(
            scene.pixel(2, height as u32 - 1),
            scene.theme.amber.to_pixel()
        );
        assert_eq!(scene.pixel(2, 2), scene.theme.bg.to_pixel());
    }

    #[test]
    fn should_clip_to_the_rect_when_the_grid_is_larger_than_the_box() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = Grid::new(10, 3);
        grid.cells.fill(Cell { ch: 'W', hl: 0 });
        grid.cursor = (2, 9);
        let rect = Rect::new(20.0, 20.0, width * 2.0, height);
        scene.paint(&grid, rect);
        let outside_right = Rect::new(rect.right() + 1.0, 20.0, 60.0, height);
        let outside_below = Rect::new(20.0, rect.bottom() + 1.0, 100.0, height);
        assert!(!scene.differs_from_background(outside_right));
        assert!(!scene.differs_from_background(outside_below));
        assert!(scene.differs_from_background(rect));
    }

    #[test]
    fn should_not_paint_the_cursor_when_it_is_outside_the_visible_area() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        scene.paint(&grid, Rect::new(0.0, 0.0, width * 3.0, height));
        assert!(!scene.differs_from_background(Rect::new(0.0, 0.0, width * 3.0, height)));
    }

    #[test]
    fn should_skip_continuation_cells_when_drawing_wide_characters() {
        let mut scene = Scene::new();
        let (width, height) = cell_size(&mut scene.fonts, &scene.theme);
        let mut grid = grid_with("");
        grid.cursor = (2, 9);
        grid.cells[0] = Cell { ch: 'W', hl: 0 };
        grid.cells[1] = Cell {
            ch: CONTINUATION,
            hl: 0,
        };
        grid.cells[2] = Cell { ch: 'W', hl: 0 };
        scene.paint(&grid, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert!(scene.differs_from_background(Rect::new(0.0, 0.0, width, height)));
        assert!(scene.differs_from_background(Rect::new(width * 2.0, 0.0, width, height)));
    }

    #[test]
    fn should_not_panic_when_the_rect_is_degenerate() {
        let mut scene = Scene::new();
        let grid = grid_with("hello");
        scene.paint(&grid, Rect::new(0.0, 0.0, 0.0, 0.0));
        scene.paint(&grid, Rect::new(-50.0, -50.0, 100.0, 100.0));
        scene.paint(&grid, Rect::new(f32::NAN, 0.0, 10.0, 10.0));
    }
}
