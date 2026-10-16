use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use std::collections::HashMap;
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocItem {
    pub depth: u8,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub title: String,
    pub body: String,
    pub toc: Vec<TocItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Agent,
    File,
}

pub fn escape_html(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            _ => result.push(ch),
        }
    }
    result
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            dash = true;
        }
    }
    if out.is_empty() {
        "section".into()
    } else {
        out.chars().take(60).collect()
    }
}

fn heading_text(events: &[(Event<'_>, std::ops::Range<usize>)], from: usize) -> String {
    let mut text = String::new();
    for (event, _) in events.iter().skip(from + 1) {
        match event {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Text(value) | Event::Code(value) => text.push_str(value),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text
}

fn mathml(source: &str, display: bool) -> String {
    let storage = pulldown_latex::Storage::new();
    let parser = pulldown_latex::Parser::new(source, &storage);
    let config = pulldown_latex::RenderConfig {
        display_mode: if display {
            pulldown_latex::config::DisplayMode::Block
        } else {
            pulldown_latex::config::DisplayMode::Inline
        },
        ..Default::default()
    };
    let mut result = String::new();
    if pulldown_latex::push_mathml(&mut result, parser, config).is_ok() {
        result
    } else {
        format!("<code>{}</code>", escape_html(source))
    }
}

fn cached_mermaid(source: &str) -> Option<String> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    let hash = crate::replies::source_hash(source.as_bytes());
    let svg = fs::read_to_string(root.join("limpid/mermaid").join(format!("{hash}.svg"))).ok()?;
    svg.trim_start().starts_with("<svg").then_some(svg)
}

fn saved_annotation(raw: &str, start: usize, end: usize) -> Option<String> {
    let value = raw.trim();
    if let Some(rest) = value.strip_prefix("<!-- limpid reply ") {
        let (field, body) = rest.split_once('\n')?;
        if !crate::replies::valid_question_id(field) {
            return None;
        }
        let body = annotation_body(body)?;
        return Some(format!(
            "<aside class=\"limpid-annotation limpid-reply\" data-src=\"{start}-{end}\"><strong>Reply {}</strong><p>{}</p></aside>",
            escape_html(field),
            escape_html(&body)
        ));
    }
    let body = annotation_body(value.strip_prefix("<!-- limpid comment\n")?)?;
    Some(format!(
        "<aside class=\"limpid-annotation limpid-comment\" data-src=\"{start}-{end}\"><p>{}</p></aside>",
        escape_html(&body)
    ))
}

fn annotation_body(body: &str) -> Option<String> {
    Some(
        body.trim_end()
            .strip_suffix("-->")?
            .trim_end()
            .replace("--&gt;", "-->"),
    )
}

fn safe_url(url: &str) -> bool {
    let prefix = url.split(['/', '#', '?']).next().unwrap_or("");
    let Some((scheme, _)) = prefix.split_once(':') else {
        return true;
    };
    let scheme: String = scheme
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && !ch.is_ascii_control())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(scheme.as_str(), "http" | "https" | "mailto" | "file")
}

pub fn render_markdown(source: &str) -> Rendered {
    render(source, RenderMode::Agent)
}

pub fn render(source: &str, mode: RenderMode) -> Rendered {
    render_with_questions(
        source,
        mode,
        &[
            "Questions".into(),
            "Open questions".into(),
            "Questions for you".into(),
        ],
    )
}

pub fn render_with_questions(
    source: &str,
    mode: RenderMode,
    question_headings: &[String],
) -> Rendered {
    let mut used = HashMap::<String, usize>::new();
    let mut question_number = 0usize;
    render_with_state(
        source,
        mode,
        question_headings,
        &mut used,
        &mut question_number,
    )
}

fn add_source_lines(body: &str, source: &str) -> String {
    let breaks = source
        .match_indices('\n')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let mut result = String::with_capacity(body.len());
    let mut copied = 0;
    for (position, _) in body.match_indices("data-src=\"") {
        let start = position + "data-src=\"".len();
        let Some(quote) = body[start..].find('"') else {
            continue;
        };
        let end = start + quote;
        result.push_str(&body[copied..=end]);
        if let Some((first, last)) = body[start..end].split_once('-')
            && let (Ok(first), Ok(last)) = (first.parse::<usize>(), last.parse::<usize>())
        {
            let start_line = breaks.partition_point(|byte| *byte < first) + 1;
            let end_line = breaks.partition_point(|byte| *byte < last.saturating_sub(1)) + 1;
            result.push_str(&format!(
                " data-line-start=\"{start_line}\" data-line-end=\"{end_line}\""
            ));
        }
        copied = end + 1;
    }
    result.push_str(&body[copied..]);
    result
}

pub(crate) fn render_with_state(
    source: &str,
    mode: RenderMode,
    question_headings: &[String],
    used: &mut HashMap<String, usize>,
    question_number: &mut usize,
) -> Rendered {
    let saved_replies = if mode == RenderMode::File {
        crate::replies::from_file_annotations(source).replies
    } else {
        Default::default()
    };
    let mut highlighter = crate::highlight::PageHighlighter::new();
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let events: Vec<_> = Parser::new_ext(source, options)
        .into_offset_iter()
        .collect();
    let mut output = Vec::with_capacity(events.len());
    let mut toc = Vec::new();
    let mut title = String::new();
    let mut in_metadata = false;
    let mut html_block: Option<String> = None;
    let mut questions_level: Option<u8> = None;
    let mut list_depth = 0usize;
    let mut question_list_depth: Option<usize> = None;
    let mut active_field_depth: Option<usize> = None;
    let mut option_number = 0usize;
    let mut list_ordered = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let (event, range) = &events[i];
        match event {
            Event::Start(Tag::MetadataBlock(_)) => in_metadata = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_metadata = false,
            Event::Start(Tag::HtmlBlock) => html_block = Some(String::new()),
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(raw) = html_block.take() {
                    match mode {
                        RenderMode::Agent => output.push(Event::Text(raw.into())),
                        RenderMode::File => output.push(Event::Html(
                            saved_annotation(&raw, range.start, range.end)
                                .unwrap_or_else(|| ammonia::clean(&raw))
                                .into(),
                        )),
                    }
                }
            }
            Event::Html(value) if html_block.is_some() => {
                html_block.as_mut().unwrap().push_str(value)
            }
            Event::Text(value) if in_metadata => {
                for line in value.lines() {
                    if let Some(value) = line.strip_prefix("title:") {
                        title = value.trim().trim_matches(['\'', '"']).to_owned();
                    }
                }
            }
            _ if in_metadata => {}
            Event::Start(Tag::Heading { level, .. }) => {
                let depth = *level as u8;
                let text = heading_text(&events, i);
                if questions_level.is_some_and(|level| depth <= level) {
                    questions_level = None;
                }
                if question_headings
                    .iter()
                    .any(|heading| heading.eq_ignore_ascii_case(text.trim()))
                {
                    questions_level = Some(depth);
                }
                let base = slug(&text);
                let mut id = base.clone();
                let mut suffix = 2;
                while used.contains_key(&id) {
                    id = format!("{base}-{suffix}");
                    suffix += 1;
                }
                used.insert(id.clone(), 1);
                if title.is_empty() {
                    title = text.clone();
                }
                if depth <= 3 {
                    toc.push(TocItem {
                        depth,
                        id: id.clone(),
                        text,
                    });
                }
                output.push(Event::Html(format!("<h{depth} id=\"{}\" data-src=\"{}-{}\"><a class=\"anchor\" href=\"#{}\" aria-hidden=\"true\">#</a>", escape_html(&id), range.start, range.end, escape_html(&id)).into()));
            }
            Event::End(TagEnd::Heading(level)) => {
                output.push(Event::Html(format!("</h{}>", *level as u8).into()))
            }
            Event::Start(Tag::Paragraph) => output.push(Event::Html(
                format!("<p data-src=\"{}-{}\">", range.start, range.end).into(),
            )),
            Event::End(TagEnd::Paragraph) => output.push(Event::Html("</p>\n".into())),
            Event::Start(Tag::BlockQuote(_)) => output.push(Event::Html(
                format!("<blockquote data-src=\"{}-{}\">", range.start, range.end).into(),
            )),
            Event::End(TagEnd::BlockQuote(_)) => output.push(Event::Html("</blockquote>\n".into())),
            Event::Start(Tag::List(number)) => {
                list_depth += 1;
                if questions_level.is_some() && list_depth == 1 && number.is_some() {
                    question_list_depth = Some(list_depth);
                }
                list_ordered.push(number.is_some());
                let tag = match number {
                    Some(1) => "<ol>".to_owned(),
                    Some(number) => format!("<ol start=\"{number}\">"),
                    None => "<ul>".to_owned(),
                };
                let tag = tag.replacen(
                    '>',
                    &format!(" data-src=\"{}-{}\">", range.start, range.end),
                    1,
                );
                output.push(Event::Html(tag.into()));
            }
            Event::End(TagEnd::List(ordered)) => {
                output.push(Event::Html(
                    if *ordered { "</ol>\n" } else { "</ul>\n" }.into(),
                ));
                if question_list_depth == Some(list_depth) {
                    question_list_depth = None;
                }
                list_ordered.pop();
                list_depth -= 1;
            }
            Event::Start(Tag::Item) => {
                let mut attributes = String::new();
                if question_list_depth == Some(list_depth) {
                    *question_number += 1;
                    option_number = 0;
                    active_field_depth = Some(list_depth);
                    attributes = format!(
                        " id=\"q{question_number}\" class=\"question-field\" data-field=\"q{question_number}\" tabindex=\"-1\""
                    );
                } else if active_field_depth.is_some_and(|depth| list_depth == depth + 1)
                    && list_ordered.last() == Some(&false)
                {
                    option_number += 1;
                    attributes =
                        format!(" class=\"question-option\" data-option=\"{option_number}\"");
                }
                output.push(Event::Html(
                    format!(
                        "<li{attributes} data-src=\"{}-{}\">",
                        range.start, range.end
                    )
                    .into(),
                ));
            }
            Event::End(TagEnd::Item) => {
                if active_field_depth == Some(list_depth) {
                    output.push(Event::Html(
                        format!(
                            "<div class=\"question-reply\" aria-label=\"Reply\">{}</div>",
                            saved_replies
                                .get(&format!("q{question_number}"))
                                .map(|reply| escape_html(reply))
                                .unwrap_or_default()
                        )
                        .into(),
                    ));
                    active_field_depth = None;
                }
                output.push(Event::Html("</li>\n".into()));
            }
            Event::Start(Tag::Table(_)) => {
                output.push(Event::Html(
                    format!(
                        "<div class=\"table-wrap\" data-src=\"{}-{}\">",
                        range.start, range.end
                    )
                    .into(),
                ));
                output.push(event.clone());
            }
            Event::End(TagEnd::Table) => {
                output.push(event.clone());
                output.push(Event::Html("</div>".into()));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => {
                        lang.split_whitespace().next().unwrap_or("")
                    }
                    _ => "",
                };
                let safe_lang: String = lang
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric() || "_+#.-".contains(*ch))
                    .collect();
                let caption = if safe_lang.is_empty() {
                    String::new()
                } else {
                    format!("<figcaption>{}</figcaption>", escape_html(&safe_lang))
                };
                let mut source_code = String::new();
                let mut end = range.end;
                let mut next = i + 1;
                while next < events.len() {
                    match &events[next].0 {
                        Event::End(TagEnd::CodeBlock) => {
                            end = events[next].1.end;
                            break;
                        }
                        Event::Text(text) | Event::Code(text) => source_code.push_str(text),
                        Event::SoftBreak | Event::HardBreak => source_code.push('\n'),
                        _ => {}
                    }
                    next += 1;
                }
                if safe_lang == "mermaid" {
                    let hash = crate::replies::source_hash(source_code.as_bytes());
                    let diagram = if let Some(svg) = cached_mermaid(&source_code) {
                        format!("<div class=\"mermaid-cached\">{svg}</div>")
                    } else {
                        format!(
                            "<pre class=\"mermaid\" data-hash=\"{hash}\">{}</pre>",
                            escape_html(&source_code)
                        )
                    };
                    output.push(Event::Html(
                        format!(
                            "<figure class=\"diagram\" data-src=\"{}-{end}\">{diagram}</figure>\n",
                            range.start
                        )
                        .into(),
                    ));
                } else {
                    let highlighted = highlighter.render(&safe_lang, &source_code);
                    output.push(Event::Html(format!(
                        "<figure class=\"code\" data-src=\"{}-{end}\">{caption}<pre><code>{highlighted}</code></pre></figure>\n",
                        range.start
                    ).into()));
                }
                i = next;
            }
            Event::Rule => output.push(Event::Html(
                format!("<hr data-src=\"{}-{}\">\n", range.start, range.end).into(),
            )),
            Event::InlineMath(source) => output.push(Event::Html(mathml(source, false).into())),
            Event::DisplayMath(source) => output.push(Event::Html(mathml(source, true).into())),
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                let mut sanitized = event.clone();
                match &mut sanitized {
                    Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. })
                        if !safe_url(dest_url) =>
                    {
                        *dest_url = "#".into()
                    }
                    _ => {}
                }
                output.push(sanitized);
            }
            Event::Html(value) | Event::InlineHtml(value) => match mode {
                RenderMode::Agent => output.push(Event::Text(value.clone())),
                RenderMode::File => {
                    let html = saved_annotation(value, range.start, range.end)
                        .unwrap_or_else(|| ammonia::clean(value));
                    output.push(Event::Html(html.into()));
                }
            },
            _ => output.push(event.clone()),
        }
        i += 1;
    }
    let mut body = String::new();
    html::push_html(&mut body, output.into_iter());
    if title.is_empty() {
        title = "Document".into();
    }
    Rendered {
        title,
        body: add_source_lines(&body, source),
        toc,
    }
}

