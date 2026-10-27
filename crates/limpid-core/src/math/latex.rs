//! TeX backend: one `preview` document per batch, compiled once, then split
//! into one SVG per page.
//!
//! Baseline: each inline formula is first set in an `\hbox`, whose height,
//! depth and width are written to the log as
//! `limpid-box N <ht> <dp> <wd>` (TeX points), then unboxed into the preview
//! environment. The `preview` package itself does not log snippet sizes in a
//! form that is stable across drivers. The SVG's own height minus the depth
//! gives the baseline offset from the top. Display formulas cannot live in a
//! box (`\tag`, `split` and the amsmath environments need a real display), so
//! they are set in the `preview` environment itself: bare math goes in
//! `\[ ... \]`, and a top-level amsmath environment stays as it is. Their
//! metrics come from the `lyx` option's `Preview: Snippet N ht dp wd` log lines
//! (scaled points).

use super::{
    Backend, BackendError, Formula, MathSettings, Rendered, find_binary, numbered_files,
    run_command, svgmeta, tail,
};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

const JOB: &str = "batch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Pdflatex,
    Latex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SvgTool {
    Dvisvgm,
    Pdftocairo,
}

struct Toolchain {
    engine: Engine,
    engine_path: PathBuf,
    svg_tool: SvgTool,
    svg_path: PathBuf,
}

pub struct LatexBackend {
    settings: MathSettings,
    path: Option<OsString>,
}

impl LatexBackend {
    pub fn new(settings: MathSettings, path: Option<OsString>) -> Self {
        Self { settings, path }
    }

    fn toolchain(&self, settings: &MathSettings) -> Option<Toolchain> {
        let path = self.path.as_deref();
        let find = |name: &str| find_binary(name, path);
        let engines: &[Engine] = match settings.latex.as_str() {
            "pdflatex" => &[Engine::Pdflatex],
            "latex" => &[Engine::Latex],
            _ => &[Engine::Pdflatex, Engine::Latex],
        };
        for &engine in engines {
            let engine_name = match engine {
                Engine::Pdflatex => "pdflatex",
                Engine::Latex => "latex",
            };
            let Some(engine_path) = find(engine_name) else {
                continue;
            };
            // DVI can only go through dvisvgm; PDF can use either tool.
            let tools: &[SvgTool] = match (engine, settings.svg.as_str()) {
                (Engine::Latex, "pdftocairo") => &[],
                (Engine::Latex, _) => &[SvgTool::Dvisvgm],
                (_, "dvisvgm") => &[SvgTool::Dvisvgm],
                (_, "pdftocairo") => &[SvgTool::Pdftocairo],
                _ => &[SvgTool::Dvisvgm, SvgTool::Pdftocairo],
            };
            for &svg_tool in tools {
                let tool_name = match svg_tool {
                    SvgTool::Dvisvgm => "dvisvgm",
                    SvgTool::Pdftocairo => "pdftocairo",
                };
                if let Some(svg_path) = find(tool_name) {
                    return Some(Toolchain {
                        engine,
                        engine_path,
                        svg_tool,
                        svg_path,
                    });
                }
            }
        }
        None
    }

    fn run(&self, mut command: Command, work: &Path) -> Result<super::CommandOutput, BackendError> {
        command
            .current_dir(work)
            .env("openin_any", "p")
            .env("openout_any", "p");
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        run_command(command, self.settings.timeout())
    }
}

impl Backend for LatexBackend {
    fn name(&self) -> &str {
        "latex"
    }

    fn available(&self, settings: &MathSettings) -> bool {
        self.toolchain(settings).is_some()
    }

    fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError> {
        let tools = self.toolchain(&self.settings).ok_or_else(|| {
            BackendError(
                "no TeX toolchain found (need pdflatex or latex, and dvisvgm or pdftocairo)".into(),
            )
        })?;
        let work = out.join("latex");
        std::fs::create_dir_all(&work)?;
        std::fs::write(work.join(format!("{JOB}.tex")), build_document(batch))?;

        let mut compile = Command::new(&tools.engine_path);
        compile.args([
            "-interaction=nonstopmode",
            "-halt-on-error",
            "-no-shell-escape",
            &format!("{JOB}.tex"),
        ]);
        let compiled = self.run(compile, &work)?;
        if !compiled.success {
            let log = std::fs::read_to_string(work.join(format!("{JOB}.log"))).unwrap_or_default();
            let detail = error_lines(&log).unwrap_or_else(|| tail(&compiled.stdout, 6));
            return Err(BackendError(format!(
                "{} failed: {detail}",
                tools.engine_path.display()
            )));
        }

        let pages = self.split_pages(&tools, &work, batch.len())?;
        let log = std::fs::read_to_string(work.join(format!("{JOB}.log"))).unwrap_or_default();
        let boxes = parse_box_log(&log);
        let snippets = parse_snippet_log(&log);
        let mut results = Vec::with_capacity(batch.len());
        for (index, (formula, page)) in batch.iter().zip(pages).enumerate() {
            let svg_text = std::fs::read_to_string(&page)?;
            let size = svgmeta::parse_svg_size(&svg_text).ok_or_else(|| {
                BackendError(format!("cannot read the size of {}", page.display()))
            })?;
            let destination = out.join(format!("{}.svg", formula.hash));
            std::fs::rename(&page, &destination)?;
            results.push(Rendered {
                svg: destination,
                width: size.width,
                height: size.height,
                baseline: baseline_from(
                    size.height,
                    if formula.display {
                        snippets.get(&index)
                    } else {
                        boxes.get(&index)
                    },
                ),
                backend: "latex".into(),
            });
        }
        let _ = std::fs::remove_dir_all(&work);
        Ok(results)
    }
}

impl LatexBackend {
    /// One SVG file per page, in page order; exactly `expected` of them.
    fn split_pages(
        &self,
        tools: &Toolchain,
        work: &Path,
        expected: usize,
    ) -> Result<Vec<PathBuf>, BackendError> {
        match tools.svg_tool {
            SvgTool::Pdftocairo => {
                // pdftocairo writes a single page per run.
                let mut pages = Vec::with_capacity(expected);
                for page in 1..=expected {
                    let mut command = Command::new(&tools.svg_path);
                    command.args([
                        "-svg",
                        "-f",
                        &page.to_string(),
                        "-l",
                        &page.to_string(),
                        &format!("{JOB}.pdf"),
                        &format!("page-{page}.svg"),
                    ]);
                    let output = self.run(command, work)?;
                    if !output.success {
                        return Err(BackendError(format!(
                            "pdftocairo failed on page {page}: {}",
                            tail(&output.stderr, 3)
                        )));
                    }
                    pages.push(work.join(format!("page-{page}.svg")));
                }
                // One page past the end must not exist, or pages and formulas
                // would be misaligned.
                let extra = expected + 1;
                let mut probe = Command::new(&tools.svg_path);
                probe.args([
                    "-svg",
                    "-f",
                    &extra.to_string(),
                    "-l",
                    &extra.to_string(),
                    &format!("{JOB}.pdf"),
                    "page-extra.svg",
                ]);
                let probed = self.run(probe, work)?;
                if probed.success && work.join("page-extra.svg").exists() {
                    return Err(BackendError(format!(
                        "the PDF has more than {expected} pages for {expected} formulas"
                    )));
                }
                Ok(pages)
            }
            SvgTool::Dvisvgm => {
                let mut command = Command::new(&tools.svg_path);
                command.args(["--page=1-", "--exact-bbox", "--output=page-%p.svg"]);
                if tools.engine == Engine::Pdflatex {
                    command.arg("--pdf");
                    command.arg(format!("{JOB}.pdf"));
                } else {
                    command.arg(format!("{JOB}.dvi"));
                }
                let output = self.run(command, work)?;
                if !output.success {
                    return Err(BackendError(format!(
                        "dvisvgm failed: {}",
                        tail(&output.stderr, 3)
                    )));
                }
                let files = numbered_files(work, "page-", ".svg");
                if files.len() != expected {
                    return Err(BackendError(format!(
                        "dvisvgm wrote {} pages for {expected} formulas",
                        files.len()
                    )));
                }
                Ok(files.into_iter().map(|(_, path)| path).collect())
            }
        }
    }
}

