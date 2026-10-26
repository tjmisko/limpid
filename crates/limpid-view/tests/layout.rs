//! Layout and paint of the document, headless: pixel snapshots against
//! committed images and behavioural checks on the laid-out geometry.

mod common;

use common::{assert_matches_snapshot, fixture, fonts, page, page_with, shot};
use limpid_core::{
    doc::{Mode, parse},
    settings::Settings,
    transport::Scheme,
};
use limpid_view::{
    layout::{Content, lay_out},
    page::SharedFonts,
    paint::Canvas,
    theme::Theme,
    window::Page,
};
use std::time::Instant;
use winit::keyboard::Key;

fn pixel(image: &limpid_view::snapshot::Image, x: u32, y: u32) -> u32 {
    image.pixels[(y * image.width + x) as usize]
}

// Snapshots. The expected images depend on the installed fonts; regenerate
// them with LIMPID_UPDATE_SNAPSHOTS=1 after changing fonts or the layout.

#[test]
fn should_match_the_committed_image_when_rendering_blocks_dark_at_scale_one() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 1000, 800, 1.0), "blocks-dark-1000x800-s1");
}

#[test]
fn should_match_the_committed_image_when_rendering_blocks_dark_at_scale_two() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    assert_matches_snapshot(&shot(&mut page, 1000, 800, 2.0), "blocks-dark-1000x800-s2");
}

#[test]
fn should_match_the_committed_image_when_rendering_blocks_light() {
    let mut page = page(&fixture("blocks.md"), Scheme::Light);
    assert_matches_snapshot(&shot(&mut page, 1000, 800, 1.0), "blocks-light-1000x800-s1");
}

#[test]
fn should_match_the_committed_image_when_the_window_is_wide_enough_for_the_rail() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    assert_matches_snapshot(
        &shot(&mut page, 1400, 800, 1.0),
        "blocks-dark-1400x800-rail",
    );
}

#[test]
fn should_match_the_committed_image_when_scrolled_into_the_lists_and_quotes() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    shot(&mut page, 1000, 800, 1.0);
    for _ in 0..3 {
        page.handle_key(&Key::Character("d".into()), true);
    }
    assert_matches_snapshot(
        &shot(&mut page, 1000, 800, 1.0),
        "blocks-dark-1000x800-scrolled",
    );
}

// Lazy layout.

fn long_document(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|n| format!("paragraph number {n}\n\n"))
        .collect()
}

#[test]
fn should_touch_few_blocks_when_laying_out_a_two_hundred_thousand_line_document_to_y_2000() {
    let source = long_document(100_000);
    assert_eq!(source.lines().count(), 200_000);
    let doc = parse(&source, Mode::File, &Settings::default());
    assert_eq!(doc.blocks.len(), 100_000);

    let mut fonts = limpid_view::text::Fonts::new();
    let started = Instant::now();
    let mut layout = lay_out(&doc, 1000.0, &Theme::dark(), 1.0, &mut fonts);
    layout.ensure_to(2000.0, &doc, &mut fonts);
    let elapsed = started.elapsed();

    assert!(
        layout.blocks.len() < 200,
        "laid out {}",
        layout.blocks.len()
    );
    assert!(layout.covered() >= 2000.0);
    assert!(!layout.is_complete());
    // Shaping 100,000 paragraphs would take far longer than this.
    assert!(elapsed.as_secs_f32() < 5.0, "{elapsed:?}");
}

#[test]
fn should_extend_the_layout_when_scrolling_deep_into_a_long_document() {
    let source = long_document(5_000);
    let mut page = page(&source, Scheme::Dark);
    shot(&mut page, 800, 600, 1.0);
    let first_screen = page.laid_out_blocks();
    for _ in 0..40 {
        page.handle_key(&Key::Named(winit::keyboard::NamedKey::PageDown), false);
    }
    shot(&mut page, 800, 600, 1.0);
    assert!(page.laid_out_blocks() > first_screen);
    assert!(page.laid_out_blocks() < 5_000);
    assert!(page.scroll() > 5_000.0);
}

