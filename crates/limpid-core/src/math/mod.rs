//! Math helper: find the formulas in a Markdown file, typeset the ones that
//! are not cached yet with an external backend, and leave `<hash>.svg` plus a
//! `<hash>.json` sidecar in the cache (see `cache.rs`).
//!
//! Sizes are CSS px at 96 dpi. `baseline` is the distance from the top of the
//! SVG down to the text baseline, so an inline box is placed with
//! `y = text_baseline - baseline`.

pub mod cache;
pub mod latex;
pub mod svgmeta;
pub mod typst;

use cache::Cache;
use pulldown_cmark::{Event, Options, Parser};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fmt,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// The `[math]` settings table. `settings.rs` is owned by another work unit;
/// until it embeds this type, the CLI reads the `[math]` table itself.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MathSettings {
    /// `auto | latex | typst | mathjax | none`.
    pub backend: String,
    /// `auto | pdflatex | latex`.
    pub latex: String,
    /// `auto | dvisvgm | pdftocairo`.
    pub svg: String,
    /// Typst binary name or path.
    pub typst: String,
    /// Formulas per external run. Smaller batches isolate bad formulas faster.
    pub batch_size: usize,
    /// Wall-clock limit for one external tool run.
    pub timeout_seconds: u64,
    /// Typst backend fallback: share of an inline formula's height below the
    /// baseline, used only when the baseline marker cannot be read back.
    pub typst_depth_fraction: f32,
}

impl Default for MathSettings {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            latex: "auto".into(),
            svg: "auto".into(),
            typst: "typst".into(),
            batch_size: 100,
            timeout_seconds: 60,
            typst_depth_fraction: 0.22,
        }
    }
}

impl MathSettings {
    pub fn validate(&self) -> Result<(), String> {
        let check = |key: &str, value: &str, allowed: &[&str]| {
            if allowed.contains(&value) {
                return Ok(());
            }
            Err(format!(
                "math.{key} = {value:?} is not one of {}",
                allowed.join(", ")
            ))
        };
        check(
            "backend",
            &self.backend,
            &["auto", "latex", "typst", "mathjax", "none"],
        )?;
        check("latex", &self.latex, &["auto", "pdflatex", "latex"])?;
        check("svg", &self.svg, &["auto", "dvisvgm", "pdftocairo"])?;
        if !(0.0..=1.0).contains(&self.typst_depth_fraction) {
            return Err("math.typst_depth_fraction must be between 0 and 1".into());
        }
        if self.batch_size == 0 {
            return Err("math.batch_size must be at least 1".into());
        }
        Ok(())
    }

    /// Read the `[math]` table of a settings file; absent file or table means
    /// defaults. Unknown keys are an error naming the key.
    pub fn from_config_source(source: &str) -> Result<Self, toml::de::Error> {
        #[derive(Deserialize, Default)]
        struct Table {
            #[serde(default)]
            math: MathSettings,
            #[serde(flatten)]
            _rest: toml::Table,
        }
        Ok(toml::from_str::<Table>(source)?.math)
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds.max(1))
    }
}

/// One formula found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    pub latex: String,
    pub display: bool,
    /// `blake3(latex + "\0" + "display"|"inline")`, first 16 hex digits.
    pub hash: String,
}

impl Formula {
    pub fn new(latex: &str, display: bool) -> Self {
        let mode = if display { "display" } else { "inline" };
        let digest = blake3::hash(format!("{latex}\0{mode}").as_bytes()).to_hex();
        Self {
            latex: latex.to_owned(),
            display,
            hash: digest[..16].to_owned(),
        }
    }
}

