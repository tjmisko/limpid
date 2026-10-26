//! TeX backend: one `preview` document per batch, compiled once, then split
//! into one SVG per page.
//!
//! Baseline: each inline formula is first set in an `\hbox`, whose height,
//! depth and width are written to the log as
//! `limpid-box N <ht> <dp> <wd>` (TeX points), then unboxed into the preview
//! environment. The `preview` package itself does not log snippet sizes in a
//! form that is stable across drivers. The SVG's own height minus the depth
//! gives the baseline offset from the top. Display formulas are not boxed
//! (they may be multi-line), so their baseline is the vertical middle.

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
                baseline: baseline_from(size.height, boxes.get(&index)),
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
         \\usepackage[active,tightpage]{preview}\n\
         \\usepackage{amsmath,amssymb}\n\
         \\PreviewBorder=0pt\n\
         \\newbox\\limpidbox\n\
         \\begin{document}\n",
    );
    for (index, formula) in batch.iter().enumerate() {
        // Trailing whitespace is trimmed (display math from the parser ends in a
        // newline, which would become a blank line, i.e. \par, inside math). The
        // newline before the closing delimiter keeps a trailing `%` comment in the
        // formula from swallowing it. Both kinds are set in a box so the page is
        // exactly as wide as the formula, and the box gives the baseline.
        let latex = formula.latex.trim_end();
        let (open, close) = if formula.display {
            ("$\\displaystyle ", "$")
        } else {
            ("\\(", "\\)")
        };
        document.push_str(&format!(
            "\\begin{{preview}}\\setbox\\limpidbox\\hbox{{{open}{latex}\n{close}}}\
             \\typeout{{limpid-box {index} \\the\\ht\\limpidbox\\space\\the\\dp\\limpidbox\\space\\the\\wd\\limpidbox}}\
             \\unhbox\\limpidbox\\end{{preview}}\n"
        ));
    }
    document.push_str("\\end{document}\n");
    document
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
        assert!(document.contains("\\usepackage[active,tightpage]{preview}"));
        assert!(document.contains("\\usepackage{amsmath,amssymb}"));
        assert!(document.contains("\\PreviewBorder=0pt"));
        assert!(document.contains("\\hbox{\\(x^2\n\\)}"));
        assert!(document.contains("limpid-box 0 "));
        assert!(document.contains("\\hbox{$\\displaystyle \\int_0^1 f\n$}"));
        assert!(document.contains("limpid-box 1 "));
        assert!(!document.contains("\\["));
        assert!(document.ends_with("\\end{document}\n"));
    }

    #[test]
    fn should_not_leave_blank_line_when_formula_ends_in_newline() {
        let display = Formula::new("a\\\\\nb\n", true);
        let inline = Formula::new("x\n\n", false);
        let document = build_document(&[&display, &inline]);
        assert!(!document.contains("\n\n$}"));
        assert!(!document.contains("\n\n\\)}"));
        assert!(document.contains("b\n$}"));
        assert!(document.contains("x\n\\)}"));
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
        let aligned = Formula::new(
            "\\begin{aligned}a &= b\\\\\nc &= d\\end{aligned}\n",
            true,
        );
        let terms: Vec<String> = (1..=30).map(|n| format!("a_{{{n}}}")).collect();
        let wide = Formula::new(&terms.join("+"), true);
        let rendered = backend.render(&[&multiline, &aligned, &wide], &out).unwrap();
        assert_eq!(rendered.len(), 3);
        assert!(
            rendered[2].width > 343.7 * 96.0 / 72.27,
            "a wide display formula must keep its natural width: {:?}",
            rendered[2]
        );
        assert!(
            rendered[0].width < 150.0,
            "display math must be tightly bounded: {:?}",
            rendered[0]
        );
        let broken = Formula::new("\\undefinedmacro", false);
        let error = backend.render(&[&broken], &out).unwrap_err();
        assert!(error.0.contains("Undefined control sequence"), "{error}");
        std::fs::remove_dir_all(&out).unwrap();
    }
}
