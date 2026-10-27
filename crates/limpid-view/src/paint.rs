//! A software canvas over a `0x00RRGGBB` pixel buffer. Everything is clipped
//! to the buffer, and every draw blends over what is already there.

use crate::{
    text::{Fonts, TextBlock},
    theme::Color,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    /// # Panics
    /// When `pixels` is shorter than `width * height`.
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        assert!(
            pixels.len() as u64 >= u64::from(width) * u64::from(height),
            "pixel buffer of {} is smaller than {width}x{height}",
            pixels.len()
        );
        Self {
            pixels,
            width,
            height,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixel at `(x, y)`, or `None` outside the canvas.
    pub fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(self.pixels[y as usize * self.width as usize + x as usize])
    }

    pub fn clear(&mut self, color: Color) {
        let len = self.width as usize * self.height as usize;
        self.pixels[..len].fill(color.to_pixel());
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some((x0, y0, x1, y1)) = self.clip(rect) else {
            return;
        };
        if color.a == 255 {
            let value = color.to_pixel();
            for y in y0..y1 {
                let row = y as usize * self.width as usize;
                self.pixels[row + x0 as usize..row + x1 as usize].fill(value);
            }
            return;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, color.r, color.g, color.b, color.a);
            }
        }
    }

    /// An outline of `thickness` pixels drawn inside `rect`.
    pub fn stroke_rect(&mut self, rect: Rect, thickness: f32, color: Color) {
        let t = thickness.min(rect.w / 2.0).min(rect.h / 2.0);
        if t <= 0.0 {
            return;
        }
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, t), color);
        self.fill_rect(Rect::new(rect.x, rect.bottom() - t, rect.w, t), color);
        self.fill_rect(Rect::new(rect.x, rect.y + t, t, rect.h - 2.0 * t), color);
        self.fill_rect(
            Rect::new(rect.right() - t, rect.y + t, t, rect.h - 2.0 * t),
            color,
        );
    }

    /// A filled rectangle with corners of `radius`, edges smoothed.
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.rounded(rect, radius, 0.0, color);
    }

    /// An outline of `thickness` pixels drawn inside a rounded `rect`.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, thickness: f32, color: Color) {
        if thickness > 0.0 {
            self.rounded(rect, radius, thickness, color);
        }
    }

    /// Fill the rounded shape, or with `ring` above zero only the band that
    /// wide along its edge.
    fn rounded(&mut self, rect: Rect, radius: f32, ring: f32, color: Color) {
        let Some((x0, y0, x1, y1)) = self.clip(rect) else {
            return;
        };
        let radius = radius.max(0.0).min(rect.w / 2.0).min(rect.h / 2.0);
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        let coverage = |px: f32, py: f32, half_w: f32, half_h: f32, r: f32| {
            let qx = (px - cx).abs() - (half_w - r);
            let qy = (py - cy).abs() - (half_h - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            let distance = outside + qx.max(qy).min(0.0) - r;
            (0.5 - distance).clamp(0.0, 1.0)
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut amount = coverage(px, py, rect.w / 2.0, rect.h / 2.0, radius);
                if ring > 0.0 {
                    let inner = coverage(
                        px,
                        py,
                        rect.w / 2.0 - ring,
                        rect.h / 2.0 - ring,
                        (radius - ring).max(0.0),
                    );
                    amount = (amount - inner).max(0.0);
                }
                let alpha = (f32::from(color.a) * amount).round() as u8;
                if alpha > 0 {
                    self.blend(x, y, color.r, color.g, color.b, alpha);
                }
            }
        }
    }

    /// A one-pixel-high line from `x` across `length`.
    pub fn hline(&mut self, x: f32, y: f32, length: f32, color: Color) {
        self.fill_rect(Rect::new(x, y, length, 1.0), color);
    }

    /// Draw shaped text with its top-left corner at `(x, y)`. Glyph colours
    /// set by spans win over `color`, which fills glyphs with no colour.
    pub fn draw_text(
        &mut self,
        fonts: &mut Fonts,
        block: &TextBlock,
        x: f32,
        y: f32,
        color: Color,
    ) {
        self.draw_text_with(fonts, block, x, y, color, None);
    }

    /// [`Self::draw_text`], with `color` also replacing the glyphs coloured
    /// `replaced`, so spans with a colour of their own keep it.
    pub fn draw_text_with(
        &mut self,
        fonts: &mut Fonts,
        block: &TextBlock,
        x: f32,
        y: f32,
        color: Color,
        replaced: Option<Color>,
    ) {
        let origin_x = x.round() as i32;
        let origin_y = y.round() as i32;
        let default = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);
        let Fonts { system, cache } = fonts;
        block
            .buffer()
            .draw(system, cache, default, |gx, gy, gw, gh, glyph| {
                let alpha = glyph.a();
                if alpha == 0 {
                    return;
                }
                let (red, green, blue) = match replaced {
                    Some(from) if (glyph.r(), glyph.g(), glyph.b()) == (from.r, from.g, from.b) => {
                        (color.r, color.g, color.b)
                    }
                    _ => (glyph.r(), glyph.g(), glyph.b()),
                };
                for dy in 0..gh as i32 {
                    for dx in 0..gw as i32 {
                        self.blend_at(
                            origin_x + gx + dx,
                            origin_y + gy + dy,
                            red,
                            green,
                            blue,
                            alpha,
                        );
                    }
                }
            });
    }

    /// Draw a tiny-skia pixmap (premultiplied RGBA) with its top-left at
    /// `(x, y)`.
    pub fn draw_pixmap(&mut self, pixmap: &tiny_skia::Pixmap, x: f32, y: f32) {
        let origin_x = x.round() as i32;
        let origin_y = y.round() as i32;
        let width = pixmap.width() as i32;
        let data = pixmap.data();
        for row in 0..pixmap.height() as i32 {
            let py = origin_y + row;
            if py < 0 || py >= self.height as i32 {
                continue;
            }
            for column in 0..width {
                let px = origin_x + column;
                if px < 0 || px >= self.width as i32 {
                    continue;
                }
                let at = (row * width + column) as usize * 4;
                let (r, g, b, a) = (data[at], data[at + 1], data[at + 2], data[at + 3]);
                if a == 0 {
                    continue;
                }
                let index = py as usize * self.width as usize + px as usize;
                let under = self.pixels[index];
                let keep = 255 - u32::from(a);
                let mix = |source: u8, shift: u32| {
                    let below = (under >> shift) & 0xff;
                    (u32::from(source) + (below * keep + 127) / 255).min(255)
                };
                self.pixels[index] = (mix(r, 16) << 16) | (mix(g, 8) << 8) | mix(b, 0);
            }
        }
    }

    /// Paint over the four corners of `rect` that lie outside a rounding of
    /// `radius` pixels with `color`, to cut a square block's corners.
    pub fn cut_corners(&mut self, rect: Rect, radius: f32, color: Color) {
        let Some((rect, radius)) = snap_round(rect, radius) else {
            return;
        };
        if radius < 1.0 {
            return;
        }
        for (square, centre) in corners(rect, radius) {
            self.corner_pixels(square, |x, y| {
                let outside = 1.0 - coverage(radius, distance(centre, x, y));
                (outside > 0.0).then_some((color, outside))
            });
        }
    }

    /// Run `draw`, then put back what was under the four corners of `rect`
    /// that lie outside a rounding of `radius` pixels, so whatever `draw`
    /// paints gets rounded corners over any background.
    pub fn with_round_corners(
        &mut self,
        rect: Rect,
        radius: f32,
        draw: impl FnOnce(&mut Canvas<'a>),
    ) {
        let Some((snapped, radius)) = snap_round(rect, radius) else {
            draw(self);
            return;
        };
        if radius < 1.0 {
            draw(self);
            return;
        }
        let mut saved = Vec::new();
        for (square, _) in corners(snapped, radius) {
            for y in square.y as i32..square.bottom() as i32 {
                for x in square.x as i32..square.right() as i32 {
                    saved.push(self.pixel_at(x, y));
                }
            }
        }
        draw(self);
        let mut saved = saved.into_iter();
        for (square, centre) in corners(snapped, radius) {
            for y in square.y as i32..square.bottom() as i32 {
                for x in square.x as i32..square.right() as i32 {
                    let Some(Some(under)) = saved.next() else {
                        continue;
                    };
                    let d = distance(centre, x as f32 + 0.5, y as f32 + 0.5);
                    let outside = 1.0 - coverage(radius, d);
                    if outside <= 0.0 {
                        continue;
                    }
                    let alpha = (outside * 255.0).round() as u8;
                    self.blend_at(
                        x,
                        y,
                        (under >> 16) as u8,
                        (under >> 8) as u8,
                        under as u8,
                        alpha,
                    );
                }
            }
        }
    }

    fn pixel_at(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(self.pixels[y as usize * self.width as usize + x as usize])
    }

    /// Blend what `shade` returns for the centre of each pixel in `square`.
    fn corner_pixels(&mut self, square: Rect, shade: impl Fn(f32, f32) -> Option<(Color, f32)>) {
        let (x0, y0) = (square.x as i32, square.y as i32);
        let (x1, y1) = (square.right() as i32, square.bottom() as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let Some((color, cover)) = shade(x as f32 + 0.5, y as f32 + 0.5) else {
                    continue;
                };
                let alpha = (f32::from(color.a) * cover).round() as u8;
                self.blend_at(x, y, color.r, color.g, color.b, alpha);
            }
        }
    }

    fn clip(&self, rect: Rect) -> Option<(i32, i32, i32, i32)> {
        if !(rect.x.is_finite() && rect.y.is_finite() && rect.w.is_finite() && rect.h.is_finite()) {
            return None;
        }
        let x0 = (rect.x.round() as i32).max(0);
        let y0 = (rect.y.round() as i32).max(0);
        let x1 = ((rect.x + rect.w).round() as i32).min(self.width as i32);
        let y1 = ((rect.y + rect.h).round() as i32).min(self.height as i32);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some((x0, y0, x1, y1))
    }

    fn blend_at(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        self.blend(x, y, r, g, b, a);
    }

    /// Straight-alpha source over an opaque destination. `x`, `y` are inside.
    fn blend(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
        let index = y as usize * self.width as usize + x as usize;
        let under = self.pixels[index];
        let alpha = u32::from(a);
        let mix = |source: u8, shift: u32| {
            let below = (under >> shift) & 0xff;
            (u32::from(source) * alpha + below * (255 - alpha) + 127) / 255
        };
        self.pixels[index] = (mix(r, 16) << 16) | (mix(g, 8) << 8) | mix(b, 0);
    }
}

