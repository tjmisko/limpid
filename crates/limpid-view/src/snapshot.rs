//! Headless rendering: lay out and paint a page into a buffer and write a PNG.
//! This is the test path; it needs no display.

use crate::{paint::Canvas, window::Page};
use std::{fs::File, io::BufWriter, path::Path};

/// Largest accepted side in device pixels, so a typo cannot allocate gigabytes.
pub const MAX_SIDE: u32 = 16_384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// `0x00RRGGBB`, row-major.
    pub pixels: Vec<u32>,
}

/// Parse `WIDTHxHEIGHT` (logical pixels).
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once(['x', 'X'])?;
    let width: u32 = width.parse().ok()?;
    let height: u32 = height.parse().ok()?;
    (width > 0 && height > 0 && width <= MAX_SIDE && height <= MAX_SIDE).then_some((width, height))
}

/// Paint `page` into a `width` by `height` logical-pixel viewport at `scale`.
pub fn render(page: &mut dyn Page, width: u32, height: u32, scale: f32) -> Result<Image, String> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(format!("scale must be a positive number, got {scale}"));
    }
    let physical = |side: u32| (f64::from(side) * f64::from(scale)).round() as u32;
    let (pixel_width, pixel_height) = (physical(width), physical(height));
    if pixel_width == 0 || pixel_height == 0 || pixel_width > MAX_SIDE || pixel_height > MAX_SIDE {
        return Err(format!(
            "{pixel_width}x{pixel_height} device pixels is outside 1..={MAX_SIDE}"
        ));
    }
    let mut pixels = vec![0u32; pixel_width as usize * pixel_height as usize];
    page.paint(
        &mut Canvas::new(&mut pixels, pixel_width, pixel_height),
        scale,
    );
    Ok(Image {
        width: pixel_width,
        height: pixel_height,
        pixels,
    })
}

pub fn write_png(path: &Path, image: &Image) -> Result<(), String> {
    let file = File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(image.pixels.len() * 3);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
    }
    writer
        .write_image_data(&bytes)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::KeyEvent;

    struct Flat;
    impl Page for Flat {
        fn title(&self) -> String {
            "flat".into()
        }
        fn paint(&mut self, canvas: &mut Canvas, _scale: f32) {
            canvas.clear(crate::theme::Color::rgb(1, 2, 3));
        }
        fn key(&mut self, _event: &KeyEvent) -> bool {
            false
        }
    }

    #[test]
    fn should_parse_sizes_when_well_formed() {
        assert_eq!(parse_size("800x600"), Some((800, 600)));
        assert_eq!(parse_size("1X2"), Some((1, 2)));
    }

    #[test]
    fn should_reject_sizes_when_malformed_or_out_of_range() {
        for text in [
            "", "800", "x600", "800x", "0x10", "10x0", "-1x5", "ax5", "99999x5", "5x5x5",
        ] {
            assert_eq!(parse_size(text), None, "{text:?}");
        }
    }

    #[test]
    fn should_multiply_dimensions_by_scale_when_rendering() {
        let image = render(&mut Flat, 80, 60, 2.0).unwrap();
        assert_eq!((image.width, image.height), (160, 120));
        assert_eq!(image.pixels.len(), 160 * 120);
        assert_eq!(image.pixels[0], 0x0001_0203);
    }

    #[test]
    fn should_round_dimensions_when_scale_is_fractional() {
        let image = render(&mut Flat, 10, 10, 1.25).unwrap();
        assert_eq!((image.width, image.height), (13, 13));
    }

    #[test]
    fn should_refuse_when_scale_or_size_is_unusable() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(render(&mut Flat, 10, 10, scale).is_err(), "{scale}");
        }
        assert!(render(&mut Flat, 10_000, 10, 4.0).is_err());
        assert!(render(&mut Flat, 1, 1, 0.1).is_err());
    }

    #[test]
    fn should_write_a_png_that_decodes_to_the_same_pixels_when_saving() {
        let image = render(&mut Flat, 5, 4, 1.0).unwrap();
        let path =
            std::env::temp_dir().join(format!("limpid-view-snap-{}.png", std::process::id()));
        write_png(&path, &image).unwrap();
        let decoder = png::Decoder::new(File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height), (5, 4));
        assert_eq!(&buffer[..3], &[1, 2, 3]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn should_report_the_path_when_the_output_directory_is_missing() {
        let image = render(&mut Flat, 2, 2, 1.0).unwrap();
        let error = write_png(Path::new("/nonexistent-dir/x.png"), &image).unwrap_err();
        assert!(error.contains("/nonexistent-dir/x.png"), "{error}");
    }
}
