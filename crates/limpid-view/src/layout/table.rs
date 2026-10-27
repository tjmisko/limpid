//! Tables: CSS auto layout over cosmic-text cells.
//!
//! Each column gets a min-content width (the widest unbreakable word of its
//! cells) and a max-content width (the widest unwrapped cell). The column
//! width is shared out the way CSS does it: max-content when it fits, shrinking
//! proportionally towards min-content when it does not, and never below
//! min-content. A table whose min-content sum exceeds the column scrolls
//! sideways while it has focus.

use super::{
    Content, LaidBlock, PaintCx,
    block::{Cx, Flow},
    boxes,
    inline::{self, RichText},
};
use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextStyle},
};
use limpid_core::doc::{Align, Block, Inline};

pub struct TableCell {
    pub text: RichText,
    pub align: Align,
}

pub struct TableRow {
    /// Top edge inside the table, device pixels.
    pub top: f32,
    pub height: f32,
    pub cells: Vec<TableCell>,
}

pub struct TableBox {
    /// Width of each column, device pixels, padding included.
    pub widths: Vec<f32>,
    /// The header row (when there is one) comes first.
    pub rows: Vec<TableRow>,
    pub header: bool,
    /// Sum of `widths`.
    pub table_width: f32,
    pub scroll_x: f32,
    /// The table is wider than its block and scrolls sideways.
    pub overflow: bool,
    pub border: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

impl TableBox {
    /// Furthest the table can scroll for a block of `block_width`.
    pub fn max_scroll(&self, block_width: f32) -> f32 {
        (self.table_width - (block_width - 2.0 * self.border)).max(0.0)
    }
}

/// Share `available` between columns. `min` and `max` are per-column
/// content widths including padding; `fill` stretches a table that already
/// fits to the whole width in proportion to `max`.
pub fn distribute(min: &[f32], max: &[f32], available: f32, fill: bool) -> Vec<f32> {
    let count = min.len().min(max.len());
    if count == 0 {
        return Vec::new();
    }
    let clean = |value: f32| {
        if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        }
    };
    let min: Vec<f32> = min[..count].iter().map(|&v| clean(v)).collect();
    let max: Vec<f32> = max[..count]
        .iter()
        .zip(&min)
        .map(|(&v, &lower)| clean(v).max(lower))
        .collect();
    let available = clean(available);
    let sum_min: f32 = min.iter().sum();
    let sum_max: f32 = max.iter().sum();
    if sum_max <= available {
        if !fill {
            return max;
        }
        if sum_max <= 0.0 {
            return vec![available / count as f32; count];
        }
        return max
            .iter()
            .map(|width| width * available / sum_max)
            .collect();
    }
    if sum_min >= available {
        return min;
    }
    let share = (available - sum_min) / (sum_max - sum_min);
    min.iter()
        .zip(&max)
        .map(|(&lower, &upper)| lower + (upper - lower) * share)
        .collect()
}

/// Measured and shaped cells share one entry point so the two stay in step.
struct Metrics {
    body_size: f32,
    body_line: f32,
    head_size: f32,
    head_line: f32,
}

fn shape(
    cx: &mut Cx,
    inlines: &[Inline],
    header: bool,
    metrics: &Metrics,
    ink: crate::theme::Color,
    width: Option<f32>,
) -> RichText {
    let (style, size, line) = if header {
        (
            TextStyle::sans(cx.theme.muted).bold(),
            metrics.head_size,
            metrics.head_line,
        )
    } else {
        (TextStyle::sans(ink), metrics.body_size, metrics.body_line)
    };
    inline::build_inlines(
        cx.fonts, inlines, style, cx.theme, size, line, width, header,
    )
}

