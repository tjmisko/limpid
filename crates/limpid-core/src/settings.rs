use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub highlight: Highlight,
    pub questions: Questions,
    pub reply: Reply,
    pub style: Style,
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

impl Settings {
    pub fn parse(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
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
}
