use crate::{grammars, render::escape_html};
use libloading::Library;
use std::{collections::HashMap, fs, path::PathBuf};
use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
use tree_sitter_language::LanguageFn;

struct Grammar {
    // A Language points into the shared object, so it must outlive the config.
    config: HighlightConfiguration,
    classes: Vec<String>,
    _library: Library,
}

pub struct PageHighlighter {
    directory: Option<PathBuf>,
    aliases: HashMap<String, String>,
    loaded: HashMap<String, Option<Grammar>>,
}

impl Default for PageHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl PageHighlighter {
    pub fn new() -> Self {
        let aliases = grammars::catalog()
            .map(|catalog| {
                catalog
                    .grammars
                    .into_iter()
                    .flat_map(|grammar| {
                        let name = grammar.name;
                        std::iter::once((name.clone(), name.clone())).chain(
                            grammar
                                .aliases
                                .into_iter()
                                .map(move |alias| (alias, name.clone())),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            directory: grammars::grammar_dir().ok(),
            aliases,
            loaded: HashMap::new(),
        }
    }

    pub fn render(&mut self, language: &str, source: &str) -> String {
        let Some(name) = self.aliases.get(&language.to_ascii_lowercase()) else {
            return escape_html(source);
        };
        let name = name.clone();
        if !self.loaded.contains_key(&name) {
            self.loaded.insert(name.clone(), self.load(&name).ok());
        }
        let Some(grammar) = self.loaded.get(&name).and_then(Option::as_ref) else {
            return escape_html(source);
        };
        let mut highlighter = Highlighter::new();
        let Ok(events) =
            highlighter.highlight(&grammar.config, source.as_bytes(), None, |injected| {
                self.aliases
                    .get(&injected.to_ascii_lowercase())
                    .and_then(|canonical| self.loaded.get(canonical))
                    .and_then(Option::as_ref)
                    .map(|loaded| &loaded.config)
            })
        else {
            return escape_html(source);
        };
        let mut html = String::new();
        for event in events {
            match event {
                Ok(HighlightEvent::Source { start, end }) => {
                    html.push_str(&escape_html(&source[start..end]));
                }
                Ok(HighlightEvent::HighlightStart(highlight)) => {
                    html.push_str("<span class=\"hl-");
                    html.push_str(&grammar.classes[highlight.0]);
                    html.push_str("\">");
                }
                Ok(HighlightEvent::HighlightEnd) => html.push_str("</span>"),
                Err(_) => return escape_html(source),
            }
        }
        html
    }

    fn load(&self, name: &str) -> Result<Grammar, Box<dyn std::error::Error>> {
        let root = self.directory.as_ref().ok_or("no grammar directory")?;
        let path = root.join(format!("{name}.so"));
        // SAFETY: grammars sync builds these shared objects from pinned Tree-sitter
        // sources. The Library is retained while its Language and config are used.
        let library = unsafe { Library::new(path)? };
        let symbol_name = format!("tree_sitter_{}", name.replace('-', "_"));
        let language = unsafe {
            let symbol =
                library.get::<unsafe extern "C" fn() -> *const ()>(symbol_name.as_bytes())?;
            Language::new(LanguageFn::from_raw(*symbol))
        };
        let queries = root.join(name);
        let highlights = fs::read_to_string(queries.join("highlights.scm"))?;
        let injections = fs::read_to_string(queries.join("injections.scm")).unwrap_or_default();
        let mut config = HighlightConfiguration::new(language, name, &highlights, &injections, "")?;
        let names = config
            .names()
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        config.configure(&names);
        let classes = names
            .iter()
            .map(|name| {
                name.chars()
                    .map(|ch| if ch == '.' || ch == '_' { '-' } else { ch })
                    .collect()
            })
            .collect();
        Ok(Grammar {
            config,
            classes,
            _library: library,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_grammar_and_unknown_fence_are_escaped() {
        let mut highlighter = PageHighlighter::new();
        assert_eq!(
            highlighter.render("unknown-language", "<script>"),
            "&lt;script&gt;"
        );
        highlighter.directory = None;
        assert_eq!(highlighter.render("rust", "<script>"), "&lt;script&gt;");
    }

    #[test]
    fn installed_rust_grammar_highlights_without_losing_source() {
        let mut highlighter = PageHighlighter::new();
        let installed = highlighter
            .directory
            .as_ref()
            .is_some_and(|dir| dir.join("rust.so").is_file());
        if installed {
            let html = highlighter.render("rs", "fn main() {}");
            assert!(html.contains("hl-"), "{html}");
            assert!(html.contains("main"), "{html}");
        }
    }
}