/// Formulas of a Markdown document in order of first appearance, each hash once.
///
/// Dollar signs before digits are currency, as in the page renderer:
/// `$5 and $6` contains no math. Code spans and fenced code never contain math.
pub fn collect(markdown: &str) -> Vec<Formula> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let protected = escape_currency(markdown, options);
    let mut seen = HashSet::new();
    let mut formulas = Vec::new();
    for event in Parser::new_ext(&protected, options) {
        let (latex, display) = match &event {
            Event::InlineMath(latex) => (latex, false),
            Event::DisplayMath(latex) => (latex, true),
            _ => continue,
        };
        let formula = Formula::new(latex, display);
        if seen.insert(formula.hash.clone()) {
            formulas.push(formula);
        }
    }
    formulas
}

/// Backslash-escape every `$` directly before a digit that sits in ordinary
/// text (found by parsing once without math), the rule `render.rs` applies.
fn escape_currency(source: &str, options: Options) -> String {
    let bytes = source.as_bytes();
    let plain = options - Options::ENABLE_MATH;
    let mut escape_at = vec![false; bytes.len()];
    for (event, range) in Parser::new_ext(source, plain).into_offset_iter() {
        if !matches!(event, Event::Text(_)) {
            continue;
        }
        for index in range {
            let is_currency = bytes[index] == b'$'
                && bytes[..index]
                    .iter()
                    .rev()
                    .take_while(|&&byte| byte == b'\\')
                    .count()
                    % 2
                    == 0
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
                && (index == 0 || bytes[index - 1] != b'$');
            escape_at[index] = is_currency;
        }
    }
    let mut escaped = Vec::with_capacity(bytes.len() + 8);
    for (index, byte) in bytes.iter().enumerate() {
        if escape_at[index] {
            escaped.push(b'\\');
        }
        escaped.push(*byte);
    }
    String::from_utf8(escaped).unwrap_or_else(|_| source.to_owned())
}

/// A typeset formula in the cache.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub svg: PathBuf,
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

impl From<std::io::Error> for BackendError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

pub trait Backend {
    fn name(&self) -> &str;
    fn available(&self, settings: &MathSettings) -> bool;
    /// Typeset `batch` into `out` (an existing directory), writing
    /// `out/<hash>.svg` for each formula. The result has one entry per formula,
    /// in order. Any failure fails the batch; the caller retries singly.
    fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError>;
}

/// Pick the backend named by `settings.backend`; `auto` tries latex, then
/// typst (MathJax joins in a later unit). `None` when nothing usable exists.
pub fn select(settings: &MathSettings) -> Option<Box<dyn Backend>> {
    select_with_path(settings, None)
}

/// `select` with an explicit `PATH` instead of the process one.
pub fn select_with_path(settings: &MathSettings, path: Option<&OsStr>) -> Option<Box<dyn Backend>> {
    let latex = || -> Box<dyn Backend> {
        Box::new(latex::LatexBackend::new(
            settings.clone(),
            path.map(OsString::from),
        ))
    };
    let typst = || -> Box<dyn Backend> {
        Box::new(typst::TypstBackend::new(
            settings.clone(),
            path.map(OsString::from),
        ))
    };
    let candidates: Vec<Box<dyn Backend>> = match settings.backend.as_str() {
        "auto" => vec![latex(), typst()],
        "latex" => vec![latex()],
        "typst" => vec![typst()],
        _ => return None,
    };
    candidates
        .into_iter()
        .find(|backend| backend.available(settings))
}

/// What a run did. `failed` pairs a formula hash with the reason.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    pub rendered: usize,
    pub cached: usize,
    pub failed: Vec<(String, String)>,
}

/// The cached render of `hash` in the default cache, if any.
pub fn cached(hash: &str) -> Option<Rendered> {
    Cache::open_default()?.get(hash)
}

/// Typeset every formula that is not cached yet, into the default cache.
pub fn render_missing(formulas: &[Formula], settings: &MathSettings) -> Report {
    let Some(cache) = Cache::open_default() else {
        return failure_report(formulas, "HOME or XDG_CACHE_HOME is required");
    };
    let backend = select(settings);
    render_missing_with(&cache, backend.as_deref(), formulas, settings)
}

