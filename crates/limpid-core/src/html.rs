//! HTML output for a [`Doc`].

use crate::doc::Doc;
use crate::render::{Rendered, TocItem, add_source_lines, render_raw_with_state};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitOptions {
    /// Add `data-line-start` / `data-line-end` next to each `data-src`.
    pub source_lines: bool,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self { source_lines: true }
    }
}

/// The page body for `doc`, byte-identical to what [`crate::render::render`]
/// produces for the same Markdown and mode.
///
/// The page is rendered from `doc.source` in the mode the document was parsed
/// with, highlighting code eagerly; `doc` itself is not read for blocks.
pub fn emit(doc: &Doc, options: &EmitOptions) -> String {
    let mut used = HashMap::new();
    let mut question_number = 0;
    let rendered = render_raw_with_state(
        &doc.source,
        doc.mode.into(),
        &doc.question_headings,
        &mut used,
        &mut question_number,
    );
    if options.source_lines {
        add_source_lines(&rendered.body, &doc.source)
    } else {
        rendered.body
    }
}

/// The same output packaged as a [`Rendered`], ready for `render::page`.
pub fn emit_rendered(doc: &Doc, options: &EmitOptions) -> Rendered {
    Rendered {
        title: doc.title.clone().unwrap_or_else(|| "Document".into()),
        body: emit(doc, options),
        toc: doc
            .toc
            .iter()
            .map(|entry| TocItem {
                depth: entry.level,
                id: entry.id.clone(),
                text: entry.text.clone(),
            })
            .collect(),
        sources: HashMap::from([(String::new(), doc.source.clone())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Mode, parse};
    use crate::render::{self, RenderMode};
    use crate::settings::Settings;

    const SAMPLES: &[&str] = &[
        "",
        "# Title\n\nPlain paragraph with *emphasis* and `code`.\n",
        "---\ntitle: Notes\n---\n## Same\n\n## Same\n\n<script>alert(1)</script>\n\n```rust\nfn main() {}\n```",
        "## Questions\n\n1. Choose?\n   - A\n   - B\n2. Why?\n\n## Other\n\n1. Not a question\n",
        "## Questions\n\n1. Pick?\n\n<!-- limpid reply q1\nA --&gt; B <script>bad</script>\n-->\n",
        "Body.\n\n<!-- limpid comment\nNote -->\n",
        "| a | b |\n|:-:|--:|\n| 1 | 2 |\n\n> [!NOTE]\n> callout\n\n- [x] done\n- [ ] todo\n",
        "Inline $x^2$ and $$\\frac{1}{2}$$, price $5 and ~$3.\n\n```mermaid\ngraph TD; A-->B\n```\n",
        "# \u{1F680}\r\n\r\n```unknown\r\n<script>alert(1)</script>",
        "Footnote[^a] and [link](javascript:alert(1)).\n\n[^a]: Note.\n",
    ];

    #[test]
    fn should_match_the_renderer_byte_for_byte_when_emitting_samples() {
        let settings = Settings::default();
        for sample in SAMPLES {
            for (mode, render_mode) in [
                (Mode::File, RenderMode::File),
                (Mode::Agent, RenderMode::Agent),
            ] {
                let doc = parse(sample, mode, &settings);
                let expected = render::render(sample, render_mode);
                assert_eq!(
                    emit(&doc, &EmitOptions::default()),
                    expected.body,
                    "{sample:?}"
                );
            }
        }
    }

    #[test]
    fn should_match_the_renderer_when_emitting_the_golden_source() {
        let source = include_str!("../tests/golden/basic.md");
        let doc = parse(source, Mode::File, &Settings::default());
        let expected = render::render(source, RenderMode::File);
        let rendered = emit_rendered(&doc, &EmitOptions::default());
        assert_eq!(rendered.body, expected.body);
        assert_eq!(rendered.title, expected.title);
        assert_eq!(rendered.toc, expected.toc);
        assert_eq!(render::page(&rendered), render::page(&expected));
    }

    #[test]
    fn should_omit_line_attributes_when_source_lines_are_off() {
        let doc = parse("# One\n\ntext\n", Mode::File, &Settings::default());
        let with = emit(&doc, &EmitOptions::default());
        let without = emit(
            &doc,
            &EmitOptions {
                source_lines: false,
            },
        );
        assert!(with.contains("data-line-start"));
        assert!(!without.contains("data-line-start"));
        assert!(without.contains("data-src="));
    }

    #[test]
    fn should_use_configured_question_headings_when_emitting_fields() {
        let mut settings = Settings::default();
        settings.questions.headings = vec!["Decisions needed".into()];
        let doc = parse(
            "# DECISIONS NEEDED\n\n1. Pick one?\n",
            Mode::Agent,
            &settings,
        );
        assert!(emit(&doc, &EmitOptions::default()).contains("id=\"q1\""));
    }

    #[test]
    fn should_fall_back_to_document_title_when_doc_has_none() {
        let doc = parse("just text", Mode::Agent, &Settings::default());
        assert_eq!(
            emit_rendered(&doc, &EmitOptions::default()).title,
            "Document"
        );
    }
}
