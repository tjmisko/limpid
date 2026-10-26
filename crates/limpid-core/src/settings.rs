use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub highlight: Highlight,
    pub questions: Questions,
    pub reply: Reply,
    pub style: Style,
    pub view: View,
    pub editor: Editor,
    pub math: Math,
    pub mermaid: Mermaid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Highlight {
    pub grammars: Vec<String>,
    pub theme: String,
}

impl Default for Highlight {
    fn default() -> Self {
        Self {
            grammars: [
                "rust",
                "python",
                "typescript",
                "tsx",
                "bash",
                "json",
                "toml",
                "yaml",
                "go",
                "c",
                "cpp",
                "lua",
                "sql",
                "html",
                "css",
                "markdown",
            ]
            .map(str::to_owned)
            .to_vec(),
            theme: "default".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Questions {
    pub headings: Vec<String>,
}

impl Default for Questions {
    fn default() -> Self {
        Self {
            headings: ["Questions", "Open questions", "Questions for you"]
                .map(str::to_owned)
                .to_vec(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reply {
    pub command: String,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            command: "wl-copy".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RailMode {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewTheme {
    Auto,
    Dark,
    Light,
}

/// The native viewer's look and feel.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    /// Family name, or `sans-serif` / `serif` / `monospace`.
    pub font: String,
    pub mono_font: String,
    /// CSS px at scale 1.
    pub font_size: u32,
    pub line_height: f32,
    /// Width of the page column, in px.
    pub content_width: u32,
    /// Width of the contents rail, in px.
    pub rail_width: u32,
    pub rail: RailMode,
    /// Wrap long code lines instead of scrolling the block.
    pub code_wrap: bool,
    pub theme: ViewTheme,
    /// Ctrl-d / Ctrl-u move this fraction of the viewport.
    pub scroll_fraction: f32,
    /// Optional key remaps: action name to key name (`j`, `ctrl+d`, `shift+g`).
    /// The viewer owns the action names and rejects ones it does not know.
    pub keys: BTreeMap<String, String>,
    /// How often an open page checks its file for changes, in milliseconds.
    /// `0` turns watching off.
    pub watch_interval_ms: u64,
}

impl Default for View {
    fn default() -> Self {
        Self {
            font: "sans-serif".into(),
            mono_font: "monospace".into(),
            font_size: 17,
            line_height: 1.65,
            content_width: 860,
            rail_width: 220,
            rail: RailMode::Auto,
            code_wrap: false,
            theme: ViewTheme::Auto,
            scroll_fraction: 0.5,
            keys: BTreeMap::new(),
            watch_interval_ms: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorInit {
    /// Start nvim with the user's own configuration.
    User,
    /// Start nvim with limpid's minimal init only.
    Clean,
}

/// The embedded editor.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Editor {
    /// Binary name or path.
    pub command: String,
    pub init: EditorInit,
    /// Kill the idle editor after this long with no box open.
    pub idle_seconds: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            command: "nvim".into(),
            init: EditorInit::User,
            idle_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MathBackend {
    Auto,
    Latex,
    Typst,
    Mathjax,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LatexEngine {
    Auto,
    Pdflatex,
    Latex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SvgConverter {
    Auto,
    Dvisvgm,
    Pdftocairo,
}

/// How the native viewer turns math into pictures.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Math {
    pub backend: MathBackend,
    pub latex: LatexEngine,
    pub svg: SvgConverter,
    /// Typst binary name or path.
    pub typst: String,
}

impl Default for Math {
    fn default() -> Self {
        Self {
            backend: MathBackend::Auto,
            latex: LatexEngine::Auto,
            svg: SvgConverter::Auto,
            typst: "typst".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mermaid {
    /// `false`: diagrams always show their source.
    pub enabled: bool,
    /// How long the Mermaid helper may take for one page, in seconds.
    pub timeout_seconds: f64,
    /// Whether the helper may show a 1x1 window when a diagram needs layout.
    pub helper_window: HelperWindow,
}

impl Default for Mermaid {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_seconds: 20.0,
            helper_window: HelperWindow::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelperWindow {
    /// Render without a window; retry in a 1x1 window when layout did not run.
    Auto,
    Never,
    Always,
}

impl Settings {
    pub fn parse(source: &str) -> Result<Self, toml::de::Error> {
        let settings: Self = toml::from_str(source)?;
        let seconds = settings.mermaid.timeout_seconds;
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(serde::de::Error::custom(
                "mermaid.timeout_seconds must be greater than zero",
            ));
        }
        Ok(settings)
    }

    /// The directory holding `config.toml`, or `None` when no config root is set.
    pub fn config_dir() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join("limpid"))
    }

    /// Read the extra stylesheet, `custom.css` by default, beside `config.toml`.
    pub fn custom_css(&self) -> Option<String> {
        let path = self.style.css_path(&Self::config_dir()?);
        std::fs::read_to_string(path).ok()
    }

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        match std::fs::read_to_string(path) {
            Ok(source) => Ok(Self::parse(&source)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }

    pub fn load_default() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = Self::config_dir().ok_or("HOME or XDG_CONFIG_HOME is required")?;
        Self::load(&dir.join("config.toml"))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub font_size: u32,
    /// Extra stylesheet. Absolute, or relative to the config directory.
    pub css: Option<String>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_size: 17,
            css: None,
        }
    }
}

impl Style {
    fn css_path(&self, dir: &Path) -> PathBuf {
        let path = PathBuf::from(self.css.as_deref().unwrap_or("custom.css"));
        if path.is_absolute() {
            path
        } else {
            dir.join(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let settings = Settings::parse("[reply]\ncommand = 'limpid-core send'\n").unwrap();
        assert_eq!(settings.reply.command, "limpid-core send");
        assert_eq!(settings.highlight.theme, "default");
        assert_eq!(settings.style.font_size, 17);
        assert_eq!(settings.style.css, None);
    }

    #[test]
    fn style_overrides_size_and_stylesheet() {
        let settings = Settings::parse("[style]\nfont_size = 20\ncss = 'mine.css'\n").unwrap();
        assert_eq!(settings.style.font_size, 20);
        assert_eq!(settings.style.css.as_deref(), Some("mine.css"));
        let dir = Path::new("/tmp/limpid-style");
        assert_eq!(settings.style.css_path(dir), dir.join("mine.css"));
        let default = Style::default();
        assert_eq!(default.css_path(dir), dir.join("custom.css"));
    }

    #[test]
    fn unknown_key_names_are_reported() {
        for source in [
            "wat = 1",
            "[highlight]\ngrmmars = []",
            "[autoopen]\npie = true",
        ] {
            let error = Settings::parse(source).unwrap_err().to_string();
            assert!(
                error.contains(
                    source
                        .split_whitespace()
                        .next()
                        .unwrap_or("wat")
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                ) || error.contains("grmmars")
                    || error.contains("pie"),
                "{error}"
            );
        }
    }

    #[test]
    fn should_use_native_viewer_defaults_when_sections_are_absent() {
        let settings = Settings::parse("").unwrap();
        assert_eq!(settings.view.font, "sans-serif");
        assert_eq!(settings.view.mono_font, "monospace");
        assert_eq!(settings.view.font_size, 17);
        assert_eq!(settings.view.line_height, 1.65);
        assert_eq!(settings.view.content_width, 860);
        assert_eq!(settings.view.rail_width, 220);
        assert_eq!(settings.view.rail, RailMode::Auto);
        assert!(!settings.view.code_wrap);
        assert_eq!(settings.view.theme, ViewTheme::Auto);
        assert_eq!(settings.view.scroll_fraction, 0.5);
        assert!(settings.view.keys.is_empty());
        assert_eq!(settings.editor.command, "nvim");
        assert_eq!(settings.editor.init, EditorInit::User);
        assert_eq!(settings.editor.idle_seconds, 300);
        assert_eq!(settings.math.backend, MathBackend::Auto);
        assert_eq!(settings.math.latex, LatexEngine::Auto);
        assert_eq!(settings.math.svg, SvgConverter::Auto);
        assert_eq!(settings.math.typst, "typst");
        assert!(settings.mermaid.enabled);
        assert_eq!(settings.mermaid.timeout_seconds, 20.0);
        assert_eq!(settings.mermaid.helper_window, HelperWindow::Auto);
        assert_eq!(settings.view.watch_interval_ms, 300);
    }

    #[test]
    fn should_parse_every_native_viewer_key_when_all_are_set() {
        let settings = Settings::parse(
            "[view]\nfont = 'Inter'\nmono_font = 'Fira Code'\nfont_size = 19\nline_height = 1.4\n\
             content_width = 700\nrail_width = 180\nrail = 'off'\ncode_wrap = true\ntheme = 'light'\n\
             scroll_fraction = 0.25\n[view.keys]\nnext_block = 'n'\nhalf_down = 'ctrl+d'\n\
             [editor]\ncommand = '/opt/nvim'\ninit = 'clean'\nidle_seconds = 10\n\
             [math]\nbackend = 'typst'\nlatex = 'pdflatex'\nsvg = 'dvisvgm'\ntypst = '/bin/typst'\n\
             [mermaid]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(settings.view.font, "Inter");
        assert_eq!(settings.view.mono_font, "Fira Code");
        assert_eq!(settings.view.font_size, 19);
        assert_eq!(settings.view.line_height, 1.4);
        assert_eq!(settings.view.content_width, 700);
        assert_eq!(settings.view.rail_width, 180);
        assert_eq!(settings.view.rail, RailMode::Off);
        assert!(settings.view.code_wrap);
        assert_eq!(settings.view.theme, ViewTheme::Light);
        assert_eq!(settings.view.scroll_fraction, 0.25);
        assert_eq!(settings.view.keys["next_block"], "n");
        assert_eq!(settings.view.keys["half_down"], "ctrl+d");
        assert_eq!(settings.editor.command, "/opt/nvim");
        assert_eq!(settings.editor.init, EditorInit::Clean);
        assert_eq!(settings.editor.idle_seconds, 10);
        assert_eq!(settings.math.backend, MathBackend::Typst);
        assert_eq!(settings.math.latex, LatexEngine::Pdflatex);
        assert_eq!(settings.math.svg, SvgConverter::Dvisvgm);
        assert_eq!(settings.math.typst, "/bin/typst");
        assert!(!settings.mermaid.enabled);
    }

    #[test]
    fn should_keep_other_defaults_when_one_native_key_is_set() {
        let settings = Settings::parse("[view]\nrail = 'on'\n").unwrap();
        assert_eq!(settings.view.rail, RailMode::On);
        assert_eq!(settings.view.content_width, 860);
        assert_eq!(settings.editor.command, "nvim");
    }

    #[test]
    fn should_name_the_key_when_a_native_section_has_an_unknown_key() {
        for (source, name) in [
            ("[view]\nfontt = 'x'", "fontt"),
            ("[editor]\nidle = 1", "idle"),
            ("[math]\nengine = 'x'", "engine"),
            ("[mermaid]\non = true", "on"),
        ] {
            let error = Settings::parse(source).unwrap_err().to_string();
            assert!(error.contains(name), "{error}");
        }
    }

    #[test]
    fn should_reject_unknown_enum_values_when_parsing_native_keys() {
        for source in [
            "[view]\nrail = 'sideways'",
            "[view]\ntheme = 'sepia'",
            "[editor]\ninit = 'wild'",
            "[math]\nbackend = 'katex'",
            "[math]\nlatex = 'xelatex'",
            "[math]\nsvg = 'inkscape'",
        ] {
            assert!(Settings::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn should_reject_wrong_value_types_when_parsing_native_keys() {
        for source in [
            "[view]\nfont_size = 'big'",
            "[view]\nfont_size = -3",
            "[view]\ncode_wrap = 'yes'",
            "[view.keys]\nnext_block = 5",
            "[mermaid]\nenabled = 'true'",
        ] {
            assert!(Settings::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn should_accept_the_mermaid_helper_keys_the_host_reads_when_they_are_set() {
        let settings = Settings::parse(
            "[mermaid]\ntimeout_seconds = 7\nhelper_window = 'never'\n[view]\nwatch_interval_ms = 0\n",
        )
        .unwrap();
        assert_eq!(settings.mermaid.timeout_seconds, 7.0);
        assert_eq!(settings.mermaid.helper_window, HelperWindow::Never);
        assert_eq!(settings.view.watch_interval_ms, 0);
    }

    #[test]
    fn should_reject_a_mermaid_timeout_that_cannot_elapse_when_parsing() {
        for value in ["0", "-1", "nan", "inf", "'soon'"] {
            let source = format!("[mermaid]\ntimeout_seconds = {value}");
            assert!(Settings::parse(&source).is_err(), "{source}");
        }
    }
}
