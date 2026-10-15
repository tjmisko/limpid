use crate::codex::Turn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct Message<'a> {
    pub role: Role,
    pub text: &'a str,
}

fn remove_tagged(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + open.len()..];
        let Some(end) = after_start.find(&close) else {
            result.push_str(&rest[start..]);
            return result;
        };
        rest = &after_start[end + close.len()..];
    }
    result.push_str(rest);
    result
}

pub fn prompt_text(text: &str) -> String {
    remove_tagged(text, "system-reminder").trim().to_owned()
}

fn starts_turn(message: &Message<'_>) -> bool {
    if message.role != Role::User {
        return false;
    }
    let mut text = prompt_text(message.text);
    for tag in [
        "local-command-stdout",
        "local-command-stderr",
        "local-command-caveat",
    ] {
        text = remove_tagged(&text, tag);
    }
    !text.trim().is_empty()
}

pub fn is_read_invocation(prompt: &str) -> bool {
    let text = prompt.trim();
    if let Some(rest) = text.strip_prefix("/read") {
        return rest.is_empty() || rest.starts_with(char::is_whitespace);
    }
    let Some(rest) = text.strip_prefix("<command-name>") else {
        return false;
    };
    let Some((name, _)) = rest.split_once("</command-name>") else {
        return false;
    };
    name.trim().trim_start_matches('/') == "read"
}

pub fn collect_turns(messages: &[Message<'_>]) -> Vec<Turn> {
    let mut opened = Vec::<Turn>::new();
    let mut current: Option<usize> = None;
    for message in messages {
        if starts_turn(message) {
            opened.push(Turn {
                prompt: prompt_text(message.text),
                response: String::new(),
            });
            current = Some(opened.len() - 1);
        } else if message.role == Role::Assistant
            && let Some(index) = current
            && !message.text.trim().is_empty()
        {
            opened[index].response = message.text.trim().into();
        }
    }
    if opened
        .last()
        .is_some_and(|turn| is_read_invocation(&turn.prompt))
    {
        opened.pop();
    }
    opened
        .into_iter()
        .filter(|turn| !turn.response.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn user(text: &str) -> Message<'_> {
        Message {
            role: Role::User,
            text,
        }
    }
    fn assistant(text: &str) -> Message<'_> {
        Message {
            role: Role::Assistant,
            text,
        }
    }

    #[test]
    fn groups_transcript_and_drops_current_read() {
        let messages = [
            user("first"),
            assistant("Working"),
            user(""),
            assistant("Done"),
            user("/read 0"),
        ];
        assert_eq!(
            collect_turns(&messages),
            vec![Turn {
                prompt: "first".into(),
                response: "Done".into()
            }]
        );
        assert_eq!(
            collect_turns(&[assistant("orphan"), user("q"), assistant("a")]).len(),
            1
        );
        assert!(collect_turns(&[user("q"), assistant(" ")]).is_empty());
    }

    #[test]
    fn strips_reminders_and_local_output() {
        let messages = [
            user("<system-reminder>multi\nline</system-reminder>real question"),
            assistant("answer"),
            user("<local-command-stdout>output</local-command-stdout>"),
            assistant("later"),
        ];
        assert_eq!(
            collect_turns(&messages),
            vec![Turn {
                prompt: "real question".into(),
                response: "later".into()
            }]
        );
    }

    #[test]
    fn handles_tagged_and_bare_read_invocations() {
        assert!(is_read_invocation(
            "<command-name>/read</command-name>\n<command-args>1</command-args>"
        ));
        assert!(is_read_invocation("/read"));
        assert!(!is_read_invocation("/readme"));
        assert!(!is_read_invocation("what does /read do?"));
        let messages = [
            user("q"),
            assistant("a"),
            user("/read 0"),
            assistant("opened"),
            user("next"),
            assistant("b"),
            user("/read"),
        ];
        assert_eq!(
            collect_turns(&messages)
                .iter()
                .map(|turn| turn.response.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "opened", "b"]
        );
    }
}
