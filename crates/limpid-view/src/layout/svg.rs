//! SVG drawing: `usvg` parses, `resvg` rasterises into a `tiny_skia::Pixmap`
//! at the device size, and the result is cached by (source hash, width,
//! height). Used for linked and inline SVG, cached math and Mermaid.

use super::{
    LaidBlock,
    block::{Cx, Flow},
    boxes::{self, RasterCache},
};
use crate::paint::Rect;
use limpid_core::doc::Block;
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
    rc::Rc,
    sync::{Arc, OnceLock},
};
use tiny_skia::Pixmap;

/// Largest side of a rasterised SVG, in pixels.
const MAX_SIDE: u32 = 16_384;

type Key = (u64, u32, u32);

thread_local! {
    static RASTERS: RefCell<RasterCache<Key>> = const { RefCell::new(RasterCache::new()) };
    static SIZES: RefCell<HashMap<u64, Option<(f32, f32)>>> = RefCell::new(HashMap::new());
}

/// A stable 64-bit hash of `bytes`, the cache key of a source.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn font_database() -> Arc<usvg::fontdb::Database> {
    static DATABASE: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DATABASE
        .get_or_init(|| {
            let mut database = usvg::fontdb::Database::new();
            database.load_system_fonts();
            for family in ["Inter", "DejaVu Sans", "Liberation Sans", "Noto Sans"] {
                let query = usvg::fontdb::Query {
                    families: &[usvg::fontdb::Family::Name(family)],
                    ..usvg::fontdb::Query::default()
                };
                if database.query(&query).is_some() {
                    database.set_sans_serif_family(family);
                    break;
                }
            }
            Arc::new(database)
        })
        .clone()
}

fn parse(bytes: &[u8]) -> Option<usvg::Tree> {
    let options = usvg::Options {
        fontdb: font_database(),
        font_family: "sans-serif".to_owned(),
        ..usvg::Options::default()
    };
    usvg::Tree::from_data(bytes, &options).ok()
}

/// The natural `(width, height)` of an SVG in CSS px, or `None` when it does
/// not parse or has no usable size.
pub fn natural_size(bytes: &[u8]) -> Option<(f32, f32)> {
    let key = hash_bytes(bytes);
    if let Some(known) = SIZES.with(|sizes| sizes.borrow().get(&key).copied()) {
        return known;
    }
    let size = parse(bytes)
        .map(|tree| (tree.size().width(), tree.size().height()))
        .filter(|(width, height)| {
            width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0
        });
    SIZES.with(|sizes| {
        let mut sizes = sizes.borrow_mut();
        if sizes.len() > 512 {
            sizes.clear();
        }
        sizes.insert(key, size);
    });
    size
}

/// The SVG `key` drawn at `width` by `height` pixels. `bytes` runs only when
/// the cache has no entry. `None` when the size is unusable, a pixmap that
/// large would not fit `budget`, or the SVG does not parse.
pub fn raster(
    key: u64,
    width: u32,
    height: u32,
    budget: usize,
    bytes: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Rc<Pixmap>> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    if (width as usize) * (height as usize) * 4 > budget {
        return None;
    }
    render_cached((key, width, height), budget, bytes, |_| {})
}

/// The SVG `key` drawn at `width` by `height` pixels as a mask filled with
/// `ink`. Coverage is the alpha channel, or the inverted luminance when the
/// SVG is fully opaque (black marks on a white page). TeX output carries
/// explicit black fills that vanish on a dark theme; this repaints them.
/// Cached by (source, size, ink).
pub fn raster_mask(
    key: u64,
    width: u32,
    height: u32,
    ink: [u8; 3],
    budget: usize,
    bytes: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Rc<Pixmap>> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    if (width as usize) * (height as usize) * 4 > budget {
        return None;
    }
    let tinted = hash_bytes(&[&key.to_le_bytes()[..], &ink[..], b"mask"].concat());
    render_cached((tinted, width, height), budget, bytes, |pixmap| {
        paint_mask(pixmap, ink);
    })
}

