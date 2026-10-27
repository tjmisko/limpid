//! Boxes that hold drawn content instead of text: display math, Mermaid
//! diagrams, and the shared pieces the image, SVG and table layouts use
//! (environment, raster cache, typeset inline math).
//!
//! Math and Mermaid come from helper processes that write SVG into the cache;
//! until a file lands the block shows its source, and
//! [`Layout::refresh_pending`](super::Layout::refresh_pending) swaps it in.

use super::{
    Content, LaidBlock, PaintCx, PendingKind,
    block::{self, Cx, Flow},
    inline::{self, InlinePlaceholder, MATH_REFERENCE_EM, RichText},
    svg,
};
use crate::{
    paint::{Canvas, Rect},
    theme::{Color, Theme},
};
use limpid_core::{doc::Block, settings::Settings};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::RwLock,
};
use tiny_skia::Pixmap;

/// Everything the drawn boxes read from outside the document. Every limit is a
/// field so that a caller (or a setting) can change it.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxEnv {
    /// Where relative image paths start. `None` means the working directory.
    pub document_dir: Option<PathBuf>,
    /// `false`: diagrams always show their source.
    pub mermaid_enabled: bool,
    /// Images with more pixels than this are not decoded.
    pub max_image_pixels: u64,
    /// Files larger than this are not read.
    pub max_file_bytes: u64,
    /// Budget of each raster cache (images and SVG), in bytes of pixels.
    pub cache_bytes: usize,
    /// Table cells: horizontal and vertical padding, CSS px.
    pub table_pad: (f32, f32),
    /// Stretch a table that fits to the full column, as `width: 100%` does.
    pub table_fill: bool,
    /// Padding inside a framed diagram, CSS px.
    pub frame_pad: f32,
}

impl Default for BoxEnv {
    fn default() -> Self {
        Self {
            document_dir: None,
            mermaid_enabled: true,
            max_image_pixels: 40_000_000,
            max_file_bytes: 64 * 1024 * 1024,
            cache_bytes: 48 * 1024 * 1024,
            table_pad: (13.0, 9.0),
            table_fill: true,
            frame_pad: 14.0,
        }
    }
}

impl BoxEnv {
    pub fn from_settings(settings: &Settings, document_dir: Option<&Path>) -> Self {
        Self {
            document_dir: document_dir.map(Path::to_path_buf),
            mermaid_enabled: settings.mermaid.enabled,
            ..Self::default()
        }
    }
}

static DEFAULT_ENV: RwLock<Option<BoxEnv>> = RwLock::new(None);

/// Replace the environment that [`lay_out_with`](super::lay_out_with) uses.
/// The page sets this once when it opens a document.
pub fn set_default_env(env: BoxEnv) {
    if let Ok(mut slot) = DEFAULT_ENV.write() {
        *slot = Some(env);
    }
}

pub fn default_env() -> BoxEnv {
    DEFAULT_ENV
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_default()
}

/// A least-recently-used set of pixmaps bounded by bytes.
pub struct RasterCache<K> {
    entries: Vec<(K, Rc<Pixmap>)>,
    bytes: usize,
}

