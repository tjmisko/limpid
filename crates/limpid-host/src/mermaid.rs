//! `limpid-host --render-mermaid FILE.md`: render the Mermaid fences of one
//! Markdown file to SVG and store them in `~/.cache/limpid/mermaid`.
//!
//! Mermaid needs a browser (layout calls `getBBox` on live SVG text), so the
//! helper drives a `WebKitWebView`. It first tries a view that is never put in
//! a window. If WebKit reports degenerate sizes there (the spike result is
//! documented on [`WindowMode`]), diagrams are rendered again in a 1x1 window
//! whose class is `limpid-helper` (or `LIMPID_WINDOW_CLASS`), the same shape
//! as `limpid-test`.
//!
//! Settings are read from `config.toml` as raw TOML because `settings.rs`
//! rejects keys it does not know yet. Keys, all optional, in `[mermaid]`:
//!
//! - `enabled` (bool, default true): false makes the helper a no-op.
//! - `timeout_seconds` (number, default 20): hard limit for the whole run.
//! - `helper_window` (`"auto"` | `"never"` | `"always"`, default `"auto"`).

use gtk4::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use webkit6::prelude::*;

const DEFAULT_TIMEOUT_SECONDS: f64 = 20.0;
const MAX_SVG_BYTES: usize = 2_000_000;
const MERMAID_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/mermaid.min.js"
));
const MESSAGE_HANDLER: &str = "mermaid-batch";
const DEFAULT_WINDOW_CLASS: &str = "limpid-helper";

/// One distinct diagram: its cache key and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    pub hash: String,
    pub source: String,
}

/// The outcome counts printed as `rendered N cached M failed K`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub rendered: usize,
    pub cached: usize,
    pub failed: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "rendered {} cached {} failed {}",
            self.rendered, self.cached, self.failed
        )
    }
}

impl Report {
    /// Exit status policy: failure only when diagrams were requested and none
    /// of them ended up in the cache.
    pub fn exit_code(&self) -> i32 {
        if self.failed > 0 && self.rendered == 0 && self.cached == 0 {
            return 1;
        }
        0
    }
}

/// When the helper may map a window.
///
/// Spike result (WebKitGTK 6, Wayland): a `WebView` that is never added to a
/// window runs Mermaid with correct `getBBox` sizes, so `Auto` and `Never`
/// never show anything. `Auto` keeps a safety net: diagrams whose SVG comes
/// back with a degenerate size are rendered again in a 1x1 window of class
/// `limpid-helper`, which needs a Hyprland rule like the one for `limpid-test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Auto,
    Never,
    Always,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HelperSettings {
    pub enabled: bool,
    pub timeout: Duration,
    pub window: WindowMode,
}

impl Default for HelperSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS),
            window: WindowMode::Auto,
        }
    }
}

impl HelperSettings {
    /// Read the `[mermaid]` table of a `config.toml` source.
    pub fn parse(source: &str) -> Result<Self, Box<dyn Error>> {
        let root: toml::Value = toml::from_str(source)?;
        let mut settings = Self::default();
        let Some(table) = root.get("mermaid") else {
            return Ok(settings);
        };
        let table = table.as_table().ok_or("mermaid must be a table")?;
        if let Some(value) = table.get("enabled") {
            settings.enabled = value
                .as_bool()
                .ok_or("mermaid.enabled must be true or false")?;
        }
        if let Some(value) = table.get("timeout_seconds") {
            let seconds = value
                .as_float()
                .or_else(|| value.as_integer().map(|integer| integer as f64))
                .ok_or("mermaid.timeout_seconds must be a number")?;
            if !seconds.is_finite() || seconds <= 0.0 {
                return Err("mermaid.timeout_seconds must be greater than zero".into());
            }
            settings.timeout = Duration::from_secs_f64(seconds);
        }
        if let Some(value) = table.get("helper_window") {
            settings.window = match value.as_str() {
                Some("auto") => WindowMode::Auto,
                Some("never") => WindowMode::Never,
                Some("always") => WindowMode::Always,
                _ => {
                    return Err(
                        "mermaid.helper_window must be \"auto\", \"never\" or \"always\"".into(),
                    );
                }
            };
        }
        Ok(settings)
    }

