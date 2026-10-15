use crate::codex::Turn;
use serde_json::Value;

fn message_text(message: &Value) -> String {
    let content = &message["content"];
    if let Some(text) = content.as_str() {
        return text.trim().into();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            if part.get("type")?.as_str()? == "text" {
                part.get("text")?.as_str()
            } else {
                None
            }
        })
        .collect::<String>()
        .trim()
        .into()
}

pub fn parse_branch(entries: &[Value]) -> Vec<Turn> {
    let mut turns = Vec::new();
    let mut current: Option<Turn> = None;
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let message = &entry["message"];
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                if let Some(turn) = current.take().filter(|turn| !turn.response.is_empty()) {
                    turns.push(turn);
                }
                current = Some(Turn {
                    prompt: message_text(message),
                    response: String::new(),
                });
            }
            Some("assistant") => {
                if let Some(turn) = current.as_mut() {
                    let text = message_text(message);
                    if !text.is_empty() {
                        turn.response = text;
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(turn) = current.filter(|turn| !turn.response.is_empty()) {
        turns.push(turn);
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_branch_and_keeps_final_assistant_text() {
        let entries = vec![
            json!({"type":"message","message":{"role":"user","content":"do it"}}),
            json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}),
            json!({"type":"toolResult"}),
            json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}),
            json!({"type":"message","message":{"role":"user","content":"pending"}}),
        ];
        assert_eq!(
            parse_branch(&entries),
            vec![Turn {
                prompt: "do it".into(),
                response: "done".into()
            }]
        );
    }
}
