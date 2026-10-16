use crate::{
    codex::Turn,
    render::{self, RenderMode, Rendered, TocItem},
    spec::{self, ResolvedTurnSpec},
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AgentPage {
    pub html: String,
    pub context: String,
    pub title: String,
    pub content_hash: String,
}

fn first_line(source: &str) -> String {
    source
        .lines()
        .map(|line| line.trim_start_matches('#').trim())
        .map(|line| line.trim_matches(['*', '_', '`', '>', '#', ' ']))
        .find(|line| !line.is_empty())
        .unwrap_or("Response")
        .chars()
        .take(80)
        .collect()
}

pub fn build(
    turns: &[Turn],
    requested: &str,
    model: Option<&str>,
    headings: &[String],
) -> Result<AgentPage, String> {
    let kept: Vec<_> = turns
        .iter()
        .filter(|turn| !turn.response.trim().is_empty())
        .collect();
    let spec = spec::parse(requested)?;
    let ResolvedTurnSpec { kind, indices } = spec::resolve(&spec, kept.len())?;
    let newest = *indices.iter().min().ok_or("No response to read yet.")?;
    let oldest = *indices.iter().max().ok_or("No response to read yet.")?;
    let response = &kept[kept.len() - 1 - newest].response;
    let title = first_line(response);
    let context = if kind == "single" || indices.len() == 1 {
        format!("turn {newest}")
    } else {
        format!("turns {newest}–{oldest}")
    };
    let mut content = String::new();
    for index in &indices {
        let turn = kept[kept.len() - 1 - index];
        content.push_str(&format!("{index}\0{}\0{}", turn.prompt, turn.response));
    }
    let content_hash = crate::replies::source_hash(content.as_bytes());
    let rendered = if kind == "single" {
        let mut rendered = render::render_with_questions(response, RenderMode::Agent, headings);
        rendered.title.clone_from(&title);
        rendered
    } else {
        let mut used = HashMap::<String, usize>::new();
        let mut question_number = 0usize;
        let mut toc = Vec::<TocItem>::new();
        let mut body = String::new();
        for index in indices.iter().rev() {
            let turn = kept[kept.len() - 1 - index];
            let rendered = render::render_with_state(
                &turn.response,
                RenderMode::Agent,
                headings,
                &mut used,
                &mut question_number,
            );
            let turn_id = format!("turn-{index}");
            toc.push(TocItem {
                depth: 1,
                id: turn_id.clone(),
                text: format!("Turn {index}"),
            });
            toc.extend(rendered.toc.into_iter().map(|mut item| {
                item.depth = 2;
                item
            }));
            let tag = if *index == 0 {
                "<span class=\"turn-tag\">most recent</span>"
            } else {
                ""
            };
            let prompt = if turn.prompt.trim().is_empty() {
                "<div class=\"prompt-text placeholder\">(no prompt recorded)</div>".into()
            } else {
                format!(
                    "<div class=\"prompt-text\">{}</div>",
                    render::escape_html(&turn.prompt)
                )
            };
            body.push_str(&format!("<section class=\"turn\" id=\"{turn_id}\"><div class=\"turn-head\"><span class=\"turn-index\">Turn {index}</span>{tag}</div><div class=\"prompt\"><span class=\"role\">You</span>{prompt}</div><div class=\"response\">{}</div></section>", rendered.body));
        }
        Rendered {
            title: title.clone(),
            body,
            toc,
        }
    };
    let meta = [model.unwrap_or(""), &context]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    Ok(AgentPage {
        html: render::page_with_meta(&rendered, &meta),
        context,
        title,
        content_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_selected_turns_oldest_first_with_shared_heading_ids() {
        let turns = [
            Turn {
                prompt: "first".into(),
                response: "## Plan\n\none".into(),
            },
            Turn {
                prompt: "second".into(),
                response: "## Plan\n\ntwo".into(),
            },
        ];
        let page = build(&turns, "0..2", Some("model"), &["Questions".into()]).unwrap();
        assert_eq!(page.context, "turns 0–1");
        assert!(page.html.find("Turn 1").unwrap() < page.html.find("Turn 0").unwrap());
        assert!(page.html.contains("id=\"plan\""));
        assert!(page.html.contains("id=\"plan-2\""));
        assert!(page.html.contains("model · turns 0–1"));
    }

    #[test]
    fn content_hash_tracks_only_the_selected_turns() {
        let turn = |prompt: &str, response: &str| Turn {
            prompt: prompt.into(),
            response: response.into(),
        };
        let before = [turn("x", "oldest"), turn("a", "one"), turn("b", "two")];
        let after = [
            turn("x", "oldest"),
            turn("a", "one"),
            turn("b", "two"),
            turn("c", "three"),
        ];
        let first = build(&before, "0", None, &[]).unwrap();
        assert_eq!(
            first.content_hash,
            build(&before, "0", None, &[]).unwrap().content_hash
        );
        assert_ne!(
            first.content_hash,
            build(&after, "0", None, &[]).unwrap().content_hash
        );
        assert_ne!(
            first.content_hash,
            build(&before, "0..3", None, &[]).unwrap().content_hash
        );
    }

    #[test]
    fn meta_line_title_and_escaping_follow_the_page_contract() {
        let turn = |response: &str| Turn {
            prompt: "p".into(),
            response: response.into(),
        };
        let no_model = build(&[turn("# Hi")], "", None, &[]).unwrap();
        assert!(no_model.html.contains("<p class=\"meta\">turn 0</p>"));
        let escaped = build(&[turn("x")], "", Some("<b>m</b>"), &[]).unwrap();
        assert!(escaped.html.contains("&lt;b&gt;m&lt;/b&gt;"));
        assert!(!escaped.html.contains("<b>m</b>"));
        assert_eq!(
            build(&[turn("***")], "", None, &[]).unwrap().title,
            "Response"
        );
        let long = format!("# {}", "x".repeat(200));
        assert_eq!(
            build(&[turn(&long)], "", None, &[]).unwrap().title.len(),
            80
        );
    }
}
