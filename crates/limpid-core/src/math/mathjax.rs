//! MathJax backend: the vendored MathJax bundle (`vendor/mathjax.min.js`, see
//! `vendor/README.md`) runs under QuickJS, so typesetting needs no TeX, Typst
//! or Node install.
//!
//! The bundle is compiled into the binary and evaluated once per backend; the
//! engine is kept between `render` calls, so the single-formula retries that
//! follow a failed batch do not pay for a second evaluation. MathJax reports
//! sizes in `ex`. One `ex` is `EX_PER_EM` of the font size, which is 8.5 px at
//! the default 17 px (`view.font_size`), and the SVG is rewritten to px. The
//! baseline is the distance from the top of the SVG to the text baseline,
//! `height + vertical-align` in ex, for inline and display formulas alike.

use super::{Backend, BackendError, Formula, MathSettings, Rendered};
use rquickjs::{Context, Function, Runtime};
use serde::Deserialize;
use std::{
    cell::RefCell,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const BUNDLE: &str = include_str!("../../../../vendor/mathjax.min.js");

/// Name of the global function the bundle defines.
const ENTRY_POINT: &str = "limpidTex2Svg";

/// Height of an `ex` as a fraction of the font size: 8.5 px at 17 px.
pub const EX_PER_EM: f32 = 0.5;

/// The font size used when the caller does not supply a usable
/// `view.font_size`.
pub const DEFAULT_FONT_SIZE_PX: f32 = 17.0;

/// Heap limit of the JavaScript engine.
const MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// Stack limit of the JavaScript engine, below the 2 MiB of a test thread so
/// deep TeX nesting hits this and fails
/// the formula instead of the process.
const STACK_LIMIT_BYTES: usize = 1024 * 1024;

struct Engine {
    // The runtime must outlive the context.
    _runtime: Runtime,
    context: Context,
    expired: Arc<AtomicBool>,
}

pub struct MathJaxBackend {
    ex_px: f32,
    timeout: Duration,
    engine: RefCell<Option<Engine>>,
}

impl MathJaxBackend {
    /// `font_size_px` is `view.font_size`; the time limit of one `render`
    /// call is `math.timeout_seconds`.
    pub fn new(font_size_px: f32, settings: &MathSettings) -> Self {
        let font_size = if font_size_px.is_finite() && font_size_px > 0.0 {
            font_size_px
        } else {
            DEFAULT_FONT_SIZE_PX
        };
        Self {
            ex_px: font_size * EX_PER_EM,
            timeout: settings.timeout(),
            engine: RefCell::new(None),
        }
    }

    pub fn ex_px(&self) -> f32 {
        self.ex_px
    }

    fn start_engine() -> Result<Engine, BackendError> {
        let runtime = Runtime::new().map_err(js_error)?;
        runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
        runtime.set_max_stack_size(STACK_LIMIT_BYTES);
        let expired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&expired);
        runtime.set_interrupt_handler(Some(Box::new(move || flag.load(Ordering::Relaxed))));
        let context = Context::full(&runtime).map_err(js_error)?;
        context
            .with(|ctx| {
                ctx.eval::<(), _>(BUNDLE)
                    .map_err(|error| describe(&ctx, error))
            })
            .map_err(|reason| BackendError(format!("cannot load the MathJax bundle: {reason}")))?;
        Ok(Engine {
            _runtime: runtime,
            context,
            expired,
        })
    }

    fn typeset(
        &self,
        engine: &Engine,
        formula: &Formula,
        deadline: Instant,
    ) -> Result<Typeset, BackendError> {
        engine.expired.store(false, Ordering::Relaxed);
        let watchdog = Watchdog::start(Arc::clone(&engine.expired), deadline);
        let reply = engine.context.with(|ctx| {
            let function: Function = ctx
                .globals()
                .get(ENTRY_POINT)
                .map_err(|error| describe(&ctx, error))?;
            function
                .call::<_, String>((formula.latex.as_str(), formula.display))
                .map_err(|error| describe(&ctx, error))
        });
        let timed_out = watchdog.finish();
        let reply = reply.map_err(|reason| {
            if timed_out {
                BackendError(format!(
                    "MathJax timed out after {} s on {:?}",
                    self.timeout.as_secs(),
                    formula.latex
                ))
            } else {
                BackendError(format!("MathJax failed on {:?}: {reason}", formula.latex))
            }
        })?;
        parse_reply(&reply, &formula.latex)
    }
}

