use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub prompt: String,
    pub response: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexTurns {
    pub model: Option<String>,
    pub turns: Vec<Turn>,
}

#[derive(Default)]
struct OpenTurn {
    prompt: Option<String>,
    last_agent_message: Option<String>,
    final_answer: Option<String>,
    last_assistant: Option<String>,
}

fn close_turn(open: &mut Option<OpenTurn>, turns: &mut Vec<Turn>) {
    let Some(turn) = open.take() else {
        return;
    };
    let prompt = turn.prompt.unwrap_or_default().trim().to_owned();
    let response = turn
        .last_agent_message
        .or(turn.final_answer)
        .or(turn.last_assistant)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !prompt.is_empty() && !response.is_empty() {
        turns.push(Turn { prompt, response });
    }
}

fn content_texts(content: &Value, item_type: &str, filter_injected: bool) -> String {
    let Some(items) = content.as_array() else {
        return String::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if item.get("type")?.as_str()? != item_type {
                return None;
            }
            let text = item.get("text")?.as_str()?;
            if filter_injected
                && [
                    "# AGENTS.md instructions",
                    "<environment_context>",
                    "<user_shell_command>",
                ]
                .iter()
                .any(|prefix| text.trim_start().starts_with(prefix))
            {
                return None;
            }
            Some(text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn parse_rollout(jsonl: &str) -> CodexTurns {
    let mut result = CodexTurns::default();
    let mut open: Option<OpenTurn> = None;
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = record.get("payload").filter(|payload| payload.is_object()) else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(model) = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|model| !model.is_empty())
                {
                    result.model = Some(model.to_owned());
                }
            }
            Some("event_msg") => match payload.get("type").and_then(Value::as_str) {
                Some("task_started") => {
                    close_turn(&mut open, &mut result.turns);
                    open = Some(OpenTurn::default());
                }
                Some("task_complete") => {
                    if let Some(turn) = open.as_mut() {
                        turn.last_agent_message = payload
                            .get("last_agent_message")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    close_turn(&mut open, &mut result.turns);
                }
                _ => {}
            },
            Some("response_item")
                if payload.get("type").and_then(Value::as_str) == Some("message") =>
            {
                match payload.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = content_texts(&payload["content"], "input_text", true);
                        if text.trim().is_empty() {
                            continue;
                        }
                        if open.as_ref().is_some_and(|turn| {
                            turn.prompt.is_some()
                                && (turn.final_answer.is_some() || turn.last_assistant.is_some())
                        }) {
                            close_turn(&mut open, &mut result.turns);
                        }
                        open.get_or_insert_with(OpenTurn::default)
                            .prompt
                            .get_or_insert(text);
                    }
                    Some("assistant") => {
                        let Some(turn) = open.as_mut() else {
                            continue;
                        };
                        let text = content_texts(&payload["content"], "output_text", false);
                        if text.trim().is_empty() {
                            continue;
                        }
                        turn.last_assistant = Some(text.clone());
                        if payload.get("phase").and_then(Value::as_str) == Some("final_answer") {
                            turn.final_answer = Some(text);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    close_turn(&mut open, &mut result.turns);
    result
}

fn list_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs = fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

fn rollouts(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for year in list_dirs(root) {
        for month in list_dirs(&year) {
            for day in list_dirs(&month) {
                if let Ok(entries) = fs::read_dir(day) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with("rollout-") && name.ends_with(".jsonl")
                            })
                        {
                            result.push(path);
                        }
                    }
                }
            }
        }
    }
    result.sort_by(|a, b| b.cmp(a));
    result
}

fn first_line(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(file)
        .take(1 << 20)
        .read_until(b'\n', &mut line)
        .ok()?;
    String::from_utf8(line).ok()
}

pub fn find_rollout(root: &Path, thread_id: Option<&str>, cwd: &Path) -> Option<PathBuf> {
    let files = rollouts(root);
    if let Some(id) = thread_id {
        let suffix = format!("-{id}.jsonl");
        if let Some(path) = files.iter().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
        }) {
            return Some(path.clone());
        }
    }
    let mut candidates = files
        .into_iter()
        .filter_map(|path| {
            let modified = fs::metadata(&path)
                .ok()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    for (_, path) in candidates.into_iter().take(200) {
        let Some(line) = first_line(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) == Some("session_meta")
            && record.pointer("/payload/cwd").and_then(Value::as_str) == cwd.to_str()
        {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture(name: &str) -> CodexTurns {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures/codex")
            .join(format!("{name}.jsonl"));
        parse_rollout(&fs::read_to_string(path).unwrap())
    }

    #[test]
    fn fixture_parity() {
        assert_eq!(
            fixture("simple").turns,
            vec![
                Turn {
                    prompt: "first prompt".into(),
                    response: "first answer".into()
                },
                Turn {
                    prompt: "second prompt".into(),
                    response: "second answer".into()
                }
            ]
        );
        assert_eq!(fixture("simple").model.as_deref(), Some("m-1"));
        assert_eq!(fixture("injected").turns[0].prompt, "real prompt");
        assert_eq!(fixture("developer").turns[0].prompt, "hello");
        assert_eq!(fixture("commentary").turns[0].response, "the final text");
        assert_eq!(
            fixture("prefers-last-agent-message").turns[0].response,
            "from task_complete"
        );
        assert_eq!(fixture("shell-turn").turns.len(), 1);
        assert_eq!(fixture("in-progress").turns.len(), 2);
        assert!(fixture("in-progress-no-answer").turns.is_empty());
        assert_eq!(fixture("malformed").turns[0].prompt, "survivor");
        assert!(fixture("empty").turns.is_empty());
        assert_eq!(fixture("model-change").model.as_deref(), Some("m-new"));
        assert_eq!(fixture("no-task-started").turns.len(), 2);
        assert_eq!(fixture("unknown-records").turns[0].response, "a");
        assert_eq!(
            fixture("unicode").turns[0].prompt,
            "naïve 日本語 🙂 \"quoted\" \\ back"
        );
    }

    #[test]
    fn finds_exact_thread_then_newest_matching_directory() {
        let root = std::env::temp_dir().join(format!("limpid-codex-find-{}", std::process::id()));
        let day = root.join("2026/03/04");
        fs::create_dir_all(&day).unwrap();
        let entries = [
            ("rollout-2026-03-04T10-00-00-aaaa.jsonl", "/work", 1),
            ("rollout-2026-03-04T11-00-00-bbbb.jsonl", "/work", 2),
            ("rollout-2026-03-04T12-00-00-cccc.jsonl", "/other", 3),
            ("rollout-2026-03-04T13-00-00-xaaaa.jsonl", "/other", 4),
        ];
        for (name, cwd, modified) in entries {
            let path = day.join(name);
            fs::write(
                &path,
                format!("{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{cwd}\"}}}}\n"),
            )
            .unwrap();
            let file = fs::File::open(path).unwrap();
            file.set_times(
                fs::FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified)),
            )
            .unwrap();
        }
        assert_eq!(
            find_rollout(&root, Some("aaaa"), Path::new("/work"))
                .unwrap()
                .file_name()
                .unwrap(),
            "rollout-2026-03-04T10-00-00-aaaa.jsonl"
        );
        assert_eq!(
            find_rollout(&root, Some("missing"), Path::new("/work"))
                .unwrap()
                .file_name()
                .unwrap(),
            "rollout-2026-03-04T11-00-00-bbbb.jsonl"
        );
        assert!(find_rollout(&root, None, Path::new("/missing")).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