impl<K: PartialEq> RasterCache<K> {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
        }
    }

    pub fn get(&mut self, key: &K) -> Option<Rc<Pixmap>> {
        let at = self.entries.iter().position(|(held, _)| held == key)?;
        let entry = self.entries.remove(at);
        let pixmap = entry.1.clone();
        self.entries.push(entry);
        Some(pixmap)
    }

    /// Keep `pixmap` under `key`, dropping the oldest entries until the cache
    /// fits `budget`. The newest entry is always kept.
    pub fn insert(&mut self, key: K, pixmap: Rc<Pixmap>, budget: usize) {
        self.entries.retain(|(held, old)| {
            let same = *held == key;
            if same {
                self.bytes -= old.data().len();
            }
            !same
        });
        self.bytes += pixmap.data().len();
        self.entries.push((key, pixmap));
        while self.bytes > budget && self.entries.len() > 1 {
            let (_, dropped) = self.entries.remove(0);
            self.bytes -= dropped.data().len();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

impl<K: PartialEq> Default for RasterCache<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// A pixmap placed in a block, optionally in a frame like a diagram figure.
pub struct MediaBox {
    pub pixmap: Rc<Pixmap>,
    /// Top left of the pixmap inside the block, device pixels.
    pub x: f32,
    pub y: f32,
    /// Surface fill and border around the whole block.
    pub framed: bool,
    pub border: f32,
    /// The math or Mermaid hash, for [`Layout::refresh_pending`](super::Layout::refresh_pending).
    pub hash: Option<String>,
}

pub fn paint_media(paint: &PaintCx, canvas: &mut Canvas, area: Rect, media: &MediaBox) {
    if !media.framed {
        canvas.draw_pixmap(&media.pixmap, area.x + media.x, area.y + media.y);
        return;
    }
    let radius = FRAME_RADIUS * paint.scale;
    canvas.with_round_corners(area, radius, |canvas| {
        canvas.fill_rect(area, paint.theme.surface);
        canvas.draw_pixmap(&media.pixmap, area.x + media.x, area.y + media.y);
    });
    canvas.stroke_rounded_rect(area, radius, media.border, paint.theme.rule);
}

/// Corner radius of a framed diagram, CSS px.
const FRAME_RADIUS: f32 = 10.0;

/// The cache directory Mermaid helpers write to.
pub fn mermaid_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("limpid").join("mermaid"))
}

/// A hash that is plain hex cannot name a path outside the cache.
fn is_plain_hash(hash: &str) -> bool {
    !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Read a file no larger than `limit` bytes.
pub fn read_capped(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    std::fs::read(path).ok()
}

fn laid(
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    height: f32,
    media: MediaBox,
) -> LaidBlock {
    LaidBlock {
        index,
        src: block.src.clone(),
        rect: Rect::new(flow.x, y, flow.width, height),
        stop,
        content: Content::Media(media),
        depth: flow.depth,
    }
}

/// The theme's ink as the mask colour of typeset math.
fn ink_rgb(theme: &Theme) -> [u8; 3] {
    [theme.ink.r, theme.ink.g, theme.ink.b]
}

/// Display math: a centred SVG, or the LaTeX source in monospace on `code_bg`
/// when the formula is not cached (or cannot be drawn).
#[allow(clippy::too_many_arguments)]
pub fn lay_math(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    latex: &str,
    hash: &str,
) -> LaidBlock {
    if let Some((media, height)) = math_media(cx, flow, hash) {
        return laid(block, flow, y, index, stop, height, media);
    }
    block::pending_with(
        cx,
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Math,
        latex.trim(),
        Some(hash.to_owned()),
        Some(cx.theme.code_bg),
    )
}

fn math_media(cx: &Cx, flow: Flow, hash: &str) -> Option<(MediaBox, f32)> {
    let rendered = limpid_core::math::cached(hash)?;
    let valid = |value: f32| value.is_finite() && value > 0.0;
    if !valid(rendered.width) || !valid(rendered.height) {
        return None;
    }
    let factor = cx.theme.font_size / MATH_REFERENCE_EM * cx.scale;
    let mut width = rendered.width * factor;
    let mut height = rendered.height * factor;
    if width > flow.width {
        height *= flow.width / width;
        width = flow.width;
    }
    let (width_px, height_px) = (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    );
    let key = svg::hash_bytes(format!("math:{hash}").as_bytes());
    let ink = ink_rgb(cx.theme);
    let pixmap = svg::raster_mask(key, width_px, height_px, ink, cx.boxes.cache_bytes, || {
        read_capped(&rendered.svg, cx.boxes.max_file_bytes)
    })?;
    let pad_y = cx.px(2.0);
    let media = MediaBox {
        x: ((flow.width - width_px as f32) / 2.0).max(0.0),
        y: pad_y,
        pixmap,
        framed: false,
        border: 0.0,
        hash: Some(hash.to_owned()),
    };
    Some((media, height_px as f32 + 2.0 * pad_y))
}

/// A Mermaid diagram from the cache, framed like a figure, or its source.
#[allow(clippy::too_many_arguments)]
pub fn lay_mermaid(
    cx: &mut Cx,
    block: &Block,
    flow: Flow,
    y: f32,
    index: usize,
    stop: bool,
    source: &str,
    hash: &str,
) -> LaidBlock {
    if cx.boxes.mermaid_enabled
        && let Some((media, height)) = mermaid_media(cx, flow, hash)
    {
        return laid(block, flow, y, index, stop, height, media);
    }
    block::pending_with(
        cx,
        block,
        flow,
        y,
        index,
        stop,
        PendingKind::Mermaid,
        source,
        Some(hash.to_owned()),
        None,
    )
}

fn mermaid_media(cx: &Cx, flow: Flow, hash: &str) -> Option<(MediaBox, f32)> {
    if !is_plain_hash(hash) {
        return None;
    }
    let path = mermaid_root()?.join(format!("{hash}.svg"));
    let bytes = read_capped(&path, cx.boxes.max_file_bytes)?;
    let (media, height) = framed_svg(cx, flow, &bytes, Some(hash.to_owned()))?;
    Some((media, height))
}

/// An SVG scaled down to fit the column, centred in a frame.
pub fn framed_svg(
    cx: &Cx,
    flow: Flow,
    bytes: &[u8],
    hash: Option<String>,
) -> Option<(MediaBox, f32)> {
    let (natural_width, natural_height) = svg::natural_size(bytes)?;
    let border = cx.px(1.0).round().max(1.0);
    let pad = cx.px(cx.boxes.frame_pad);
    let available = (flow.width - 2.0 * (pad + border)).max(1.0);
    let (width, height) = fit(
        natural_width * cx.scale,
        natural_height * cx.scale,
        available,
    );
    let (width_px, height_px) = (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    );
    let key = svg::hash_bytes(bytes);
    let pixmap = svg::raster(key, width_px, height_px, cx.boxes.cache_bytes, || {
        Some(bytes.to_vec())
    })?;
    let media = MediaBox {
        x: ((flow.width - width_px as f32) / 2.0).max(0.0),
        y: pad + border,
        pixmap,
        framed: true,
        border,
        hash,
    };
    Some((media, height_px as f32 + 2.0 * (pad + border)))
}

/// Scale `(width, height)` down to at most `max_width`, keeping the ratio.
pub fn fit(width: f32, height: f32, max_width: f32) -> (f32, f32) {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return (1.0, 1.0);
    }
    if width <= max_width {
        return (width, height);
    }
    (max_width, height * max_width / width)
}

