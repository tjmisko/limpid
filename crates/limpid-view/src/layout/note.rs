//! The Markdown subset comment and reply bodies show (`renderCommentMarkdown`
//! in `page.js`): inline code, links, bold, italic, strikethrough, and
//! bulleted and numbered lists. Anything else stays literal.

use limpid_core::doc::Inline;

/// Deepest nesting of emphasis and links followed.
const MAX_DEPTH: usize = 8;

/// Whether `url` may be followed: no scheme, or a known one.
fn safe_link(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return true;
    };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    !valid
        || ["http", "https", "mailto", "file"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// The text after a bullet marker, if `line` is a bulleted item.
fn bullet(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix(['-', '*', '+'])?;
    item_rest(rest)
}

/// The text after a number marker, if `line` is a numbered item.
fn numbered(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digits = trimmed.len()
        - trimmed
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if digits == 0 {
        return None;
    }
    let rest = trimmed[digits..].strip_prefix(['.', ')'])?;
    item_rest(rest)
}

/// What follows a marker, which needs at least one space after it.
fn item_rest(rest: &str) -> Option<&str> {
    let body = rest.trim_start();
    (body.len() < rest.len()).then_some(body)
}

/// `text` as inlines: paragraphs and list items on lines of their own.
pub fn inlines(text: &str) -> Vec<Inline> {
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    let mut out: Vec<Inline> = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if lines[at].trim().is_empty() {
            at += 1;
            continue;
        }
        if !out.is_empty() {
            out.push(Inline::HardBreak);
        }
        let ordered = numbered(lines[at]).is_some();
        if ordered || bullet(lines[at]).is_some() {
            let pick = if ordered { numbered } else { bullet };
            let mut count = 0;
            while let Some(body) = lines.get(at).copied().and_then(pick) {
                if count > 0 {
                    out.push(Inline::HardBreak);
                }
                count += 1;
                let marker = if ordered {
                    format!("{count}. ")
                } else {
                    "\u{2022} ".to_owned()
                };
                out.push(Inline::Text(marker));
                out.extend(parse(body, 0));
                at += 1;
            }
            continue;
        }
        let mut first = true;
        while at < lines.len()
            && !lines[at].trim().is_empty()
            && numbered(lines[at]).is_none()
            && bullet(lines[at]).is_none()
        {
            if !first {
                out.push(Inline::HardBreak);
            }
            first = false;
            out.extend(parse(lines[at], 0));
            at += 1;
        }
    }
    out
}

/// The index of the first `needle` at or after `from`.
fn find(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(needle.len() - 1)).find(|&at| chars[at..].starts_with(needle))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Inline syntax in one line.
fn parse(line: &str, depth: usize) -> Vec<Inline> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<Inline> = Vec::new();
    let mut text = String::new();
    let mut at = 0;
    while at < chars.len() {
        let rest = at + 1;
        let matched = if depth >= MAX_DEPTH {
            None
        } else {
            match chars[at] {
                '`' => code(&chars, at),
                '[' => link(&chars, at, depth),
                '*' if chars.get(rest) == Some(&'*') => {
                    wrapped(&chars, at, &['*', '*'], depth, Inline::Strong)
                }
                '*' => wrapped(&chars, at, &['*'], depth, Inline::Emph),
                '~' if chars.get(rest) == Some(&'~') => {
                    wrapped(&chars, at, &['~', '~'], depth, Inline::Strike)
                }
                '_' => underscored(&chars, at, depth),
                _ => None,
            }
        };
        match matched {
            Some((inline, next)) => {
                if !text.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut text)));
                }
                out.push(inline);
                at = next;
            }
            None => {
                text.push(chars[at]);
                at += 1;
            }
        }
    }
    if !text.is_empty() {
        out.push(Inline::Text(text));
    }
    out
}

/// A code span at `at`: the inline and the index after it.
fn code(chars: &[char], at: usize) -> Option<(Inline, usize)> {
    let end = chars[at + 1..].iter().position(|&c| c == '`')? + at + 1;
    (end > at + 1).then(|| (Inline::Code(chars[at + 1..end].iter().collect()), end + 1))
}

/// `[label](url)` at `at`, unless the URL is not safe to follow.
fn link(chars: &[char], at: usize, depth: usize) -> Option<(Inline, usize)> {
    let close = chars[at + 1..].iter().position(|&c| c == ']')? + at + 1;
    if close == at + 1 || chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let url_start = close + 2;
    let url_end = chars[url_start..]
        .iter()
        .position(|&c| c == ')' || c.is_whitespace())?
        + url_start;
    if url_end == url_start || chars[url_end] != ')' {
        return None;
    }
    let href: String = chars[url_start..url_end].iter().collect();
    if !safe_link(&href) {
        return None;
    }
    let label: String = chars[at + 1..close].iter().collect();
    let inlines = parse(&label, depth + 1);
    Some((
        Inline::Link {
            href,
            title: None,
            inlines,
        },
        url_end + 1,
    ))
}

