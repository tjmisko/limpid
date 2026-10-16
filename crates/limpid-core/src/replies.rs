use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentReplies {
    pub replies: BTreeMap<String, String>,
    pub comments: BTreeMap<String, Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub text: String,
    pub quote: String,
    pub start_line: usize,
    pub end_line: usize,
}

pub fn compose_message(title: &str, agent: &str, context: &str, replies: &AgentReplies) -> String {
    let mut lines = vec![format!("re: {title} ({agent}, {context})")];
    let mut answers = replies.replies.iter().collect::<Vec<_>>();
    answers.sort_by_key(|(field, _)| {
        field
            .strip_prefix('q')
            .and_then(|number| number.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    for (field, text) in answers {
        if !text.trim().is_empty() {
            lines.push(format!("{field}: {}", text.trim()));
        }
    }
    let mut comments = replies.comments.iter().collect::<Vec<_>>();
    comments.sort_by_key(|(key, _)| comment_position(key));
    for (key, comment) in comments {
        if comment.text.trim().is_empty() {
            continue;
        }
        let quoted = truncate_words(&comment.quote, 100).replace('"', "\\\"");
        let text = comment.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let location = match comment_turn(key) {
            Some(turn) => format!("{turn}, L{}-{}", comment.start_line, comment.end_line),
            None => format!("L{}-{}", comment.start_line, comment.end_line),
        };
        lines.push(format!(
            "\"{quoted}\" ({location}): user commented: \"{}\"",
            text.replace('"', "\\\"")
        ));
    }
    lines.join("\n")
}

fn comment_turn(key: &str) -> Option<String> {
    let (turn, _) = key.split_once(':')?;
    let number = turn.strip_prefix("turn-")?;
    Some(format!("turn {number}"))
}

fn truncate_words(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut result = String::new();
    for word in text.split_whitespace() {
        let next = if result.is_empty() {
            word.chars().count()
        } else {
            result.chars().count() + 1 + word.chars().count()
        };
        if next > limit {
            break;
        }
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
    }
    if result.is_empty() {
        return text.chars().take(limit).collect::<String>() + "…";
    }
    result.push('…');
    result
}

fn comment_position(key: &str) -> (usize, usize) {
    let (turn_order, range) = if let Some((turn, range)) = key.split_once(':') {
        let turn_order = turn
            .strip_prefix("turn-")
            .and_then(|number| number.parse::<usize>().ok())
            .map(|number| usize::MAX - number)
            .unwrap_or(usize::MAX);
        (turn_order, range)
    } else {
        (0, key)
    };
    let start = range
        .split_once('-')
        .and_then(|(start, _)| start.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    (turn_order, start)
}

enum AnnotationKind {
    Reply { field: String, text: String },
    Comment { text: String },
}

struct AnnotationSpan {
    start: usize,
    end: usize,
    kind: AnnotationKind,
}

fn parse_annotation(raw: &str) -> Option<AnnotationKind> {
    let raw = raw.trim();
    let (field, body) = if let Some(rest) = raw.strip_prefix("<!-- limpid reply ") {
        let (field, body) = rest.split_once('\n')?;
        if !valid_question_id(field) {
            return None;
        }
        (Some(field.to_owned()), body)
    } else {
        (None, raw.strip_prefix("<!-- limpid comment\n")?)
    };
    let text = body
        .trim_end()
        .strip_suffix("-->")?
        .trim_end()
        .replace("--&gt;", "-->");
    Some(match field {
        Some(field) => AnnotationKind::Reply { field, text },
        None => AnnotationKind::Comment { text },
    })
}

fn annotation_spans(source: &str) -> Vec<AnnotationSpan> {
    if !source.contains("<!-- limpid ") {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut block_start = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::HtmlBlock) => block_start = Some(range.start),
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(start) = block_start.take()
                    && let Some(kind) = parse_annotation(&source[start..range.end])
                {
                    spans.push(AnnotationSpan {
                        start,
                        end: range.end,
                        kind,
                    });
                }
            }
            _ => {}
        }
    }
    spans
}

pub fn from_file_annotations(source: &str) -> AgentReplies {
    let mut saved = AgentReplies::default();
    let mut cursor = 0;
    let mut line_number = 1;
    let mut preceding_content: Option<(usize, String)> = None;
    for span in annotation_spans(source) {
        for line in source[cursor..span.start].split_inclusive('\n') {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                preceding_content = Some((line_number, trimmed.to_owned()));
            }
            line_number += line.bytes().filter(|byte| *byte == b'\n').count();
        }
        let start_line = line_number;
        line_number += source[span.start..span.end]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        cursor = span.end;
        match span.kind {
            AnnotationKind::Reply { field, text } => {
                saved.replies.insert(field, text);
            }
            AnnotationKind::Comment { text } => {
                let (line, quote) = preceding_content.clone().unwrap_or_default();
                saved.comments.insert(
                    format!("{start_line}-{line_number}"),
                    Comment {
                        text,
                        quote,
                        start_line: line,
                        end_line: line,
                    },
                );
            }
        }
    }
    saved
}