// The rail.

fn background_at(page: &mut limpid_view::page::DocPage, width: u32, x: u32) -> u32 {
    pixel(&shot(page, width, 400, 1.0), x, 300)
}

#[test]
fn should_show_the_rail_exactly_when_the_window_reaches_content_plus_rail_plus_gutters() {
    let source = fixture("blocks.md");
    let theme = Theme::dark();
    let threshold = (theme.content_width + theme.rail_width + 2.0 * theme.margin) as u32;
    let rail_colour = 0x0a0e12;
    let mut wide = page(&source, Scheme::Dark);
    assert_eq!(background_at(&mut wide, threshold, 5), rail_colour);
    let mut narrow = page(&source, Scheme::Dark);
    assert_eq!(
        background_at(&mut narrow, threshold - 1, 5),
        theme.bg.to_pixel()
    );
}

#[test]
fn should_honour_the_rail_setting_when_forcing_it_on_or_off() {
    let source = fixture("blocks.md");
    let mut forced_on = page_with(&source, "[view]\nrail = \"on\"\n", Scheme::Dark);
    assert_eq!(background_at(&mut forced_on, 700, 5), 0x0a0e12);
    let mut forced_off = page_with(&source, "[view]\nrail = \"off\"\n", Scheme::Dark);
    assert_eq!(
        background_at(&mut forced_off, 1600, 5),
        Theme::dark().bg.to_pixel()
    );
}

#[test]
fn should_follow_the_viewport_when_the_rail_marks_the_active_heading() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    shot(&mut page, 1400, 800, 1.0);
    let top = page.layout().unwrap().rail_active(0.0);
    assert_eq!(top, Some(0));
    for _ in 0..2 {
        page.handle_key(&Key::Character("d".into()), true);
    }
    shot(&mut page, 1400, 800, 1.0);
    let layout = page.layout().unwrap();
    let lower = layout.rail_active(page.scroll() + 1.0);
    assert!(lower > Some(0), "{lower:?}");
}

// Code.

fn code_overflows(page: &mut limpid_view::page::DocPage) -> bool {
    shot(page, 700, 400, 1.0);
    match &page.layout().unwrap().blocks[0].content {
        Content::Code { overflow, .. } => *overflow,
        _ => panic!("first block is not code"),
    }
}

#[test]
fn should_wrap_long_code_lines_only_when_code_wrap_is_set() {
    let source = format!("```\n{}\n```\n", "wrap me ".repeat(60));
    assert!(code_overflows(&mut page(&source, Scheme::Dark)));
    assert!(!code_overflows(&mut page_with(
        &source,
        "[view]\ncode_wrap = true\n",
        Scheme::Dark
    )));
}

#[test]
fn should_scroll_a_wide_code_block_sideways_and_repaint_when_pressing_l() {
    let source = format!("```\n{}\n```\n", "0123456789".repeat(80));
    let mut page = page(&source, Scheme::Dark);
    let before = shot(&mut page, 700, 300, 1.0);
    assert!(page.handle_key(&Key::Character("l".into()), false));
    let after = shot(&mut page, 700, 300, 1.0);
    assert_ne!(before.pixels, after.pixels);
}

// Settings.

#[test]
fn should_narrow_the_column_when_content_width_is_configured() {
    let source = "a paragraph\n";
    let mut wide = page(source, Scheme::Dark);
    let mut narrow = page_with(source, "[view]\ncontent_width = 500\n", Scheme::Dark);
    shot(&mut wide, 1400, 400, 1.0);
    shot(&mut narrow, 1400, 400, 1.0);
    let wide_width = wide.layout().unwrap().column_width;
    let narrow_width = narrow.layout().unwrap().column_width;
    assert!(
        narrow_width < wide_width - 300.0,
        "{narrow_width} vs {wide_width}"
    );
}