impl Backend for MathJaxBackend {
    fn name(&self) -> &str {
        "mathjax"
    }

    /// The bundle is part of the binary, so MathJax is always available.
    fn available(&self, _settings: &MathSettings) -> bool {
        true
    }

    fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError> {
        let mut slot = self.engine.borrow_mut();
        if slot.is_none() {
            *slot = Some(Self::start_engine()?);
        }
        let Some(engine) = slot.as_ref() else {
            return Err(BackendError("the MathJax engine did not start".into()));
        };
        let deadline = Instant::now() + self.timeout;
        let mut results = Vec::with_capacity(batch.len());
        for formula in batch {
            let typeset = self.typeset(engine, formula, deadline)?;
            let svg_text = svg_in_px(&typeset.svg, &typeset, self.ex_px).ok_or_else(|| {
                BackendError(format!("MathJax returned no <svg> for {:?}", formula.latex))
            })?;
            let destination = out.join(format!("{}.svg", formula.hash));
            std::fs::write(&destination, svg_text)?;
            let height = typeset.height_ex * self.ex_px;
            results.push(Rendered {
                svg: destination,
                width: typeset.width_ex * self.ex_px,
                height,
                baseline: (typeset.baseline_ex * self.ex_px).clamp(0.0, height),
                backend: "mathjax".into(),
            });
        }
        Ok(results)
    }
}