/// An inline formula drawn over its reserved space in a [`RichText`].
#[derive(Debug, Clone, PartialEq)]
pub struct TypesetMath {
    /// Left edge of the reserved space, block-local device pixels.
    pub x: f32,
    pub width: f32,
    pub baseline: f32,
    pub hash: String,
    /// Typeset `(width, height, baseline)` in CSS px at the reference em.
    pub size: (f32, f32, f32),
}

/// Where each sized inline formula of `text` sits.
pub fn typeset_math(text: &RichText) -> Vec<TypesetMath> {
    let sized: Vec<&InlinePlaceholder> = text
        .placeholders
        .iter()
        .filter(|placeholder| placeholder.kind == PendingKind::Math && placeholder.size.is_some())
        .collect();
    if sized.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut total = 0usize;
    for line in &text.block.buffer().lines {
        starts.push(total);
        total += line.text().len() + 1;
    }
    sized
        .into_iter()
        .filter_map(|placeholder| {
            let mut left = f32::INFINITY;
            let mut right = f32::NEG_INFINITY;
            let mut baseline = None;
            for glyph in text.block.glyphs() {
                let offset = starts.get(glyph.line)? + glyph.start;
                if !placeholder.range.contains(&offset) {
                    continue;
                }
                left = left.min(glyph.x);
                right = right.max(glyph.x + glyph.width);
                baseline.get_or_insert(glyph.baseline);
            }
            Some(TypesetMath {
                x: left,
                width: right - left,
                baseline: baseline?,
                hash: placeholder.hash.clone(),
                size: placeholder.size?,
            })
        })
        .collect()
}

thread_local! {
    static INLINE_MATH_BUDGET: RefCell<usize> = const { RefCell::new(32 * 1024 * 1024) };
}