#[test]
fn should_use_the_configured_line_height_when_laying_out_paragraphs() {
    let source = "a\n\nb\n";
    let height = |settings: &str| {
        let mut page = page_with(source, settings, Scheme::Dark);
        shot(&mut page, 800, 400, 1.0);
        page.layout().unwrap().blocks[0].rect.h
    };
    assert!(height("[view]\nline_height = 2.4\n") > height("[view]\nline_height = 1.2\n") * 1.5);
}

#[test]
fn should_render_in_the_light_theme_when_the_config_forces_it() {
    let mut page = page_with("text\n", "[view]\ntheme = \"light\"\n", Scheme::Dark);
    let image = shot(&mut page, 300, 200, 1.0);
    assert_eq!(pixel(&image, 0, 0), Theme::light().bg.to_pixel());
}

// Awkward input. None of these may panic or produce a zero-height page.

fn paint_without_panicking(source: &str) {
    for (width, height, scale) in [(800, 600, 1.0), (40, 30, 1.0), (1000, 300, 2.0)] {
        let mut page = page(source, Scheme::Dark);
        shot(&mut page, width, height, scale);
    }
}

#[test]
fn should_not_panic_when_the_document_has_crlf_line_endings() {
    paint_without_panicking(
        "# Title\r\n\r\nbody\r\n\r\n```rs\r\nfn a() {}\r\n```\r\n\r\n- a\r\n- b\r\n",
    );
}

#[test]
fn should_not_panic_when_a_fence_is_unclosed_or_the_language_is_unknown() {
    paint_without_panicking("```zzz\nnever closed\n\nmore");
    paint_without_panicking("```\n");
}

#[test]
fn should_not_panic_when_blocks_nest_very_deeply() {
    paint_without_panicking(&format!("{}deep\n", "> ".repeat(200)));
    let mut lists = String::new();
    for depth in 0..120 {
        lists.push_str(&" ".repeat(depth * 2));
        lists.push_str("- item\n");
    }
    paint_without_panicking(&lists);
}

#[test]
fn should_not_panic_when_text_is_empty_wide_or_exotic() {
    paint_without_panicking("");
    paint_without_panicking("\n\n\n");
    paint_without_panicking(
        &"\u{4e16}\u{754c} \u{1f600} caf\u{e9} \u{200f}\u{5e9}\u{5dc}\u{5d5}\u{5dd} ".repeat(300),
    );
    paint_without_panicking(&format!("# {}\n", "long ".repeat(500)));
    paint_without_panicking(&"x".repeat(100_000));
    paint_without_panicking("<div>raw</div>\n\n<span>inline</span> text\n");
    paint_without_panicking("| a | b |\n|---|---|\n| 1 | 2 |\n\n$$x^2$$\n\n![alt](nothing.png)\n");
}

#[test]
fn should_draw_every_kind_of_content_when_painting_the_fixture() {
    let mut page = page(&fixture("blocks.md"), Scheme::Dark);
    let image = shot(&mut page, 1000, 3000, 1.0);
    let bg = Theme::dark().bg.to_pixel();
    let rows_with_ink = (0..image.height)
        .filter(|&y| (0..image.width).any(|x| pixel(&image, x, y) != bg))
        .count();
    assert!(rows_with_ink > 800, "{rows_with_ink}");
}

#[test]
fn should_share_one_font_system_between_pages() {
    let shared: SharedFonts = fonts();
    let make = |source: &str| {
        limpid_view::page::DocPage::from_source(
            source,
            "t".into(),
            &Settings::default(),
            Scheme::Dark,
            shared.clone(),
        )
    };
    let (mut one, mut two) = (make("one\n"), make("two\n"));
    for page in [&mut one, &mut two] {
        let mut pixels = vec![0u32; 200 * 100];
        page.paint(&mut Canvas::new(&mut pixels, 200, 100), 1.0);
    }
}