pub fn page(rendered: &Rendered) -> String {
    page_with_meta(rendered, "")
}

pub fn page_with_meta(rendered: &Rendered, meta: &str) -> String {
    let items = rendered.toc.iter().enumerate().map(|(index, item)| {
        format!("<li class=\"depth-{}\"><a href=\"#{}\"><span class=\"num\">{:02}</span>{}</a></li>", item.depth, escape_html(&item.id), index + 1, escape_html(&item.text))
    }).collect::<String>();
    let rail = if items.is_empty() {
        String::new()
    } else {
        format!(
            "<aside class=\"rail\"><nav aria-label=\"Contents\"><h2>Contents</h2><ul class=\"toc\">{items}</ul></nav></aside>"
        )
    };
    let compact = if items.is_empty() {
        String::new()
    } else {
        format!(
            "<details class=\"toc-compact\"><summary>Contents · {}</summary><ul class=\"toc\">{items}</ul></details>",
            rendered.toc.len()
        )
    };
    let shell = if items.is_empty() {
        "shell"
    } else {
        "shell has-rail"
    };
    let meta = if meta.is_empty() {
        String::new()
    } else {
        format!("<p class=\"meta\">{}</p>", escape_html(meta))
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}</style></head><body><div class=\"{shell}\">{rail}<main class=\"content\"><article class=\"page\">{meta}{compact}{}</article></main></div><script>{}</script></body></html>",
        escape_html(&rendered.title),
        include_str!("page.css"),
        rendered.body,
        include_str!("page.js")
    )
}

