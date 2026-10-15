use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub highlight: Highlight,
    pub questions: Questions,
    pub reply: Reply,
    pub autoopen: Autoopen,
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Autoopen {
    pub claude: bool,
    pub pi: bool,
    pub codex: bool,
}

impl Settings {
    pub fn parse(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        match std::fs::read_to_string(path) {
            Ok(source) => Ok(Self::parse(&source)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }

    pub fn load_default() -> Result<Self, Box<dyn std::error::Error>> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
            })
            .ok_or("HOME or XDG_CONFIG_HOME is required")?;
        Self::load(&base.join("limpid/config.toml"))
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
        assert!(!settings.autoopen.claude);
        assert!(!settings.autoopen.pi);
        assert!(!settings.autoopen.codex);
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