/// Lay out a table block.
#[allow(clippy::too_many_arguments)]
pub fn lay(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    align: &[Align],
    head: &[Vec<Inline>],
    body: &[Vec<Vec<Inline>>],
) -> LaidBlock {
    let border = cx.px(1.0).round().max(1.0);
    let pad_x = cx.px(cx.boxes.table_pad.0);
    let pad_y = cx.px(cx.boxes.table_pad.1);
    let body_size = cx.font_px(14.0);
    let head_size = cx.font_px(12.0);
    let metrics = Metrics {
        body_size,
        body_line: body_size * cx.theme.line_height,
        head_size,
        head_line: head_size * cx.theme.line_height,
    };
    let header = !head.is_empty();
    let columns = body
        .iter()
        .map(Vec::len)
        .chain(std::iter::once(head.len()))
        .max()
        .unwrap_or(0)
        .max(1);
    let empty: Vec<Inline> = Vec::new();
    let mut grid: Vec<(bool, Vec<&[Inline]>)> = Vec::new();
    if header {
        grid.push((
            true,
            (0..columns).map(|c| cell_at(head, c, &empty)).collect(),
        ));
    }
    for row in body {
        grid.push((
            false,
            (0..columns).map(|c| cell_at(row, c, &empty)).collect(),
        ));
    }

    let mut min = vec![0.0f32; columns];
    let mut max = vec![0.0f32; columns];
    for (is_header, cells) in &grid {
        for (column, inlines) in cells.iter().enumerate() {
            let widest = shape(cx, inlines, *is_header, &metrics, flow.ink, None).content_width();
            // A header does not wrap (`white-space: nowrap`); other cells
            // break between words only.
            let narrowest = if *is_header {
                widest
            } else {
                let unwrapped = shape(cx, inlines, false, &metrics, flow.ink, None);
                longest_word(&unwrapped)
            };
            max[column] = max[column].max(widest.ceil());
            min[column] = min[column].max(narrowest.min(widest).ceil());
        }
    }
    let column_min: Vec<f32> = min.iter().map(|w| w + 2.0 * pad_x).collect();
    let column_max: Vec<f32> = max.iter().map(|w| w + 2.0 * pad_x).collect();
    let available = (flow.width - 2.0 * border).max(1.0);
    let widths = distribute(&column_min, &column_max, available, cx.boxes.table_fill);
    let table_width: f32 = widths.iter().sum();

    let mut rows = Vec::with_capacity(grid.len());
    let mut top = 0.0f32;
    let rule = border;
    let last = grid.len().saturating_sub(1);
    for (row_index, (is_header, cells)) in grid.iter().enumerate() {
        let mut shaped = Vec::with_capacity(columns);
        let mut tallest = 0.0f32;
        for (column, inlines) in cells.iter().enumerate() {
            let inner = (widths[column] - 2.0 * pad_x).max(1.0);
            // The measured width is rounded up; nudge so a cell never wraps by a hair.
            let text = shape(
                cx,
                inlines,
                *is_header,
                &metrics,
                flow.ink,
                Some(inner + 0.5),
            );
            let own = if inlines.is_empty() {
                if *is_header {
                    metrics.head_line
                } else {
                    metrics.body_line
                }
            } else {
                text.height()
            };
            tallest = tallest.max(own);
            shaped.push(TableCell {
                text,
                align: align.get(column).copied().unwrap_or(Align::None),
            });
        }
        let height = tallest + 2.0 * pad_y + if row_index == last { 0.0 } else { rule };
        rows.push(TableRow {
            top,
            height,
            cells: shaped,
        });
        top += height;
    }
    let overflow = table_width > available + 0.5;
    let table = TableBox {
        widths,
        rows,
        header,
        table_width,
        scroll_x: 0.0,
        overflow,
        border,
        pad_x,
        pad_y,
    };
    LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, top + 2.0 * border),
        stop,
        content: Content::Table(table),
        depth: flow.depth,
    }
}