/// `rect` snapped to whole pixels, and `radius` limited to half its sides.
/// `None` when the rectangle is empty or not finite.
fn snap_round(rect: Rect, radius: f32) -> Option<(Rect, f32)> {
    if !(rect.x.is_finite() && rect.y.is_finite() && rect.w.is_finite() && rect.h.is_finite()) {
        return None;
    }
    let (x0, y0) = (rect.x.round(), rect.y.round());
    let snapped = Rect::new(
        x0,
        y0,
        rect.right().round() - x0,
        rect.bottom().round() - y0,
    );
    if snapped.w < 1.0 || snapped.h < 1.0 {
        return None;
    }
    let radius = radius.round().min(snapped.w / 2.0).min(snapped.h / 2.0);
    Some((snapped, radius.max(0.0)))
}

/// The four `radius` squares at the corners of `rect` with their circle
/// centres.
fn corners(rect: Rect, radius: f32) -> [(Rect, (f32, f32)); 4] {
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.right() - radius, rect.bottom() - radius);
    [
        (
            Rect::new(left, top, radius, radius),
            (left + radius, top + radius),
        ),
        (Rect::new(right, top, radius, radius), (right, top + radius)),
        (
            Rect::new(left, bottom, radius, radius),
            (left + radius, bottom),
        ),
        (Rect::new(right, bottom, radius, radius), (right, bottom)),
    ]
}

