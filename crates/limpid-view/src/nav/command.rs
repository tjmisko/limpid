//! The `:` command line: parsing, and the small text editor behind both the
//! `:` and `/` prompts.

/// What a command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `:q`, refused while there are unsaved changes.
    Quit,
    /// `:q!`
    QuitForce,
    /// `:qa`
    QuitAll,
    /// `:w`
    Write,
    /// `:wq`
    WriteQuit,
    /// An empty line.
    Empty,
    Unknown(String),
}

/// Parse a command line. Leading colons and surrounding space are ignored.
pub fn parse(line: &str) -> Command {
    let name = line.trim().trim_start_matches(':').trim();
    match name {
        "" => Command::Empty,
        "q" | "quit" => Command::Quit,
        "q!" | "quit!" => Command::QuitForce,
        "qa" | "qall" => Command::QuitAll,
        "w" | "write" => Command::Write,
        "wq" | "x" => Command::WriteQuit,
        other => Command::Unknown(other.to_owned()),
    }
}

/// What the prompt shows while it is empty.
pub const COMMAND_PLACEHOLDER: &str = "q    w    wq    q!    qa";

/// The text being typed on a prompt line. Edits happen at the end of the line
/// only, as in a shell without cursor keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
}

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Append typed characters. Control characters and newlines are dropped.
    pub fn insert(&mut self, typed: &str) {
        self.text
            .extend(typed.chars().filter(|character| !character.is_control()));
    }

    /// Remove the last character. Returns false when the line was empty, which
    /// is how Backspace on an empty prompt closes it.
    pub fn backspace(&mut self) -> bool {
        self.text.pop().is_some()
    }

    /// Remove the last word and the space before it (Ctrl-w).
    pub fn delete_word(&mut self) {
        let trimmed = self.text.trim_end().len();
        self.text.truncate(trimmed);
        let keep = self.text.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        self.text.truncate(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_the_known_commands_when_the_line_is_exact() {
        assert_eq!(parse("q"), Command::Quit);
        assert_eq!(parse("q!"), Command::QuitForce);
        assert_eq!(parse("qa"), Command::QuitAll);
        assert_eq!(parse("w"), Command::Write);
        assert_eq!(parse("wq"), Command::WriteQuit);
    }

    #[test]
    fn should_ignore_colons_and_space_when_parsing() {
        assert_eq!(parse(":q"), Command::Quit);
        assert_eq!(parse("  ::wq  "), Command::WriteQuit);
        assert_eq!(parse(": q!"), Command::QuitForce);
    }

    #[test]
    fn should_report_empty_and_unknown_lines_when_nothing_matches() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse(" : "), Command::Empty);
        assert_eq!(parse("frobnicate"), Command::Unknown("frobnicate".into()));
        assert_eq!(parse("Q"), Command::Unknown("Q".into()));
        assert_eq!(parse("q !"), Command::Unknown("q !".into()));
        assert_eq!(parse("wq!"), Command::Unknown("wq!".into()));
    }

    #[test]
    fn should_survive_odd_input_when_parsing() {
        assert_eq!(parse("\u{0}"), Command::Unknown("\u{0}".into()));
        assert!(matches!(parse(&"q".repeat(10_000)), Command::Unknown(_)));
        assert_eq!(
            parse("\u{3053}\u{3093}"),
            Command::Unknown("\u{3053}\u{3093}".into())
        );
    }

    #[test]
    fn should_append_and_delete_when_editing_a_line() {
        let mut line = LineEditor::default();
        assert!(!line.backspace(), "nothing to delete");
        line.insert("wq");
        assert_eq!(line.text(), "wq");
        assert!(line.backspace());
        assert_eq!(line.text(), "w");
        line.clear();
        assert!(line.is_empty());
    }

    #[test]
    fn should_drop_control_characters_when_inserting() {
        let mut line = LineEditor::default();
        line.insert("a\nb\tc\u{1b}d");
        assert_eq!(line.text(), "abcd");
    }

    #[test]
    fn should_delete_a_word_at_a_time_when_pressing_ctrl_w() {
        let mut line = LineEditor::default();
        line.insert("foo bar  baz");
        line.delete_word();
        assert_eq!(line.text(), "foo bar  ");
        line.delete_word();
        assert_eq!(line.text(), "foo ");
        line.delete_word();
        assert_eq!(line.text(), "");
        line.delete_word();
        assert_eq!(line.text(), "");
    }

    #[test]
    fn should_delete_whole_characters_when_the_line_has_multibyte_text() {
        let mut line = LineEditor::default();
        line.insert("caf\u{e9}\u{4e16}");
        assert!(line.backspace());
        assert_eq!(line.text(), "caf\u{e9}");
    }
}
