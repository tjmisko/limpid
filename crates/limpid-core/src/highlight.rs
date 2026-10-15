use crate::{grammars, render::escape_html};
use libloading::Library;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::PathBuf,
};
use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
use tree_sitter_language::LanguageFn;

struct Grammar {
    // A Language points into the shared object, so it must outlive the config.
    config: HighlightConfiguration,
    injections: String,
    _library: Library,
}

pub struct PageHighlighter {
    directory: Option<PathBuf>,
    aliases: HashMap<String, String>,
    loaded: HashMap<String, Option<Grammar>>,
    classes: Vec<String>,
    configured_count: usize,
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
            classes: Vec::new(),
            configured_count: 0,
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
        let static_injections = self
            .loaded
            .get(&name)
            .and_then(Option::as_ref)
            .map(|grammar| {
                grammar
                    .injections
                    .split("#set! injection.language \"")
                    .skip(1)
                    .filter_map(|tail| tail.split('"').next())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for injected in static_injections {
            if let Some(canonical) = self.aliases.get(&injected).cloned()
                && !self.loaded.contains_key(&canonical)
            {
                self.loaded
                    .insert(canonical.clone(), self.load(&canonical).ok());
            }
        }
        if name == "markdown" {
            let fenced = source
                .lines()
                .filter_map(|line| {
                    let line = line.trim_start();
                    let info = line
                        .strip_prefix("```")
                        .or_else(|| line.strip_prefix("~~~"))?;
                    let alias = info.split_whitespace().next()?.to_ascii_lowercase();
                    self.aliases.get(&alias).cloned()
                })
                .collect::<Vec<_>>();
            for injected in fenced {
                if !self.loaded.contains_key(&injected) {
                    self.loaded
                        .insert(injected.clone(), self.load(&injected).ok());
                }
            }
        }
        self.configure_loaded();
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
                    html.push_str(&self.classes[highlight.0]);
                    html.push_str("\">");
                }
                Ok(HighlightEvent::HighlightEnd) => html.push_str("</span>"),
                Err(_) => return escape_html(source),
            }
        }
        html
    }

    fn configure_loaded(&mut self) {
        if self.configured_count == self.loaded.len() {
            return;
        }
        let names = self
            .loaded
            .values()
            .filter_map(Option::as_ref)
            .flat_map(|grammar| grammar.config.names().iter().map(|name| name.to_string()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for grammar in self.loaded.values_mut().filter_map(Option::as_mut) {
            grammar.config.configure(&names);
        }
        self.classes = names
            .iter()
            .map(|name| {
                name.chars()
                    .map(|ch| if ch == '.' || ch == '_' { '-' } else { ch })
                    .collect()
            })
            .collect();
        self.configured_count = self.loaded.len();
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
        let config = HighlightConfiguration::new(language, name, &highlights, &injections, "")?;
        Ok(Grammar {
            config,
            injections,
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
    fn broken_shared_object_falls_back_to_plain_code() {
        let root = std::env::temp_dir().join(format!("limpid-bad-grammar-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("rust.so"), b"not a shared object").unwrap();
        let mut highlighter = PageHighlighter::new();
        highlighter.directory = Some(root.clone());
        assert_eq!(highlighter.render("rust", "<script>"), "&lt;script&gt;");
        fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn installed_default_queries_compile() {
        let highlighter = PageHighlighter::new();
        let Some(directory) = &highlighter.directory else {
            return;
        };
        let defaults = crate::settings::Highlight::default();
        let mut failures = Vec::new();
        for name in defaults.grammars {
            if directory.join(format!("{name}.so")).is_file()
                && let Err(error) = highlighter.load(&name)
            {
                failures.push(format!("{name}: {error}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn rust_macro_body_is_highlighted_when_installed() {
        let mut highlighter = PageHighlighter::new();
        if !highlighter
            .directory
            .as_ref()
            .is_some_and(|dir| dir.join("rust.so").is_file())
        {
            return;
        }
        let html = highlighter.render("rust", "macro_rules! make { () => { fn generated() {} }; }");
        assert!(
            html.contains("hl-keyword-function") && html.contains("fn</span>"),
            "{html}"
        );
    }

    #[test]
    fn markdown_fence_injects_its_installed_language() {
        let mut highlighter = PageHighlighter::new();
        if !highlighter
            .directory
            .as_ref()
            .is_some_and(|dir| dir.join("markdown.so").is_file() && dir.join("rust.so").is_file())
        {
            return;
        }
        let html = highlighter.render("markdown", "```rust\nfn main() {}\n```");
        assert!(
            html.contains("hl-keyword-function") && html.contains("fn</span>"),
            "{html}"
        );
    }
}