/// Width of the widest run of glyphs between spaces in text laid out on
/// unwrapped lines: the min-content width of the cell.
fn longest_word(text: &RichText) -> f32 {
    let buffer = text.block.buffer();
    let mut widest = 0.0f32;
    let mut current: Option<(usize, f32, f32)> = None;
    for glyph in text.block.glyphs() {
        let is_break = buffer
            .lines
            .get(glyph.line)
            .and_then(|line| line.text().get(glyph.start..glyph.end))
            .is_some_and(|piece| piece == " " || piece == "\t");
        let continues = current.is_some_and(|(line, ..)| line == glyph.line);
        if (is_break || !continues)
            && let Some((_, left, right)) = current.take()
        {
            widest = widest.max(right - left);
        }
        if is_break {
            continue;
        }
        current = Some(match current {
            Some((line, left, right)) => (line, left, right.max(glyph.x + glyph.width)),
            None => (glyph.line, glyph.x, glyph.x + glyph.width),
        });
    }
    if let Some((_, left, right)) = current {
        widest = widest.max(right - left);
    }
    widest
}

fn cell_at<'a>(row: &'a [Vec<Inline>], column: usize, empty: &'a Vec<Inline>) -> &'a [Inline] {
    row.get(column).unwrap_or(empty)
}

/// Corner radius of a table, CSS px.
const RADIUS: f32 = 10.0;

pub fn paint(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    let Content::Table(table) = &block.content else {
        return;
    };
    let theme = paint.theme;
    let area = paint.window_rect(&block.rect);
    let radius = RADIUS * paint.scale;
    canvas.with_round_corners(area, radius, |canvas| {
        paint_square(paint, canvas, fonts, block);
    });
    canvas.stroke_rounded_rect(area, radius, table.border, theme.rule);
}

/// The table with square corners, which [`paint`] then rounds.
fn paint_square(paint: &PaintCx, canvas: &mut Canvas, fonts: &mut Fonts, block: &LaidBlock) {
    let Content::Table(table) = &block.content else {
        return;
    };
    let theme = paint.theme;
    let area = paint.window_rect(&block.rect);
    canvas.stroke_rect(area, table.border, theme.rule);
    let inner = Rect::new(
        area.x + table.border,
        area.y + table.border,
        area.w - 2.0 * table.border,
        area.h - 2.0 * table.border,
    );
    if !table.overflow {
        paint_rows(canvas, fonts, table, inner.x, inner.y, paint);
        return;
    }
    paint_clipped(canvas, fonts, table, inner, paint);
}

/// Draw every visible row with the table's top left at `(x, y)`.
fn paint_rows(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    table: &TableBox,
    x: f32,
    y: f32,
    paint: &PaintCx,
) {
    let theme = paint.theme;
    let line = table.border;
    let viewport = canvas.height() as f32;
    let last = table.rows.len().saturating_sub(1);
    for (row_index, row) in table.rows.iter().enumerate() {
        let top = y + row.top;
        if top > viewport || top + row.height < 0.0 {
            continue;
        }
        let is_header = table.header && row_index == 0;
        let band = Rect::new(x, top, table.table_width, row.height);
        if is_header {
            canvas.fill_rect(band, theme.surface);
        } else {
            let body_index = row_index - usize::from(table.header);
            if body_index % 2 == 1 {
                canvas.fill_rect(band, boxes::stripe(theme));
            }
        }
        if row_index != last {
            canvas.fill_rect(
                Rect::new(x, top + row.height - line, table.table_width, line),
                inline::rule_soft(theme),
            );
        }
        let mut column_x = x;
        for (column, cell) in row.cells.iter().enumerate() {
            let width = table.widths[column];
            let inner = width - 2.0 * table.pad_x;
            let free = (inner - cell.text.content_width()).max(0.0);
            let offset = match cell.align {
                Align::Center => free / 2.0,
                Align::Right => free,
                Align::None | Align::Left => 0.0,
            };
            let text_y = top + table.pad_y;
            inline::paint(
                canvas,
                fonts,
                &cell.text,
                column_x + table.pad_x + offset,
                text_y,
                theme,
                paint.scale,
            );
            column_x += width;
        }
    }
}