fn distance(centre: (f32, f32), x: f32, y: f32) -> f32 {
    (x - centre.0).hypot(y - centre.1)
}

/// How much of a pixel at `distance` from the centre of a circle of `radius`
/// lies inside it.
fn coverage(radius: f32, distance: f32) -> f32 {
    (radius - distance + 0.5).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextStyle;
    use crate::theme::Families;

    const WHITE: Color = Color::rgb(255, 255, 255);
    const RED: Color = Color::rgb(255, 0, 0);

    fn buffer(width: u32, height: u32) -> Vec<u32> {
        vec![0; (width * height) as usize]
    }

    #[test]
    fn should_fill_only_the_rect_when_rect_is_inside() {
        let mut pixels = buffer(10, 10);
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        canvas.fill_rect(Rect::new(2.0, 3.0, 4.0, 2.0), RED);
        assert_eq!(canvas.pixel(2, 3), Some(0xff0000));
        assert_eq!(canvas.pixel(5, 4), Some(0xff0000));
        assert_eq!(canvas.pixel(6, 4), Some(0));
        assert_eq!(canvas.pixel(2, 5), Some(0));
        assert_eq!(canvas.pixel(1, 3), Some(0));
    }

    #[test]
    fn should_leave_the_corner_empty_when_filling_a_rounded_rect() {
        let mut pixels = buffer(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 7.0, RED);
        assert_eq!(canvas.pixel(0, 0), Some(0));
        assert_eq!(canvas.pixel(10, 10), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
    }

    #[test]
    fn should_only_colour_the_edge_when_stroking_a_rounded_rect() {
        let mut pixels = buffer(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.stroke_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 7.0, 1.0, RED);
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 10), Some(0));
        assert_eq!(canvas.pixel(0, 0), Some(0));
    }

    #[test]
    fn should_clip_without_panicking_when_rect_leaves_the_canvas() {
        let mut pixels = buffer(8, 8);
        let mut canvas = Canvas::new(&mut pixels, 8, 8);
        canvas.fill_rect(Rect::new(-5.0, -5.0, 8.0, 8.0), RED);
        canvas.fill_rect(Rect::new(6.0, 6.0, 100.0, 100.0), RED);
        assert_eq!(canvas.pixel(0, 0), Some(0xff0000));
        assert_eq!(canvas.pixel(4, 4), Some(0));
        assert_eq!(canvas.pixel(7, 7), Some(0xff0000));
        assert_eq!(canvas.pixel(8, 8), None);
        assert_eq!(canvas.pixel(-1, 0), None);
    }

    #[test]
    fn should_ignore_rects_when_they_are_empty_negative_or_not_finite() {
        let mut pixels = buffer(4, 4);
        let mut canvas = Canvas::new(&mut pixels, 4, 4);
        for rect in [
            Rect::new(1.0, 1.0, 0.0, 2.0),
            Rect::new(1.0, 1.0, -3.0, 2.0),
            Rect::new(f32::NAN, 0.0, 2.0, 2.0),
            Rect::new(0.0, 0.0, f32::INFINITY, 2.0),
            Rect::new(100.0, 100.0, 2.0, 2.0),
        ] {
            canvas.fill_rect(rect, RED);
        }
        assert!(pixels.iter().all(|&pixel| pixel == 0));
    }

    #[test]
    fn should_blend_half_alpha_over_the_background_when_color_is_translucent() {
        let mut pixels = vec![0x0000_00ff; 4];
        let mut canvas = Canvas::new(&mut pixels, 2, 2);
        canvas.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::rgba(255, 0, 0, 128));
        let pixel = canvas.pixel(0, 0).unwrap();
        let (r, g, b) = ((pixel >> 16) & 0xff, (pixel >> 8) & 0xff, pixel & 0xff);
        assert!((127..=129).contains(&r), "{r}");
        assert_eq!(g, 0);
        assert!((126..=128).contains(&b), "{b}");
    }

    #[test]
    fn should_leave_pixels_alone_when_alpha_is_zero() {
        let mut pixels = vec![0x0012_3456; 4];
        let mut canvas = Canvas::new(&mut pixels, 2, 2);
        canvas.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::rgba(255, 0, 0, 0));
        assert_eq!(canvas.pixel(1, 1), Some(0x0012_3456));
    }

    #[test]
    fn should_draw_a_border_and_keep_the_middle_when_stroking() {
        let mut pixels = buffer(10, 10);
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        canvas.stroke_rect(Rect::new(1.0, 1.0, 8.0, 8.0), 1.0, WHITE);
        assert_eq!(canvas.pixel(1, 1), Some(0xffffff));
        assert_eq!(canvas.pixel(8, 8), Some(0xffffff));
        assert_eq!(canvas.pixel(4, 1), Some(0xffffff));
        assert_eq!(canvas.pixel(1, 4), Some(0xffffff));
        assert_eq!(canvas.pixel(4, 4), Some(0));
        assert_eq!(canvas.pixel(0, 0), Some(0));
    }

    #[test]
    fn should_fill_solid_when_stroke_is_thicker_than_the_rect() {
        let mut pixels = buffer(6, 6);
        let mut canvas = Canvas::new(&mut pixels, 6, 6);
        canvas.stroke_rect(Rect::new(1.0, 1.0, 4.0, 4.0), 50.0, WHITE);
        assert_eq!(canvas.pixel(2, 2), Some(0xffffff));
        assert_eq!(canvas.pixel(0, 0), Some(0));
    }

    #[test]
    fn should_draw_one_pixel_row_when_drawing_an_hline() {
        let mut pixels = buffer(6, 3);
        let mut canvas = Canvas::new(&mut pixels, 6, 3);
        canvas.hline(1.0, 1.0, 3.0, WHITE);
        assert_eq!(canvas.pixel(1, 1), Some(0xffffff));
        assert_eq!(canvas.pixel(3, 1), Some(0xffffff));
        assert_eq!(canvas.pixel(4, 1), Some(0));
        assert_eq!(canvas.pixel(2, 0), Some(0));
        assert_eq!(canvas.pixel(2, 2), Some(0));
    }

    #[test]
    fn should_composite_premultiplied_pixels_when_drawing_a_pixmap() {
        let mut pixmap = tiny_skia::Pixmap::new(2, 1).unwrap();
        // Premultiplied: half-transparent white, then fully transparent.
        pixmap
            .data_mut()
            .copy_from_slice(&[128, 128, 128, 128, 0, 0, 0, 0]);
        let mut pixels = vec![0u32; 4];
        let mut canvas = Canvas::new(&mut pixels, 2, 2);
        canvas.draw_pixmap(&pixmap, 0.0, 0.0);
        let lit = canvas.pixel(0, 0).unwrap() & 0xff;
        assert!((127..=129).contains(&lit), "{lit}");
        assert_eq!(canvas.pixel(1, 0), Some(0));
    }

    #[test]
    fn should_clip_a_pixmap_when_it_hangs_off_the_edges() {
        let mut pixmap = tiny_skia::Pixmap::new(4, 4).unwrap();
        pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let mut pixels = buffer(4, 4);
        let mut canvas = Canvas::new(&mut pixels, 4, 4);
        canvas.draw_pixmap(&pixmap, -2.0, 3.0);
        canvas.draw_pixmap(&pixmap, 3.0, -3.0);
        assert_eq!(canvas.pixel(0, 3), Some(0xff0000));
        assert_eq!(canvas.pixel(1, 3), Some(0xff0000));
        assert_eq!(canvas.pixel(2, 3), Some(0));
        assert_eq!(canvas.pixel(3, 0), Some(0xff0000));
        assert_eq!(canvas.pixel(3, 1), Some(0));
    }

    #[test]
    fn should_paint_glyph_pixels_and_clip_when_drawing_text() {
        let mut fonts = Fonts::new();
        let mut block = TextBlock::new(&mut fonts, 16.0, 24.0);
        block.set_text(
            &mut fonts,
            "Hello",
            TextStyle::sans(WHITE),
            &Families::default(),
        );
        block.set_width(&mut fonts, Some(300.0));
        let mut pixels = buffer(100, 40);
        let mut canvas = Canvas::new(&mut pixels, 100, 40);
        canvas.draw_text(&mut fonts, &block, 4.0, 4.0, WHITE);
        assert!(pixels.iter().any(|&pixel| pixel != 0));

        // Entirely off the canvas, and partly off, must not panic.
        let mut canvas = Canvas::new(&mut pixels, 100, 40);
        canvas.draw_text(&mut fonts, &block, -500.0, -500.0, WHITE);
        canvas.draw_text(&mut fonts, &block, 90.0, 30.0, WHITE);
    }

    #[test]
    #[should_panic(expected = "smaller than")]
    fn should_panic_when_the_buffer_is_too_small() {
        let mut pixels = buffer(2, 2);
        let _ = Canvas::new(&mut pixels, 3, 3);
    }

    #[test]
    fn should_not_panic_when_a_round_rect_leaves_the_canvas() {
        let mut pixels = buffer(10, 10);
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        canvas.fill_rounded_rect(Rect::new(-20.0, -20.0, 100.0, 100.0), 6.0, RED);
        canvas.stroke_rounded_rect(Rect::new(-3.0, -3.0, 30.0, 30.0), 6.0, 2.0, RED);
        canvas.fill_rounded_rect(Rect::new(2.0, 2.0, 1.0, 1.0), 6.0, RED);
    }

    #[test]
    fn should_leave_the_corner_empty_and_the_middle_filled_when_filling_a_round_rect() {
        let mut pixels = buffer(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 6.0, RED);
        assert_eq!(canvas.pixel(0, 0), Some(0));
        assert_eq!(canvas.pixel(19, 19), Some(0));
        assert_eq!(canvas.pixel(10, 10), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
        assert_eq!(canvas.pixel(0, 10), Some(0xff0000));
    }

    #[test]
    fn should_match_a_plain_rect_when_the_radius_is_zero() {
        let mut round = buffer(8, 8);
        let mut plain = buffer(8, 8);
        Canvas::new(&mut round, 8, 8).fill_rounded_rect(Rect::new(1.0, 1.0, 6.0, 6.0), 0.0, RED);
        Canvas::new(&mut plain, 8, 8).fill_rect(Rect::new(1.0, 1.0, 6.0, 6.0), RED);
        assert_eq!(round, plain);
    }

    #[test]
    fn should_keep_the_middle_empty_and_the_edges_drawn_when_stroking_a_round_rect() {
        let mut pixels = buffer(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.stroke_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 6.0, 1.0, RED);
        assert_eq!(canvas.pixel(0, 0), Some(0));
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
        assert_eq!(canvas.pixel(0, 10), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 10), Some(0));
        assert_ne!(canvas.pixel(2, 2), Some(0), "the arc passes near (2, 2)");
    }

    #[test]
    fn should_restore_the_background_in_the_corners_when_drawing_with_round_corners() {
        let mut pixels = vec![0x00ff00; 400];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.with_round_corners(Rect::new(0.0, 0.0, 20.0, 20.0), 6.0, |canvas| {
            canvas.fill_rect(Rect::new(0.0, 0.0, 20.0, 20.0), Color::rgb(255, 0, 0));
        });
        assert_eq!(canvas.pixel(0, 0), Some(0x00ff00));
        assert_eq!(canvas.pixel(19, 19), Some(0x00ff00));
        assert_eq!(canvas.pixel(10, 10), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
    }

    #[test]
    fn should_paint_only_the_corners_when_cutting_a_filled_rect() {
        let mut pixels = vec![0xff0000; 400];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        canvas.cut_corners(Rect::new(0.0, 0.0, 20.0, 20.0), 6.0, Color::rgb(0, 0, 255));
        assert_eq!(canvas.pixel(0, 0), Some(0x0000ff));
        assert_eq!(canvas.pixel(19, 0), Some(0x0000ff));
        assert_eq!(canvas.pixel(10, 10), Some(0xff0000));
        assert_eq!(canvas.pixel(10, 0), Some(0xff0000));
    }

    #[test]
    fn should_not_panic_when_a_round_rect_is_tiny_or_off_canvas() {
        let mut pixels = buffer(6, 6);
        let mut canvas = Canvas::new(&mut pixels, 6, 6);
        canvas.fill_rounded_rect(Rect::new(-4.0, -4.0, 9.0, 3.0), 10.0, RED);
        canvas.stroke_rounded_rect(Rect::new(4.0, 4.0, 1.0, 1.0), 10.0, 3.0, RED);
        canvas.cut_corners(Rect::new(1.0, 1.0, 0.0, 0.0), 4.0, RED);
        canvas.fill_rounded_rect(Rect::new(f32::NAN, 0.0, 4.0, 4.0), 2.0, RED);
    }
}