    /// Load from the user's `config.toml`; a missing file means defaults.
    pub fn load_default() -> Result<Self, Box<dyn Error>> {
        let Some(directory) = limpid_core::settings::Settings::config_dir() else {
            return Ok(Self::default());
        };
        match fs::read_to_string(directory.join("config.toml")) {
            Ok(source) => Self::parse(&source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }
}

/// The distinct Mermaid fences of `markdown`, in document order.
pub fn collect_diagrams(markdown: &str) -> Vec<Diagram> {
    let settings = limpid_core::settings::Settings::default();
    let doc = limpid_core::doc::parse(markdown, limpid_core::doc::Mode::File, &settings);
    let mut seen = HashSet::new();
    let mut diagrams = Vec::new();
    collect_from_blocks(&doc.blocks, &mut seen, &mut diagrams);
    diagrams
}

fn collect_from_blocks(
    blocks: &[limpid_core::doc::Block],
    seen: &mut HashSet<String>,
    diagrams: &mut Vec<Diagram>,
) {
    use limpid_core::doc::BlockKind;
    for block in blocks {
        match &block.kind {
            BlockKind::Mermaid { source, hash } => {
                if seen.insert(hash.clone()) {
                    diagrams.push(Diagram {
                        hash: hash.clone(),
                        source: source.clone(),
                    });
                }
            }
            BlockKind::List { items, .. } => {
                for item in items {
                    collect_from_blocks(&item.blocks, seen, diagrams);
                }
            }
            BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
                collect_from_blocks(blocks, seen, diagrams);
            }
            _ => {}
        }
    }
}

/// Why a rendered SVG was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgProblem {
    NotSvg,
    TooLarge,
    ForeignObject,
    /// The `viewBox` is zero, negative, NaN or infinite: layout never ran.
    DegenerateSize,
}

impl fmt::Display for SvgProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotSvg => "output is not an SVG document",
            Self::TooLarge => "output is larger than 2 MB",
            Self::ForeignObject => "output contains foreignObject",
            Self::DegenerateSize => "output has a degenerate size",
        })
    }
}

/// Check that an SVG can be drawn by `resvg` and was laid out for real.
pub fn validate_svg(svg: &str) -> Result<(), SvgProblem> {
    if svg.len() > MAX_SVG_BYTES {
        return Err(SvgProblem::TooLarge);
    }
    if !svg.trim_start().starts_with("<svg") {
        return Err(SvgProblem::NotSvg);
    }
    if svg.contains("foreignObject") {
        return Err(SvgProblem::ForeignObject);
    }
    match view_box_size(svg) {
        Some((width, height)) if !(width.is_finite() && height.is_finite()) => {
            Err(SvgProblem::DegenerateSize)
        }
        Some((width, height)) if width <= 0.0 || height <= 0.0 => Err(SvgProblem::DegenerateSize),
        Some(_) => Ok(()),
        None if has_view_box(svg) => Err(SvgProblem::DegenerateSize),
        None => Ok(()),
    }
}

fn has_view_box(svg: &str) -> bool {
    svg.find("viewBox=\"").is_some()
}

/// Width and height of the root `viewBox`, `None` when absent or unparsable.
fn view_box_size(svg: &str) -> Option<(f64, f64)> {
    let tag_end = svg.find('>')?;
    let tag = &svg[..tag_end];
    let start = tag.find("viewBox=\"")? + "viewBox=\"".len();
    let value = &tag[start..];
    let value = &value[..value.find('"')?];
    let numbers: Vec<f64> = value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if numbers.len() != 4 {
        return None;
    }
    Some((numbers[2], numbers[3]))
}