/// The `.tex` source for one batch. Page `i` (from 0) holds `batch[i]`.
pub fn build_document(batch: &[&Formula]) -> String {
    let mut document = String::from(
        "\\documentclass{article}\n\
         \\usepackage[active,tightpage,lyx]{preview}\n\
         \\usepackage{amsmath,amssymb}\n\
         \\PreviewBorder=0pt\n\
         \\newbox\\limpidbox\n\
         \\begin{document}\n",
    );
    for (index, formula) in batch.iter().enumerate() {
        // Trailing whitespace is trimmed (display math from the parser ends in a
        // newline, which would become a blank line, i.e. \par, inside math). The
        // newline before the closing delimiter keeps a trailing `%` comment in the
        // formula from swallowing it. Formulas that are safe to box are set in a
        // box so the page is exactly as wide as the formula, and the box gives
        // the baseline.
        let latex = formula.latex.trim_end();
        let layout = if formula.display {
            Layout::Bare(display_body(latex))
        } else {
            Layout::Boxed {
                open: "\\(",
                close: "\\)",
                body: latex.to_owned(),
            }
        };
        match layout {
            Layout::Boxed { open, close, body } => document.push_str(&format!(
                "\\begin{{preview}}\\setbox\\limpidbox\\hbox{{{open}{body}\n{close}}}\
                 \\typeout{{limpid-box {index} \\the\\ht\\limpidbox\\space\\the\\dp\\limpidbox\\space\\the\\wd\\limpidbox}}\
                 \\unhbox\\limpidbox\\end{{preview}}\n"
            )),
            Layout::Bare(body) => {
                document.push_str(&format!("\\begin{{preview}}\n{body}\n\\end{{preview}}\n"))
            }
        }
    }
    document.push_str("\\end{document}\n");
    document
}

/// How one formula is placed on its page.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Layout {
    /// Inline math, set in an `\hbox` (so its metrics are logged) between the
    /// delimiters.
    Boxed {
        open: &'static str,
        close: &'static str,
        body: String,
    },
    /// Display math, emitted as is inside the `preview` environment, in
    /// paragraph mode.
    Bare(String),
}

/// Environments that are display math by themselves and must stay top-level.
const DISPLAY_ENVIRONMENTS: &[&str] = &[
    "equation",
    "equation*",
    "align",
    "align*",
    "alignat",
    "alignat*",
    "xalignat",
    "xalignat*",
    "xxalignat",
    "flalign",
    "flalign*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "eqnarray",
    "eqnarray*",
    "displaymath",
];

/// The paragraph-mode source of one display formula.
fn display_body(latex: &str) -> String {
    if starts_with_display_environment(latex) {
        return latex.trim_start().to_owned();
    }
    if has_top_level_alignment(latex) {
        let name = if has_top_level(latex, '&') {
            "aligned"
        } else {
            "gathered"
        };
        return format!("\\[\\begin{{{name}}}{latex}\n\\end{{{name}}}\n\\]");
    }
    format!("\\[{latex}\n\\]")
}

fn starts_with_display_environment(latex: &str) -> bool {
    let Some(rest) = latex.trim_start().strip_prefix("\\begin{") else {
        return false;
    };
    let Some((name, _)) = rest.split_once('}') else {
        return false;
    };
    DISPLAY_ENVIRONMENTS.contains(&name)
}

fn has_top_level_alignment(latex: &str) -> bool {
    has_top_level(latex, '\\') || has_top_level(latex, '&')
}

/// Whether `target` (`\\` for a line break, or `&`) occurs outside every
/// brace group and `\begin..\end` environment. Escaped `\&` and `\%` comments
/// are skipped.
fn has_top_level(latex: &str, target: char) -> bool {
    let mut depth = 0usize;
    let mut chars = latex.char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        match character {
            '%' => while chars.next_if(|(_, next)| *next != '\n').is_some() {},
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            '&' if target == '&' && depth == 0 => return true,
            '\\' => {
                let rest = &latex[at + 1..];
                if let Some(after) = rest.strip_prefix("begin{") {
                    let Some(end) = after.find('}') else {
                        return false;
                    };
                    depth += 1;
                    skip_bytes(&mut chars, at + 1 + "begin{".len() + end);
                    continue;
                }
                if let Some(after) = rest.strip_prefix("end{") {
                    let Some(end) = after.find('}') else {
                        return false;
                    };
                    depth = depth.saturating_sub(1);
                    skip_bytes(&mut chars, at + 1 + "end{".len() + end);
                    continue;
                }
                if rest.starts_with('\\') {
                    if target == '\\' && depth == 0 {
                        return true;
                    }
                    chars.next();
                    continue;
                }
                // Any other escape (`\&`, `\{`, `\%`, a control word): skip one char.
                chars.next();
            }
            _ => {}
        }
    }
    false
}

fn skip_bytes(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, through: usize) {
    while chars.next_if(|(at, _)| *at <= through).is_some() {}
}

