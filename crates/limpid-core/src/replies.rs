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
    comments.sort_by_key(|(range, _)| {
        range
            .split_once('-')
            .and_then(|(start, _)| start.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    for (_, comment) in comments {
        if comment.text.trim().is_empty() {
            continue;
        }
        let quote = comment
            .quote
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(48)
            .collect::<String>()
            .replace('"', "\\\"");
        lines.push(format!(
            "c \"{}\" (L{}-{}): {}",
            quote,
            comment.start_line,
            comment.end_line,
            comment.text.trim()
        ));
    }
    lines.join("\n")
}

pub fn from_file_annotations(source: &str) -> AgentReplies {
    let lines = source.lines().collect::<Vec<_>>();
    let mut saved = AgentReplies::default();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        let field = line
            .strip_prefix("<!-- limpid reply ")
            .filter(|id| id.starts_with('q') && id[1..].bytes().all(|byte| byte.is_ascii_digit()));
        let comment = line == "<!-- limpid comment";
        if field.is_none() && !comment {
            index += 1;
            continue;
        }
        let start = index;
        let mut body = Vec::new();
        index += 1;
        while index < lines.len() && lines[index].trim() != "-->" {
            body.push(lines[index]);
            index += 1;
        }
        if index == lines.len() {
            break;
        }
        let text = body.join("\n").replace("--&gt;", "-->");
        if let Some(field) = field {
            saved.replies.insert(field.to_owned(), text);
        } else {
            let previous = (0..start)
                .rev()
                .find(|line| !lines[*line].trim().is_empty());
            let (quote, line) = previous
                .map(|line| (lines[line].trim().to_owned(), line + 1))
                .unwrap_or_default();
            saved.comments.insert(
                format!("{start}-{index}"),
                Comment {
                    text,
                    quote,
                    start_line: line,
                    end_line: line,
                },
            );
        }
        index += 1;
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
            "re: Plan (claude, turn 0)\nq1: SQLite\nq2: no\nq10: later\nc \"Use the existing pool\" (L42-48): this leaks"
        );
    }

    #[test]
    fn reads_saved_annotations_for_review() {
        let source = "Use the pool.\n\n<!-- limpid comment\nA --> B\n-->\n\n<!-- limpid reply q1\nSQLite --&gt; Postgres\n-->\n";
        let saved = from_file_annotations(source);
        assert_eq!(saved.replies["q1"], "SQLite --> Postgres");
        assert_eq!(
            saved.comments.values().next().unwrap().quote,
            "Use the pool."
        );
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