/// Mermaid has no JS-visible config hook in the vendored bundle, so the
/// label mode travels in an init directive placed after any frontmatter.
pub fn prepare_source(source: &str) -> String {
    const DIRECTIVE: &str =
        "%%{init: {\"htmlLabels\": false, \"flowchart\": {\"htmlLabels\": false}}}%%\n";
    let trimmed = source.trim_start_matches(['\u{feff}', '\n', '\r', ' ', '\t']);
    if let Some(after_open) = trimmed.strip_prefix("---")
        && after_open.starts_with(['\n', '\r'])
        && let Some(close) = after_open.find("\n---")
    {
        let frontmatter_end = "---".len() + close + "\n---".len();
        let rest = &trimmed[frontmatter_end..];
        let line_end = rest.find('\n').map_or(rest.len(), |index| index + 1);
        let (head, tail) = trimmed.split_at(frontmatter_end + line_end);
        let separator = if head.ends_with('\n') { "" } else { "\n" };
        return format!("{head}{separator}{DIRECTIVE}{tail}");
    }
    format!("{DIRECTIVE}{source}")
}

/// Escape JSON for embedding inside a `<script>` element.
fn script_safe_json(value: &serde_json::Value) -> String {
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// The helper page: loads the bundle, renders each job in turn and posts one
/// message per diagram and a final `{done: true}`.
pub fn build_page(script_uri: &str, diagrams: &[Diagram]) -> String {
    let jobs: Vec<serde_json::Value> = diagrams
        .iter()
        .map(|diagram| {
            serde_json::json!({
                "hash": diagram.hash,
                "source": prepare_source(&diagram.source),
            })
        })
        .collect();
    let jobs = script_safe_json(&serde_json::Value::Array(jobs));
    let script_uri = serde_json::Value::String(script_uri.to_owned()).to_string();
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<body style="margin:0">
<script>
const jobs = {jobs};
const post = (message) => window.webkit.messageHandlers["{MESSAGE_HANDLER}"].postMessage(message);
const load = () => new Promise((resolve, reject) => {{
  const script = document.createElement("script");
  script.src = {script_uri};
  script.onload = resolve;
  script.onerror = () => reject(new Error("could not load mermaid.min.js"));
  document.head.append(script);
}});
(async () => {{
  try {{
    await load();
    if (!window.limpidRenderMermaid) throw new Error("renderer is missing from the bundle");
  }} catch (error) {{
    post({{ done: true, fatal: String(error && error.message || error) }});
    return;
  }}
  for (const [index, job] of jobs.entries()) {{
    try {{
      const svg = await window.limpidRenderMermaid("limpid-mermaid-" + index, job.source);
      post({{ hash: job.hash, svg }});
    }} catch (error) {{
      post({{ hash: job.hash, error: String(error && error.message || error) }});
    }}
  }}
  post({{ done: true }});
}})();
</script>
"#
    )
}

fn cache_root() -> PathBuf {
    crate::cache_dir()
}

fn mermaid_cache_directory() -> PathBuf {
    cache_root().join("mermaid")
}

/// True when the cache already holds a usable SVG for `hash`.
fn is_cached(hash: &str) -> bool {
    let path = mermaid_cache_directory().join(format!("{hash}.svg"));
    fs::read_to_string(path).is_ok_and(|svg| svg.trim_start().starts_with("<svg"))
}

fn ensure_script_asset() -> Result<PathBuf, Box<dyn Error>> {
    let assets = cache_root().join("assets");
    fs::create_dir_all(&assets)?;
    let name = format!(
        "{}.js",
        limpid_core::replies::source_hash(MERMAID_SOURCE.as_bytes())
    );
    let path = assets.join(name);
    if !path.exists() {
        fs::write(&path, MERMAID_SOURCE)?;
    }
    Ok(path)
}

/// Per-run directory removed on drop, so the helper leaves no files behind.
struct WorkDirectory(PathBuf);

