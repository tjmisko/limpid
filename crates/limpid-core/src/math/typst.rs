//! Typst backend: LaTeX math is converted to Typst math with the `mitex`
//! crate, every formula goes on its own auto-sized page of one `.typ` file,
//! and `typst compile --format svg` writes one SVG per page.
//!
//! The converted text uses MiTeX's own functions (`mitexsqrt`, matrices and
//! so on), which live in the `mitex-scope` of the `@preview/mitex` Typst
//! package. The document therefore passes each formula through
//! `eval(..., scope: mitex-scope)`. The package is fetched by `typst` on its
//! first use, so that first run needs the network.
//!
//! Baseline: Typst does not report the baseline of an equation, so each
//! inline formula is followed on its line by a tiny box filled with a unique
//! colour. A box sits on the baseline, so the bottom of that shape in the SVG
//! is the baseline. `read_marker` finds it (summing the enclosing group
//! translations), the shape is removed from the saved SVG, and the baseline is
//! read off. If the marker cannot be found, the depth falls back to
//! `math.typst_depth_fraction` of the height. Display formulas are block
//! level and use the vertical middle.

use super::{
    Backend, BackendError, Formula, MathSettings, Rendered, find_binary, numbered_files,
    run_command, svgmeta, tail,
};
use std::{ffi::OsString, path::Path, process::Command};

const JOB: &str = "batch";
const MITEX_IMPORT: &str = "#import \"@preview/mitex:0.2.4\": mitex-scope";
/// Fill colour of the baseline marker box; nothing else may use it.
const MARKER_COLOR: &str = "#fe01fd";
/// Side of the marker box in Typst points.
const MARKER_SIZE_PT: f32 = 0.01;

pub struct TypstBackend {
    settings: MathSettings,
    path: Option<OsString>,
}

impl TypstBackend {
    pub fn new(settings: MathSettings, path: Option<OsString>) -> Self {
        Self { settings, path }
    }
}

impl Backend for TypstBackend {
    fn name(&self) -> &str {
        "typst"
    }

    fn available(&self, settings: &MathSettings) -> bool {
        find_binary(&settings.typst, self.path.as_deref()).is_some()
    }

    fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError> {
        let binary = find_binary(&self.settings.typst, self.path.as_deref()).ok_or_else(|| {
            BackendError(format!("typst binary {:?} not found", self.settings.typst))
        })?;
        let source = build_document(batch)?;
        let work = out.join("typst");
        std::fs::create_dir_all(&work)?;
        std::fs::write(work.join(format!("{JOB}.typ")), source)?;
        let mut command = Command::new(binary);
        command.current_dir(&work).args([
            "compile",
            "--format",
            "svg",
            &format!("{JOB}.typ"),
            "page-{p}.svg",
        ]);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        let output = run_command(command, self.settings.timeout())?;
        if !output.success {
            return Err(BackendError(format!(
                "typst failed: {}",
                tail(&output.stderr, 6)
            )));
        }
        let pages = numbered_files(&work, "page-", ".svg");
        if pages.len() != batch.len() {
            return Err(BackendError(format!(
                "typst wrote {} pages for {} formulas",
                pages.len(),
                batch.len()
            )));
        }
        let mut results = Vec::with_capacity(batch.len());
        for (formula, (_, page)) in batch.iter().zip(pages) {
            let mut svg_text = std::fs::read_to_string(&page)?;
            let size = svgmeta::parse_svg_size(&svg_text).ok_or_else(|| {
                BackendError(format!("cannot read the size of {}", page.display()))
            })?;
            let marker = if formula.display {
                None
            } else {
                read_marker(&svg_text, size.height)
            };
            let baseline = match marker {
                Some(found) => {
                    svg_text = found.cleaned;
                    std::fs::write(&page, &svg_text)?;
                    found.baseline.clamp(0.0, size.height)
                }
                None => estimate_baseline(
                    size.height,
                    formula.display,
                    self.settings.typst_depth_fraction,
                ),
            };
            let destination = out.join(format!("{}.svg", formula.hash));
            std::fs::rename(&page, &destination)?;
            results.push(Rendered {
                svg: destination,
                width: size.width,
                height: size.height,
                baseline,
                backend: "typst".into(),
            });
        }
        let _ = std::fs::remove_dir_all(&work);
        Ok(results)
    }
}

