//! Stage 2: a page imported over the bridge shows in the native viewer exactly
//! as an ordinary local agent page does, and cannot be submitted.

mod common;

use common::{fonts, shot};
use limpid_core::{
    bridge::{Delivery, OpenPage},
    bridge_import::{handoff_for, import_page},
    doc::{AnnotationKind, BlockKind, Mode},
    replies::{AgentReplies, sidecar_path},
    settings::Settings,
    transport::Scheme,
};
use limpid_view::{
    feedback::Feedback,
    nav::keymap::KeyInput,
    page::{DocPage, mode_for},
    window::Page,
};
use serde_json::json;
use std::{fs, path::PathBuf};

const MARKDOWN: &str = "# Pick a store\n\nUnicode: café 日本語\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - Postgres\n\nA closing paragraph.\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("limpid-remote-it-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn annotations() -> serde_json::Value {
    json!({
        "replies": {"q1": "SQLite"},
        "comments": {
            "0-14": {"text": "Name it better", "quote": "# Pick a store", "start_line": 1, "end_line": 1}
        }
    })
}

fn open_page(markdown: &str, annotations: serde_json::Value) -> OpenPage {
    OpenPage {
        route: "r1".into(),
        session: "sess-a".into(),
        page_id: "p1".into(),
        instance: "i1".into(),
        remote_path: Some("/nonexistent/remote/page.md".into()),
        markdown: markdown.into(),
        annotations,
        delivery: Delivery::Remote,
    }
}

/// What `limpid-core claude` writes locally for the same page.
fn local_agent_page(dir: &std::path::Path, markdown: &str, replies: &AgentReplies) -> PathBuf {
    let page = dir.join("claude-local.html");
    fs::write(page.with_extension("md"), markdown).unwrap();
    replies.save(&sidecar_path(&page)).unwrap();
    fs::write(
        page.with_extension("meta.json"),
        r#"{"agent":"claude","session_id":"sess-a","turn_spec":"0","source":null,"title":"Pick a store","feedback_sock":null}"#,
    )
    .unwrap();
    page.with_extension("md")
}

struct Opened {
    page: DocPage,
}