/// Text between `mark` and the next `mark`, which holds none of its first
/// character and is not empty.
fn wrapped(
    chars: &[char],
    at: usize,
    mark: &[char],
    depth: usize,
    make: fn(Vec<Inline>) -> Inline,
) -> Option<(Inline, usize)> {
    let start = at + mark.len();
    let end = find(chars, start, mark)?;
    let body = &chars[start..end];
    if body.is_empty() || body.contains(&mark[0]) {
        return None;
    }
    let body: String = body.iter().collect();
    Some((make(parse(&body, depth + 1)), end + mark.len()))
}

/// `_italic_`, only at word edges so `snake_case_name` stays literal.
fn underscored(chars: &[char], at: usize, depth: usize) -> Option<(Inline, usize)> {
    if at > 0 && is_word(chars[at - 1]) {
        return None;
    }
    let (inline, next) = wrapped(chars, at, &['_'], depth, Inline::Emph)?;
    chars
        .get(next)
        .is_none_or(|&c| !is_word(c))
        .then_some((inline, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Inline {
        Inline::Text(value.into())
    }

    #[test]
    fn should_keep_plain_text_literal_when_no_syntax_is_present() {
        assert_eq!(inlines("just words"), vec![text("just words")]);
    }

    #[test]
    fn should_make_inline_code_when_text_is_in_backticks() {
        assert_eq!(
            inlines("use `a*b*` here"),
            vec![text("use "), Inline::Code("a*b*".into()), text(" here")]
        );
    }

    #[test]
    fn should_make_bold_italic_and_strike_when_marked() {
        assert_eq!(
            inlines("**b** *i* _j_ ~~s~~"),
            vec![
                Inline::Strong(vec![text("b")]),
                text(" "),
                Inline::Emph(vec![text("i")]),
                text(" "),
                Inline::Emph(vec![text("j")]),
                text(" "),
                Inline::Strike(vec![text("s")]),
            ]
        );
    }

    #[test]
    fn should_leave_snake_case_literal_when_underscores_sit_inside_a_word() {
        assert_eq!(inlines("a_b_c"), vec![text("a_b_c")]);
    }

    #[test]
    fn should_make_a_link_when_the_url_is_safe() {
        let linked = inlines("see [docs](https://example.com/a) now");
        assert_eq!(
            linked[1],
            Inline::Link {
                href: "https://example.com/a".into(),
                title: None,
                inlines: vec![text("docs")],
            }
        );
    }

    #[test]
    fn should_leave_a_link_literal_when_the_scheme_is_not_allowed() {
        assert_eq!(
            inlines("[x](javascript:alert)"),
            vec![text("[x](javascript:alert)")]
        );
    }

    #[test]
    fn should_leave_unclosed_markers_literal_when_there_is_no_partner() {
        assert_eq!(inlines("**open and `tick"), vec![text("**open and `tick")]);
        assert_eq!(inlines("a ** b"), vec![text("a ** b")]);
    }

    #[test]
    fn should_number_items_from_one_when_lines_are_a_numbered_list() {
        let list = inlines("3) first\n7. second");
        assert_eq!(
            list,
            vec![
                text("1. "),
                text("first"),
                Inline::HardBreak,
                text("2. "),
                text("second"),
            ]
        );
    }

    #[test]
    fn should_bullet_items_when_lines_start_with_a_dash_star_or_plus() {
        let list = inlines("- a\n* b\n+ c");
        assert_eq!(list.iter().filter(|i| **i == text("\u{2022} ")).count(), 3);
    }

    #[test]
    fn should_not_treat_emphasis_as_a_bullet_when_no_space_follows_the_star() {
        assert_eq!(inlines("*word*"), vec![Inline::Emph(vec![text("word")])]);
    }

    #[test]
    fn should_break_lines_and_separate_blocks_when_text_has_paragraphs() {
        assert_eq!(
            inlines("one\ntwo\n\n- x"),
            vec![
                text("one"),
                Inline::HardBreak,
                text("two"),
                Inline::HardBreak,
                text("\u{2022} "),
                text("x"),
            ]
        );
    }

    #[test]
    fn should_return_nothing_when_the_text_is_blank() {
        assert!(inlines("").is_empty());
        assert!(inlines("\n \n").is_empty());
    }

    #[test]
    fn should_not_panic_when_syntax_nests_deeply_or_is_multibyte() {
        let deep = "*".repeat(200) + "x" + &"*".repeat(200);
        let _ = inlines(&deep);
        let _ = inlines("[é](é) `é` **é** _é_ ~~é~~");
        let _ = inlines("[](x) [a]( ) [a](b");
    }
}