/// Distance from the top of the SVG to the baseline when no marker is
/// available, see the module notes.
pub fn estimate_baseline(height: f32, display: bool, depth_fraction: f32) -> f32 {
    if display {
        return height / 2.0;
    }
    height * (1.0 - depth_fraction.clamp(0.0, 1.0))
}

/// The marker's baseline and the SVG with the marker shape removed.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerReading {
    pub baseline: f32,
    pub cleaned: String,
}

/// Find the marker shape in a Typst SVG. `height_px` is the SVG height in
/// CSS px; coordinates inside are in the viewBox's units and are scaled to px.
/// `None` when the marker, the viewBox or a coordinate is missing.
pub fn read_marker(svg: &str, height_px: f32) -> Option<MarkerReading> {
    let color_at = svg.find(&format!("fill=\"{MARKER_COLOR}\""))?;
    let shape_start = svg[..color_at].rfind('<')?;
    let shape_end = svg[color_at..].find('>')? + color_at + 1;
    let shape = &svg[shape_start..shape_end];
    let path_data = attribute_value(shape, "d")?;
    let bottom_in_group = path_numbers(path_data)
        .into_iter()
        .skip(1)
        .step_by(2)
        .fold(f32::NEG_INFINITY, f32::max);
    if !bottom_in_group.is_finite() {
        return None;
    }
    let group_offset = enclosing_vertical_offset(&svg[..shape_start])?;
    let view_height = view_box_height(svg)?;
    if view_height <= 0.0 {
        return None;
    }
    let baseline = (group_offset + bottom_in_group) * height_px / view_height;
    let mut cleaned = String::with_capacity(svg.len());
    cleaned.push_str(&svg[..shape_start]);
    cleaned.push_str(&svg[shape_end..]);
    baseline
        .is_finite()
        .then_some(MarkerReading { baseline, cleaned })
}

fn attribute_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