fn open(markdown_path: &std::path::Path, settings: &Settings) -> Opened {
    let handoff = limpid_core::transport::Handoff {
        path: markdown_path.to_path_buf(),
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    assert_eq!(mode_for(&handoff), Mode::Agent);
    let page =
        DocPage::open_as(markdown_path, settings, Scheme::Dark, fonts(), Mode::Agent).unwrap();
    let title = page.title();
    let feedback = Feedback::new(handoff, title, settings);
    Opened {
        page: page.with_feedback(feedback),
    }
}

fn annotation_blocks(page: &DocPage) -> Vec<(AnnotationKind, String)> {
    page.doc()
        .blocks
        .iter()
        .filter_map(|block| match &block.kind {
            BlockKind::Annotation { kind, text, .. } => Some((*kind, text.clone())),
            _ => None,
        })
        .collect()
}

fn replies_of(open: &OpenPage) -> AgentReplies {
    serde_json::from_value(open.annotations.clone()).unwrap()
}

#[test]
fn should_render_identically_to_a_local_agent_page_when_the_page_is_imported() {
    let root = scratch("render");
    let settings = Settings::default();
    let sent = open_page(MARKDOWN, annotations());
    let imported = import_page(&root.join("receiver"), &sent).unwrap();
    let local = local_agent_page(&root, MARKDOWN, &replies_of(&sent));

    let mut remote = open(&imported.markdown, &settings);
    let mut ordinary = open(&local, &settings);

    assert_eq!(remote.page.title(), ordinary.page.title());
    assert_eq!(
        remote.page.doc().blocks.len(),
        ordinary.page.doc().blocks.len()
    );
    let annotations = annotation_blocks(&remote.page);
    assert_eq!(annotations, annotation_blocks(&ordinary.page));
    assert!(
        annotations.iter().any(|(_, text)| text.contains("SQLite")),
        "{annotations:?}"
    );
    assert!(
        annotations
            .iter()
            .any(|(_, text)| text.contains("Name it better")),
        "{annotations:?}"
    );
    assert_eq!(
        shot(&mut remote.page, 900, 600, 1.0),
        shot(&mut ordinary.page, 900, 600, 1.0),
        "imported page paints the same pixels as a local agent page"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_render_without_the_sender_files_when_only_the_receiver_cache_exists() {
    let root = scratch("nosender");
    let sent = open_page(MARKDOWN, annotations());
    let imported = import_page(&root.join("receiver"), &sent).unwrap();
    assert!(!std::path::Path::new("/nonexistent/remote/page.md").exists());
    let mut page = open(&imported.markdown, &Settings::default());
    let image = shot(&mut page.page, 900, 600, 1.0);
    assert!(
        image.pixels.iter().any(|p| *p != image.pixels[0]),
        "something was painted"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_show_two_instances_with_their_own_annotations_when_opened_separately() {
    let root = scratch("two");
    let receiver = root.join("receiver");
    let mut one = open_page(MARKDOWN, json!({"replies": {"q1": "first answer"}}));
    one.instance = "i1".into();
    let mut two = open_page(MARKDOWN, json!({"replies": {"q1": "second answer"}}));
    two.instance = "i2".into();
    let a = import_page(&receiver, &one).unwrap();
    let b = import_page(&receiver, &two).unwrap();
    let settings = Settings::default();
    let page_a = open(&a.markdown, &settings);
    let page_b = open(&b.markdown, &settings);
    let text = |p: &Opened| {
        annotation_blocks(&p.page)
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join("|")
    };
    assert!(text(&page_a).contains("first answer"));
    assert!(!text(&page_a).contains("second answer"));
    assert!(text(&page_b).contains("second answer"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn should_save_but_not_submit_when_writing_and_quitting_a_remote_preview() {
    let root = scratch("preview");
    let marker = root.join("delivered.txt");
    let settings = Settings::parse(&format!(
        "[reply]\ncommand = \"cat > '{}'\"\n",
        marker.display()
    ))
    .unwrap();
    let sent = open_page(MARKDOWN, annotations());
    let imported = import_page(&root.join("receiver"), &sent).unwrap();
    let handoff = handoff_for(&imported, Scheme::Dark);
    let page = DocPage::open_as(
        &imported.markdown,
        &settings,
        Scheme::Dark,
        fonts(),
        Mode::Agent,
    )
    .unwrap();
    let title = page.title();
    let mut page = page.with_feedback(Feedback::new(handoff, title, &settings));
    shot(&mut page, 900, 600, 1.0);
    for key in [":", "w", "q", "enter"] {
        page.handle_input(&KeyInput::named(key));
        shot(&mut page, 900, 600, 1.0);
    }
    let status = page.nav().status_view(page.elapsed_ms()).message;
    assert!(status.contains("not sent"), "{status}");
    assert!(!marker.exists(), "no local delivery ran");
    let _ = fs::remove_dir_all(root);
}

fn magenta_count(image: &limpid_view::snapshot::Image) -> usize {
    image.pixels.iter().filter(|p| **p == 0xff00ff).count()
}

#[test]
fn should_not_draw_local_files_when_a_remote_page_references_them() {
    let root = scratch("assets");
    let sentinel = root.join("sentinel.png");
    limpid_view::snapshot::write_png(
        &sentinel,
        &limpid_view::snapshot::Image {
            width: 120,
            height: 80,
            pixels: vec![0xff00ff; 120 * 80],
        },
    )
    .unwrap();
    // Relative to the cache directory the page is imported into.
    let receiver = root.join("receiver");
    fs::create_dir_all(&receiver).unwrap();
    fs::copy(&sentinel, receiver.join("rel.png")).unwrap();
    let markdown = format!(
        "# Assets\n\n![a]({})\n\n![b](file://{})\n\n![c](rel.png)\n",
        sentinel.display(),
        sentinel.display()
    );
    let settings = Settings::default();

    let local = local_agent_page(&root, &markdown, &AgentReplies::default());
    let mut control = open(&local, &settings);
    let drawn = magenta_count(&shot(&mut control.page, 900, 900, 1.0));
    assert!(drawn > 0, "the sentinel draws on a local page");

    let imported = import_page(&receiver, &open_page(&markdown, json!(null))).unwrap();
    let mut remote = open(&imported.markdown, &settings);
    let image = shot(&mut remote.page, 900, 900, 1.0);
    assert_eq!(magenta_count(&image), 0, "no local file is drawn");
    let _ = fs::remove_dir_all(root);
}
