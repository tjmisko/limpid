//! Tables, images, SVG, math and Mermaid, headless: snapshots against
//! committed images and behaviour of the laid-out boxes.
//!
//! The math and Mermaid caches are pre-seeded in a private directory that
//! `XDG_CACHE_HOME` points at, so nothing here reads the user's cache or runs a
//! helper.

mod common;

use common::{FIXTURES, assert_matches_snapshot, fixture, page, shot};
use limpid_core::{
    doc::{Block, BlockKind, Doc, Inline, Mode, parse},
    settings::Settings,
    transport::Scheme,
};
use limpid_view::{
    layout::{
        Content, Layout, LayoutOptions, PendingKind,
        boxes::{self, BoxEnv},
        image as images, lay_out_in, svg,
    },
    paint::Canvas,
    text::Fonts,
    theme::Theme,
};
use std::{
    path::{Path, PathBuf},
    sync::Once,
};

const DARK_INK: u32 = 0xe8eeed;
const LIGHT_INK: u32 = 0x20252a;
const MERMAID_COLOR: u32 = 0xab1234;

fn cache_root() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("boxes-cache")
}

fn walk_inlines(inlines: &[Inline], out: &mut Vec<(String, String, bool)>) {
    for inline in inlines {
        match inline {
            Inline::Math { latex, hash } => out.push((latex.clone(), hash.clone(), false)),
            Inline::Emph(children) | Inline::Strong(children) | Inline::Strike(children) => {
                walk_inlines(children, out)
            }
            Inline::Link { inlines, .. } => walk_inlines(inlines, out),
            _ => {}
        }
    }
}

fn collect(blocks: &[Block], math: &mut Vec<(String, String, bool)>, mermaid: &mut Vec<String>) {
    for block in blocks {
        match &block.kind {
            BlockKind::Math { latex, hash } => math.push((latex.clone(), hash.clone(), true)),
            BlockKind::Mermaid { source, hash } if source.contains("Seeded") => {
                mermaid.push(hash.clone())
            }
            BlockKind::Paragraph(inlines) => walk_inlines(inlines, math),
            _ => {}
        }
    }
}

fn svg_of(width: f32, height: f32, color: &str) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><rect width=\"{width}\" height=\"{height}\" fill=\"{color}\"/></svg>"
    )
}

/// Write one formula into the math cache the way the helper does.
fn seed_math(root: &Path, hash: &str, width: f32, height: f32, baseline: f32) {
    let dir = root.join("limpid").join("math");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{hash}.svg")),
        svg_of(width, height, "#000000"),
    )
    .unwrap();
    std::fs::write(
        dir.join(format!("{hash}.json")),
        format!(
            "{{\"width\":{width},\"height\":{height},\"baseline\":{baseline},\"backend\":\"test\"}}"
        ),
    )
    .unwrap();
}

fn seed_mermaid(root: &Path, hash: &str) {
    let dir = root.join("limpid").join("mermaid");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{hash}.svg")),
        svg_of(300.0, 120.0, "#ab1234"),
    )
    .unwrap();
}

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = cache_root();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // SAFETY: runs once, before any test of this binary reads the variable.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &root) };
        let settings = Settings::default();
        for name in ["math.md", "mermaid.md"] {
            let doc = parse(&fixture(name), Mode::File, &settings);
            let (mut math, mut mermaid) = (Vec::new(), Vec::new());
            collect(&doc.blocks, &mut math, &mut mermaid);
            for (latex, hash, display) in math {
                if latex.contains("neverTypeset") {
                    continue;
                }
                if display {
                    seed_math(&root, &hash, 120.0, 48.0, 30.0);
                } else {
                    seed_math(&root, &hash, 40.0, 20.0, 15.0);
                }
            }
            for hash in mermaid {
                seed_mermaid(&root, &hash);
            }
        }
        boxes::set_default_env(BoxEnv {
            document_dir: Some(PathBuf::from(FIXTURES)),
            ..BoxEnv::default()
        });
    });
}