/// All numbers in an SVG path `d` attribute, in order. Command letters are
/// separators; the marker path uses only absolute `M`/`L`/`H`/`V`/`Z`-free
/// polygons in Typst output, so the numbers pair up as x, y.
fn path_numbers(data: &str) -> Vec<f32> {
    data.split(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn view_box_height(svg: &str) -> Option<f32> {
    let tag_end = svg.find('>')?;
    let tag = &svg[svg.find("<svg")?..tag_end];
    let numbers = path_numbers(attribute_value(tag, "viewBox")?);
    (numbers.len() == 4).then(|| numbers[3])
}

/// Sum of the vertical translations of the `<g>` elements still open at the
/// end of `before`. `None` when a transform is not a plain translation.
fn enclosing_vertical_offset(before: &str) -> Option<f32> {
    let mut stack: Vec<f32> = Vec::new();
    let mut rest = before;
    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        let close = rest.find('>')?;
        let tag = &rest[..=close];
        rest = &rest[close + 1..];
        if tag.starts_with("</g") {
            stack.pop();
            continue;
        }
        if !(tag.starts_with("<g ") || tag.starts_with("<g>")) || tag.ends_with("/>") {
            continue;
        }
        stack.push(translation_y(tag)?);
    }
    Some(stack.iter().sum())
}

fn translation_y(tag: &str) -> Option<f32> {
    let Some(transform) = attribute_value(tag, "transform") else {
        return Some(0.0);
    };
    let numbers = path_numbers(transform);
    if let Some(arguments) = transform.strip_prefix("matrix(") {
        let numbers = path_numbers(arguments);
        let [a, b, c, d, _, f] = numbers[..] else {
            return None;
        };
        let plain = (a - 1.0).abs() < 1e-6 && b == 0.0 && c == 0.0 && (d - 1.0).abs() < 1e-6;
        return plain.then_some(f);
    }
    if transform.starts_with("translate(") {
        return numbers.get(1).copied().or(Some(0.0));
    }
    None
}

/// The `.typ` source for one batch. Page `i` (from 0) holds `batch[i]`.
/// Fails when a formula cannot be converted, naming its hash.
pub fn build_document(batch: &[&Formula]) -> Result<String, BackendError> {
    let mut document =
        format!("{MITEX_IMPORT}\n#set page(width: auto, height: auto, margin: 0pt)\n");
    for (index, formula) in batch.iter().enumerate() {
        let converted = mitex::convert_math(&formula.latex, None).map_err(|error| {
            BackendError(format!(
                "mitex cannot convert formula {}: {error}",
                formula.hash
            ))
        })?;
        let math = if formula.display {
            format!("$ {converted} $")
        } else {
            format!("${converted}$")
        };
        if index > 0 {
            document.push_str("#pagebreak()\n");
        }
        let marker = if formula.display {
            String::new()
        } else {
            format!(
                "#box(width: {MARKER_SIZE_PT}pt, height: {MARKER_SIZE_PT}pt, fill: rgb(\"{MARKER_COLOR}\"))"
            )
        };
        document.push_str(&format!(
            "#eval({}, mode: \"markup\", scope: mitex-scope){marker}\n",
            typst_string(&math)
        ));
    }
    Ok(document)
}

/// A Typst string literal for `text`.
fn typst_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn should_separate_pages_and_size_them_to_content_when_building() {
        let a = Formula::new("x^2", false);
        let b = Formula::new("\\frac{a}{b}", true);
        let document = build_document(&[&a, &b]).unwrap();
        assert!(document.starts_with(MITEX_IMPORT));
        assert!(document.contains("#set page(width: auto, height: auto, margin: 0pt)"));
        assert_eq!(document.matches("#pagebreak()").count(), 1);
        assert_eq!(document.matches("#eval(").count(), 2);
        assert!(document.contains("scope: mitex-scope"));
        let first = document
            .lines()
            .find(|line| line.starts_with("#eval("))
            .unwrap();
        assert!(first.contains("\"$"), "inline math has no padding: {first}");
        let second = document
            .lines()
            .filter(|line| line.starts_with("#eval("))
            .nth(1)
            .unwrap();
        assert!(second.contains("\"$ "), "display math is padded: {second}");
    }

    #[test]
    fn should_escape_quotes_and_backslashes_when_embedding_in_a_string() {
        assert_eq!(typst_string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(typst_string(""), "\"\"");
    }

    #[test]
    fn should_not_let_formula_text_break_out_of_the_string_when_it_has_quotes() {
        let formula = Formula::new("\\text{say \"hi\"}", false);
        let document = build_document(&[&formula]).unwrap();
        let line = document
            .lines()
            .find(|line| line.starts_with("#eval("))
            .unwrap();
        let literal_end = line.find(", mode:").unwrap();
        let literal = &line["#eval(".len()..literal_end];
        assert!(literal.starts_with('"') && literal.ends_with('"'));
        let inner = &literal[1..literal.len() - 1];
        let mut escaped = false;
        for character in inner.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            assert_ne!(character, '"', "unescaped quote in {literal}");
            escaped = character == '\\';
        }
    }

    #[test]
    fn should_use_configured_fraction_when_marker_is_missing() {
        assert!((estimate_baseline(10.0, false, 0.22) - 7.8).abs() < 1e-5);
        assert!((estimate_baseline(10.0, false, 0.5) - 5.0).abs() < 1e-5);
        assert_eq!(estimate_baseline(10.0, true, 0.22), 5.0);
        assert_eq!(estimate_baseline(10.0, false, 7.0), 0.0);
    }

    #[test]
    fn should_add_marker_after_inline_formulas_only_when_building() {
        let inline = Formula::new("x", false);
        let display = Formula::new("x", true);
        let document = build_document(&[&inline, &display]).unwrap();
        let mut lines = document.lines().filter(|line| line.starts_with("#eval("));
        assert!(lines.next().unwrap().contains(MARKER_COLOR));
        assert!(!lines.next().unwrap().contains(MARKER_COLOR));
    }

    fn marked_svg(group_y: f32, bottom: f32) -> String {
        format!(
            "<svg class=\"typst-doc\" viewBox=\"0 0 20 10\" width=\"20pt\" height=\"10pt\">\
             <g><g transform=\"matrix(1 0 0 1 0 {group_y})\"><g class=\"typst-text\"/>\
             <path class=\"typst-shape\" fill=\"{MARKER_COLOR}\" d=\"M 0 0 L 0 -0.01 L 0.01 -0.01 L 0.01 {bottom} Z\"/>\
             </g></g><path fill=\"#000\" d=\"M 1 1\"/></svg>"
        )
    }

    #[test]
    fn should_read_baseline_from_marker_when_it_sits_in_a_translated_group() {
        // Group offset 7, shape bottom at 0, viewBox 10 tall, SVG 13.333px tall.
        let height_px = 10.0 * 96.0 / 72.0;
        let reading = read_marker(&marked_svg(7.0, 0.0), height_px).unwrap();
        assert!((reading.baseline - 7.0 * height_px / 10.0).abs() < 1e-3);
        assert!(!reading.cleaned.contains(MARKER_COLOR));
        assert!(reading.cleaned.contains("fill=\"#000\""));
    }

    #[test]
    fn should_give_different_baselines_when_formulas_have_different_depths() {
        let shallow = read_marker(&marked_svg(9.0, 0.0), 10.0).unwrap();
        let deep = read_marker(&marked_svg(5.0, 0.0), 10.0).unwrap();
        assert!(deep.baseline < shallow.baseline);
    }

    #[test]
    fn should_return_none_when_marker_is_absent_or_transform_is_not_a_translation() {
        assert_eq!(
            read_marker("<svg viewBox=\"0 0 4 4\"><g/></svg>", 4.0),
            None
        );
        let rotated = marked_svg(1.0, 0.0).replace("matrix(1 0 0 1 0 1)", "matrix(0 1 -1 0 0 1)");
        assert_eq!(read_marker(&rotated, 10.0), None);
        assert_eq!(
            read_marker(&marked_svg(1.0, 0.0).replace("viewBox", "vb"), 10.0),
            None
        );
    }

    #[test]
    fn should_be_unavailable_when_binary_is_missing() {
        let settings = MathSettings::default();
        let backend = TypstBackend::new(settings.clone(), Some(OsString::new()));
        assert!(!backend.available(&settings));
        let custom = MathSettings {
            typst: "/nonexistent/typst".into(),
            ..MathSettings::default()
        };
        assert!(!backend.available(&custom));
        assert_eq!(backend.name(), "typst");
    }

    #[test]
    fn should_be_available_when_typst_setting_names_an_executable_path() {
        let settings = MathSettings {
            typst: "/bin/sh".into(),
            ..MathSettings::default()
        };
        let backend = TypstBackend::new(settings.clone(), Some(OsStr::new("").to_owned()));
        assert!(backend.available(&settings));
    }

    #[test]
    fn should_run_the_configured_binary_and_split_pages_when_typst_is_faked() {
        // A stand-in `typst` that writes two pages, so the page handling and
        // size parsing are tested without the real compiler.
        let dir = std::env::temp_dir().join(format!("limpid-typst-fake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-typst");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf '<svg width=\"72pt\" height=\"36pt\" viewBox=\"0 0 72 36\"></svg>' > page-1.svg\n\
             printf '<svg width=\"36pt\" height=\"18pt\" viewBox=\"0 0 36 18\"></svg>' > page-2.svg\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let settings = MathSettings {
            typst: script.to_string_lossy().into_owned(),
            ..MathSettings::default()
        };
        let backend = TypstBackend::new(settings, Some(OsString::new()));
        let a = Formula::new("a", false);
        let b = Formula::new("b", true);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let results = backend.render(&[&a, &b], &out).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!((results[0].width, results[0].height), (96.0, 48.0));
        assert_eq!(results[1].baseline, 12.0);
        assert!(out.join(format!("{}.svg", a.hash)).is_file());
        assert_eq!(results[0].backend, "typst");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_when_page_count_differs_from_formula_count() {
        let dir = std::env::temp_dir().join(format!("limpid-typst-short-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-typst");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let settings = MathSettings {
            typst: script.to_string_lossy().into_owned(),
            ..MathSettings::default()
        };
        let backend = TypstBackend::new(settings, Some(OsString::new()));
        let a = Formula::new("a", false);
        let error = backend.render(&[&a], &dir).unwrap_err();
        assert!(error.0.contains("0 pages for 1"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