/// Replace `pixmap` with `ink` at the coverage the drawing had.
fn paint_mask(pixmap: &mut Pixmap, ink: [u8; 3]) {
    let opaque = pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 255);
    for pixel in pixmap.data_mut().as_chunks_mut::<4>().0 {
        let coverage = if opaque {
            let luminance =
                (u32::from(pixel[0]) * 77 + u32::from(pixel[1]) * 151 + u32::from(pixel[2]) * 28)
                    >> 8;
            255 - luminance.min(255) as u8
        } else {
            pixel[3]
        };
        let scale = |channel: u8| ((u32::from(channel) * u32::from(coverage) + 127) / 255) as u8;
        pixel[0] = scale(ink[0]);
        pixel[1] = scale(ink[1]);
        pixel[2] = scale(ink[2]);
        pixel[3] = coverage;
    }
}

fn render_cached(
    cache_key: Key,
    budget: usize,
    bytes: impl FnOnce() -> Option<Vec<u8>>,
    finish: impl FnOnce(&mut Pixmap),
) -> Option<Rc<Pixmap>> {
    if let Some(hit) = RASTERS.with(|cache| cache.borrow_mut().get(&cache_key)) {
        return Some(hit);
    }
    let (_, width, height) = cache_key;
    let tree = parse(&bytes()?)?;
    let size = tree.size();
    let mut pixmap = Pixmap::new(width, height)?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    finish(&mut pixmap);
    let pixmap = Rc::new(pixmap);
    RASTERS.with(|cache| cache.borrow_mut().insert(cache_key, pixmap.clone(), budget));
    Some(pixmap)
}

/// Empty the raster cache (the tests use it; a page could on memory pressure).
pub fn clear_cache() {
    RASTERS.with(|cache| cache.borrow_mut().clear());
    SIZES.with(|sizes| sizes.borrow_mut().clear());
}

/// How many rasters the cache holds, for tests.
pub fn cached_rasters() -> usize {
    RASTERS.with(|cache| cache.borrow().len())
}