/// Cap on pixels the inline math cache holds (default 32 MiB).
pub fn set_inline_math_budget(bytes: usize) {
    INLINE_MATH_BUDGET.with(|budget| *budget.borrow_mut() = bytes);
}

/// Paint the formulas of `text`, whose top left is at `(x, y)`.
pub fn paint_typeset_math(
    canvas: &mut Canvas,
    text: &RichText,
    x: f32,
    y: f32,
    typeset: &[TypesetMath],
    ink: Color,
) {
    let ink = [ink.r, ink.g, ink.b];
    let budget = INLINE_MATH_BUDGET.with(|budget| *budget.borrow());
    let factor = text.font_size / MATH_REFERENCE_EM;
    for formula in typeset {
        let (width, height, baseline) = formula.size;
        let (width_px, height_px) = (
            (width * factor).round().max(1.0) as u32,
            (height * factor).round().max(1.0) as u32,
        );
        let key = svg::hash_bytes(format!("math:{}", formula.hash).as_bytes());
        let Some(pixmap) = svg::raster_mask(key, width_px, height_px, ink, budget, || {
            let rendered = limpid_core::math::cached(&formula.hash)?;
            read_capped(&rendered.svg, 64 * 1024 * 1024)
        }) else {
            continue;
        };
        let left = x + formula.x + ((formula.width - width_px as f32) / 2.0).max(0.0);
        let top = y + formula.baseline - baseline * factor;
        canvas.draw_pixmap(&pixmap, left, top);
    }
}

/// Whether painting `typeset` replaces the box that `inline::paint` would
/// draw at `x` on `baseline`.
pub fn covers(typeset: &[TypesetMath], x: f32, baseline: f32) -> bool {
    typeset
        .iter()
        .any(|formula| (formula.x - x).abs() < 0.5 && (formula.baseline - baseline).abs() < 0.5)
}

/// Whether any math or Mermaid hash in `hashes` is drawn (or waiting) in
/// `block` or below it.
pub fn mentions(block: &LaidBlock, hashes: &[String]) -> bool {
    let wanted = |hash: &str| hashes.iter().any(|candidate| candidate == hash);
    let in_text = |text: &RichText| {
        text.placeholders
            .iter()
            .any(|placeholder| wanted(&placeholder.hash))
    };
    let own = match &block.content {
        Content::Text(text) | Content::Heading { text, .. } => in_text(text),
        Content::Pending(pending) => pending.hash.as_deref().is_some_and(wanted),
        Content::Media(media) => media.hash.as_deref().is_some_and(wanted),
        Content::Table(table) => table
            .rows
            .iter()
            .any(|row| row.cells.iter().any(|cell| in_text(&cell.text))),
        Content::Item { marker, .. } => match marker {
            super::Marker::Text(text) => in_text(text),
            super::Marker::Task { .. } => false,
        },
        _ => false,
    };
    own || block.children().iter().any(|child| mentions(child, hashes))
}

/// Skip decorations covered by typeset math when painting inline text.
pub fn skip_placeholder_box(typeset: &[TypesetMath], deco: &inline::DecoRect) -> bool {
    deco.kind == inline::DecorationKind::Placeholder && covers(typeset, deco.x, deco.baseline)
}