fn env() -> BoxEnv {
    BoxEnv {
        document_dir: Some(PathBuf::from(FIXTURES)),
        ..BoxEnv::default()
    }
}

fn parsed(markdown: &str) -> Doc {
    parse(markdown, Mode::File, &Settings::default())
}

fn laid(markdown: &str, width: f32, scale: f32, env: BoxEnv) -> (Doc, Layout, Fonts) {
    init();
    let doc = parsed(markdown);
    let mut fonts = Fonts::new();
    let layout = lay_out_in(
        &doc,
        width,
        &Theme::dark(),
        scale,
        &mut fonts,
        LayoutOptions::default(),
        env,
    );
    (doc, layout, fonts)
}

fn paint_all(
    layout: &mut Layout,
    fonts: &mut Fonts,
    width: u32,
    height: u32,
    scroll: f32,
) -> Vec<u32> {
    let mut pixels = vec![0u32; (width * height) as usize];
    let mut canvas = Canvas::new(&mut pixels, width, height);
    canvas.clear(layout.theme().bg);
    layout.paint(&mut canvas, fonts, scroll);
    pixels
}

fn count(pixels: &[u32], color: u32) -> usize {
    pixels.iter().filter(|&&pixel| pixel == color).count()
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("boxes-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env_in(dir: &Path) -> BoxEnv {
    BoxEnv {
        document_dir: Some(dir.to_path_buf()),
        ..BoxEnv::default()
    }
}

// Snapshots. The expected images depend on the installed fonts; regenerate
// them with LIMPID_UPDATE_SNAPSHOTS=1 after changing fonts or the layout.

#[test]
fn should_match_the_committed_image_when_rendering_tables_wide() {
    init();
    let mut page = page(&fixture("tables.md"), Scheme::Dark);
    assert_matches_snapshot(
        &shot(&mut page, 1000, 1100, 1.0),
        "tables-dark-1000x1100-s1",
    );
}

#[test]
fn should_match_the_committed_image_when_rendering_tables_narrow() {
    init();
    let mut page = page(&fixture("tables.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 520, 1500, 1.0), "tables-dark-520x1500-s1");
}

#[test]
fn should_match_the_committed_image_when_rendering_tables_light_at_scale_two() {
    init();
    let mut page = page(&fixture("tables.md"), Scheme::Light);
    assert_matches_snapshot(&shot(&mut page, 1000, 800, 2.0), "tables-light-1000x800-s2");
}

#[test]
fn should_match_the_committed_image_when_rendering_images_and_svg() {
    init();
    let mut page = page(&fixture("media.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 1000, 1200, 1.0), "media-dark-1000x1200-s1");
}

#[test]
fn should_match_the_committed_image_when_rendering_math() {
    init();
    let mut page = page(&fixture("math.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 1000, 520, 1.0), "math-dark-1000x520-s1");
}

#[test]
fn should_paint_math_in_ink_when_the_svg_fill_is_black_on_the_dark_theme() {
    init();
    let mut page = page(&fixture("math.md"), Scheme::Dark);
    let image = shot(&mut page, 1000, 520, 1.0);
    let ink = image
        .pixels
        .iter()
        .filter(|&&pixel| pixel == DARK_INK)
        .count();
    assert!(ink > 1000, "only {ink} ink pixels");
    assert_eq!(image.pixels.iter().filter(|&&pixel| pixel == 0).count(), 0);
}

#[test]
fn should_paint_math_in_ink_when_the_svg_fill_is_black_on_the_light_theme() {
    init();
    let mut page = page(&fixture("math.md"), Scheme::Light);
    let image = shot(&mut page, 1000, 520, 1.0);
    let ink = image
        .pixels
        .iter()
        .filter(|&&pixel| pixel == LIGHT_INK)
        .count();
    assert!(ink > 1000, "only {ink} ink pixels");
    // The light ink is dark but not black, so no pixel is pure black.
    assert_eq!(image.pixels.iter().filter(|&&pixel| pixel == 0).count(), 0);
}

#[test]
fn should_match_the_committed_image_when_rendering_math_on_the_light_theme() {
    init();
    let mut page = page(&fixture("math.md"), Scheme::Light);
    assert_matches_snapshot(&shot(&mut page, 1000, 520, 1.0), "math-light-1000x520-s1");
}

#[test]
fn should_keep_mermaid_colours_when_math_is_masked() {
    let (_, mut layout, mut fonts) = laid(&fixture("mermaid.md"), 1000.0, 1.0, env());
    let pixels = paint_all(&mut layout, &mut fonts, 1000, 760, 0.0);
    assert!(
        count(&pixels, MERMAID_COLOR) > 1000,
        "diagram lost its colour"
    );
}

#[test]
fn should_match_the_committed_image_when_rendering_mermaid() {
    init();
    let mut page = page(&fixture("mermaid.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 1000, 760, 1.0), "mermaid-dark-1000x760-s1");
}

// Tables.

#[test]
fn should_scroll_a_wide_table_sideways_when_the_window_is_narrow() {
    let (_, mut layout, _) = laid(&fixture("tables.md"), 520.0, 1.0, env());
    let wide = layout
        .blocks
        .iter()
        .position(|block| matches!(&block.content, Content::Table(table) if table.overflow))
        .expect("the six column table overflows a narrow window");
    let top = layout.blocks[wide].rect.y;
    let found = layout
        .table_block_in(top, top + 10.0)
        .expect("found by position");
    assert!(found.scroll_table_x(80.0));
    let Content::Table(table) = &found.content else {
        panic!("not a table");
    };
    assert_eq!(table.scroll_x, 80.0);
}

#[test]
fn should_not_scroll_tables_that_fit_when_the_window_is_wide() {
    let (_, layout, _) = laid(&fixture("tables.md"), 1400.0, 1.0, env());
    for block in &layout.blocks {
        if let Content::Table(table) = &block.content {
            assert!(
                !table.overflow,
                "table at y={} overflows a wide window",
                block.rect.y
            );
        }
    }
}

#[test]
fn should_draw_the_scrolled_part_of_a_wide_table_when_scroll_x_moves() {
    let (_, mut layout, mut fonts) = laid(&fixture("tables.md"), 520.0, 1.0, env());
    let wide = layout
        .blocks
        .iter()
        .position(|block| matches!(&block.content, Content::Table(table) if table.overflow))
        .unwrap();
    let scroll = (layout.blocks[wide].rect.y - 20.0).max(0.0);
    let before = paint_all(&mut layout, &mut fonts, 520, 600, scroll);
    assert!(layout.blocks[wide].scroll_table_x(200.0));
    let after = paint_all(&mut layout, &mut fonts, 520, 600, scroll);
    assert_ne!(before, after);
}

#[test]
fn should_keep_every_stop_when_a_document_mixes_all_box_kinds() {
    let markdown = format!(
        "{}\n\n{}\n\n{}\n",
        fixture("tables.md"),
        fixture("media.md"),
        fixture("math.md")
    );
    let (_, layout, _) = laid(&markdown, 900.0, 1.0, env());
    let stops = layout.stops().count();
    assert!(stops >= 12, "{stops}");
}

// Images.

#[test]
fn should_decode_and_scale_a_png_to_its_natural_size_when_it_fits() {
    let (_, layout, _) = laid("![p](boxes-picture.png)\n", 1000.0, 1.0, env());
    let Content::Media(media) = &layout.blocks[0].content else {
        panic!("not drawn");
    };
    assert_eq!((media.pixmap.width(), media.pixmap.height()), (240, 120));
    assert_eq!(layout.blocks[0].rect.h, 120.0);
    assert!(layout.blocks[0].stop);
}

#[test]
fn should_double_the_pixels_when_the_scale_is_two() {
    let (_, layout, _) = laid("![p](boxes-picture.png)\n", 2000.0, 2.0, env());
    let Content::Media(media) = &layout.blocks[0].content else {
        panic!("not drawn");
    };
    assert_eq!((media.pixmap.width(), media.pixmap.height()), (480, 240));
}

#[test]
fn should_shrink_an_image_to_the_column_when_it_is_wider() {
    let dir = scratch("wide-image");
    image_file(&dir.join("big.png"), 1200, 600, image::ImageFormat::Png);
    let (_, layout, _) = laid("![big](big.png)\n", 600.0, 1.0, env_in(&dir));
    let Content::Media(media) = &layout.blocks[0].content else {
        panic!("not drawn");
    };
    assert_eq!(media.pixmap.width() as f32, layout.blocks[0].rect.w);
    assert_eq!(
        media.pixmap.height() as f32 * 2.0,
        media.pixmap.width() as f32
    );
}

fn image_file(path: &Path, width: u32, height: u32, format: image::ImageFormat) {
    let buffer = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 90])
    });
    buffer.save_with_format(path, format).unwrap();
}

#[test]
fn should_decode_a_jpeg_when_the_source_is_a_jpeg_file() {
    let dir = scratch("jpeg");
    image_file(&dir.join("photo.jpg"), 64, 32, image::ImageFormat::Jpeg);
    let (_, layout, _) = laid("![photo](photo.jpg)\n", 800.0, 1.0, env_in(&dir));
    let Content::Media(media) = &layout.blocks[0].content else {
        panic!("jpeg not drawn");
    };
    assert_eq!((media.pixmap.width(), media.pixmap.height()), (64, 32));
}

#[test]
fn should_resolve_an_image_next_to_the_document_when_the_path_has_escapes() {
    let dir = scratch("escapes");
    image_file(&dir.join("my pic.png"), 10, 10, image::ImageFormat::Png);
    let (_, layout, _) = laid("![x](my%20pic.png)\n", 800.0, 1.0, env_in(&dir));
    assert!(matches!(layout.blocks[0].content, Content::Media(_)));
}

#[test]
fn should_show_alt_text_and_the_reason_when_the_image_is_missing() {
    let (_, layout, _) = laid("![The missing chart](nope-q7.png)\n", 800.0, 1.0, env());
    let Content::Pending(pending) = &layout.blocks[0].content else {
        panic!("missing image is not a text box");
    };
    assert_eq!(pending.kind, PendingKind::Image);
    assert!(layout.blocks[0].stop);
    assert!(layout.blocks[0].rect.h > 40.0);
}

#[test]
fn should_not_fetch_remote_images_when_the_source_is_a_url() {
    let (_, layout, _) = laid(
        "![logo](https://example.invalid/x.png)\n",
        800.0,
        1.0,
        env(),
    );
    assert!(matches!(layout.blocks[0].content, Content::Pending(_)));
}

#[test]
fn should_fall_back_to_the_alt_box_when_the_file_is_not_an_image() {
    let dir = scratch("corrupt");
    std::fs::write(dir.join("broken.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    std::fs::write(dir.join("notes.png"), b"just text").unwrap();
    let (_, layout, _) = laid(
        "![a](broken.png)\n\n![b](notes.png)\n",
        800.0,
        1.0,
        env_in(&dir),
    );
    assert!(
        layout
            .blocks
            .iter()
            .all(|block| matches!(block.content, Content::Pending(_)))
    );
}

#[test]
fn should_refuse_to_decode_when_the_image_exceeds_the_pixel_limit() {
    let dir = scratch("limit");
    image_file(&dir.join("big.png"), 200, 200, image::ImageFormat::Png);
    let env = BoxEnv {
        max_image_pixels: 10_000,
        ..env_in(&dir)
    };
    let (_, layout, _) = laid("![big](big.png)\n", 800.0, 1.0, env);
    assert!(matches!(layout.blocks[0].content, Content::Pending(_)));
}

#[test]
fn should_refuse_to_read_when_the_file_exceeds_the_byte_limit() {
    let dir = scratch("bytes");
    image_file(&dir.join("big.png"), 50, 50, image::ImageFormat::Png);
    let env = BoxEnv {
        max_file_bytes: 64,
        ..env_in(&dir)
    };
    let (_, layout, _) = laid("![big](big.png)\n", 800.0, 1.0, env);
    assert!(matches!(layout.blocks[0].content, Content::Pending(_)));
}

#[test]
fn should_reuse_the_scaled_pixmap_when_the_same_image_is_laid_out_twice() {
    images::clear_cache();
    let (_, first, _) = laid("![p](boxes-picture.png)\n", 1000.0, 1.0, env());
    let (_, second, _) = laid("![p](boxes-picture.png)\n", 1000.0, 1.0, env());
    let (Content::Media(a), Content::Media(b)) =
        (&first.blocks[0].content, &second.blocks[0].content)
    else {
        panic!("not drawn");
    };
    assert!(std::rc::Rc::ptr_eq(&a.pixmap, &b.pixmap));
    assert_eq!(images::cached_images(), 1);
}

#[test]
fn should_decode_again_when_the_file_changes() {
    let dir = scratch("changed");
    image_file(&dir.join("a.png"), 20, 10, image::ImageFormat::Png);
    let (_, first, _) = laid("![p](a.png)\n", 800.0, 1.0, env_in(&dir));
    image_file(&dir.join("a.png"), 40, 20, image::ImageFormat::Png);
    let (_, second, _) = laid("![p](a.png)\n", 800.0, 1.0, env_in(&dir));
    let widths: Vec<u32> = [&first, &second]
        .iter()
        .map(|layout| match &layout.blocks[0].content {
            Content::Media(media) => media.pixmap.width(),
            _ => 0,
        })
        .collect();
    assert_eq!(widths, vec![20, 40]);
}

#[test]
fn should_paint_the_picture_colours_when_an_image_is_on_screen() {
    let (_, mut layout, mut fonts) = laid("![p](boxes-picture.png)\n", 1000.0, 1.0, env());
    let pixels = paint_all(&mut layout, &mut fonts, 1000, 300, 0.0);
    assert!(count(&pixels, 0xe68c28) > 5000, "orange half missing");
    assert!(count(&pixels, 0x1ea096) > 5000, "teal half missing");
}

// SVG.

#[test]
fn should_draw_a_linked_svg_at_its_natural_size_when_it_fits() {
    let (_, layout, _) = laid("![art](boxes-art.svg)\n", 1000.0, 1.0, env());
    let Content::Media(media) = &layout.blocks[0].content else {
        panic!("svg not drawn");
    };
    assert_eq!((media.pixmap.width(), media.pixmap.height()), (160, 80));
}

#[test]
fn should_cache_the_raster_by_size_when_the_same_svg_is_laid_out_at_two_scales() {
    svg::clear_cache();
    laid("![art](boxes-art.svg)\n", 1000.0, 1.0, env());
    laid("![art](boxes-art.svg)\n", 1000.0, 1.0, env());
    assert_eq!(svg::cached_rasters(), 1);
    laid("![art](boxes-art.svg)\n", 2000.0, 2.0, env());
    assert_eq!(svg::cached_rasters(), 2);
}

#[test]
fn should_show_the_alt_box_when_the_svg_file_does_not_parse() {
    let dir = scratch("badsvg");
    std::fs::write(dir.join("bad.svg"), "<svg><unclosed").unwrap();
    let (_, layout, _) = laid("![bad](bad.svg)\n", 800.0, 1.0, env_in(&dir));
    assert!(matches!(layout.blocks[0].content, Content::Pending(_)));
}

#[test]
fn should_show_the_alt_box_when_the_source_is_a_data_uri() {
    for markdown in [
        "![x](data:image/svg+xml;base64,@@@)\n",
        "![x](data:image/svg+xml;utf8,%3Csvg)\n",
        "![x](data:image/png;base64,AAAA)\n",
        "![x](data:nothing)\n",
    ] {
        let (_, layout, _) = laid(markdown, 800.0, 1.0, env());
        assert!(
            matches!(layout.blocks[0].content, Content::Pending(_)),
            "{markdown}"
        );
    }
}

// Math.

#[test]
fn should_centre_display_math_when_the_formula_is_cached() {
    let (_, layout, _) = laid(&fixture("math.md"), 1000.0, 1.0, env());
    let drawn = layout
        .blocks
        .iter()
        .find_map(|block| match &block.content {
            Content::Media(media) => Some((block, media)),
            _ => None,
        })
        .expect("a cached display formula is drawn");
    let (block, media) = drawn;
    let factor = Theme::dark().font_size / 16.0;
    assert_eq!(media.pixmap.width(), (120.0 * factor).round() as u32);
    let left = media.x;
    let right = block.rect.w - media.x - media.pixmap.width() as f32;
    assert!((left - right).abs() <= 1.0, "{left} vs {right}");
    assert!(block.stop);
}

#[test]
fn should_show_latex_source_on_code_background_when_display_math_is_not_cached() {
    let (_, layout, _) = laid(&fixture("math.md"), 1000.0, 1.0, env());
    let source = layout
        .blocks
        .iter()
        .find_map(|block| match &block.content {
            Content::Pending(pending) if pending.kind == PendingKind::Math => Some(pending),
            _ => None,
        })
        .expect("the uncached formula shows its source");
    assert_eq!(source.fill, Some(Theme::dark().code_bg));
}

#[test]
fn should_paint_cached_math_colours_when_math_is_on_screen() {
    let (_, mut layout, mut fonts) = laid(&fixture("math.md"), 1000.0, 1.0, env());
    let pixels = paint_all(&mut layout, &mut fonts, 1000, 500, 0.0);
    assert!(count(&pixels, DARK_INK) > 1000, "display formula missing");
    assert_eq!(
        count(&pixels, 0x000000),
        0,
        "black TeX fill reached the page"
    );
}

#[test]
fn should_sit_inline_math_on_the_text_baseline_when_the_formula_is_cached() {
    let (_, layout, _) = laid("Text $a^2$ more text.\n", 1000.0, 1.0, env_seeded_inline());
    let Content::Text(text) = &layout.blocks[0].content else {
        panic!("not text");
    };
    let typeset = boxes::typeset_math(text);
    assert_eq!(typeset.len(), 1);
    let factor = text.font_size / 16.0;
    let formula = &typeset[0];
    // The SVG's baseline row lands on the text baseline.
    assert_eq!(formula.size.2, 15.0);
    assert!(formula.baseline - formula.size.2 * factor > 0.0);
    assert!(formula.width > 0.0);
}

fn env_seeded_inline() -> BoxEnv {
    init();
    let doc = parsed("Text $a^2$ more text.\n");
    let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
        panic!("no paragraph");
    };
    let mut found = Vec::new();
    walk_inlines(inlines, &mut found);
    seed_math(&cache_root(), &found[0].1, 40.0, 20.0, 15.0);
    env()
}

#[test]
fn should_paint_inline_math_over_the_line_when_it_is_in_a_list_item() {
    let markdown = "- item $a^2$ end\n";
    let _ = env_seeded_for(markdown);
    let (_, mut layout, mut fonts) = laid(markdown, 1000.0, 1.0, env());
    let pixels = paint_all(&mut layout, &mut fonts, 1000, 200, 0.0);
    assert!(count(&pixels, DARK_INK) > 100);
}

fn env_seeded_for(markdown: &str) -> BoxEnv {
    init();
    let doc = parsed(markdown);
    let mut found = Vec::new();
    for block in &doc.blocks {
        if let BlockKind::List { items, .. } = &block.kind {
            for item in items {
                for inner in &item.blocks {
                    if let BlockKind::Paragraph(inlines) = &inner.kind {
                        walk_inlines(inlines, &mut found);
                    }
                }
            }
        }
    }
    for (_, hash, _) in &found {
        seed_math(&cache_root(), hash, 40.0, 20.0, 15.0);
    }
    env()
}

#[test]
fn should_swap_source_for_the_drawing_when_the_cache_gains_the_formula() {
    init();
    let markdown = "before\n\n$$\n\\lateArrivalZ5 + 2\n$$\n\nafter\n";
    let (doc, mut layout, mut fonts) = laid(markdown, 900.0, 1.0, env());
    let BlockKind::Math { hash, .. } = &doc.blocks[1].kind else {
        panic!("no display math");
    };
    assert!(matches!(layout.blocks[1].content, Content::Pending(_)));
    let after_before = layout.blocks[2].rect.y;
    let first_rect = layout.blocks[0].rect;
    seed_math(&cache_root(), hash, 120.0, 48.0, 30.0);
    assert!(layout.refresh_pending(std::slice::from_ref(hash)));
    assert!(!layout.is_complete());
    layout.ensure_to(1e6, &doc, &mut fonts);
    assert!(layout.is_complete());
    assert!(matches!(layout.blocks[1].content, Content::Media(_)));
    assert_eq!(layout.blocks[0].rect, first_rect);
    assert_ne!(layout.blocks[2].rect.y, after_before);
    assert!(layout.blocks[2].rect.y >= layout.blocks[1].rect.bottom());
}

#[test]
fn should_leave_the_layout_alone_when_the_hash_is_not_on_the_page() {
    let (_, mut layout, _) = laid("plain\n\n$$\nx\n$$\n", 900.0, 1.0, env());
    let count = layout.blocks.len();
    assert!(!layout.refresh_pending(&["deadbeefdeadbeef".to_owned()]));
    assert!(!layout.refresh_pending(&[]));
    assert_eq!(layout.blocks.len(), count);
    assert!(layout.is_complete());
}

#[test]
fn should_refresh_a_formula_inside_a_quote_when_the_cache_gains_it() {
    init();
    let markdown = "> note\n>\n> $$\n> \\nestedArrivalZ5\n> $$\n\nafter\n";
    let (doc, mut layout, mut fonts) = laid(markdown, 900.0, 1.0, env());
    let BlockKind::Quote { blocks, .. } = &doc.blocks[0].kind else {
        panic!("no quote");
    };
    let hash = blocks
        .iter()
        .find_map(|block| match &block.kind {
            BlockKind::Math { hash, .. } => Some(hash.clone()),
            _ => None,
        })
        .expect("math in the quote");
    seed_math(&cache_root(), &hash, 120.0, 48.0, 30.0);
    assert!(layout.refresh_pending(&[hash]));
    layout.ensure_to(1e6, &doc, &mut fonts);
    let quote = &layout.blocks[0];
    assert!(
        quote
            .children()
            .iter()
            .any(|child| matches!(child.content, Content::Media(_)))
    );
}

#[test]
fn should_refresh_from_the_first_block_when_inline_math_in_the_first_paragraph_arrives() {
    init();
    let markdown = "first $\\inlineArrivalZ5$ line\n\nsecond\n";
    let (doc, mut layout, mut fonts) = laid(markdown, 900.0, 1.0, env());
    let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
        panic!("no paragraph");
    };
    let mut found = Vec::new();
    walk_inlines(inlines, &mut found);
    let hash = found[0].1.clone();
    seed_math(&cache_root(), &hash, 40.0, 20.0, 15.0);
    assert!(layout.refresh_pending(&[hash]));
    layout.ensure_to(1e6, &doc, &mut fonts);
    let Content::Text(text) = &layout.blocks[0].content else {
        panic!("not text");
    };
    assert_eq!(boxes::typeset_math(text).len(), 1);
    assert_eq!(layout.blocks.len(), 2);
}

// Mermaid.

#[test]
fn should_frame_the_diagram_and_fit_the_column_when_the_svg_is_cached() {
    let (_, layout, _) = laid(&fixture("mermaid.md"), 400.0, 1.0, env());
    let framed = layout
        .blocks
        .iter()
        .find_map(|block| match &block.content {
            Content::Media(media) if media.framed => Some((block, media)),
            _ => None,
        })
        .expect("the seeded diagram is drawn");
    let (block, media) = framed;
    assert!(media.pixmap.width() as f32 + 2.0 * media.border < block.rect.w);
    assert!(block.rect.h > media.pixmap.height() as f32);
    assert!(block.stop);
}

#[test]
fn should_show_source_when_the_diagram_is_not_cached() {
    let (_, layout, _) = laid(&fixture("mermaid.md"), 1000.0, 1.0, env());
    let sources = layout
        .blocks
        .iter()
        .filter(
            |block| matches!(&block.content, Content::Pending(p) if p.kind == PendingKind::Mermaid),
        )
        .count();
    assert_eq!(sources, 1);
}

#[test]
fn should_show_source_when_mermaid_is_disabled_even_if_the_svg_is_cached() {
    let disabled = BoxEnv {
        mermaid_enabled: false,
        ..env()
    };
    let (_, layout, _) = laid(&fixture("mermaid.md"), 1000.0, 1.0, disabled);
    let drawn = layout
        .blocks
        .iter()
        .filter(|block| matches!(block.content, Content::Media(_)))
        .count();
    assert_eq!(drawn, 0);
}

#[test]
fn should_show_source_when_the_cached_diagram_is_not_an_svg() {
    init();
    let markdown = "```mermaid\ngraph TD; BrokenCacheQ3-->B\n```\n";
    let doc = parsed(markdown);
    let BlockKind::Mermaid { hash, .. } = &doc.blocks[0].kind else {
        panic!("no diagram");
    };
    let dir = cache_root().join("limpid").join("mermaid");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{hash}.svg")), "this is not an svg").unwrap();
    let (_, layout, _) = laid(markdown, 900.0, 1.0, env());
    assert!(matches!(layout.blocks[0].content, Content::Pending(_)));
}

#[test]
fn should_paint_the_diagram_colours_when_the_diagram_is_on_screen() {
    let (_, mut layout, mut fonts) = laid(&fixture("mermaid.md"), 1000.0, 1.0, env());
    let pixels = paint_all(&mut layout, &mut fonts, 1000, 700, 0.0);
    assert!(count(&pixels, MERMAID_COLOR) > 5000);
}

// Robustness across all of it.

#[test]
fn should_survive_garbage_geometry_when_every_box_kind_is_on_the_page() {
    let markdown = format!(
        "{}\n\n{}\n\n{}\n\n{}\n",
        fixture("tables.md"),
        fixture("media.md"),
        fixture("math.md"),
        fixture("mermaid.md")
    );
    for (width, scale) in [
        (0.0, 1.0),
        (1.0, 1.0),
        (-5.0, 1.0),
        (f32::NAN, 1.0),
        (300.0, f32::NAN),
        (50.0, 0.1),
        (4000.0, 3.0),
    ] {
        let (doc, mut layout, mut fonts) = laid(&markdown, width, scale, env());
        layout.ensure_to(1e7, &doc, &mut fonts);
        assert!(layout.is_complete());
        let pixels = paint_all(&mut layout, &mut fonts, 200, 200, 0.0);
        assert_eq!(pixels.len(), 40_000);
    }
}

#[test]
fn should_paint_without_panicking_when_the_viewport_is_between_blocks_or_past_the_end() {
    let markdown = format!("{}\n\n{}\n", fixture("tables.md"), fixture("media.md"));
    let (doc, mut layout, mut fonts) = laid(&markdown, 900.0, 1.0, env());
    layout.ensure_to(1e7, &doc, &mut fonts);
    for scroll in [0.0, 333.3, 1e5, -50.0] {
        paint_all(&mut layout, &mut fonts, 400, 300, scroll);
    }
}