/// Height, depth and width in px of one boxed formula.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxMetrics {
    pub height: f32,
    pub depth: f32,
    pub width: f32,
}

/// Read `limpid-box N ht dp wd` lines from a TeX log, keyed by formula index.
/// Lines that do not parse are ignored.
pub fn parse_box_log(log: &str) -> HashMap<usize, BoxMetrics> {
    log.lines()
        .filter_map(|line| {
            let mut words = line.trim().strip_prefix("limpid-box ")?.split_whitespace();
            let index = words.next()?.parse().ok()?;
            let height = svgmeta::tex_dimension_to_px(words.next()?)?;
            let depth = svgmeta::tex_dimension_to_px(words.next()?)?;
            let width = svgmeta::tex_dimension_to_px(words.next()?)?;
            Some((
                index,
                BoxMetrics {
                    height,
                    depth,
                    width,
                },
            ))
        })
        .collect()
}

/// Read `Preview: Snippet N ht dp wd` lines (scaled points, `N` from 1) from a
/// TeX log, keyed by formula index (from 0). Lines that do not parse are
/// ignored.
pub fn parse_snippet_log(log: &str) -> HashMap<usize, BoxMetrics> {
    let scaled_points_to_px = |word: &str| {
        let scaled: f64 = word.parse().ok()?;
        svgmeta::tex_dimension_to_px(&format!("{}pt", scaled / 65536.0))
    };
    log.lines()
        .filter_map(|line| {
            let mut words = line
                .trim()
                .strip_prefix("Preview: Snippet ")?
                .split_whitespace();
            let number: usize = words.next()?.parse().ok()?;
            let height = scaled_points_to_px(words.next()?)?;
            let depth = scaled_points_to_px(words.next()?)?;
            let width = scaled_points_to_px(words.next()?)?;
            Some((
                number.checked_sub(1)?,
                BoxMetrics {
                    height,
                    depth,
                    width,
                },
            ))
        })
        .collect()
}

/// Distance from the top of the SVG to the baseline.
pub fn baseline_from(svg_height: f32, metrics: Option<&BoxMetrics>) -> f32 {
    match metrics {
        Some(metrics) => (svg_height - metrics.depth).clamp(0.0, svg_height),
        None => svg_height / 2.0,
    }
}

