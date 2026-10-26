//! Images: PNG and JPEG decode through the `image` crate, scale to fit the
//! column, cached by (path, modification time, width). A missing, remote or
//! undecodable image becomes a box with its alt text and the reason.

use super::{
    Content, LaidBlock, PendingKind,
    block::{self, Cx, Flow},
    boxes::{self, BoxEnv, MediaBox, RasterCache},
    svg,
};
use crate::paint::Rect;
use limpid_core::doc::Block;
use std::{
    cell::RefCell,
    io::Cursor,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};
use tiny_skia::Pixmap;

type Key = (PathBuf, Option<SystemTime>, u64, u32);

thread_local! {
    static PIXMAPS: RefCell<RasterCache<Key>> = const { RefCell::new(RasterCache::new()) };
}

/// Why an image could not be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Remote,
    Missing,
    TooLarge,
    Unsupported,
}

impl Failure {
    fn describe(&self) -> &'static str {
        match self {
            Failure::Remote => "remote images are not fetched",
            Failure::Missing => "file not found",
            Failure::TooLarge => "too large to decode",
            Failure::Unsupported => "not a PNG or JPEG image",
        }
    }
}

/// Where `src` points: a path on disk, or the reason it cannot be one.
pub fn resolve(src: &str, env: &BoxEnv) -> Result<PathBuf, Failure> {
    let src = src.trim();
    if src.is_empty() {
        return Err(Failure::Missing);
    }
    let local = match src.strip_prefix("file://") {
        Some(rest) => rest,
        None => {
            if has_scheme(src) {
                return Err(Failure::Remote);
            }
            src
        }
    };
    let local = local.split(['?', '#']).next().unwrap_or(local);
    let path = PathBuf::from(percent_decode(local));
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(match &env.document_dir {
        Some(dir) => dir.join(path),
        None => path,
    })
}

/// A URL scheme is two or more letters, digits, `+`, `-` or `.` before `:`.
/// A Windows drive letter is one letter and so is not a scheme.
fn has_scheme(src: &str) -> bool {
    let Some((scheme, _)) = src.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|first: char| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn percent_decode(text: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(text)).into_owned()
}

fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2]))
        {
            out.push((high * 16 + low) as u8);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    out
}

/// The decoded size of an encoded image without decoding its pixels.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Premultiplied RGBA from straight RGBA.
fn premultiplied(rgba: &image::RgbaImage) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(rgba.width(), rgba.height())?;
    for (source, target) in rgba.pixels().zip(pixmap.data_mut().as_chunks_mut::<4>().0) {
        let alpha = u32::from(source[3]);
        let scale = |channel: u8| ((u32::from(channel) * alpha + 127) / 255) as u8;
        *target = [
            scale(source[0]),
            scale(source[1]),
            scale(source[2]),
            source[3],
        ];
    }
    Some(pixmap)
}

/// A decoded raster image at `(width, height)` device pixels, from the cache
/// when possible.
fn pixmap_at(
    path: &Path,
    bytes: &[u8],
    modified: Option<SystemTime>,
    natural: (u32, u32),
    width: u32,
    height: u32,
    env: &BoxEnv,
) -> Option<Rc<Pixmap>> {
    let key: Key = (path.to_path_buf(), modified, bytes.len() as u64, width);
    if let Some(hit) = PIXMAPS.with(|cache| cache.borrow_mut().get(&key)) {
        return Some(hit);
    }
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let resized;
    let source = if (decoded.width(), decoded.height()) == (width, height) {
        &decoded
    } else {
        resized = image::imageops::resize(
            &decoded,
            width,
            height,
            image::imageops::FilterType::Triangle,
        );
        &resized
    };
    debug_assert_eq!(natural.0, decoded.width());
    let pixmap = Rc::new(premultiplied(source)?);
    PIXMAPS.with(|cache| {
        cache
            .borrow_mut()
            .insert(key, pixmap.clone(), env.cache_bytes)
    });
    Some(pixmap)
}

/// Drop every cached image.
pub fn clear_cache() {
    PIXMAPS.with(|cache| cache.borrow_mut().clear());
}

/// How many images the cache holds, for tests.
pub fn cached_images() -> usize {
    PIXMAPS.with(|cache| cache.borrow().len())
}

/// The text a failed image shows: its alt text, then where it should have been.
pub fn failure_text(alt: &str, src: &str, failure: &Failure) -> String {
    let label = if alt.trim().is_empty() {
        "image"
    } else {
        alt.trim()
    };
    // A source can be very long; show its start only.
    let shown: String = src.chars().take(96).collect();
    let ellipsis = if shown.len() < src.len() { "..." } else { "" };
    format!("[{label}]\n{shown}{ellipsis}  ({})", failure.describe())
}

#[allow(clippy::too_many_arguments)]
pub fn lay(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    src: &str,
    alt: &str,
) -> LaidBlock {
    match load(cx, block, flow, y, index, stop, src) {
        Ok(laid) => laid,
        Err(failure) => block::pending_with(
            cx,
            block,
            flow,
            y,
            index,
            stop,
            PendingKind::Image,
            &failure_text(alt, src, &failure),
            None,
            None,
        ),
    }
}