/// Paint a table wider than its block through an off-screen buffer, because
/// the canvas has no clip rectangle. Only the rows on screen are buffered.
fn paint_clipped(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    table: &TableBox,
    clip: Rect,
    paint: &PaintCx,
) {
    let top = clip.y.max(0.0).round();
    let bottom = clip.bottom().min(canvas.height() as f32).round();
    let left = clip.x.max(0.0).round();
    let right = clip.right().min(canvas.width() as f32).round();
    if bottom <= top || right <= left {
        return;
    }
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    let mut pixels = vec![paint.theme.bg.to_pixel(); width as usize * height as usize];
    {
        let mut buffer = Canvas::new(&mut pixels, width, height);
        paint_rows(
            &mut buffer,
            fonts,
            table,
            clip.x - table.scroll_x - left,
            clip.y - top,
            paint,
        );
    }
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return;
    };
    for (pixel, out) in pixels
        .iter()
        .zip(pixmap.data_mut().as_chunks_mut::<4>().0.iter_mut())
    {
        out.copy_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255]);
    }
    canvas.draw_pixmap(&pixmap, left, top);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{Layout, lay_out},
        text::Fonts,
        theme::Theme,
    };
    use limpid_core::{
        doc::{Mode, parse},
        settings::Settings,
    };

    fn approx(left: &[f32], right: &[f32]) {
        assert_eq!(left.len(), right.len(), "{left:?} vs {right:?}");
        for (a, b) in left.iter().zip(right) {
            assert!((a - b).abs() < 0.01, "{left:?} vs {right:?}");
        }
    }

    // Column-width algorithm, with explicit numbers.

    #[test]
    fn should_use_max_content_widths_when_everything_fits_and_fill_is_off() {
        approx(
            &distribute(&[10.0, 20.0], &[50.0, 80.0], 200.0, false),
            &[50.0, 80.0],
        );
    }

    #[test]
    fn should_stretch_in_proportion_to_max_content_when_filling() {
        // 50:80 of 260 is 100:160.
        approx(
            &distribute(&[10.0, 20.0], &[50.0, 80.0], 260.0, true),
            &[100.0, 160.0],
        );
    }

    #[test]
    fn should_shrink_proportionally_between_min_and_max_when_max_overflows() {
        // min 30 total, max 130 total, room for 80: half way between.
        approx(
            &distribute(&[10.0, 20.0], &[50.0, 80.0], 80.0, true),
            &[10.0 + 40.0 * 0.5, 20.0 + 60.0 * 0.5],
        );
    }

    #[test]
    fn should_never_go_below_min_content_when_the_room_is_too_small() {
        approx(
            &distribute(&[40.0, 60.0], &[90.0, 90.0], 50.0, true),
            &[40.0, 60.0],
        );
    }

    #[test]
    fn should_take_exactly_the_room_when_min_content_equals_it() {
        approx(
            &distribute(&[40.0, 60.0], &[90.0, 90.0], 100.0, true),
            &[40.0, 60.0],
        );
    }

    #[test]
    fn should_split_evenly_when_every_column_is_empty_and_filling() {
        approx(
            &distribute(&[0.0, 0.0, 0.0], &[0.0, 0.0, 0.0], 90.0, true),
            &[30.0, 30.0, 30.0],
        );
    }

    #[test]
    fn should_repair_max_below_min_and_garbage_values_when_distributing() {
        // max below min is raised to min; NaN and negative count as zero.
        approx(
            &distribute(&[30.0, f32::NAN], &[10.0, -5.0], 100.0, false),
            &[30.0, 0.0],
        );
        assert!(distribute(&[], &[], 100.0, true).is_empty());
        // No usable room means min-content.
        approx(&distribute(&[10.0], &[20.0], f32::NAN, false), &[10.0]);
    }

    #[test]
    fn should_use_the_shorter_list_when_min_and_max_lengths_differ() {
        assert_eq!(distribute(&[1.0, 2.0, 3.0], &[5.0], 100.0, false).len(), 1);
    }

    // Layout.

    fn rich(text: &str) -> RichText {
        let style = TextStyle::sans(Theme::dark().ink);
        let flat = inline::Flat::plain(text, style);
        inline::build(
            &mut Fonts::new(),
            &flat,
            style,
            &Theme::dark(),
            20.0,
            30.0,
            None,
        )
    }

    #[test]
    fn should_measure_the_widest_word_when_the_cell_has_several() {
        let widest_alone = rich("extraordinary").content_width();
        let sentence = rich("a extraordinary b to be");
        let longest = longest_word(&sentence);
        assert!(
            (longest - widest_alone).abs() < 1.0,
            "{longest} vs {widest_alone}"
        );
        assert!(longest < sentence.content_width());
    }

    #[test]
    fn should_measure_zero_when_the_cell_is_empty_or_only_spaces() {
        assert_eq!(longest_word(&rich("")), 0.0);
        assert_eq!(longest_word(&rich("   ")), 0.0);
    }

    #[test]
    fn should_restart_the_word_at_a_hard_break_when_measuring_lines() {
        let two_lines = rich("ab\nabcdefgh");
        let alone = rich("abcdefgh").content_width();
        assert!((longest_word(&two_lines) - alone).abs() < 1.0);
    }

    fn table(markdown: &str, width: f32) -> Layout {
        let doc = parse(markdown, Mode::File, &Settings::default());
        lay_out(&doc, width, &Theme::dark(), 1.0, &mut Fonts::new())
    }

    fn table_box(layout: &Layout) -> &TableBox {
        match &layout.blocks[0].content {
            Content::Table(table) => table,
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn should_fill_the_column_when_the_table_is_narrow() {
        let layout = table("| a | b |\n|---|---|\n| 1 | 2 |\n", 1000.0);
        let table = table_box(&layout);
        assert!(!table.overflow);
        let available = layout.blocks[0].rect.w - 2.0 * table.border;
        assert!(
            (table.table_width - available).abs() < 0.5,
            "{}",
            table.table_width
        );
        assert!(layout.blocks[0].stop);
    }

    #[test]
    fn should_make_the_header_row_first_and_taller_than_nothing_when_a_header_exists() {
        let layout = table("| h1 | h2 |\n|----|----|\n| x | y |\n", 900.0);
        let table = table_box(&layout);
        assert!(table.header);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].top, 0.0);
        assert!(table.rows[1].top >= table.rows[0].height);
        assert!(layout.blocks[0].rect.h > table.rows[1].top + table.rows[1].height);
    }

    #[test]
    fn should_wrap_a_long_cell_and_grow_the_row_when_the_column_is_narrow() {
        let long = "word ".repeat(16);
        let markdown = format!("| a | b |\n|---|---|\n| {long} | x |\n");
        let narrow = table(&markdown, 400.0);
        let wide = table(&markdown, 1400.0);
        let narrow_row = &table_box(&narrow).rows[1];
        let wide_row = &table_box(&wide).rows[1];
        assert_eq!(wide_row.cells[0].text.block.line_count(), 1);
        assert!(narrow_row.cells[0].text.block.line_count() > 1);
        assert!(narrow_row.height > wide_row.height);
    }

    #[test]
    fn should_overflow_and_scroll_when_a_word_is_wider_than_the_column() {
        let word = "x".repeat(200);
        let markdown = format!("| a | b |\n|---|---|\n| {word} | y |\n");
        let layout = table(&markdown, 500.0);
        let table = table_box(&layout);
        assert!(table.overflow);
        assert!(table.table_width > layout.blocks[0].rect.w);
        assert!(table.max_scroll(layout.blocks[0].rect.w) > 0.0);
    }

    #[test]
    fn should_not_break_a_header_when_the_column_is_narrow() {
        let markdown = "| a very long header title | b |\n|---|---|\n| 1 | 2 |\n";
        let layout = table(markdown, 360.0);
        let table = table_box(&layout);
        assert_eq!(table.rows[0].cells[0].text.block.line_count(), 1);
    }

    #[test]
    fn should_carry_alignment_to_the_cells_when_the_delimiter_row_sets_it() {
        let layout = table(
            "| l | c | r | n |\n|:--|:-:|--:|---|\n| 1 | 2 | 3 | 4 |\n",
            900.0,
        );
        let aligns: Vec<Align> = table_box(&layout).rows[1]
            .cells
            .iter()
            .map(|c| c.align)
            .collect();
        assert_eq!(
            aligns,
            vec![Align::Left, Align::Center, Align::Right, Align::None]
        );
    }

    #[test]
    fn should_pad_missing_cells_when_a_row_is_ragged() {
        let layout = table(
            "| a | b | c |\n|---|---|---|\n| 1 |\n| 1 | 2 | 3 | 4 |\n",
            900.0,
        );
        let table = table_box(&layout);
        assert!(
            table
                .rows
                .iter()
                .all(|row| row.cells.len() == table.widths.len())
        );
    }

    #[test]
    fn should_survive_an_empty_table_and_empty_cells_when_laying_out() {
        let layout = table("| | |\n|-|-|\n| | |\n", 900.0);
        let table = table_box(&layout);
        assert!(table.rows.iter().all(|row| row.height > 0.0));
        assert!(layout.blocks[0].rect.h > 0.0);
    }

    #[test]
    fn should_clamp_horizontal_scroll_to_the_overflow_when_scrolling() {
        let word = "x".repeat(200);
        let markdown = format!("| a | b |\n|---|---|\n| {word} | y |\n");
        let mut layout = table(&markdown, 500.0);
        let block = &mut layout.blocks[0];
        assert!(block.scroll_table_x(40.0));
        assert!(block.scroll_table_x(1e6));
        assert!(!block.scroll_table_x(1e6));
        assert!(block.scroll_table_x(-1e6));
        assert!(!block.scroll_table_x(-1.0));
    }

    #[test]
    fn should_not_scroll_when_the_table_fits() {
        let mut layout = table("| a |\n|---|\n| 1 |\n", 900.0);
        assert!(!layout.blocks[0].scroll_table_x(50.0));
    }

    #[test]
    fn should_double_the_geometry_when_scale_is_two() {
        let markdown = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let doc = parse(markdown, Mode::File, &Settings::default());
        let mut fonts = Fonts::new();
        let one = lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut fonts);
        let two = lay_out(&doc, 2000.0, &Theme::dark(), 2.0, &mut fonts);
        let ratio = two.blocks[0].rect.h / one.blocks[0].rect.h;
        assert!((1.8..2.2).contains(&ratio), "{ratio}");
    }

    #[test]
    fn should_round_the_corners_of_a_table_when_painting() {
        let doc = parse(
            "| a | b |\n|---|---|\n| 1 | 2 |\n",
            Mode::File,
            &Settings::default(),
        );
        let theme = Theme::dark();
        let mut fonts = Fonts::new();
        let mut layout = crate::layout::lay_out(&doc, 800.0, &theme, 1.0, &mut fonts);
        let (width, height) = (800u32, 400u32);
        let mut pixels = vec![0u32; (width * height) as usize];
        let mut canvas = Canvas::new(&mut pixels, width, height);
        canvas.clear(theme.bg);
        layout.paint(&mut canvas, &mut fonts, 0.0);
        let block = &layout.blocks[0];
        let left = layout.column_x.round() as i32;
        let top = block.rect.y.round() as i32;
        let middle = left + (block.rect.w / 2.0) as i32;
        assert_eq!(
            canvas.pixel(left, top),
            Some(theme.bg.to_pixel()),
            "the corner shows the page"
        );
        assert_eq!(
            canvas.pixel(middle, top),
            Some(theme.rule.to_pixel()),
            "the edge keeps its border"
        );
    }
}
