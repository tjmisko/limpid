use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub const HELIX_REV: &str = "ba40e547426b0f9896c8bdc699a4ab11f2b37dbc";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub helix_rev: String,
    pub grammars: Vec<Grammar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grammar {
    pub name: String,
    pub git: String,
    pub rev: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    pub query_dir: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

pub fn catalog() -> Result<Catalog, Box<dyn Error>> {
    Ok(toml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../catalog.toml"
    )))?)
}

pub fn generate(helix_source: &str) -> Result<Catalog, Box<dyn Error>> {
    let document: toml::Value = toml::from_str(helix_source)?;
    let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut query_dirs = BTreeMap::<String, String>::new();
    for language in document
        .get("language")
        .and_then(toml::Value::as_array)
        .ok_or("Helix source has no languages")?
    {
        let Some(name) = language.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let grammar = language
            .get("grammar")
            .and_then(toml::Value::as_str)
            .unwrap_or(name);
        query_dirs
            .entry(grammar.to_owned())
            .or_insert_with(|| format!("runtime/queries/{name}"));
        let entry = aliases.entry(grammar.to_owned()).or_default();
        entry.insert(name.to_owned());
        if let Some(file_types) = language.get("file-types").and_then(toml::Value::as_array) {
            for extension in file_types.iter().filter_map(toml::Value::as_str) {
                if extension.len() <= 20
                    && extension
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_'))
                {
                    entry.insert(extension.to_ascii_lowercase());
                }
            }
        }
    }
    for (name, alias) in [
        ("rust", "rs"),
        ("python", "py"),
        ("bash", "sh"),
        ("typescript", "ts"),
        ("tsx", "tsx"),
    ] {
        aliases.entry(name.into()).or_default().insert(alias.into());
    }
    let mut grammars = Vec::new();
    for entry in document
        .get("grammar")
        .and_then(toml::Value::as_array)
        .ok_or("Helix source has no grammars")?
    {
        let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let Some(source) = entry.get("source").and_then(toml::Value::as_table) else {
            continue;
        };
        let (Some(git), Some(rev)) = (
            source.get("git").and_then(toml::Value::as_str),
            source.get("rev").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        grammars.push(Grammar {
            name: name.into(),
            git: git.into(),
            rev: rev.into(),
            subpath: source
                .get("subpath")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            query_dir: query_dirs
                .get(name)
                .cloned()
                .unwrap_or_else(|| format!("runtime/queries/{name}")),
            aliases: aliases
                .remove(name)
                .unwrap_or_default()
                .into_iter()
                .filter(|alias| alias != name)
                .collect(),
        });
    }
    grammars.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog {
        helix_rev: HELIX_REV.into(),
        grammars,
    })
}

pub fn grammar_dir() -> Result<PathBuf, Box<dyn Error>> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("HOME or XDG_DATA_HOME is required")?;
    Ok(base.join("limpid/grammars"))
}

pub fn list(install_dir: &Path) -> Result<String, Box<dyn Error>> {
    let catalog = catalog()?;
    let mut output = String::new();
    for grammar in catalog.grammars {
        let installed = install_dir.join(format!("{}.so", grammar.name)).is_file();
        output.push_str(&format!(
            "{}\t{}\t{}\n",
            grammar.name,
            if installed { "installed" } else { "missing" },
            grammar.aliases.join(",")
        ));
    }
    Ok(output)
}

fn run(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "command failed ({:?}): {}",
        command,
        String::from_utf8_lossy(&output.stderr).trim()
    )
    .into())
}

fn checkout(url: &str, rev: &str, destination: &Path) -> Result<(), Box<dyn Error>> {
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    run(Command::new("git")
        .args(["clone", "--filter=blob:none", "--no-checkout", url])
        .arg(destination))?;
    run(Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["fetch", "--depth", "1", "origin", rev]))?;
    run(Command::new("git")
        .arg("-C")
        .arg(destination)
        .args(["checkout", "--detach", rev]))?;
    Ok(())
}

