//! Helpers shared by the layout tests: building pages, and comparing a
//! rendered image with a committed PNG under a tolerance.

#![allow(dead_code)]

use limpid_core::{settings::Settings, transport::Scheme};
use limpid_view::{
    page::{DocPage, SharedFonts},
    snapshot::{Image, render, write_png},
    text::Fonts,
};
use std::{cell::RefCell, fs::File, path::PathBuf, rc::Rc};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A channel may differ by this much before a pixel counts as different.
const CHANNEL_TOLERANCE: u8 = 24;
/// The share of pixels allowed to differ.
const PIXEL_BUDGET: f64 = 0.01;

pub fn fonts() -> SharedFonts {
    Rc::new(RefCell::new(Fonts::new()))
}

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

pub fn page(source: &str, scheme: Scheme) -> DocPage {
    DocPage::from_source(
        source,
        "fixture".into(),
        &Settings::default(),
        scheme,
        fonts(),
    )
}

pub fn page_with(source: &str, settings: &str, scheme: Scheme) -> DocPage {
    DocPage::from_source(
        source,
        "fixture".into(),
        &Settings::parse(settings).unwrap(),
        scheme,
        fonts(),
    )
}

pub fn shot(page: &mut DocPage, width: u32, height: u32, scale: f32) -> Image {
    render(page, width, height, scale).unwrap()
}

fn decode(path: &PathBuf) -> Option<Image> {
    let file = File::open(path).ok()?;
    let mut reader = png::Decoder::new(file).read_info().unwrap();
    let mut rgb = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgb).unwrap();
    let pixels = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|pixel| (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]))
        .collect();
    Some(Image {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// How many pixels differ by more than the channel tolerance.
pub fn differing_pixels(left: &Image, right: &Image) -> usize {
    assert_eq!((left.width, left.height), (right.width, right.height));
    left.pixels
        .iter()
        .zip(&right.pixels)
        .filter(|(a, b)| {
            [16, 8, 0].iter().any(|shift| {
                let channel = |pixel: u32| ((pixel >> shift) & 0xff) as u8;
                channel(**a).abs_diff(channel(**b)) > CHANNEL_TOLERANCE
            })
        })
        .count()
}

/// Compare `actual` with `fixtures/snapshots/<name>.png`. On a mismatch the
/// actual image is written next to it as `<name>.actual.png`. With
/// `LIMPID_UPDATE_SNAPSHOTS=1` the expected image is (re)written instead.
pub fn assert_matches_snapshot(actual: &Image, name: &str) {
    let directory = PathBuf::from(FIXTURES).join("snapshots");
    let expected_path = directory.join(format!("{name}.png"));
    let actual_path = directory.join(format!("{name}.actual.png"));
    if std::env::var_os("LIMPID_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(&directory).unwrap();
        write_png(&expected_path, actual).unwrap();
        let _ = std::fs::remove_file(&actual_path);
        return;
    }
    let Some(expected) = decode(&expected_path) else {
        write_png(&actual_path, actual).unwrap();
        panic!(
            "no expected image at {}; run with LIMPID_UPDATE_SNAPSHOTS=1 to create it (actual written to {})",
            expected_path.display(),
            actual_path.display()
        );
    };
    let same_size = (expected.width, expected.height) == (actual.width, actual.height);
    let differing = if same_size {
        differing_pixels(&expected, actual)
    } else {
        usize::MAX
    };
    let budget = (f64::from(expected.width) * f64::from(expected.height) * PIXEL_BUDGET) as usize;
    if !same_size || differing > budget {
        write_png(&actual_path, actual).unwrap();
        panic!(
            "{name}: {} differs from {} ({}x{} vs {}x{}, {differing} pixels over tolerance, budget {budget})",
            actual_path.display(),
            expected_path.display(),
            actual.width,
            actual.height,
            expected.width,
            expected.height,
        );
    }
    let _ = std::fs::remove_file(&actual_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(value: u32) -> Image {
        Image {
            width: 10,
            height: 10,
            pixels: vec![value; 100],
        }
    }

    #[test]
    fn should_count_only_pixels_beyond_the_tolerance_when_diffing() {
        let base = flat(0x102030);
        let mut other = flat(0x102030);
        other.pixels[0] = 0x102030 + 10;
        other.pixels[1] = 0xffffff;
        assert_eq!(differing_pixels(&base, &other), 1);
    }
}