fn load(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    src: &str,
) -> Result<LaidBlock, Failure> {
    let path = resolve(src, cx.boxes)?;
    let modified = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let bytes = boxes::read_capped(&path, cx.boxes.max_file_bytes).ok_or_else(|| {
        if path.is_file() {
            Failure::TooLarge
        } else {
            Failure::Missing
        }
    })?;
    let is_svg = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
    if is_svg {
        return svg::lay(cx, block, flow, y, index, stop, &bytes).ok_or(Failure::Unsupported);
    }
    let (natural_width, natural_height) = dimensions(&bytes).ok_or(Failure::Unsupported)?;
    if natural_width == 0
        || natural_height == 0
        || u64::from(natural_width) * u64::from(natural_height) > cx.boxes.max_image_pixels
    {
        return Err(Failure::TooLarge);
    }
    let (width, height) = boxes::fit(
        natural_width as f32 * cx.scale,
        natural_height as f32 * cx.scale,
        flow.width,
    );
    let (width, height) = (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    );
    let pixmap = pixmap_at(
        &path,
        &bytes,
        modified,
        (natural_width, natural_height),
        width,
        height,
        cx.boxes,
    )
    .ok_or(Failure::Unsupported)?;
    Ok(LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height as f32),
        stop,
        content: Content::Media(MediaBox {
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

    fn env_in(dir: &str) -> BoxEnv {
        BoxEnv {
            document_dir: Some(PathBuf::from(dir)),
            ..BoxEnv::default()
        }
    }

    #[test]
    fn should_join_a_relative_path_with_the_document_directory_when_resolving() {
        assert_eq!(
            resolve("img/a.png", &env_in("/docs")).unwrap(),
            PathBuf::from("/docs/img/a.png")
        );
    }

    #[test]
    fn should_keep_an_absolute_path_when_resolving() {
        assert_eq!(
            resolve("/tmp/a.png", &env_in("/docs")).unwrap(),
            PathBuf::from("/tmp/a.png")
        );
        assert_eq!(
            resolve("file:///tmp/a.png", &env_in("/docs")).unwrap(),
            PathBuf::from("/tmp/a.png")
        );
    }

    #[test]
    fn should_decode_percent_escapes_and_drop_the_query_when_resolving() {
        assert_eq!(
            resolve("my%20pic.png?raw=1#x", &env_in("/d")).unwrap(),
            PathBuf::from("/d/my pic.png")
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
    }

    #[test]
    fn should_shorten_a_long_source_when_describing_a_failure() {
        let long = format!("https://example.com/{}.png", "a".repeat(5000));
        let text = failure_text("x", &long, &Failure::Unsupported);
        assert!(text.len() < 200, "{}", text.len());
        assert!(text.contains("..."));
    }

    #[test]
    fn should_refuse_remote_urls_when_resolving() {
        let env = env_in("/d");
        for src in ["http://x/a.png", "https://x/a.png", "ftp://x/a"] {
            assert_eq!(resolve(src, &env), Err(Failure::Remote), "{src}");
        }
    }

    #[test]
    fn should_treat_a_colon_in_a_file_name_as_a_path_when_it_is_not_a_scheme() {
        let env = env_in("/d");
        assert_eq!(resolve("ab:c.png", &env), Err(Failure::Remote));
        assert_eq!(resolve("a:b.png", &env), Ok(PathBuf::from("/d/a:b.png")));
        assert_eq!(
            resolve("dir/a:b.png", &env).unwrap(),
            PathBuf::from("/d/dir/a:b.png")
        );
        assert_eq!(
            resolve("C:/pic.png", &env),
            Ok(PathBuf::from("/d/C:/pic.png"))
        );
    }

    #[test]
    fn should_report_missing_when_the_source_is_empty() {
        assert_eq!(resolve("   ", &BoxEnv::default()), Err(Failure::Missing));
    }

    #[test]
    fn should_use_a_generic_label_when_the_alt_text_is_empty() {
        let text = failure_text("", "a.png", &Failure::Missing);
        assert!(text.starts_with("[image]"));
        assert!(text.contains("a.png"));
        assert!(text.contains("file not found"));
        let text = failure_text("  A chart ", "a.png", &Failure::Remote);
        assert!(text.starts_with("[A chart]"));
    }

    #[test]
    fn should_premultiply_colour_by_alpha_when_converting() {
        let mut rgba = image::RgbaImage::new(2, 1);
        rgba.put_pixel(0, 0, image::Rgba([200, 100, 50, 128]));
        rgba.put_pixel(1, 0, image::Rgba([10, 20, 30, 0]));
        let pixmap = premultiplied(&rgba).unwrap();
        assert_eq!(&pixmap.data()[0..4], &[100, 50, 25, 128]);
        assert_eq!(&pixmap.data()[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn should_not_read_dimensions_from_garbage_when_the_bytes_are_not_an_image() {
        assert_eq!(dimensions(b"hello"), None);
        assert_eq!(dimensions(b""), None);
        assert_eq!(dimensions(&[0x89, b'P', b'N', b'G']), None);
    }
}