/// The colour of the faint stripe on every other table row.
pub fn stripe(theme: &crate::theme::Theme) -> Color {
    match theme.scheme {
        limpid_core::transport::Scheme::Dark => Color::rgba(255, 255, 255, 5),
        limpid_core::transport::Scheme::Light => Color::rgba(0, 0, 0, 6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixmap(width: u32, height: u32) -> Rc<Pixmap> {
        Rc::new(Pixmap::new(width, height).unwrap())
    }

    #[test]
    fn should_evict_the_oldest_entry_when_the_budget_is_exceeded() {
        let mut cache = RasterCache::new();
        // Each 10x10 pixmap is 400 bytes.
        cache.insert(1, pixmap(10, 10), 900);
        cache.insert(2, pixmap(10, 10), 900);
        cache.insert(3, pixmap(10, 10), 900);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_some());
        assert_eq!(cache.bytes(), 800);
    }

    #[test]
    fn should_keep_the_most_recently_used_entry_when_evicting() {
        let mut cache = RasterCache::new();
        cache.insert(1, pixmap(10, 10), 900);
        cache.insert(2, pixmap(10, 10), 900);
        assert!(cache.get(&1).is_some());
        cache.insert(3, pixmap(10, 10), 900);
        assert!(cache.get(&1).is_some());
        assert!(cache.get(&2).is_none());
    }

    #[test]
    fn should_keep_a_single_entry_larger_than_the_budget_when_it_is_the_newest() {
        let mut cache = RasterCache::new();
        cache.insert(1, pixmap(20, 20), 10);
        assert_eq!(cache.len(), 1);
        cache.insert(2, pixmap(20, 20), 10);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(&2).is_some());
    }

    #[test]
    fn should_not_double_count_bytes_when_the_same_key_is_inserted_twice() {
        let mut cache = RasterCache::new();
        cache.insert(7, pixmap(10, 10), 10_000);
        cache.insert(7, pixmap(10, 10), 10_000);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 400);
    }

    #[test]
    fn should_shrink_to_the_width_keeping_the_ratio_when_content_is_too_wide() {
        assert_eq!(fit(400.0, 200.0, 100.0), (100.0, 50.0));
        assert_eq!(fit(50.0, 20.0, 100.0), (50.0, 20.0));
    }

    #[test]
    fn should_return_a_unit_size_when_the_size_is_not_usable() {
        assert_eq!(fit(0.0, 10.0, 100.0), (1.0, 1.0));
        assert_eq!(fit(f32::NAN, 10.0, 100.0), (1.0, 1.0));
        assert_eq!(fit(-3.0, 10.0, 100.0), (1.0, 1.0));
    }

    #[test]
    fn should_reject_a_hash_that_could_leave_the_cache_when_it_is_not_hex() {
        assert!(is_plain_hash("00ffAB"));
        assert!(!is_plain_hash(""));
        assert!(!is_plain_hash("../etc/passwd"));
        assert!(!is_plain_hash("ab/cd"));
    }

    #[test]
    fn should_refuse_a_file_over_the_limit_when_reading_capped() {
        let dir = std::env::temp_dir().join(format!("limpid-boxes-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.bin");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        assert!(read_capped(&path, 99).is_none());
        assert_eq!(read_capped(&path, 100).unwrap().len(), 100);
        assert!(read_capped(&dir, 1000).is_none());
        assert!(read_capped(&dir.join("missing"), 1000).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_build_an_environment_from_settings_when_mermaid_is_disabled() {
        let settings = Settings::parse("[mermaid]\nenabled = false\n").unwrap();
        let env = BoxEnv::from_settings(&settings, Some(Path::new("/doc")));
        assert!(!env.mermaid_enabled);
        assert_eq!(env.document_dir.as_deref(), Some(Path::new("/doc")));
        assert_eq!(env.table_pad, BoxEnv::default().table_pad);
    }

    #[test]
    fn should_round_the_corners_of_a_framed_diagram_when_painting() {
        use crate::{layout::PaintCx, theme::Theme};
        let theme = Theme::dark();
        let paint = PaintCx {
            theme: &theme,
            scale: 1.0,
            origin_x: 0.0,
            origin_y: 0.0,
            viewport: 100.0,
            heading_tint: None,
        };
        let media = MediaBox {
            pixmap: pixmap(1, 1),
            x: 1.0,
            y: 1.0,
            framed: true,
            border: 1.0,
            hash: None,
        };
        let mut pixels = vec![theme.bg.to_pixel(); 60 * 60];
        let mut canvas = Canvas::new(&mut pixels, 60, 60);
        paint_media(&paint, &mut canvas, Rect::new(0.0, 0.0, 60.0, 60.0), &media);
        assert_eq!(canvas.pixel(0, 0), Some(theme.bg.to_pixel()));
        assert_eq!(canvas.pixel(30, 0), Some(theme.rule.to_pixel()));
        assert_eq!(canvas.pixel(30, 30), Some(theme.surface.to_pixel()));
    }
}