fn failure_report(formulas: &[Formula], reason: &str) -> Report {
    Report {
        failed: formulas
            .iter()
            .map(|formula| (formula.hash.clone(), reason.to_owned()))
            .collect(),
        ..Report::default()
    }
}

/// `render_missing` with the cache and backend supplied by the caller.
pub fn render_missing_with(
    cache: &Cache,
    backend: Option<&dyn Backend>,
    formulas: &[Formula],
    settings: &MathSettings,
) -> Report {
    let mut report = Report::default();
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    for formula in formulas {
        if !seen.insert(formula.hash.as_str()) {
            continue;
        }
        if cache.get(&formula.hash).is_some() {
            report.cached += 1;
        } else {
            missing.push(formula);
        }
    }
    if missing.is_empty() {
        return report;
    }
    let Some(backend) = backend else {
        let reason = "no math backend is available";
        report
            .failed
            .extend(missing.iter().map(|f| (f.hash.clone(), reason.to_owned())));
        return report;
    };
    let work = unique_work_dir(cache.root());
    for chunk in missing.chunks(settings.batch_size.max(1)) {
        let _ = std::fs::remove_dir_all(&work);
        if let Err(error) = std::fs::create_dir_all(&work) {
            let reason = format!("cannot create work directory: {error}");
            report
                .failed
                .extend(chunk.iter().map(|f| (f.hash.clone(), reason.clone())));
            continue;
        }
        render_chunk(cache, backend, chunk, &work, &mut report);
    }
    let _ = std::fs::remove_dir_all(&work);
    report
}

static WORK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch directory name no other call in this process or another one
/// shares: process id plus a per-process counter.
fn unique_work_dir(root: &Path) -> PathBuf {
    let serial = WORK_COUNTER.fetch_add(1, Ordering::Relaxed);
    root.join(format!(".work-{}-{serial}", std::process::id()))
}

fn render_chunk(
    cache: &Cache,
    backend: &dyn Backend,
    chunk: &[&Formula],
    work: &Path,
    report: &mut Report,
) {
    let batch_error = match try_backend(backend, chunk, work) {
        Ok(results) => {
            store_all(cache, chunk, &results, report);
            return;
        }
        Err(error) => error,
    };
    if let [only] = chunk {
        report.failed.push((only.hash.clone(), batch_error.0));
        return;
    }
    // One bad formula fails the whole batch; find it by rendering singly.
    for formula in chunk {
        let _ = std::fs::remove_dir_all(work);
        if let Err(error) = std::fs::create_dir_all(work) {
            report
                .failed
                .push((formula.hash.clone(), error.to_string()));
            continue;
        }
        match try_backend(backend, &[formula], work) {
            Ok(results) => store_all(cache, &[formula], &results, report),
            Err(error) => report.failed.push((formula.hash.clone(), error.0)),
        }
    }
}

fn try_backend(
    backend: &dyn Backend,
    chunk: &[&Formula],
    work: &Path,
) -> Result<Vec<Rendered>, BackendError> {
    let results = backend.render(chunk, work)?;
    if results.len() != chunk.len() {
        return Err(BackendError(format!(
            "{} returned {} results for {} formulas",
            backend.name(),
            results.len(),
            chunk.len()
        )));
    }
    Ok(results)
}

fn store_all(cache: &Cache, chunk: &[&Formula], results: &[Rendered], report: &mut Report) {
    for (formula, rendered) in chunk.iter().zip(results) {
        match cache.store(&formula.hash, rendered) {
            Ok(_) => report.rendered += 1,
            Err(error) => report
                .failed
                .push((formula.hash.clone(), error.to_string())),
        }
    }
}

/// Find an executable: a name looked up in `path` (default: the process
/// `PATH`), or a path containing a separator checked directly.
pub fn find_binary(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let candidate = PathBuf::from(name);
        return is_executable(&candidate).then_some(candidate);
    }
    let search = match path {
        Some(path) => path.to_owned(),
        None => std::env::var_os("PATH")?,
    };
    std::env::split_paths(&search)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub(crate) struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `command` to completion or kill it after `timeout`.