pub fn externalize_assets(html: &str, cache: &Path) -> std::io::Result<String> {
    let assets = cache.join("assets");
    fs::create_dir_all(&assets)?;
    let editor = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/editor.min.js"
    ));
    let editor_hash = blake3::hash(editor.as_bytes()).to_hex();
    let editor_name = format!("{editor_hash}.js");
    if !assets.join(&editor_name).exists() {
        fs::write(assets.join(&editor_name), editor)?;
    }
    let mermaid_name = if html.contains("<pre class=\"mermaid\"") {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/mermaid.min.js"
        ));
        let name = format!("{}.js", blake3::hash(source.as_bytes()).to_hex());
        if !assets.join(&name).exists() {
            fs::write(assets.join(&name), source)?;
        }
        Some(name)
    } else {
        None
    };
    let mut page = html.to_owned();
    for (name, source, inline, reference) in [
        (
            "css",
            include_str!("page.css"),
            "<style>{}</style>",
            "<link rel=\"stylesheet\" href=\"assets/{}.css\">",
        ),
        (
            "js",
            include_str!("page.js"),
            "<script>{}</script>",
            "<script src=\"assets/{}.js\"></script>",
        ),
    ] {
        let hash = blake3::hash(source.as_bytes()).to_hex();
        let filename = format!("{hash}.{name}");
        let destination = assets.join(&filename);
        if !destination.exists() {
            fs::write(&destination, source)?;
        }
        let from = inline.replace("{}", source);
        let mut to = reference.replace("{}", hash.as_ref());
        if name == "js" {
            to = to.replacen("><", &format!(" data-editor=\"assets/{editor_name}\"><"), 1);
            if let Some(mermaid_name) = &mermaid_name {
                to = to.replacen(
                    "><",
                    &format!(" data-mermaid=\"assets/{mermaid_name}\"><"),
                    1,
                );
            }
        }
        page = page.replacen(&from, &to, 1);
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_code_and_html() {
        let result = render_markdown(
            "---\ntitle: Notes\n---\n## Same\n\n## Same\n\n<script>alert(1)</script>\n\n```rust\nfn main() {}\n```",
        );
        assert_eq!(result.title, "Notes");
        assert_eq!(result.toc[0].id, "same");
        assert_eq!(result.toc[1].id, "same-2");
        assert!(result.body.contains("&lt;script&gt;"));
        assert!(result.body.contains("<figure class=\"code\""));
        assert!(result.body.contains("data-line-start=\"4\""));
    }

    #[test]
    fn cached_pages_reference_shared_assets_and_exported_pages_remain_inline() {
        let page = page(&render_markdown("# Assets"));
        let cache = std::env::temp_dir().join(format!("limpid-assets-{}", std::process::id()));
        let cached = externalize_assets(&page, &cache).unwrap();
        assert!(page.contains("<style>"));
        assert!(page.contains("<script>"));
        assert!(!cached.contains("<style>"));
        assert!(cached.contains("data-editor=\"assets/"));
        assert!(cached.contains("rel=\"stylesheet\" href=\"assets/"));
        assert_eq!(std::fs::read_dir(cache.join("assets")).unwrap().count(), 3);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn mermaid_starts_as_readable_source_and_loads_only_on_diagram_pages() {
        let page = page(&render_markdown("```mermaid\ngraph TD; A-->B\n```"));
        assert!(page.contains("<pre class=\"mermaid\""));
        assert!(page.contains("graph TD; A--&gt;B"));
        let cache =
            std::env::temp_dir().join(format!("limpid-mermaid-assets-{}", std::process::id()));
        let cached = externalize_assets(&page, &cache).unwrap();
        assert!(cached.contains("data-mermaid=\"assets/"));
        assert_eq!(std::fs::read_dir(cache.join("assets")).unwrap().count(), 4);
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn duplicate_and_explicit_suffix_headings_remain_unique() {
        let result = render_markdown("## Same\n\n## Same\n\n## Same-2");
        assert_eq!(
            result
                .toc
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["same", "same-2", "same-2-2"]
        );
    }

    #[test]
    fn very_long_heading_has_bounded_slug() {
        let title = "A".repeat(400);
        let result = render_markdown(&format!("# {title}"));
        assert_eq!(result.toc[0].id.len(), 60);
        assert_eq!(result.title, title);
    }

    #[test]
    fn sanitizes_local_html() {
        let result = render(
            "<script>alert(1)</script>\n\n<img src=x onerror=alert(2)>",
            RenderMode::File,
        );
        assert!(!result.body.contains("<script>"));
        assert!(!result.body.contains("onerror"));
    }

    #[test]
    fn question_fields_follow_numbered_items_across_sections() {
        let source = "## Questions\n\n1. Choose?\n   - A\n   - B\n2. Why?\n\n## Other\n\n1. Not a question\n\n### Questions\n\n- Bullet ignored\n\n1. Last?";
        let result = render_markdown(source);
        assert_eq!(result.body.matches("class=\"question-field\"").count(), 3);
        assert!(result.body.contains("id=\"q1\""));
        assert!(result.body.contains("id=\"q2\""));
        assert!(result.body.contains("id=\"q3\""));
        assert_eq!(result.body.matches("class=\"question-option\"").count(), 2);
    }

    #[test]
    fn empty_questions_and_nested_numbering_have_no_extra_fields() {
        let result =
            render_markdown("## Questions\n\nNo list.\n\n1. Main?\n   1. Nested is not a field");
        assert_eq!(result.body.matches("class=\"question-field\"").count(), 1);
        assert_eq!(result.body.matches("class=\"question-option\"").count(), 0);
    }

    #[test]
    fn custom_question_headings_are_case_insensitive() {
        let headings = vec!["Decisions needed".into()];
        let result = render_with_questions(
            "# DECISIONS NEEDED\n\n1. Pick one?",
            RenderMode::Agent,
            &headings,
        );
        assert!(result.body.contains("id=\"q1\""));
        let result =
            render_with_questions("# Questions\n\n1. Ignored?", RenderMode::Agent, &headings);
        assert!(!result.body.contains("class=\"question-field\""));
    }

    #[test]
    fn renders_file_comments_without_a_label() {
        let markdown = "Body.\n\n<!-- limpid comment\nNote -->\n";
        let result = render(markdown, RenderMode::File);
        assert!(
            result.body.contains("class=\"limpid-annotation limpid-comment\""),
            "{}",
            result.body
        );
        assert!(result.body.contains("<p>Note</p>"));
        assert!(!result.body.contains("Comment"));
    }

    #[test]
    fn renders_mathml_without_page_javascript() {
        let result = render_markdown("Inline $x^2$ and $$\\frac{1}{2}$$.");
        assert!(result.body.contains("<math"));
        assert!(result.body.contains("<msup>"));
        assert!(result.body.contains("<mfrac>"));
    }

    #[test]
    fn shows_saved_file_annotations_and_keeps_their_text_escaped() {
        let markdown = "## Questions\n\n1. Pick?\n\n<!-- limpid reply q1\nA --&gt; B <script>bad</script>\n-->\n";
        let result = render(markdown, RenderMode::File);
        assert!(
            result.body.contains("class=\"limpid-annotation limpid-reply\""),
            "{}",
            result.body
        );
        assert!(
            result
                .body
                .contains("A --&gt; B &lt;script&gt;bad&lt;/script&gt;")
        );
        assert!(!result.body.contains("<script>bad</script>"));
        assert!(result.body.contains("class=\"question-reply\" aria-label=\"Reply\">A --&gt; B &lt;script&gt;bad&lt;/script&gt;</div>"));
    }

    #[test]
    fn markdown_links_cannot_execute_javascript() {
        let result = render_markdown(
            "[bad](javascript:alert(1)) [also bad](data:text/html,hi) [good](https://example.org)",
        );
        assert!(!result.body.contains("javascript:"));
        assert!(!result.body.contains("data:text"));
        assert!(result.body.contains("href=\"#\""));
        assert!(result.body.contains("href=\"https://example.org\""));
    }

    #[test]
    fn malformed_and_large_markdown_stays_readable() {
        let unclosed = render_markdown("# 🚀\r\n\r\n```unknown\r\n<script>alert(1)</script>");
        assert_eq!(unclosed.toc[0].id, "section");
        assert!(
            unclosed
                .body
                .contains("&lt;script&gt;alert(1)&lt;/script&gt;")
        );
        assert!(unclosed.body.contains("data-src=\"0-"));
        assert!(render_markdown("Just a paragraph").toc.is_empty());

        let large = format!("# Big\n\n{}", "x".repeat(10 * 1024 * 1024));
        let result = render_markdown(&large);
        assert!(result.body.len() > 10 * 1024 * 1024);
    }
}