impl WorkDirectory {
    fn create() -> Result<Self, Box<dyn Error>> {
        let path = cache_root().join(format!("mermaid-helper-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for WorkDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
enum Outcome {
    Svg(String),
    Failed(String),
}

type Finish = Box<dyn FnOnce(Vec<(Diagram, Outcome)>)>;

/// Render `diagrams` in one web view. `finish` runs exactly once, with one
/// outcome per diagram, when the page reports done, the page fails, or the
/// timeout passes.
fn run_pass(
    app: &gtk4::Application,
    page: &Path,
    diagrams: Vec<Diagram>,
    windowed: bool,
    timeout: Duration,
    finish: Finish,
) {
    let manager = webkit6::UserContentManager::new();
    manager.register_script_message_handler(MESSAGE_HANDLER, None);
    let view = webkit6::WebView::builder()
        .user_content_manager(&manager)
        .build();
    let window = windowed.then(|| {
        let window = gtk4::ApplicationWindow::builder()
            .application(app)
            .default_width(1)
            .default_height(1)
            .decorated(false)
            .focusable(false)
            .child(&view)
            .build();
        window.present();
        window
    });

    let results: Rc<RefCell<Vec<(Diagram, Outcome)>>> = Rc::default();
    let finished = Rc::new(Cell::new(false));
    let finish: Rc<RefCell<Option<Finish>>> = Rc::new(RefCell::new(Some(finish)));
    let expected = diagrams;
    let known = expected.clone();
    let conclude = {
        let results = results.clone();
        let finished = finished.clone();
        let view = view.clone();
        let window = window.clone();
        move |reason: &str| {
            if finished.replace(true) {
                return;
            }
            let mut outcomes = results.borrow().clone();
            for diagram in &expected {
                if outcomes.iter().any(|(done, _)| done.hash == diagram.hash) {
                    continue;
                }
                outcomes.push((diagram.clone(), Outcome::Failed(reason.to_owned())));
            }
            view.terminate_web_process();
            if let Some(window) = &window {
                window.destroy();
            }
            if let Some(finish) = finish.borrow_mut().take() {
                finish(outcomes);
            }
        }
    };
    let conclude = Rc::new(conclude);

    let message_results = results.clone();
    let message_conclude = conclude.clone();
    let known = Rc::new(known);
    manager.connect_script_message_received(Some(MESSAGE_HANDLER), move |_, value| {
        let Some(json) = value.to_json(0) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&json) else {
            return;
        };
        if message.get("done").and_then(serde_json::Value::as_bool) == Some(true) {
            let reason = message
                .get("fatal")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("renderer finished without a result");
            message_conclude(reason);
            return;
        }
        let Some(hash) = message.get("hash").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(diagram) = known.iter().find(|diagram| diagram.hash == hash) else {
            return;
        };
        let outcome = match (
            message.get("svg").and_then(serde_json::Value::as_str),
            message.get("error").and_then(serde_json::Value::as_str),
        ) {
            (Some(svg), _) => Outcome::Svg(svg.to_owned()),
            (None, Some(error)) => Outcome::Failed(error.to_owned()),
            (None, None) => Outcome::Failed("renderer sent an empty result".to_owned()),
        };
        message_results
            .borrow_mut()
            .push((diagram.clone(), outcome));
    });
    let failure_conclude = conclude.clone();
    view.connect_load_failed(move |_, _, uri, error| {
        failure_conclude(&format!("could not load {uri}: {error}"));
        true
    });
    let crash_conclude = conclude.clone();
    view.connect_web_process_terminated(move |_, reason| {
        crash_conclude(&format!("web process terminated: {reason:?}"));
    });
    let timeout_conclude = conclude;
    glib::timeout_add_local_once(timeout, move || timeout_conclude("timed out"));
    view.load_uri(&gio::File::for_path(page).uri());
}

fn write_page(
    work: &WorkDirectory,
    name: &str,
    script: &Path,
    diagrams: &[Diagram],
) -> Result<PathBuf, Box<dyn Error>> {
    let page = work.0.join(format!("{name}.html"));
    let script_uri = gio::File::for_path(script).uri();
    fs::write(&page, build_page(script_uri.as_str(), diagrams))?;
    Ok(page)
}

/// Accept a rendered SVG into the cache or classify the failure.
fn store(hash: &str, svg: &str) -> Result<(), SvgProblem> {
    validate_svg(svg)?;
    if let Err(error) = crate::cache_mermaid(hash, svg) {
        eprintln!("limpid-host: could not cache Mermaid diagram {hash}: {error}");
        return Err(SvgProblem::NotSvg);
    }
    Ok(())
}

/// Entry point for `--render-mermaid FILE`. Prints the report; exits the
/// process with status 1 when nothing could be rendered.
pub fn run_cli(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let [file] = arguments else {
        return Err("usage: limpid-host --render-mermaid FILE.md".into());
    };
    let settings = HelperSettings::load_default()?;
    let markdown = fs::read_to_string(file)?;
    let report = render_file(&markdown, &settings)?;
    println!("{report}");
    // Exit explicitly so no WebKit child outlives the helper.
    std::process::exit(report.exit_code());
}

/// Render every uncached diagram of `markdown`.
fn render_file(markdown: &str, settings: &HelperSettings) -> Result<Report, Box<dyn Error>> {
    if !settings.enabled {
        return Ok(Report::default());
    }
    let mut report = Report::default();
    let mut pending = Vec::new();
    for diagram in collect_diagrams(markdown) {
        if is_cached(&diagram.hash) {
            report.cached += 1;
            continue;
        }
        pending.push(diagram);
    }
    if pending.is_empty() {
        return Ok(report);
    }
    configure_environment();
    let report = Rc::new(RefCell::new(report));
    let work = Rc::new(WorkDirectory::create()?);
    let script = ensure_script_asset()?;
    let first_windowed = settings.window == WindowMode::Always;
    let allow_fallback = settings.window == WindowMode::Auto;
    let timeout = settings.timeout;
    let started = std::time::Instant::now();

    let class = std::env::var("LIMPID_WINDOW_CLASS").unwrap_or(DEFAULT_WINDOW_CLASS.to_owned());
    if settings.window != WindowMode::Never {
        glib::set_prgname(Some(&class));
    }
    let app = gtk4::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let activation_report = report.clone();
    let activation_work = work.clone();
    app.connect_activate(move |app| {
        let hold = app.hold();
        let first_page = match write_page(&activation_work, "first", &script, &pending) {
            Ok(page) => page,
            Err(error) => {
                eprintln!("limpid-host: {error}");
                activation_report.borrow_mut().failed += pending.len();
                drop(hold);
                return;
            }
        };
        let app_for_retry = app.clone();
        let report_for_first = activation_report.clone();
        let work_for_retry = activation_work.clone();
        let script_for_retry = script.clone();
        run_pass(
            app,
            &first_page,
            pending.clone(),
            first_windowed,
            timeout,
            Box::new(move |outcomes| {
                let mut retry = Vec::new();
                for (diagram, outcome) in outcomes {
                    let problem = match outcome {
                        Outcome::Svg(svg) => match store(&diagram.hash, &svg) {
                            Ok(()) => {
                                report_for_first.borrow_mut().rendered += 1;
                                continue;
                            }
                            Err(problem) => problem.to_string(),
                        },
                        Outcome::Failed(reason) => reason,
                    };
                    let degenerate = problem == SvgProblem::DegenerateSize.to_string();
                    if allow_fallback && !first_windowed && degenerate {
                        retry.push(diagram);
                        continue;
                    }
                    eprintln!("limpid-host: diagram {}: {problem}", diagram.hash);
                    report_for_first.borrow_mut().failed += 1;
                }
                if retry.is_empty() {
                    drop(hold);
                    return;
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                let retry_page =
                    match write_page(&work_for_retry, "retry", &script_for_retry, &retry) {
                        Ok(page) => page,
                        Err(error) => {
                            eprintln!("limpid-host: {error}");
                            report_for_first.borrow_mut().failed += retry.len();
                            drop(hold);
                            return;
                        }
                    };
                let report_for_retry = report_for_first.clone();
                run_pass(
                    &app_for_retry,
                    &retry_page,
                    retry,
                    true,
                    remaining.max(Duration::from_secs(1)),
                    Box::new(move |outcomes| {
                        for (diagram, outcome) in outcomes {
                            let stored = match outcome {
                                Outcome::Svg(svg) => store(&diagram.hash, &svg),
                                Outcome::Failed(reason) => {
                                    eprintln!("limpid-host: diagram {}: {reason}", diagram.hash);
                                    report_for_retry.borrow_mut().failed += 1;
                                    continue;
                                }
                            };
                            match stored {
                                Ok(()) => report_for_retry.borrow_mut().rendered += 1,
                                Err(problem) => {
                                    eprintln!("limpid-host: diagram {}: {problem}", diagram.hash);
                                    report_for_retry.borrow_mut().failed += 1;
                                }
                            }
                        }
                        drop(hold);
                    }),
                );
            }),
        );
    });
    app.run_with_args(&["limpid-host"]);
    let final_report = *report.borrow();
    Ok(final_report)
}

fn configure_environment() {
    if std::env::var("LIMPID_JIT").as_deref() != Ok("1") {
        // SAFETY: no other threads exist yet, so the environment is not read
        // concurrently.
        unsafe {
            std::env::set_var("JSC_useJIT", "0");
        }
    }
    if std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_none() {
        // SAFETY: as above.
        unsafe {
            std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_SVG: &str =
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 120 40\"><g/></svg>";

    #[test]
    fn should_collect_each_distinct_diagram_once_when_fences_repeat() {
        let markdown = "```mermaid\ngraph TD; A-->B\n```\n\ntext\n\n```mermaid\ngraph TD; A-->B\n```\n\n```mermaid\ngraph LR; X-->Y\n```\n";
        let diagrams = collect_diagrams(markdown);
        assert_eq!(diagrams.len(), 2);
        assert_eq!(diagrams[0].source.trim(), "graph TD; A-->B");
        assert_eq!(diagrams[1].source.trim(), "graph LR; X-->Y");
    }

    #[test]
    fn should_collect_diagrams_inside_lists_and_quotes_when_nested() {
        let markdown = "- item\n\n  ```mermaid\n  graph TD; A-->B\n  ```\n\n> ```mermaid\n> graph LR; C-->D\n> ```\n";
        assert_eq!(collect_diagrams(markdown).len(), 2);
    }

    #[test]
    fn should_collect_nothing_when_document_has_no_mermaid() {
        assert!(collect_diagrams("# Title\n\n```rust\nfn main() {}\n```\n").is_empty());
        assert!(collect_diagrams("").is_empty());
    }

    #[test]
    fn should_key_diagrams_by_source_hash_when_collecting() {
        let diagrams = collect_diagrams("```mermaid\ngraph TD; A-->B\n```\n");
        assert_eq!(
            diagrams[0].hash,
            limpid_core::replies::source_hash(diagrams[0].source.as_bytes())
        );
    }

    #[test]
    fn should_accept_svg_when_it_is_sized_and_has_no_foreign_object() {
        assert_eq!(validate_svg(GOOD_SVG), Ok(()));
    }

    #[test]
    fn should_reject_svg_when_it_contains_foreign_object() {
        let svg = "<svg viewBox=\"0 0 10 10\"><foreignObject/></svg>";
        assert_eq!(validate_svg(svg), Err(SvgProblem::ForeignObject));
    }

    #[test]
    fn should_reject_output_when_it_is_not_svg() {
        assert_eq!(validate_svg("<div>hi</div>"), Err(SvgProblem::NotSvg));
        assert_eq!(validate_svg(""), Err(SvgProblem::NotSvg));
    }

    #[test]
    fn should_reject_svg_when_view_box_is_degenerate() {
        for view_box in [
            "0 0 0 0",
            "0 0 -5 10",
            "0 0 NaN 10",
            "0 0 inf 10",
            "garbage",
        ] {
            let svg = format!("<svg viewBox=\"{view_box}\"></svg>");
            assert_eq!(
                validate_svg(&svg),
                Err(SvgProblem::DegenerateSize),
                "{view_box}"
            );
        }
    }

    #[test]
    fn should_reject_svg_when_it_exceeds_the_size_limit() {
        let svg = format!(
            "<svg viewBox=\"0 0 1 1\">{}</svg>",
            "a".repeat(MAX_SVG_BYTES)
        );
        assert_eq!(validate_svg(&svg), Err(SvgProblem::TooLarge));
    }

    #[test]
    fn should_put_directive_first_when_source_has_no_frontmatter() {
        let prepared = prepare_source("graph TD; A-->B");
        assert!(prepared.starts_with("%%{init:"));
        assert!(prepared.contains("\"htmlLabels\": false"));
        assert!(prepared.ends_with("graph TD; A-->B"));
    }

    #[test]
    fn should_put_directive_after_frontmatter_when_source_has_one() {
        let prepared = prepare_source("---\ntitle: T\n---\ngraph TD; A-->B\n");
        assert!(prepared.starts_with("---\ntitle: T\n---\n%%{init:"));
        assert!(prepared.ends_with("graph TD; A-->B\n"));
    }

    #[test]
    fn should_keep_page_inert_when_source_contains_script_close() {
        let diagram = Diagram {
            hash: "h".into(),
            source: "graph TD; A[\"</script><script>alert(1)</script>\"]".into(),
        };
        let page = build_page("file:///m.js", &[diagram]);
        assert_eq!(page.matches("</script>").count(), 1);
    }

    #[test]
    fn should_print_counts_when_displaying_a_report() {
        let report = Report {
            rendered: 2,
            cached: 1,
            failed: 3,
        };
        assert_eq!(report.to_string(), "rendered 2 cached 1 failed 3");
    }

    #[test]
    fn should_exit_nonzero_only_when_nothing_was_rendered_or_cached() {
        let exit = |rendered, cached, failed| {
            Report {
                rendered,
                cached,
                failed,
            }
            .exit_code()
        };
        assert_eq!(exit(0, 0, 2), 1);
        assert_eq!(exit(1, 0, 2), 0);
        assert_eq!(exit(0, 2, 1), 0);
        assert_eq!(exit(0, 0, 0), 0);
    }

    #[test]
    fn should_use_defaults_when_config_has_no_mermaid_table() {
        assert_eq!(
            HelperSettings::parse("").unwrap(),
            HelperSettings::default()
        );
        assert_eq!(HelperSettings::default().timeout, Duration::from_secs(20));
    }

    #[test]
    fn should_read_helper_keys_when_present() {
        let settings = HelperSettings::parse(
            "[mermaid]\nenabled = false\ntimeout_seconds = 5\nhelper_window = \"never\"\n",
        )
        .unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.timeout, Duration::from_secs(5));
        assert_eq!(settings.window, WindowMode::Never);
    }

    #[test]
    fn should_reject_settings_when_values_are_invalid() {
        for source in [
            "[mermaid]\ntimeout_seconds = 0",
            "[mermaid]\ntimeout_seconds = -3",
            "[mermaid]\ntimeout_seconds = \"20\"",
            "[mermaid]\nenabled = \"yes\"",
            "[mermaid]\nhelper_window = \"sometimes\"",
            "mermaid = 3",
        ] {
            assert!(HelperSettings::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn should_ignore_other_keys_when_reading_helper_settings() {
        let settings = HelperSettings::parse("[mermaid]\nfuture_key = 1\n[view]\nfont = \"x\"\n");
        assert!(settings.is_ok());
    }

    #[test]
    fn should_do_nothing_when_helper_is_disabled() {
        let settings = HelperSettings {
            enabled: false,
            ..HelperSettings::default()
        };
        let report = render_file("```mermaid\ngraph TD; A-->B\n```\n", &settings).unwrap();
        assert_eq!(report, Report::default());
    }

    #[test]
    fn should_do_nothing_when_document_has_no_diagrams() {
        let report = render_file("# plain\n", &HelperSettings::default()).unwrap();
        assert_eq!(report, Report::default());
    }
}