pub(crate) fn run_command(
    mut command: Command,
    timeout: Duration,
) -> Result<CommandOutput, BackendError> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| BackendError(format!("cannot run {program}: {error}")))?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BackendError(format!(
                    "{program} timed out after {} s",
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(BackendError(format!("waiting for {program}: {error}"))),
        }
    };
    Ok(CommandOutput {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Files `<prefix><number><suffix>` in `dir`, sorted by number.
pub(crate) fn numbered_files(dir: &Path, prefix: &str, suffix: &str) -> Vec<(usize, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((number.parse::<usize>().ok()?, entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// The last non-empty lines of tool output, for error messages.
pub(crate) fn tail(text: &str, lines: usize) -> String {
    let kept: Vec<_> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(lines)
        .collect();
    kept.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("limpid-math-mod-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn latex_of(markdown: &str) -> Vec<(String, bool)> {
        collect(markdown)
            .into_iter()
            .map(|f| (f.latex, f.display))
            .collect()
    }

    #[test]
    fn should_collect_inline_and_display_when_both_are_present() {
        let found = latex_of("Inline $x^2$ and\n\n$$\\int_0^1 f$$\n");
        assert_eq!(
            found,
            vec![("x^2".into(), false), ("\\int_0^1 f".into(), true)]
        );
    }

    #[test]
    fn should_treat_dollar_digit_as_currency_when_collecting() {
        assert!(collect("It costs $5 and then $6 more.").is_empty());
        assert_eq!(
            latex_of("Cost $5 but $x$ is math"),
            vec![("x".into(), false)]
        );
    }

    #[test]
    fn should_keep_math_that_starts_with_a_command_when_it_holds_a_digit() {
        assert_eq!(
            latex_of(r"price $\mathrm{5}$"),
            vec![(r"\mathrm{5}".into(), false)]
        );
    }

    #[test]
    fn should_skip_code_when_collecting() {
        assert!(collect("`$x$` and\n\n```\n$$y$$\n```\n").is_empty());
    }

    #[test]
    fn should_skip_escaped_dollars_when_collecting() {
        assert!(collect(r"\$x\$ is not math").is_empty());
    }

    #[test]
    fn should_return_each_hash_once_when_a_formula_repeats() {
        let found = collect("$x$ again $x$ and $$x$$");
        assert_eq!(found.len(), 2);
        assert_ne!(found[0].hash, found[1].hash);
    }

    #[test]
    fn should_hash_by_source_and_mode_when_building_a_formula() {
        let inline = Formula::new("x", false);
        assert_eq!(inline.hash.len(), 16);
        assert_eq!(inline, Formula::new("x", false));
        assert_ne!(inline.hash, Formula::new("x", true).hash);
        assert_ne!(inline.hash, Formula::new("y", false).hash);
        let expected = blake3::hash(b"x\0inline").to_hex();
        assert_eq!(inline.hash, expected[..16]);
    }

    #[test]
    fn should_not_panic_when_markdown_is_odd() {
        for source in [
            "",
            "$",
            "$$",
            "$$$$",
            "$ $",
            "\u{1F600} $x$ \u{1F600}",
            "$\\$",
            "$5$",
        ] {
            let _ = collect(source);
        }
    }

    #[test]
    fn should_report_unknown_value_when_validating_settings() {
        let mut settings = MathSettings::default();
        assert!(settings.validate().is_ok());
        settings.backend = "mathjx".into();
        assert!(settings.validate().unwrap_err().contains("math.backend"));
        settings.backend = "auto".into();
        settings.batch_size = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn should_read_math_table_when_config_has_other_tables() {
        let source = "[reply]\ncommand = 'x'\n[math]\nbackend = 'typst'\ntypst = '/opt/typst'\n";
        let settings = MathSettings::from_config_source(source).unwrap();
        assert_eq!(settings.backend, "typst");
        assert_eq!(settings.typst, "/opt/typst");
        assert_eq!(settings.latex, "auto");
        let error = MathSettings::from_config_source("[math]\nbakend = 'x'\n").unwrap_err();
        assert!(error.to_string().contains("bakend"), "{error}");
    }

    #[test]
    fn should_find_no_binary_when_path_is_empty() {
        assert_eq!(find_binary("sh", Some(OsStr::new(""))), None);
        assert_eq!(find_binary("", Some(OsStr::new("/usr/bin"))), None);
        assert_eq!(find_binary("definitely-not-a-tool", None), None);
    }

    #[test]
    fn should_find_binary_when_given_a_path_or_search_dir() {
        assert!(find_binary("/bin/sh", Some(OsStr::new(""))).is_some());
        assert!(find_binary("sh", Some(OsStr::new("/bin:/usr/bin"))).is_some());
        assert_eq!(find_binary("/etc/hostname", None), None);
    }

    #[test]
    fn should_select_nothing_when_no_tool_is_on_path() {
        let settings = MathSettings::default();
        assert!(select_with_path(&settings, Some(OsStr::new(""))).is_none());
        for backend in ["latex", "typst", "mathjax", "none"] {
            let settings = MathSettings {
                backend: backend.into(),
                ..MathSettings::default()
            };
            assert!(
                select_with_path(&settings, Some(OsStr::new(""))).is_none(),
                "{backend}"
            );
        }
    }

    #[test]
    fn should_not_select_anything_when_backend_is_none() {
        let settings = MathSettings {
            backend: "none".into(),
            ..MathSettings::default()
        };
        assert!(select(&settings).is_none());
    }

    /// Writes a fixed SVG per formula; fails any batch that contains a
    /// formula whose source is `bad`.
    struct FakeBackend {
        calls: RefCell<Vec<usize>>,
    }

    impl Backend for FakeBackend {
        fn name(&self) -> &str {
            "fake"
        }
        fn available(&self, _: &MathSettings) -> bool {
            true
        }
        fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError> {
            self.calls.borrow_mut().push(batch.len());
            if batch.iter().any(|f| f.latex == "bad") {
                return Err(BackendError("fake: undefined control sequence".into()));
            }
            batch
                .iter()
                .map(|formula| {
                    let svg = out.join(format!("{}.svg", formula.hash));
                    std::fs::write(&svg, r#"<svg width="10" height="5"/>"#)?;
                    Ok(Rendered {
                        svg,
                        width: 10.0,
                        height: 5.0,
                        baseline: 4.0,
                        backend: "fake".into(),
                    })
                })
                .collect()
        }
    }

    /// Writes a marker into its work directory, waits, and fails if another
    /// call removed the marker in the meantime.
    struct SlowBackend;

    impl Backend for SlowBackend {
        fn name(&self) -> &str {
            "slow"
        }
        fn available(&self, _: &MathSettings) -> bool {
            true
        }
        fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError> {
            let marker = out.join("marker");
            std::fs::write(&marker, "x")?;
            std::thread::sleep(Duration::from_millis(150));
            if !marker.exists() {
                return Err(BackendError("work directory was removed".into()));
            }
            FakeBackend {
                calls: RefCell::new(vec![]),
            }
            .render(batch, out)
        }
    }

    #[test]
    fn should_not_share_work_directory_when_renders_run_concurrently() {
        let dir = scratch("concurrent");
        let cache = Cache::new(dir.join("cache"));
        let settings = MathSettings::default();
        let first = collect("$a$ $b$");
        let second = collect("$c$ $d$");
        let (one, two) = std::thread::scope(|scope| {
            let one =
                scope.spawn(|| render_missing_with(&cache, Some(&SlowBackend), &first, &settings));
            let two =
                scope.spawn(|| render_missing_with(&cache, Some(&SlowBackend), &second, &settings));
            (one.join().unwrap(), two.join().unwrap())
        });
        assert!(one.failed.is_empty(), "{:?}", one.failed);
        assert!(two.failed.is_empty(), "{:?}", two.failed);
        assert_eq!(one.rendered + two.rendered, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_render_then_report_cached_when_run_twice() {
        let dir = scratch("twice");
        let cache = Cache::new(dir.join("cache"));
        let backend = FakeBackend {
            calls: RefCell::new(vec![]),
        };
        let formulas = collect("$a$ $b$ $$c$$");
        let settings = MathSettings::default();
        let first = render_missing_with(&cache, Some(&backend), &formulas, &settings);
        assert_eq!(
            (first.rendered, first.cached, first.failed.len()),
            (3, 0, 0)
        );
        let second = render_missing_with(&cache, Some(&backend), &formulas, &settings);
        assert_eq!(
            (second.rendered, second.cached, second.failed.len()),
            (0, 3, 0)
        );
        assert_eq!(
            backend.calls.borrow().len(),
            1,
            "second run must not call the backend"
        );
        let entry = cache.get(&formulas[0].hash).unwrap();
        assert_eq!(entry.backend, "fake");
        let leftovers = std::fs::read_dir(cache.root())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".work-"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_isolate_the_bad_formula_when_a_batch_fails() {
        let dir = scratch("retry");
        let cache = Cache::new(dir.join("cache"));
        let backend = FakeBackend {
            calls: RefCell::new(vec![]),
        };
        let formulas = collect("$a$ $bad$ $b$");
        let report =
            render_missing_with(&cache, Some(&backend), &formulas, &MathSettings::default());
        assert_eq!(report.rendered, 2);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, formulas[1].hash);
        assert!(report.failed[0].1.contains("undefined control sequence"));
        assert!(cache.get(&formulas[1].hash).is_none());
        assert!(cache.get(&formulas[0].hash).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_split_into_batches_when_batch_size_is_small() {
        let dir = scratch("batches");
        let cache = Cache::new(dir.join("cache"));
        let backend = FakeBackend {
            calls: RefCell::new(vec![]),
        };
        let formulas = collect("$a$ $b$ $c$ $d$ $e$");
        let settings = MathSettings {
            batch_size: 2,
            ..MathSettings::default()
        };
        let report = render_missing_with(&cache, Some(&backend), &formulas, &settings);
        assert_eq!(report.rendered, 5);
        assert_eq!(*backend.calls.borrow(), vec![2, 2, 1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_every_missing_formula_when_no_backend_exists() {
        let dir = scratch("nobackend");
        let cache = Cache::new(dir.join("cache"));
        let formulas = collect("$a$ $b$");
        let report = render_missing_with(&cache, None, &formulas, &MathSettings::default());
        assert_eq!(report.failed.len(), 2);
        assert!(report.failed[0].1.contains("no math backend"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_batch_when_backend_returns_wrong_count() {
        struct Short;
        impl Backend for Short {
            fn name(&self) -> &str {
                "short"
            }
            fn available(&self, _: &MathSettings) -> bool {
                true
            }
            fn render(&self, _: &[&Formula], _: &Path) -> Result<Vec<Rendered>, BackendError> {
                Ok(vec![])
            }
        }
        let dir = scratch("short");
        let cache = Cache::new(dir.join("cache"));
        let formulas = collect("$a$");
        let report = render_missing_with(&cache, Some(&Short), &formulas, &MathSettings::default());
        assert_eq!(report.rendered, 0);
        assert!(report.failed[0].1.contains("0 results for 1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_kill_the_process_when_it_exceeds_the_timeout() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let started = Instant::now();
        let error = run_command(command, Duration::from_millis(100))
            .err()
            .unwrap();
        assert!(error.0.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn should_report_spawn_error_when_program_is_missing() {
        let error = run_command(
            Command::new("/nonexistent/limpid-tool"),
            Duration::from_secs(1),
        )
        .err()
        .unwrap();
        assert!(error.0.contains("cannot run"));
    }

    #[test]
    fn should_keep_last_lines_when_taking_a_tail() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("", 3), "");
    }
}
