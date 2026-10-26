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
//! Baseline: Typst does not report the baseline of an equation to the
//! document or to the SVG, and `measure` only returns the extent. The module
//! therefore estimates the depth below the baseline as a fixed fraction of the
//! formula height (`DEPTH_FRACTION`) for inline formulas, and uses the
//! vertical middle for display formulas. It is the weakest part of this
//! backend; prefer the `latex` backend when inline alignment matters.

use super::{
    Backend, BackendError, Formula, MathSettings, Rendered, find_binary, numbered_files,
    run_command, svgmeta, tail,
};
use std::{ffi::OsString, path::Path, process::Command};

const JOB: &str = "batch";
const MITEX_IMPORT: &str = "#import \"@preview/mitex:0.2.4\": mitex-scope";
/// Estimated share of an inline formula's height that lies below the baseline.
pub const DEPTH_FRACTION: f32 = 0.22;

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
            let size =
                svgmeta::parse_svg_size(&std::fs::read_to_string(&page)?).ok_or_else(|| {
                    BackendError(format!("cannot read the size of {}", page.display()))
                })?;
            let destination = out.join(format!("{}.svg", formula.hash));
            std::fs::rename(&page, &destination)?;
            results.push(Rendered {
                svg: destination,
                width: size.width,
                height: size.height,
                baseline: estimate_baseline(size.height, formula.display),
                backend: "typst".into(),
            });
        }
        let _ = std::fs::remove_dir_all(&work);
        Ok(results)
    }
}

/// Distance from the top of the SVG to the baseline, see the module notes.
pub fn estimate_baseline(height: f32, display: bool) -> f32 {
    if display {
        return height / 2.0;
    }
    height * (1.0 - DEPTH_FRACTION)
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
        document.push_str(&format!(
            "#eval({}, mode: \"markup\", scope: mitex-scope)\n",
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
    fn should_put_baseline_below_middle_when_estimating_inline_and_at_middle_for_display() {
        assert!((estimate_baseline(10.0, false) - 7.8).abs() < 1e-5);
        assert_eq!(estimate_baseline(10.0, true), 5.0);
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