/// Flips `expired` once `deadline` passes, unless finished first.
struct Watchdog {
    expired: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn start(expired: Arc<AtomicBool>, deadline: Instant) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&done);
        let flag = Arc::clone(&expired);
        let thread = std::thread::spawn(move || {
            while !watched.load(Ordering::Relaxed) {
                if Instant::now() >= deadline {
                    flag.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Self {
            expired,
            done,
            thread: Some(thread),
        }
    }

    /// Stop watching; true if the deadline fired.
    fn finish(mut self) -> bool {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.expired.load(Ordering::Relaxed)
    }
}

fn js_error(error: rquickjs::Error) -> BackendError {
    BackendError(format!("QuickJS: {error}"))
}

/// The message of a JavaScript exception, if one is pending.
fn describe(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    let exception = ctx.catch();
    if let Some(object) = exception.as_exception() {
        return object.message().unwrap_or_else(|| error.to_string());
    }
    exception
        .into_string()
        .and_then(|text| text.to_string().ok())
        .unwrap_or_else(|| error.to_string())
}

/// What `limpidTex2Svg` returned, sizes in ex.
#[derive(Debug, Clone, PartialEq)]
pub struct Typeset {
    pub svg: String,
    pub width_ex: f32,
    pub height_ex: f32,
    pub baseline_ex: f32,
}

#[derive(Deserialize)]
struct Reply {
    svg: Option<String>,
    #[serde(rename = "widthEx")]
    width_ex: Option<f32>,
    #[serde(rename = "heightEx")]
    height_ex: Option<f32>,
    #[serde(rename = "baselineEx")]
    baseline_ex: Option<f32>,
    error: Option<String>,
}

/// Parse the JSON reply of the bundle. A TeX error becomes an error naming the
/// formula; a reply with missing or unusable sizes is rejected.
pub fn parse_reply(reply: &str, latex: &str) -> Result<Typeset, BackendError> {
    let parsed: Reply = serde_json::from_str(reply)
        .map_err(|error| BackendError(format!("unreadable MathJax reply: {error}")))?;
    if let Some(message) = parsed.error {
        return Err(BackendError(format!("TeX error in {latex:?}: {message}")));
    }
    let (Some(svg), Some(width_ex), Some(height_ex), Some(baseline_ex)) = (
        parsed.svg,
        parsed.width_ex,
        parsed.height_ex,
        parsed.baseline_ex,
    ) else {
        return Err(BackendError(format!(
            "incomplete MathJax reply for {latex:?}"
        )));
    };
    let finite = [width_ex, height_ex, baseline_ex]
        .iter()
        .all(|value| value.is_finite());
    if !finite || width_ex <= 0.0 || height_ex <= 0.0 {
        return Err(BackendError(format!(
            "MathJax gave {latex:?} no usable size"
        )));
    }
    Ok(Typeset {
        svg,
        width_ex,
        height_ex,
        baseline_ex,
    })
}

/// The SVG with its root `width`/`height` in px and the `vertical-align`
/// style removed (the baseline lives in the sidecar). `None` when the text has
/// no root `<svg>` tag.
pub fn svg_in_px(svg: &str, typeset: &Typeset, ex_px: f32) -> Option<String> {
    let start = svg.find("<svg")?;
    let tag_end = svg[start..].find('>')? + start;
    let tag = &svg[start..tag_end];
    let width = format!("{:.3}", typeset.width_ex * ex_px);
    let height = format!("{:.3}", typeset.height_ex * ex_px);
    let tag = set_attribute(tag, "width", Some(&width));
    let tag = set_attribute(&tag, "height", Some(&height));
    let tag = set_attribute(&tag, "style", None);
    Some(format!("{}{}{}", &svg[..start], tag, &svg[tag_end..]))
}

/// Replace (`Some`) or remove (`None`) a double-quoted attribute of one tag.
fn set_attribute(tag: &str, name: &str, value: Option<&str>) -> String {
    let needle = format!(" {name}=\"");
    let Some(at) = tag.find(&needle) else {
        return match value {
            Some(value) => format!("{tag} {name}=\"{value}\""),
            None => tag.to_owned(),
        };
    };
    let value_start = at + needle.len();
    let Some(length) = tag[value_start..].find('"') else {
        return tag.to_owned();
    };
    let value_end = value_start + length + 1;
    let replacement = value
        .map(|value| format!(" {name}=\"{value}\""))
        .unwrap_or_default();
    format!("{}{}{}", &tag[..at], replacement, &tag[value_end..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{self, select_with_path};
    use std::{ffi::OsStr, path::PathBuf};

    fn formula(latex: &str, display: bool) -> Formula {
        Formula::new(latex, display)
    }

    fn backend() -> MathJaxBackend {
        MathJaxBackend::new(17.0, &MathSettings::default())
    }

    fn work_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("limpid-mathjax-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn should_load_the_bundle_when_the_engine_starts() {
        let engine = MathJaxBackend::start_engine().expect("bundle loads under QuickJS");
        let defined = engine
            .context
            .with(|ctx| ctx.globals().get::<_, Function>(ENTRY_POINT).is_ok());
        assert!(defined);
    }

    #[test]
    fn should_render_paths_when_the_formula_is_x_squared() {
        let dir = work_dir("squared");
        let x2 = formula("x^2", false);
        let results = backend().render(&[&x2], &dir).unwrap();
        let svg = std::fs::read_to_string(&results[0].svg).unwrap();
        assert!(svg.contains("<path"), "{svg}");
        assert!(!svg.contains("<use"), "fontCache none keeps glyphs inline");
        assert_eq!(results[0].backend, "mathjax");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_report_sizes_in_px_when_the_font_size_is_17() {
        let dir = work_dir("sizes");
        let x2 = formula("x^2", false);
        let rendered = backend().render(&[&x2], &dir).unwrap().remove(0);
        // MathJax: width 2.282ex, height 1.912ex, vertical-align -0.025ex.
        assert!((rendered.width - 2.282 * 8.5).abs() < 0.1, "{rendered:?}");
        assert!((rendered.height - 1.912 * 8.5).abs() < 0.1, "{rendered:?}");
        assert!(rendered.baseline > 0.0 && rendered.baseline < rendered.height);
        let svg = std::fs::read_to_string(&rendered.svg).unwrap();
        assert!(svg.contains("width=\"19.397\""), "{svg}");
        assert!(!svg.contains("vertical-align"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_scale_sizes_when_the_font_size_changes() {
        let dir = work_dir("scale");
        let x2 = formula("x^2", false);
        let small = backend().render(&[&x2], &dir).unwrap().remove(0);
        let large = MathJaxBackend::new(34.0, &MathSettings::default())
            .render(&[&x2], &dir)
            .unwrap()
            .remove(0);
        assert!((large.width / small.width - 2.0).abs() < 0.01);
        assert!((large.baseline / small.baseline - 2.0).abs() < 0.01);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_use_the_default_font_size_when_it_is_not_positive() {
        let settings = MathSettings::default();
        assert_eq!(MathJaxBackend::new(0.0, &settings).ex_px(), 8.5);
        assert_eq!(MathJaxBackend::new(f32::NAN, &settings).ex_px(), 8.5);
        assert_eq!(MathJaxBackend::new(-3.0, &settings).ex_px(), 8.5);
    }

    #[test]
    fn should_place_the_baseline_above_the_bottom_when_the_formula_has_a_descender() {
        let dir = work_dir("descender");
        let (deep, flat) = (formula("y_{j}", false), formula("x", false));
        let results = backend().render(&[&deep, &flat], &dir).unwrap();
        let depth = |r: &Rendered| r.height - r.baseline;
        assert!(depth(&results[0]) > depth(&results[1]), "{results:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_fail_naming_the_formula_when_the_tex_is_malformed() {
        let dir = work_dir("malformed");
        let bad = formula("\\frac{1", false);
        let error = backend().render(&[&bad], &dir).unwrap_err();
        assert!(error.0.contains("frac"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_render_the_good_formulas_when_a_batch_has_a_malformed_one() {
        let cache_dir = work_dir("batch-cache");
        let cache = math::cache::Cache::new(cache_dir.clone());
        let formulas = [
            formula("a+b", false),
            formula("\\undefinedcommand{", true),
            formula("\\sum_{i=1}^n i", true),
        ];
        let backend = backend();
        let report =
            math::render_missing_with(&cache, Some(&backend), &formulas, &MathSettings::default());
        assert_eq!(report.rendered, 2, "{report:?}");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(report.failed[0].0, formulas[1].hash);
        assert!(cache.get(&formulas[0].hash).is_some());
        assert!(cache.get(&formulas[2].hash).is_some());
        std::fs::remove_dir_all(cache_dir).unwrap();
    }

    #[test]
    fn should_render_when_the_formula_uses_amsmath_and_mhchem() {
        let dir = work_dir("packages");
        let formulas = [
            formula("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}", true),
            formula("\\ce{H2O}", false),
            formula("\\color{red}{x}", false),
        ];
        let refs: Vec<&Formula> = formulas.iter().collect();
        let results = backend().render(&refs, &dir).unwrap();
        assert_eq!(results.len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_survive_odd_input_when_the_formula_is_empty_or_huge_or_hostile() {
        let dir = work_dir("odd");
        let backend = backend();
        let nested = format!("{}x{}", "{".repeat(5000), "}".repeat(5000));
        let odd = [
            "",
            " ",
            "\\",
            "{",
            "}",
            "\\\\\\\\",
            "\u{0}",
            "\u{e9}\u{2211}",
            nested.as_str(),
        ];
        for latex in odd {
            let probe = formula(latex, false);
            // Either outcome is fine; panicking or hanging is not.
            let _ = backend.render(&[&probe], &dir);
        }
        let after = formula("x", false);
        assert!(
            backend.render(&[&after], &dir).is_ok(),
            "engine still usable"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_stop_when_the_formula_runs_past_the_time_limit() {
        let dir = work_dir("timeout");
        let settings = MathSettings {
            timeout_seconds: 1,
            ..MathSettings::default()
        };
        let backend = MathJaxBackend::new(17.0, &settings);
        // Exponential macro expansion: either MathJax's own limit or the
        // watchdog must end it.
        let bomb = formula(
            "\\def\\a{\\b\\b}\\def\\b{\\c\\c}\\def\\c{\\d\\d}\\def\\d{\\e\\e}\\def\\e{\\f\\f}\\def\\f{\\g\\g}\\def\\g{\\h\\h}\\def\\h{\\i\\i}\\def\\i{\\j\\j}\\def\\j{\\k\\k}\\def\\k{\\l\\l}\\def\\l{x}\\a",
            false,
        );
        let started = Instant::now();
        let _ = backend.render(&[&bomb], &dir);
        assert!(started.elapsed() < Duration::from_secs(30));
        let after = formula("x", false);
        assert!(
            backend.render(&[&after], &dir).is_ok(),
            "engine usable after a stop"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_reuse_the_engine_when_render_is_called_again() {
        let dir = work_dir("reuse");
        let backend = backend();
        let x = formula("x", false);
        backend.render(&[&x], &dir).unwrap();
        assert!(backend.engine.borrow().is_some());
        backend.render(&[&x], &dir).unwrap();
        assert!(backend.engine.borrow().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn should_select_mathjax_when_latex_and_typst_are_unavailable() {
        let settings = MathSettings::default();
        let empty = std::env::temp_dir().join("limpid-mathjax-empty-path");
        let backend = select_with_path(&settings, Some(OsStr::new(&empty)))
            .expect("mathjax is always available");
        assert_eq!(backend.name(), "mathjax");
    }

    #[test]
    fn should_select_mathjax_when_the_setting_names_it() {
        let settings = MathSettings {
            backend: "mathjax".into(),
            ..MathSettings::default()
        };
        let backend = select_with_path(&settings, None).unwrap();
        assert_eq!(backend.name(), "mathjax");
    }

    #[test]
    fn should_not_select_mathjax_when_the_backend_is_none() {
        let settings = MathSettings {
            backend: "none".into(),
            ..MathSettings::default()
        };
        assert!(select_with_path(&settings, None).is_none());
    }

    #[test]
    fn should_reject_the_reply_when_it_is_not_json() {
        assert!(parse_reply("not json", "x").is_err());
    }

    #[test]
    fn should_reject_the_reply_when_a_size_is_missing_or_not_positive() {
        let missing = r#"{"svg":"<svg/>","widthEx":1}"#;
        assert!(parse_reply(missing, "x").is_err());
        let zero = r#"{"svg":"<svg/>","widthEx":0,"heightEx":1,"baselineEx":0}"#;
        assert!(parse_reply(zero, "x").is_err());
        let ok = r#"{"svg":"<svg/>","widthEx":1,"heightEx":2,"baselineEx":1.5}"#;
        assert_eq!(parse_reply(ok, "x").unwrap().baseline_ex, 1.5);
    }

    #[test]
    fn should_surface_the_message_when_the_reply_is_an_error() {
        let error = parse_reply(r#"{"error":"Missing close brace"}"#, "\\frac{").unwrap_err();
        assert!(error.0.contains("Missing close brace"));
        assert!(error.0.contains("frac"));
    }

    #[test]
    fn should_rewrite_only_the_root_tag_when_converting_ex_to_px() {
        let svg = "<svg style=\"vertical-align: -1ex;\" width=\"2ex\" height=\"3ex\" viewBox=\"0 0 1 1\"><rect width=\"9ex\"/></svg>";
        let typeset = Typeset {
            svg: svg.into(),
            width_ex: 2.0,
            height_ex: 3.0,
            baseline_ex: 2.0,
        };
        let out = svg_in_px(svg, &typeset, 10.0).unwrap();
        assert_eq!(
            out,
            "<svg width=\"20.000\" height=\"30.000\" viewBox=\"0 0 1 1\"><rect width=\"9ex\"/></svg>"
        );
    }

    #[test]
    fn should_return_none_when_the_text_has_no_svg_root() {
        let typeset = Typeset {
            svg: String::new(),
            width_ex: 1.0,
            height_ex: 1.0,
            baseline_ex: 0.0,
        };
        assert!(svg_in_px("<div/>", &typeset, 8.5).is_none());
    }
}