/// The `! ...` error lines of a TeX log with their `l.NN` context line.
fn error_lines(log: &str) -> Option<String> {
    let mut lines = log.lines();
    while let Some(line) = lines.next() {
        if !line.starts_with('!') {
            continue;
        }
        let context = lines.find(|candidate| candidate.starts_with("l."));
        return Some(match context {
            Some(context) => format!("{line} ({context})"),
            None => line.to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{Backend, Formula, MathSettings};

    #[test]
    fn should_build_one_preview_per_formula_when_batching() {
        let inline = Formula::new("x^2", false);
        let display = Formula::new("\\int_0^1 f", true);
        let document = build_document(&[&inline, &display]);
        assert_eq!(document.matches("\\begin{preview}").count(), 2);
        assert!(document.contains("\\usepackage[active,tightpage,lyx]{preview}"));
        assert!(document.contains("\\usepackage{amsmath,amssymb}"));
        assert!(document.contains("\\PreviewBorder=0pt"));
        assert!(document.contains("\\hbox{\\(x^2\n\\)}"));
        assert!(document.contains("limpid-box 0 "));
        assert!(document.contains("\\begin{preview}\n\\[\\int_0^1 f\n\\]\n\\end{preview}"));
        assert!(!document.contains("limpid-box 1 "));
        assert!(!document.contains("displaystyle"));
        assert!(document.ends_with("\\end{document}\n"));
    }

    #[test]
    fn should_not_leave_blank_line_when_formula_ends_in_newline() {
        let display = Formula::new("a\\\\\nb\n", true);
        let inline = Formula::new("x\n\n", false);
        let document = build_document(&[&display, &inline]);
        assert!(!document.contains("\n\n\\]"));
        assert!(!document.contains("\n\n\\)}"));
        assert!(!document.contains("\n\n\\end{gathered}"));
        assert!(document.contains("b\n\\end{gathered}\n\\]"));
        assert!(document.contains("x\n\\)}"));
    }

    #[test]
    fn should_wrap_in_display_brackets_when_formula_is_bare_math() {
        assert_eq!(
            display_body("E = mc^2 \\tag{1}"),
            "\\[E = mc^2 \\tag{1}\n\\]"
        );
        assert_eq!(
            display_body("\\int_0^1 f(x)\\,dx"),
            "\\[\\int_0^1 f(x)\\,dx\n\\]"
        );
    }

    #[test]
    fn should_wrap_in_gathered_when_display_formula_has_a_top_level_line_break() {
        let body = display_body("a = b \\\\ c = d");
        assert!(body.starts_with("\\[\\begin{gathered}") && body.contains("\\end{gathered}\n\\]"));
    }

    #[test]
    fn should_wrap_in_aligned_when_display_formula_has_a_top_level_ampersand() {
        assert!(display_body("a &= b \\\\ c &= d").starts_with("\\[\\begin{aligned}"));
        assert!(display_body("a &= b").starts_with("\\[\\begin{aligned}"));
    }

    #[test]
    fn should_not_wrap_when_line_break_is_inside_a_nested_environment_or_braces() {
        for latex in [
            "\\begin{pmatrix} a \\\\ b \\end{pmatrix}",
            "\\frac{a \\\\ b}{c}",
            "\\text{a \\& b}",
            "x % a \\\\ comment\ny",
            "\\left\\{ x \\right.",
        ] {
            assert_eq!(display_body(latex), format!("\\[{latex}\n\\]"), "{latex}");
        }
    }

    #[test]
    fn should_keep_top_level_environment_as_itself_when_it_is_a_display_environment() {
        for name in [
            "align",
            "align*",
            "alignat",
            "alignat*",
            "flalign",
            "gather",
            "gather*",
            "multline",
            "multline*",
            "equation",
            "equation*",
            "eqnarray",
            "eqnarray*",
        ] {
            let latex = format!("\\begin{{{name}}}a &= b\\end{{{name}}}");
            assert_eq!(display_body(&latex), latex, "{name}");
            assert_eq!(display_body(&format!("\n{latex}")), latex, "{name} padded");
        }
    }

    #[test]
    fn should_keep_split_inside_its_equation_when_formula_is_equation_with_split() {
        let latex = "\\begin{equation}\\begin{split}a&=b\\\\&=c\\end{split}\\end{equation}";
        let document = build_document(&[&Formula::new(latex, true)]);
        assert!(document.contains(&format!("\\begin{{preview}}\n{latex}\n\\end{{preview}}")));
        assert!(!document.contains("aligned") && !document.contains("\\["));
    }

    #[test]
    fn should_emit_tag_inside_display_brackets_when_formula_has_a_tag() {
        let document = build_document(&[&Formula::new("E = mc^2 \\tag{1}", true)]);
        assert!(document.contains("\\[E = mc^2 \\tag{1}\n\\]"));
        assert!(!document.contains("\\hbox"));
    }

    #[test]
    fn should_keep_starred_gather_bare_when_formula_is_gather_star() {
        let latex = "\\begin{gather*}a\\\\b\\end{gather*}";
        let document = build_document(&[&Formula::new(latex, true)]);
        assert!(document.contains(&format!("\\begin{{preview}}\n{latex}\n\\end{{preview}}")));
        assert!(!document.contains("gathered"));
    }

    #[test]
    fn should_not_box_display_formula_when_it_is_an_environment() {
        let document =
            build_document(&[&Formula::new("\\begin{alignat}{2}a&=b\\end{alignat}", true)]);
        assert!(document.contains("\\begin{preview}\n\\begin{alignat}"));
        assert!(!document.contains("\\hbox"));
        assert!(!document.contains("limpid-box 0"));
    }

    #[test]
    fn should_close_delimiter_on_its_own_line_when_formula_ends_in_a_comment() {
        let formula = Formula::new("x % note", false);
        let document = build_document(&[&formula]);
        assert!(document.contains("x % note\n\\)"));
    }

    #[test]
    fn should_parse_box_lines_when_log_has_noise() {
        let log = "(./batch.aux)\n\
            limpid-box 0 6.94444pt 0.0pt 5.0pt\n\
            Overfull \\hbox\n\
            limpid-box 2 9.5pt 3.25pt 20.0pt\n\
            limpid-box x 1pt 1pt 1pt\n\
            limpid-box 3 1sp 1pt 1pt\n";
        let boxes = parse_box_log(log);
        assert_eq!(boxes.len(), 2);
        let second = boxes[&2];
        assert!((second.depth - 3.25 * 96.0 / 72.27).abs() < 1e-3);
        assert!(boxes[&0].depth.abs() < 1e-6);
        assert!(!boxes.contains_key(&3));
    }

    #[test]
    fn should_parse_snippet_lines_when_log_has_noise() {
        let log = "Preview: Tightpage 0 0 0 0\n\
            Preview: Snippet 1 950272 0 22609920\n\
            Preview: Snippet 3 1688296 65536 22609920\n\
            Preview: Snippet x 1 1 1\n\
            Preview: Snippet 0 1 1 1\n\
            Preview: Snippet 2 1\n";
        let snippets = parse_snippet_log(log);
        assert_eq!(snippets.len(), 2);
        let first = snippets[&0];
        assert!((first.height - 950272.0 / 65536.0 * 96.0 / 72.27).abs() < 1e-2);
        assert!(first.depth.abs() < 1e-6);
        assert!((snippets[&2].depth - 96.0 / 72.27).abs() < 1e-3);
    }

    #[test]
    fn should_place_baseline_above_depth_when_metrics_exist() {
        let metrics = BoxMetrics {
            height: 8.0,
            depth: 3.0,
            width: 10.0,
        };
        assert_eq!(baseline_from(11.0, Some(&metrics)), 8.0);
        assert_eq!(baseline_from(2.0, Some(&metrics)), 0.0);
        assert_eq!(baseline_from(10.0, None), 5.0);
    }

    #[test]
    fn should_extract_first_error_when_log_has_one() {
        let log = "stuff\n! Undefined control sequence.\n<argument> \\foo\nl.9 ...\\foo\n\nmore";
        let error = error_lines(log).unwrap();
        assert!(error.starts_with("! Undefined control sequence."));
        assert!(error.contains("l.9"));
        assert_eq!(error_lines("all fine"), None);
    }

    #[test]
    fn should_be_unavailable_when_path_is_empty() {
        let settings = MathSettings::default();
        let backend = LatexBackend::new(settings.clone(), Some(OsString::new()));
        assert!(!backend.available(&settings));
        assert_eq!(backend.name(), "latex");
    }

    #[test]
    fn should_reject_latex_engine_when_only_pdftocairo_is_requested() {
        let settings = MathSettings {
            latex: "latex".into(),
            svg: "pdftocairo".into(),
            ..MathSettings::default()
        };
        let backend = LatexBackend::new(settings.clone(), None);
        assert!(!backend.available(&settings));
    }

    fn real_tools_present() -> bool {
        find_binary("pdflatex", None).is_some() && find_binary("pdftocairo", None).is_some()
    }

    #[test]
    fn should_typeset_with_real_pdflatex_when_installed() {
        if !real_tools_present() {
            eprintln!("skipped: pdflatex or pdftocairo missing");
            return;
        }
        let settings = MathSettings {
            svg: "pdftocairo".into(),
            ..MathSettings::default()
        };
        let backend = LatexBackend::new(settings.clone(), None);
        assert!(backend.available(&settings));
        let inline = Formula::new("x^2", false);
        let lowered = Formula::new("y_{i}", false);
        let display = Formula::new("\\int_0^1 f(x)\\,dx", true);
        let out = std::env::temp_dir().join(format!("limpid-latex-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let results = backend
            .render(&[&inline, &lowered, &display], &out)
            .unwrap();
        assert_eq!(results.len(), 3);
        for (rendered, formula) in results.iter().zip([&inline, &lowered, &display]) {
            assert_eq!(rendered.svg, out.join(format!("{}.svg", formula.hash)));
            assert!(
                std::fs::read_to_string(&rendered.svg)
                    .unwrap()
                    .contains("<svg")
            );
            assert!(
                rendered.width > 1.0 && rendered.height > 1.0,
                "{rendered:?}"
            );
            assert!(rendered.baseline > 0.0 && rendered.baseline <= rendered.height);
        }
        assert!(
            results[1].baseline < results[1].height,
            "a subscript must hang below the baseline"
        );
        assert!(
            (results[0].baseline - results[0].height).abs() < 0.5,
            "x^2 has no depth"
        );
        // Multi-line display math as pulldown-cmark yields it, and a wide one.
        let multiline = Formula::new("\\sum_{n=1}^\\infty n\n", true);
        let aligned = Formula::new("\\begin{aligned}a &= b\\\\\nc &= d\\end{aligned}\n", true);
        let terms: Vec<String> = (1..=30).map(|n| format!("a_{{{n}}}")).collect();
        let wide = Formula::new(&terms.join("+"), true);
        let rendered = backend
            .render(&[&multiline, &aligned, &wide], &out)
            .unwrap();
        assert_eq!(rendered.len(), 3);
        assert!(
            rendered[2].width > 343.7 * 96.0 / 72.27,
            "a wide display formula must keep its natural width: {:?}",
            rendered[2]
        );
        // Line breaks and display environments typeset, and are taller than one line.
        let one_line = Formula::new("a = b", true);
        let broken_lines = Formula::new("a = b \\\\ c = d", true);
        let environments: Vec<Formula> = [
            ("align", "a &= b \\\\ c &= d"),
            ("align*", "a &= b \\\\ c &= d"),
            ("gather", "a = b \\\\ c = d"),
            ("gather*", "a = b \\\\ c = d"),
            ("multline", "a = b \\\\ c = d"),
            ("equation", "a = b"),
        ]
        .iter()
        .map(|(name, body)| Formula::new(&format!("\\begin{{{name}}}{body}\\end{{{name}}}"), true))
        .collect();
        for formula in &environments {
            let rendered = backend.render(&[formula], &out).unwrap();
            assert_eq!(rendered.len(), 1, "{}", formula.latex);
        }
        let both = backend.render(&[&one_line, &broken_lines], &out).unwrap();
        assert!(
            both[1].height > both[0].height * 1.6,
            "two lines must be about twice as tall: {both:?}"
        );
        let broken = Formula::new("\\undefinedmacro", false);
        let error = backend.render(&[&broken], &out).unwrap_err();
        assert!(error.0.contains("Undefined control sequence"), "{error}");
        std::fs::remove_dir_all(&out).unwrap();
    }

    /// Typeset one display formula with real pdflatex; `None` when the tools
    /// are missing.
    fn typeset_display(latex: &str, label: &str) -> Option<Rendered> {
        if !real_tools_present() {
            eprintln!("skipped: pdflatex or pdftocairo missing");
            return None;
        }
        let settings = MathSettings {
            svg: "pdftocairo".into(),
            ..MathSettings::default()
        };
        let backend = LatexBackend::new(settings, None);
        let formula = Formula::new(latex, true);
        let out = std::env::temp_dir().join(format!(
            "limpid-latex-display-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let rendered = backend
            .render(&[&formula], &out)
            .unwrap_or_else(|error| panic!("{latex}: {error}"));
        std::fs::remove_dir_all(&out).unwrap();
        let rendered = rendered.into_iter().next().unwrap();
        assert!(
            rendered.width > 1.0 && rendered.height > 1.0,
            "{rendered:?}"
        );
        assert!(rendered.baseline > 0.0 && rendered.baseline <= rendered.height);
        Some(rendered)
    }

    #[test]
    fn should_typeset_tag_when_pdflatex_is_installed() {
        typeset_display("E = mc^2 \\tag{1}", "tag");
    }

    #[test]
    fn should_typeset_equation_with_split_when_pdflatex_is_installed() {
        let split = typeset_display(
            "\\begin{equation}\\begin{split}a&=b\\\\&=c\\end{split}\\end{equation}",
            "split",
        );
        let single = typeset_display("\\begin{equation}a=b\\end{equation}", "single");
        if let (Some(split), Some(single)) = (split, single) {
            assert!(split.height > single.height * 1.5, "{split:?} {single:?}");
        }
    }

    #[test]
    fn should_typeset_align_with_tag_when_pdflatex_is_installed() {
        typeset_display(
            "\\begin{align}a &= b \\tag{A}\\\\ c &= d \\nonumber\\end{align}",
            "aligntag",
        );
    }

    #[test]
    fn should_typeset_alignat_when_pdflatex_is_installed() {
        typeset_display(
            "\\begin{alignat}{2}a&=b&\\quad c&=d\\end{alignat}",
            "alignat",
        );
    }

    #[test]
    fn should_typeset_gather_star_when_pdflatex_is_installed() {
        typeset_display("\\begin{gather*}a=b\\\\c=d\\end{gather*}", "gatherstar");
    }

    #[test]
    fn should_typeset_plain_display_integral_when_pdflatex_is_installed() {
        let Some(rendered) = typeset_display("\\int_0^1 f(x)\\,dx", "integral") else {
            return;
        };
        assert!(rendered.height > 15.0, "{rendered:?}");
    }
}
