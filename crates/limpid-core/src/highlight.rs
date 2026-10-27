use crate::{doc::HlSpan, grammars, render::escape_html};
use libloading::Library;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    ops::Range,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tree_sitter::Language;
use tree_sitter_highlight::{
    HighlightConfiguration, HighlightEvent, Highlighter as TreeSitterHighlighter,
};
use tree_sitter_language::LanguageFn;

struct Grammar {
    // A Language points into the shared object, so it must outlive the config.
    config: HighlightConfiguration,
    injections: String,
    _library: Library,
}

static CONSTRUCTED: AtomicUsize = AtomicUsize::new(0);

/// Compiled grammars, cached per language for the life of the value. Parse
/// trees are dropped after each call. Build one per process and reuse it.
pub struct Highlighter {
    directory: Option<PathBuf>,
    aliases: HashMap<String, String>,
    enabled: Option<BTreeSet<String>>,
    loaded: HashMap<String, Option<Grammar>>,
    classes: Vec<String>,
    configured_count: usize,
}

/// One step of a highlight run over the source.
enum Step {
    Source(Range<usize>),
    Open(usize),
    Close,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// The spans of `source` in `language`, as byte ranges plus the class name
/// without the `hl-` prefix. Spans nest: an inner span follows its outer one.
/// An unknown, disabled or broken grammar yields no spans.
pub fn highlight_spans(language: &str, source: &str, highlighter: &mut Highlighter) -> Vec<HlSpan> {
    highlighter.spans(language, source)
}

impl Highlighter {
    /// How many highlighters this process has built. Tests use it to prove a
    /// code path never reaches for one.
    pub fn constructed_count() -> usize {
        CONSTRUCTED.load(Ordering::SeqCst)
    }

    pub fn new() -> Self {
        CONSTRUCTED.fetch_add(1, Ordering::SeqCst);
        let aliases: HashMap<String, String> = grammars::catalog()
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
        // A configured grammar list limits which languages highlight; an
        // unreadable config leaves every installed grammar available.
        let enabled = crate::settings::Settings::load_default()
            .ok()
            .map(|settings| {
                settings
                    .highlight
                    .grammars
                    .iter()
                    .filter_map(|name| aliases.get(&name.to_ascii_lowercase()).cloned())
                    .collect::<BTreeSet<_>>()
            });
        Self {
            directory: grammars::grammar_dir().ok(),
            aliases,
            enabled,
            loaded: HashMap::new(),
            classes: Vec::new(),
            configured_count: 0,
        }
    }

    pub fn render(&mut self, language: &str, source: &str) -> String {
        let Some(steps) = self.run(language, source) else {
            return escape_html(source);
        };
        let mut html = String::new();
        for step in steps {
            match step {
                Step::Source(range) => html.push_str(&escape_html(&source[range])),
                Step::Open(class) => {
                    html.push_str("<span class=\"hl-");
                    html.push_str(&self.classes[class]);
                    html.push_str("\">");
                }
                Step::Close => html.push_str("</span>"),
            }
        }
        html
    }

    /// See [`highlight_spans`].
    pub fn spans(&mut self, language: &str, source: &str) -> Vec<HlSpan> {
        let Some(steps) = self.run(language, source) else {
            return Vec::new();
        };
        let mut spans: Vec<HlSpan> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        let mut position = 0;
        for step in steps {
            match step {
                Step::Source(range) => position = range.end,
                Step::Open(class) => {
                    open.push(spans.len());
                    spans.push(HlSpan {
                        range: position..position,
                        class: self.classes[class].clone(),
                    });
                }
                Step::Close => {
                    if let Some(index) = open.pop() {
                        spans[index].range.end = position;
                    }
                }
            }
        }
        spans.retain(|span| !span.range.is_empty());
        spans
    }

    /// Highlight `source`, or `None` when the language has no usable grammar.
    fn run(&mut self, language: &str, source: &str) -> Option<Vec<Step>> {
        let name = self.aliases.get(&language.to_ascii_lowercase())?.clone();
        if self
            .enabled
            .as_ref()
            .is_some_and(|enabled| !enabled.contains(&name))
        {
            return None;
        }
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
        let grammar = self.loaded.get(&name).and_then(Option::as_ref)?;
        let mut tree_sitter = TreeSitterHighlighter::new();
        let events = tree_sitter
            .highlight(&grammar.config, source.as_bytes(), None, |injected| {
                self.aliases
                    .get(&injected.to_ascii_lowercase())
                    .and_then(|canonical| self.loaded.get(canonical))
                    .and_then(Option::as_ref)
                    .map(|loaded| &loaded.config)
            })
            .ok()?;
        let mut steps = Vec::new();
        for event in events {
            steps.push(match event.ok()? {
                HighlightEvent::Source { start, end } => Step::Source(start..end),
                HighlightEvent::HighlightStart(highlight) => Step::Open(highlight.0),
                HighlightEvent::HighlightEnd => Step::Close,
            });
        }
        Some(steps)
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
        let mut highlighter = Highlighter::new();
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
        let mut highlighter = Highlighter::new();
        highlighter.directory = Some(root.clone());
        assert_eq!(highlighter.render("rust", "<script>"), "&lt;script&gt;");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_grammar_list_disables_other_languages() {
        let mut highlighter = Highlighter::new();
        highlighter.enabled = Some(BTreeSet::from(["rust".to_owned()]));
        if highlighter
            .directory
            .as_ref()
            .is_some_and(|dir| dir.join("python.so").is_file())
        {
            let html = highlighter.render("python", "print('hi')");
            assert!(!html.contains("hl-"), "{html}");
        }
    }

    #[test]
    fn installed_rust_grammar_highlights_without_losing_source() {
        let mut highlighter = Highlighter::new();
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
        let highlighter = Highlighter::new();
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
        let mut highlighter = Highlighter::new();
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
        let mut highlighter = Highlighter::new();
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