pub fn deliver_reply(
    command_line: &str,
    message: &str,
    agent: &str,
    session: Option<&str>,
    source: Option<&Path>,
    pane: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(command_line)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env_remove("LIMPID_AGENT")
        .env_remove("LIMPID_SESSION")
        .env_remove("LIMPID_SOURCE")
        .env_remove("LIMPID_PANE")
        .env("LIMPID_AGENT", agent);
    if let Some(value) = session {
        command.env("LIMPID_SESSION", value);
    }
    if let Some(value) = source {
        command.env("LIMPID_SOURCE", value);
    }
    if let Some(value) = pane {
        command.env("LIMPID_PANE", value);
    }
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or("reply command has no stdin")?
        .write_all(message.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "reply command exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

impl AgentReplies {
    pub fn has_feedback(&self) -> bool {
        self.replies.values().any(|text| !text.trim().is_empty())
            || self
                .comments
                .values()
                .any(|comment| !comment.text.trim().is_empty())
    }

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        atomic_write(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

pub const ANNOTATIONS_ID: &str = "limpid-annotations";

/// The page block that carries replies and comments inside the artifact.
pub fn annotations_block(replies: &AgentReplies) -> String {
    let json = serde_json::to_string(replies)
        .unwrap_or_else(|_| "{}".into())
        .replace('<', "\\u003c");
    format!("<script type=\"application/json\" id=\"{ANNOTATIONS_ID}\">{json}</script>")
}

/// Read the embedded annotations, or an empty set when the page has none.
pub fn read_annotations(html: &str) -> AgentReplies {
    let marker = format!("id=\"{ANNOTATIONS_ID}\"");
    let Some(open) = html.find(&marker) else {
        return AgentReplies::default();
    };
    let Some(head) = html[open..].find('>') else {
        return AgentReplies::default();
    };
    let start = open + head + 1;
    let Some(tail) = html[start..].find("</script>") else {
        return AgentReplies::default();
    };
    serde_json::from_str(html[start..start + tail].trim()).unwrap_or_default()
}

/// Replace the embedded annotations block, appending one when absent.
pub fn write_annotations(html: &str, replies: &AgentReplies) -> String {
    let block = annotations_block(replies);
    let marker = format!("id=\"{ANNOTATIONS_ID}\"");
    if let Some(open) = html.find(&marker)
        && let Some(tag) = html[..open].rfind("<script")
        && let Some(close) = html[open..].find("</script>")
    {
        let end = open + close + "</script>".len();
        return format!("{}{}{}", &html[..tag], block, &html[end..]);
    }
    if let Some(body) = html.rfind("</body>") {
        return format!("{}{}{}", &html[..body], block, &html[body..]);
    }
    format!("{html}{block}")
}

pub fn read_annotations_file(path: &Path) -> Result<AgentReplies, Box<dyn std::error::Error>> {
    match fs::read_to_string(path) {
        Ok(html) => Ok(read_annotations(&html)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AgentReplies::default()),
        Err(error) => Err(Box::new(error)),
    }
}

pub fn write_annotations_file(
    path: &Path,
    replies: &AgentReplies,
) -> Result<(), Box<dyn std::error::Error>> {
    let html = fs::read_to_string(path)?;
    atomic_write(path, write_annotations(&html, replies).as_bytes())?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FileRevision {
    pub path: PathBuf,
    pub source: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAnnotation<'a> {
    Reply { field: &'a str, text: &'a str },
    Comment { text: &'a str },
}

#[derive(Debug, Clone)]
pub enum FileEdit {
    Reply { field: String, text: String },
    Comment { byte_end: usize, text: String },
    CommentReplace { start: usize, end: usize, text: String },
    CommentDelete { start: usize, end: usize },
}

impl FileRevision {
    pub fn open(path: &Path) -> Result<Self, std::io::Error> {
        let path = fs::canonicalize(path)?;
        Ok(Self {
            source: fs::read(&path)?,
            path,
        })
    }

    pub fn open_checked(
        path: &Path,
        expected_hash: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let revision = Self::open(path)?;
        if source_hash(&revision.source) != expected_hash {
            return Err("source changed since it was rendered".into());
        }
        Ok(revision)
    }

    pub fn apply(&self, edits: &[FileEdit]) -> Result<(), Box<dyn std::error::Error>> {
        if fs::read(&self.path)? != self.source {
            return Err("source changed since it was rendered".into());
        }
        let source = std::str::from_utf8(&self.source)?;
        let reply_spans = annotation_spans(source)
            .into_iter()
            .filter_map(|AnnotationSpan { start, end, kind }| match kind {
                AnnotationKind::Reply { field, .. } => Some((field, (start, end))),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        enum Op {
            Insert { at: usize, text: String },
            Replace { start: usize, end: usize, text: String },
        }
        let mut operations = Vec::new();
        for edit in edits {
            match edit {
                FileEdit::Reply { field, text } => {
                    if !valid_question_id(field) {
                        return Err("invalid question id".into());
                    }
                    if let Some((start, end)) = reply_spans.get(field) {
                        let suffix = if source[*start..*end].ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        };
                        operations.push(Op::Replace {
                            start: *start,
                            end: *end,
                            text: format!(
                                "<!-- limpid reply {field}\n{}\n-->{suffix}",
                                escape_comment(text)
                            ),
                        });
                    } else {
                        operations.push(Op::Insert {
                            at: self.source.len(),
                            text: format!(
                                "\n\n<!-- limpid reply {field}\n{}\n-->\n",
                                escape_comment(text)
                            ),
                        });
                    }
                }
                FileEdit::Comment { byte_end, text } => {
                    if *byte_end > self.source.len() || !source.is_char_boundary(*byte_end) {
                        return Err("annotation source range is invalid".into());
                    }
                    operations.push(Op::Insert {
                        at: *byte_end,
                        text: format!("\n\n<!-- limpid comment\n{}\n-->\n", escape_comment(text)),
                    });
                }
                FileEdit::CommentReplace { start, end, text } => {
                    check_annotation_range(source, *start, *end)?;
                    let suffix = if source[*start..*end].ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    };
                    operations.push(Op::Replace {
                        start: *start,
                        end: *end,
                        text: format!("<!-- limpid comment\n{}\n-->{suffix}", escape_comment(text)),
                    });
                }
                FileEdit::CommentDelete { start, end } => {
                    check_annotation_range(source, *start, *end)?;
                    operations.push(Op::Replace {
                        start: *start,
                        end: *end,
                        text: String::new(),
                    });
                }
            }
        }
        operations.sort_by_key(|operation| match operation {
            Op::Insert { at, .. } => *at,
            Op::Replace { start, .. } => *start,
        });
        let mut updated = Vec::with_capacity(self.source.len());
        let mut cursor = 0;
        for operation in operations {
            let (start, end, text) = match operation {
                Op::Insert { at, text } => (at, at, text),
                Op::Replace { start, end, text } => (start, end, text),
            };
            if start < cursor || end < start || end > self.source.len() {
                return Err("annotation source range is invalid".into());
            }
            updated.extend_from_slice(&self.source[cursor..start]);
            updated.extend_from_slice(text.as_bytes());
            cursor = end;
        }
        updated.extend_from_slice(&self.source[cursor..]);
        atomic_write(&self.path, &updated)?;
        Ok(())
    }

    pub fn insert_after(
        &self,
        byte_end: usize,
        annotation: FileAnnotation<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if fs::read(&self.path)? != self.source {
            return Err("source changed since it was rendered".into());
        }
        if byte_end > self.source.len()
            || !std::str::from_utf8(&self.source)?.is_char_boundary(byte_end)
        {
            return Err("annotation source range is invalid".into());
        }
        let comment = match annotation {
            FileAnnotation::Reply { field, text } => {
                if !valid_question_id(field) {
                    return Err("invalid question id".into());
                }
                format!(
                    "\n\n<!-- limpid reply {field}\n{}\n-->\n",
                    escape_comment(text)
                )
            }
            FileAnnotation::Comment { text } => {
                format!("\n\n<!-- limpid comment\n{}\n-->\n", escape_comment(text))
            }
        };
        let mut updated = Vec::with_capacity(self.source.len() + comment.len());
        updated.extend_from_slice(&self.source[..byte_end]);
        updated.extend_from_slice(comment.as_bytes());
        updated.extend_from_slice(&self.source[byte_end..]);
        atomic_write(&self.path, &updated)?;
        Ok(())
    }

    pub fn upsert_reply(
        &self,
        byte_end: usize,
        field: &str,
        text: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !valid_question_id(field) {
            return Err("invalid question id".into());
        }
        if fs::read(&self.path)? != self.source {
            return Err("source changed since it was rendered".into());
        }
        let source = std::str::from_utf8(&self.source)?;
        let mut existing = annotation_spans(source)
            .into_iter()
            .filter(|span| {
                matches!(&span.kind, AnnotationKind::Reply { field: saved, .. } if saved == field)
            })
            .collect::<Vec<_>>();
        if let Some(AnnotationSpan { start, end, .. }) = existing.pop() {
            let suffix = if source[start..end].ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let replacement = format!(
                "<!-- limpid reply {field}\n{}\n-->{suffix}",
                escape_comment(text)
            );
            let mut updated = Vec::with_capacity(self.source.len() + replacement.len());
            let mut cursor = 0;
            for earlier in existing {
                updated.extend_from_slice(&self.source[cursor..earlier.start]);
                cursor = earlier.end;
            }
            updated.extend_from_slice(&self.source[cursor..start]);
            updated.extend_from_slice(replacement.as_bytes());
            updated.extend_from_slice(&self.source[end..]);
            atomic_write(&self.path, &updated)?;
            Ok(())
        } else {
            self.insert_after(byte_end, FileAnnotation::Reply { field, text })
        }
    }
}

pub fn source_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn valid_question_id(field: &str) -> bool {
    field
        .strip_prefix('q')
        .and_then(|digits| digits.parse::<usize>().ok().map(|number| (digits, number)))
        .is_some_and(|(digits, number)| number > 0 && number.to_string() == digits)
}

fn escape_comment(text: &str) -> String {
    text.replace("-->", "--&gt;")
}

fn check_annotation_range(
    source: &str,
    start: usize,
    end: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return Err("annotation source range is invalid".into());
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("destination has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("destination has no filename"))?
        .to_string_lossy();
    for attempt in 0..100u32 {
        let temp = parent.join(format!(
            ".{name}.limpid-{}-{attempt}.tmp",
            std::process::id()
        ));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Ok(metadata) = fs::metadata(path)
            && let Err(error) = file.set_permissions(metadata.permissions())
        {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        let result = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temp, path));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        return result;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve temporary file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_replies_are_atomic_escaped_and_refuse_stale_content() {
        let path = std::env::temp_dir().join(format!("limpid-replies-{}.md", std::process::id()));
        fs::write(&path, "## Questions\n\n1. Pick?\n").unwrap();
        let revision =
            FileRevision::open_checked(&path, &source_hash(&fs::read(&path).unwrap())).unwrap();
        for field in ["q", "q0", "q01", "q-1", "q184467440737095516160"] {
            assert!(
                revision
                    .insert_after(0, FileAnnotation::Reply { field, text: "bad" })
                    .unwrap_err()
                    .to_string()
                    .contains("invalid question id")
            );
        }
        revision
            .insert_after(
                revision.source.len(),
                FileAnnotation::Reply {
                    field: "q1",
                    text: "A --> B",
                },
            )
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("<!-- limpid reply q1\nA --&gt; B\n-->"));
        assert!(
            revision
                .insert_after(0, FileAnnotation::Comment { text: "late" })
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        assert!(FileRevision::open_checked(&path, &source_hash(b"different")).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn applies_a_batch_of_replies_and_comments_in_one_pass() {
        let path = std::env::temp_dir().join(format!(
            "limpid-batch-{}-{:?}.md",
            std::process::id(),
            std::thread::current().id()
        ));
        let source = "## Questions\n\n1. Pick?\n   - SQLite\n   - Postgres\n\nTail.\n";
        fs::write(&path, source).unwrap();
        let revision = FileRevision::open(&path).unwrap();
        let pick = source.find("SQLite").unwrap() + "SQLite".len();
        let tail = source.find("Tail.").unwrap();
        revision
            .apply(&[
                FileEdit::Reply {
                    field: "q1".into(),
                    text: "SQLite".into(),
                },
                FileEdit::Comment {
                    byte_end: pick,
                    text: "why sqlite".into(),
                },
                FileEdit::Comment {
                    byte_end: tail,
                    text: "and the tail".into(),
                },
            ])
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let parsed = from_file_annotations(&saved);
        assert_eq!(parsed.replies["q1"], "SQLite");
        assert_eq!(parsed.comments.len(), 2);
        assert!(parsed.comments.values().any(|c| c.text == "why sqlite"));
        assert!(parsed.comments.values().any(|c| c.text == "and the tail"));
        fs::write(
            &path,
            "## Questions\n\n1. Pick?\n\n<!-- limpid reply q1\nOld\n-->\n",
        )
        .unwrap();
        let revision = FileRevision::open(&path).unwrap();
        revision
            .apply(&[
                FileEdit::Reply {
                    field: "q1".into(),
                    text: "New".into(),
                },
                FileEdit::Comment {
                    byte_end: 0,
                    text: "early".into(),
                },
            ])
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(saved.matches("<!-- limpid reply q1").count(), 1);
        assert_eq!(from_file_annotations(&saved).replies["q1"], "New");
        assert!(revision.apply(&[]).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn edits_and_deletes_a_file_comment_by_annotation_range() {
        let path = std::env::temp_dir().join(format!(
            "limpid-edit-comment-{}-{:?}.md",
            std::process::id(),
            std::thread::current().id()
        ));
        let source = "## Heading\n\nBody.\n\n<!-- limpid comment\nFirst note\n-->\n";
        fs::write(&path, source).unwrap();
        let start = source.find("<!-- limpid comment").unwrap();
        let end = source[start..].find("-->").unwrap() + start + "-->".len();
        let revision = FileRevision::open(&path).unwrap();
        revision
            .apply(&[FileEdit::CommentReplace {
                start,
                end,
                text: "Edited note".into(),
            }])
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(saved.matches("<!-- limpid comment").count(), 1);
        assert_eq!(
            from_file_annotations(&saved).comments.values().next().unwrap().text,
            "Edited note"
        );

        let start = saved.find("<!-- limpid comment").unwrap();
        let end = saved[start..].find("-->").unwrap() + start + "-->".len();
        let revision = FileRevision::open(&path).unwrap();
        revision
            .apply(&[FileEdit::CommentDelete { start, end }])
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("limpid comment"));
        assert!(from_file_annotations(&saved).comments.is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn editing_a_file_reply_replaces_its_annotation() {
        let path =
            std::env::temp_dir().join(format!("limpid-edit-reply-{}.md", std::process::id()));
        fs::write(&path, "## Questions\n\n1. Pick?\n").unwrap();
        let first = FileRevision::open(&path).unwrap();
        first
            .upsert_reply(first.source.len(), "q1", "SQLite")
            .unwrap();
        let next = FileRevision::open(&path).unwrap();
        next.upsert_reply(0, "q1", "Postgres --> SQLite").unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(saved.matches("<!-- limpid reply q1").count(), 1);
        assert_eq!(
            from_file_annotations(&saved).replies["q1"],
            "Postgres --> SQLite"
        );
        assert!(first.upsert_reply(0, "q1", "stale").is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn editing_a_legacy_file_with_duplicate_replies_keeps_one_answer() {
        let path =
            std::env::temp_dir().join(format!("limpid-duplicate-reply-{}.md", std::process::id()));
        fs::write(
            &path,
            "## Questions\n\n1. Pick?\n\n<!-- limpid reply q1\nFirst\n-->\n\n<!-- limpid reply q1\nSecond\n-->\n",
        )
        .unwrap();
        FileRevision::open(&path)
            .unwrap()
            .upsert_reply(0, "q1", "Final")
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(saved.matches("<!-- limpid reply q1").count(), 1);
        assert_eq!(from_file_annotations(&saved).replies["q1"], "Final");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn agent_replies_round_trip() {
        let path = std::env::temp_dir().join(format!("limpid-replies-{}.json", std::process::id()));
        let mut replies = AgentReplies::default();
        replies.replies.insert("q1".into(), "SQLite".into());
        replies.comments.insert(
            "12-34".into(),
            Comment {
                text: "Fix this".into(),
                quote: "Use the existing pool".into(),
                start_line: 42,
                end_line: 48,
            },
        );
        replies.save(&path).unwrap();
        assert_eq!(AgentReplies::load(&path).unwrap(), replies);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn embedded_annotations_round_trip() {
        let mut replies = AgentReplies::default();
        replies.replies.insert("q1".into(), "SQLite".into());
        replies.comments.insert(
            "turn-0:12-34".into(),
            Comment {
                text: "Fix <this> & that".into(),
                quote: "Use the existing pool".into(),
                start_line: 42,
                end_line: 48,
            },
        );
        let page = write_annotations("<html><body><p>Body</p></body></html>", &replies);
        assert!(page.contains("<p>Body</p>"));
        assert!(!page.contains("Fix <this>"));
        assert_eq!(read_annotations(&page), replies);
        let again = write_annotations(&page, &replies);
        assert_eq!(read_annotations(&again), replies);
        assert_eq!(again.matches("limpid-annotations").count(), 1);
    }

    #[test]
    fn missing_annotations_are_empty() {
        assert_eq!(read_annotations("<html></html>"), AgentReplies::default());
    }

    #[test]
    fn composes_answered_fields_and_comments_in_source_order() {
        let mut replies = AgentReplies::default();
        replies.replies.insert("q10".into(), "later".into());
        replies.replies.insert("q2".into(), "no".into());
        replies.replies.insert("q1".into(), "SQLite".into());
        replies.comments.insert(
            "42-48".into(),
            Comment {
                text: "this leaks".into(),
                quote: "Use the existing pool".into(),
                start_line: 42,
                end_line: 48,
            },
        );
        assert_eq!(
            compose_message("Plan", "claude", "turn 0", &replies),
            "re: Plan (claude, turn 0)\nq1: SQLite\nq2: no\nq10: later\n\"Use the existing pool\" (L42-48): user commented: \"this leaks\""
        );
    }

    #[test]
    fn composes_agent_comments_in_displayed_turn_order() {
        let mut replies = AgentReplies::default();
        for (key, text) in [
            ("turn-0:50-60", "newer late"),
            ("turn-1:40-50", "older late"),
            ("turn-0:10-20", "newer early"),
            ("turn-1:5-10", "older early"),
        ] {
            replies.comments.insert(
                key.into(),
                Comment {
                    text: text.into(),
                    quote: text.into(),
                    start_line: 1,
                    end_line: 1,
                },
            );
        }
        let message = compose_message("Plan", "codex", "turns 0–1", &replies);
        let comments = message
            .lines()
            .filter(|line| line.starts_with('"'))
            .collect::<Vec<_>>();
        assert_eq!(
            comments,
            [
                "\"older early\" (turn 1, L1-1): user commented: \"older early\"",
                "\"older late\" (turn 1, L1-1): user commented: \"older late\"",
                "\"newer early\" (turn 0, L1-1): user commented: \"newer early\"",
                "\"newer late\" (turn 0, L1-1): user commented: \"newer late\"",
            ]
        );
    }

    #[test]
    fn truncates_quotes_on_word_boundaries() {
        assert_eq!(truncate_words("one two three", 100), "one two three");
        assert_eq!(truncate_words("aaaa bbbb cccc dddd", 10), "aaaa bbbb…");
        assert_eq!(truncate_words("supercalifragilistic", 5), "super…");
    }

    #[test]
    fn reads_saved_annotations_for_review() {
        let source = "Use the pool.\n\n<!-- limpid comment\nA --&gt; B\n-->\n\n<!-- limpid reply q1\nSQLite --&gt; Postgres\n-->\n";
        let saved = from_file_annotations(source);
        assert_eq!(saved.replies["q1"], "SQLite --> Postgres");
        assert_eq!(
            saved.comments.values().next().unwrap().quote,
            "Use the pool."
        );
    }

    #[test]
    fn repeated_file_annotations_keep_the_source_quote() {
        let source = "Use the pool.\n\n<!-- limpid comment\nFirst note\n-->\n\n<!-- limpid reply q1\nSQLite\n-->\n\n<!-- limpid comment\nSecond note\n-->\n";
        let saved = from_file_annotations(source);
        assert_eq!(saved.comments.len(), 2);
        for comment in saved.comments.values() {
            assert_eq!(comment.quote, "Use the pool.");
            assert_eq!(comment.start_line, 1);
        }
    }

    #[test]
    fn reply_markers_in_code_examples_are_not_saved_annotations() {
        let path =
            std::env::temp_dir().join(format!("limpid-reply-example-{}.md", std::process::id()));
        let source = "```markdown\n<!-- limpid reply q1\nExample only\n-->\n```\n\n## Questions\n\n1. Pick?\n";
        fs::write(&path, source).unwrap();
        assert!(from_file_annotations(source).replies.is_empty());
        let revision = FileRevision::open(&path).unwrap();
        revision
            .upsert_reply(source.len(), "q1", "Actual answer")
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("<!-- limpid reply q1\nExample only\n-->"));
        assert_eq!(from_file_annotations(&saved).replies["q1"], "Actual answer");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn fake_reply_command_receives_message_and_context() {
        let dir = std::env::temp_dir().join(format!("limpid-send-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let message_path = dir.join("message");
        let context_path = dir.join("context");
        let command = format!(
            "cat > {}; printf '%s|%s|%s|%s' \"$LIMPID_AGENT\" \"$LIMPID_SESSION\" \"$LIMPID_SOURCE\" \"$LIMPID_PANE\" > {}",
            message_path.display(),
            context_path.display()
        );
        deliver_reply(
            &command,
            "q1: SQLite",
            "pi",
            Some("session-42"),
            Some(Path::new("/tmp/turn.jsonl")),
            Some("7"),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&message_path).unwrap(), "q1: SQLite");
        assert_eq!(
            fs::read_to_string(&context_path).unwrap(),
            "pi|session-42|/tmp/turn.jsonl|7"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