fn compile(grammar: &Grammar, root: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let base = root.join(grammar.subpath.as_deref().unwrap_or(""));
    let src = base.join("src");
    let parser = src.join("parser.c");
    if !parser.is_file() {
        return Err(format!("{} has no src/parser.c", grammar.name).into());
    }
    let scanner_c = src.join("scanner.c");
    let scanner_cpp = src.join("scanner.cc");
    let output = destination.with_extension("so.tmp");
    if scanner_cpp.is_file() {
        let parser_obj = destination.with_extension("parser.o");
        let scanner_obj = destination.with_extension("scanner.o");
        run(Command::new("cc")
            .args(["-O2", "-fPIC", "-c", "-I"])
            .arg(&src)
            .arg(&parser)
            .arg("-o")
            .arg(&parser_obj))?;
        run(Command::new("c++")
            .args(["-O2", "-fPIC", "-c", "-I"])
            .arg(&src)
            .arg(&scanner_cpp)
            .arg("-o")
            .arg(&scanner_obj))?;
        run(Command::new("c++")
            .arg("-shared")
            .arg(&parser_obj)
            .arg(&scanner_obj)
            .arg("-o")
            .arg(&output))?;
        fs::remove_file(parser_obj)?;
        fs::remove_file(scanner_obj)?;
    } else {
        let mut command = Command::new("cc");
        command
            .args(["-O2", "-fPIC", "-shared", "-I"])
            .arg(&src)
            .arg(&parser);
        if scanner_c.is_file() {
            command.arg(scanner_c);
        }
        command.arg("-o").arg(&output);
        run(&mut command)?;
    }
    fs::rename(output, destination)?;
    Ok(())
}

pub fn sync(names: &[String], install_dir: &Path) -> Result<(), Box<dyn Error>> {
    let catalog = catalog()?;
    let selected = names
        .iter()
        .map(|name| {
            catalog
                .grammars
                .iter()
                .find(|grammar| grammar.name == *name || grammar.aliases.contains(name))
                .cloned()
                .ok_or_else(|| format!("unknown grammar: {name}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    fs::create_dir_all(install_dir)?;
    let sources = install_dir.join(".sources");
    fs::create_dir_all(&sources)?;
    let helix = sources.join("helix");
    checkout(
        "https://github.com/helix-editor/helix.git",
        &catalog.helix_rev,
        &helix,
    )?;
    for grammar in &selected {
        let source = sources.join(&grammar.name);
        checkout(&grammar.git, &grammar.rev, &source)?;
        compile(
            grammar,
            &source,
            &install_dir.join(format!("{}.so", grammar.name)),
        )?;
        let query_dir = install_dir.join(&grammar.name);
        fs::create_dir_all(&query_dir)?;
        for query in ["highlights.scm", "injections.scm"] {
            let from = helix.join(&grammar.query_dir).join(query);
            let to = query_dir.join(query);
            if from.is_file() {
                fs::copy(from, to)?;
            } else if to.exists() {
                fs::remove_file(to)?;
            }
        }
    }
    let selected_names = selected
        .iter()
        .map(|grammar| grammar.name.as_str())
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(install_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("so")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| !selected_names.contains(stem))
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generates_pinned_aliases_and_sources() {
        let source = r#"[[language]]
name = "rust"
file-types = ["rs"]

[[grammar]]
name = "rust"
source = { git = "https://example.org/tree-sitter-rust", rev = "abc" }
"#;
        let catalog = generate(source).unwrap();
        assert_eq!(catalog.grammars[0].name, "rust");
        assert_eq!(catalog.grammars[0].query_dir, "runtime/queries/rust");
        assert!(catalog.grammars[0].aliases.contains(&"rs".into()));
        assert_eq!(catalog.helix_rev, HELIX_REV);
    }

    #[test]
    fn committed_catalog_includes_default_grammars() {
        let catalog = catalog().unwrap();
        assert!(catalog.grammars.len() >= 200);
        for name in [
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
        ] {
            assert!(
                catalog.grammars.iter().any(|grammar| grammar.name == name),
                "{name}"
            );
        }
        assert_eq!(catalog.helix_rev, HELIX_REV);
    }
}