/// A standalone SVG block (an image file or a `data:` URI), scaled down to
/// the column and left aligned.
pub fn lay(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    bytes: &[u8],
) -> Option<LaidBlock> {
    let (natural_width, natural_height) = natural_size(bytes)?;
    let (width, height) = boxes::fit(
        natural_width * cx.scale,
        natural_height * cx.scale,
        flow.width,
    );
    let (width_px, height_px) = (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    );
    let pixmap = raster(
        hash_bytes(bytes),
        width_px,
        height_px,
        cx.boxes.cache_bytes,
        || Some(bytes.to_vec()),
    )?;
    Some(LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height_px as f32),
        stop,
        content: super::Content::Media(boxes::MediaBox {
            pixmap,
            x: 0.0,
            y: 0.0,
            framed: false,
            border: 0.0,
            hash: None,
        }),
        depth: flow.depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_SQUARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10"><rect width="20" height="10" fill="#ff0000"/></svg>"##;

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * pixmap.width() + x) * 4) as usize;
        pixmap.data()[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn should_report_the_natural_size_when_the_svg_declares_one() {
        assert_eq!(natural_size(RED_SQUARE.as_bytes()), Some((20.0, 10.0)));
    }

    #[test]
    fn should_use_the_view_box_when_the_width_is_a_percentage() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" viewBox="0 0 40 30"><rect width="40" height="30"/></svg>"#;
        assert_eq!(natural_size(svg.as_bytes()), Some((40.0, 30.0)));
    }

    #[test]
    fn should_return_none_when_the_bytes_are_not_an_svg() {
        assert_eq!(natural_size(b"not svg at all"), None);
        assert_eq!(natural_size(b""), None);
        assert_eq!(natural_size(&[0xff, 0xfe, 0x00]), None);
    }

    #[test]
    fn should_fill_the_pixmap_at_the_requested_size_when_rasterising() {
        clear_cache();
        let pixmap = raster(1, 40, 20, 1 << 20, || Some(RED_SQUARE.as_bytes().to_vec())).unwrap();
        assert_eq!((pixmap.width(), pixmap.height()), (40, 20));
        assert_eq!(pixel(&pixmap, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&pixmap, 39, 19), [255, 0, 0, 255]);
    }

    #[test]
    fn should_not_read_the_source_again_when_the_raster_is_cached() {
        clear_cache();
        let first = raster(2, 20, 10, 1 << 20, || Some(RED_SQUARE.as_bytes().to_vec())).unwrap();
        let second = raster(2, 20, 10, 1 << 20, || panic!("source read on a cache hit")).unwrap();
        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn should_draw_a_separate_raster_when_the_size_differs() {
        clear_cache();
        raster(3, 20, 10, 1 << 20, || Some(RED_SQUARE.as_bytes().to_vec())).unwrap();
        raster(3, 40, 20, 1 << 20, || Some(RED_SQUARE.as_bytes().to_vec())).unwrap();
        assert_eq!(cached_rasters(), 2);
    }

    #[test]
    fn should_refuse_an_absurd_size_when_rasterising() {
        clear_cache();
        let svg = || Some(RED_SQUARE.as_bytes().to_vec());
        assert!(raster(4, 0, 10, 1 << 20, svg).is_none());
        assert!(raster(4, 10, 0, 1 << 20, svg).is_none());
        assert!(raster(4, 100_000, 10, usize::MAX, svg).is_none());
        assert!(raster(4, 1000, 1000, 1000, svg).is_none());
    }

    #[test]
    fn should_return_none_when_the_source_does_not_parse_or_is_missing() {
        clear_cache();
        assert!(raster(5, 10, 10, 1 << 20, || Some(b"<svg".to_vec())).is_none());
        assert!(raster(6, 10, 10, 1 << 20, || None).is_none());
    }

    const BLACK_MARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10"><rect width="10" height="10" fill="#000000"/></svg>"##;
    const BLACK_ON_WHITE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10"><rect width="20" height="10" fill="#ffffff"/><rect width="10" height="10" fill="#000000"/></svg>"##;
    const INK: [u8; 3] = [0xe8, 0xee, 0xed];

    #[test]
    fn should_fill_marks_with_ink_when_the_svg_has_alpha() {
        clear_cache();
        let svg = || Some(BLACK_MARK.as_bytes().to_vec());
        let pixmap = raster_mask(10, 20, 10, INK, 1 << 20, svg).unwrap();
        assert_eq!(pixel(&pixmap, 2, 5), [0xe8, 0xee, 0xed, 255]);
        assert_eq!(pixel(&pixmap, 15, 5)[3], 0);
    }

    #[test]
    fn should_fill_dark_marks_with_ink_when_the_svg_is_opaque() {
        clear_cache();
        let svg = || Some(BLACK_ON_WHITE.as_bytes().to_vec());
        let pixmap = raster_mask(11, 20, 10, INK, 1 << 20, svg).unwrap();
        assert_eq!(pixel(&pixmap, 2, 5), [0xe8, 0xee, 0xed, 255]);
        assert_eq!(pixel(&pixmap, 15, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn should_draw_a_separate_raster_when_the_ink_differs() {
        clear_cache();
        let svg = || Some(BLACK_MARK.as_bytes().to_vec());
        let light = raster_mask(12, 20, 10, [0x20, 0x25, 0x2a], 1 << 20, svg).unwrap();
        let dark = raster_mask(12, 20, 10, INK, 1 << 20, svg).unwrap();
        assert_eq!(cached_rasters(), 2);
        assert_ne!(pixel(&light, 2, 5), pixel(&dark, 2, 5));
    }

    #[test]
    fn should_not_read_the_source_again_when_the_mask_is_cached() {
        clear_cache();
        let first = raster_mask(13, 20, 10, INK, 1 << 20, || {
            Some(BLACK_MARK.as_bytes().to_vec())
        })
        .unwrap();
        let second = raster_mask(13, 20, 10, INK, 1 << 20, || panic!("re-read")).unwrap();
        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn should_return_none_when_the_mask_size_is_absurd_or_the_source_is_bad() {
        clear_cache();
        let svg = || Some(BLACK_MARK.as_bytes().to_vec());
        assert!(raster_mask(14, 0, 10, INK, 1 << 20, svg).is_none());
        assert!(raster_mask(14, 100_000, 10, INK, usize::MAX, svg).is_none());
        assert!(raster_mask(14, 10, 10, INK, 1 << 20, || Some(b"<svg".to_vec())).is_none());
        assert!(raster_mask(14, 10, 10, INK, 1 << 20, || None).is_none());
    }
}
