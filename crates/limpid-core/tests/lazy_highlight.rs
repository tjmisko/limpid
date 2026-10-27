//! Parsing never highlights; highlighting is on demand. Kept in its own test
//! binary so the process-wide highlighter counter is not shared with other
//! tests.

use limpid_core::doc::{Block, BlockKind, Mode, parse};
use limpid_core::highlight::{Highlighter, highlight_spans};
use limpid_core::render::{self, RenderMode};
use limpid_core::settings::Settings;
use std::sync::{Mutex, MutexGuard};

const SAMPLE: &str = include_str!("../../limpid-view/tests/fixtures/sample.md");
const RUST_FENCE: &str =
    "```rust\nfn main() {\n    let answer = \"é\";\n    println!(\"{answer}\");\n}\n```\n";

/// Tests that build highlighters take this so the counter test sees only its
/// own constructions.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The highlight classes in `html`, skipping the empty spans the HTML
/// emitter prints for zero-width captures.
fn hl_classes_in_html(html: &str) -> Vec<String> {
    const OPEN: &str = "<span class=\"hl-";
    let mut classes = Vec::new();
    for tail in html.split(OPEN).skip(1) {
        let Some((class, after)) = tail.split_once("\">") else {
            continue;
        };
        if after.starts_with("</span>") {
            continue;
        }
        classes.push(class.to_owned());
    }
    classes
}

fn rust_installed() -> bool {
    limpid_core::grammars::grammar_dir()
        .map(|dir| dir.join("rust.so").is_file())
        .unwrap_or(false)
}

fn assert_all_spans_empty(blocks: &[Block]) {
    for block in blocks {
        match &block.kind {
            BlockKind::Code { spans, .. } => assert!(spans.is_empty()),
            BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
                assert_all_spans_empty(blocks)
            }
            BlockKind::List { items, .. } => {
                for item in items {
                    assert_all_spans_empty(&item.blocks);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn should_not_construct_a_highlighter_when_parsing_and_should_leave_spans_empty() {
    let _guard = serial();
    let before = Highlighter::constructed_count();
    let settings = Settings::default();
    for source in [SAMPLE, RUST_FENCE, "> ```python\n> x = 1\n> ```\n"] {
        for mode in [Mode::File, Mode::Agent] {
            let doc = parse(source, mode, &settings);
            assert!(doc.code_block_count() > 0);
            assert_all_spans_empty(&doc.blocks);
        }
    }
    assert_eq!(Highlighter::constructed_count(), before);

    let mut highlighter = Highlighter::new();
    assert_eq!(Highlighter::constructed_count(), before + 1);
    let _ = highlight_spans("rust", "fn x() {}", &mut highlighter);
    assert_eq!(Highlighter::constructed_count(), before + 1);
}

#[test]
fn should_count_code_blocks_when_they_are_nested_in_containers() {
    let _guard = serial();
    let source = "```rust\na\n```\n\n- item\n\n  ```sh\n  b\n  ```\n\n> ```\n> c\n> ```\n\n```mermaid\ngraph TD; A-->B\n```\n\n    indented\n";
    let doc = parse(source, Mode::File, &Settings::default());
    assert_eq!(doc.code_block_count(), 4);
    let empty = parse("", Mode::File, &Settings::default());
    assert_eq!(empty.code_block_count(), 0);
}

#[test]
fn should_yield_the_html_emitters_classes_when_highlighting_rust() {
    let _guard = serial();
    if !rust_installed() {
        return;
    }
    let doc = parse(RUST_FENCE, Mode::File, &Settings::default());
    let BlockKind::Code { lang, text, .. } = &doc.blocks[0].kind else {
        panic!("{:?}", doc.blocks);
    };
    let mut highlighter = Highlighter::new();
    let spans = highlight_spans(lang.as_deref().unwrap(), text, &mut highlighter);
    assert!(!spans.is_empty());
    let rendered = render::render(RUST_FENCE, RenderMode::File);
    let from_spans: Vec<_> = spans.iter().map(|span| span.class.clone()).collect();
    assert_eq!(from_spans, hl_classes_in_html(&rendered.body));
    for span in &spans {
        assert!(span.range.end <= text.len());
        assert!(text.is_char_boundary(span.range.start));
        assert!(text.is_char_boundary(span.range.end));
        assert!(!span.range.is_empty());
    }
}

#[test]
fn should_find_the_keyword_range_when_highlighting_a_rust_function() {
    let _guard = serial();
    if !rust_installed() {
        return;
    }
    let mut highlighter = Highlighter::new();
    let spans = highlight_spans("rust", "fn main() {}", &mut highlighter);
    assert!(
        spans
            .iter()
            .any(|span| span.range == (0..2) && span.class.starts_with("keyword")),
        "{spans:?}"
    );
}

#[test]
fn should_return_no_spans_when_language_is_unknown_or_source_is_empty() {
    let _guard = serial();
    let mut highlighter = Highlighter::new();
    assert!(highlight_spans("no-such-language", "<b>x</b>", &mut highlighter).is_empty());
    assert!(highlight_spans("", "x", &mut highlighter).is_empty());
    assert!(highlight_spans("rust", "", &mut highlighter).is_empty());
}

#[test]
fn should_reuse_the_highlighter_when_called_repeatedly() {
    let _guard = serial();
    if !rust_installed() {
        return;
    }
    let mut highlighter = Highlighter::new();
    let first = highlight_spans("rust", "let x = 1;", &mut highlighter);
    let second = highlight_spans("rs", "let x = 1;", &mut highlighter);
    assert_eq!(first, second);
}

#[test]
fn should_emit_highlight_classes_when_the_page_is_rendered_from_a_doc() {
    let _guard = serial();
    if !rust_installed() {
        return;
    }
    let doc = parse(RUST_FENCE, Mode::File, &Settings::default());
    let html = limpid_core::html::emit(&doc, &limpid_core::html::EmitOptions::default());
    assert!(html.contains("hl-"), "{html}");
}
