//! Reading the size of an SVG file from its root element, and the size
//! numbers that the TeX tools print. Everything is converted to CSS px at
//! 96 dpi, the unit the cache sidecars use.

/// Size of an SVG in CSS px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvgSize {
    pub width: f32,
    pub height: f32,
}

const PX_PER_INCH: f32 = 96.0;

/// Convert a CSS-like length (`10.5pt`, `3mm`, `12`, `1.5in`) to px.
/// `pt` and `bp` are PostScript points (72 per inch); `tpt` is a TeX point
/// (72.27 per inch).
pub fn length_to_px(length: &str) -> Option<f32> {
    let length = length.trim();
    let split = length
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(length.len());
    let (number, unit) = length.split_at(split);
    let value: f32 = number.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let per_inch = match unit.trim() {
        "" | "px" => return Some(value),
        "pt" | "bp" => 72.0,
        "tpt" => 72.27,
        "in" => 1.0,
        "cm" => 2.54,
        "mm" => 25.4,
        "pc" => 6.0,
        _ => return None,
    };
    Some(value / per_inch * PX_PER_INCH)
}

/// Read the size from the root `<svg>` tag: `width`/`height`, else `viewBox`.
pub fn parse_svg_size(svg: &str) -> Option<SvgSize> {
    let start = svg.find("<svg")?;
    let tag_end = svg[start..].find('>')? + start;
    let tag = &svg[start..tag_end];
    let width = attribute(tag, "width").and_then(length_to_px);
    let height = attribute(tag, "height").and_then(length_to_px);
    if let (Some(width), Some(height)) = (width, height) {
        return Some(SvgSize { width, height });
    }
    let view_box = attribute(tag, "viewBox")?;
    let numbers: Vec<f32> = view_box
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    let [_, _, width, height] = numbers[..] else {
        return None;
    };
    // A viewBox is in user units; with no unit they are px by convention.
    Some(SvgSize { width, height })
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(found) = tag[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let preceded_by_space = tag[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace());
        if !preceded_by_space {
            continue;
        }
        let Some(rest) = tag[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let value = &rest[1..];
        let end = value.find(quote)?;
        return Some(&value[..end]);
    }
    None
}

/// Parse a TeX dimension such as `6.94444pt` into px.
pub fn tex_dimension_to_px(dimension: &str) -> Option<f32> {
    let number = dimension.trim().strip_suffix("pt")?;
    length_to_px(&format!("{number}tpt"))
}

/// The `graphic size: 10.5pt x 7.2pt (...)` line `dvisvgm` prints on stderr,
/// one per page, in page order. Sizes are PostScript points.
pub fn parse_dvisvgm_sizes(output: &str) -> Vec<SvgSize> {
    output
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("graphic size:")?;
            let (width, rest) = rest.split_once(" x ")?;
            let height = rest.split_whitespace().next()?;
            Some(SvgSize {
                width: length_to_px(width)?,
                height: length_to_px(height)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_convert_points_to_px_when_unit_is_pt() {
        assert_eq!(length_to_px("72pt"), Some(96.0));
        assert_eq!(length_to_px("1in"), Some(96.0));
        assert_eq!(length_to_px(" 12 "), Some(12.0));
    }

    #[test]
    fn should_reject_lengths_when_unit_or_number_is_unknown() {
        assert_eq!(length_to_px("12em"), None);
        assert_eq!(length_to_px(""), None);
        assert_eq!(length_to_px("pt"), None);
        assert_eq!(length_to_px("auto"), None);
    }

    #[test]
    fn should_read_size_when_svg_has_width_and_height() {
        let svg = r#"<?xml version="1.0"?><svg xmlns="x" width="10.163pt" height="8.11pt" viewBox="0 0 10.163 8.11">"#;
        let size = parse_svg_size(svg).unwrap();
        assert!((size.width - 13.550667).abs() < 1e-3);
        assert!((size.height - 10.813333).abs() < 1e-3);
    }

    #[test]
    fn should_fall_back_to_view_box_when_width_is_missing() {
        let size = parse_svg_size(r#"<svg viewBox="0 0 30 12.5">"#).unwrap();
        assert_eq!(
            size,
            SvgSize {
                width: 30.0,
                height: 12.5
            }
        );
    }

    #[test]
    fn should_not_confuse_stroke_width_with_width_when_reading_attributes() {
        let size = parse_svg_size(r#"<svg stroke-width="9" width="20" height="10">"#).unwrap();
        assert_eq!(
            size,
            SvgSize {
                width: 20.0,
                height: 10.0
            }
        );
    }

    #[test]
    fn should_return_none_when_svg_is_garbage() {
        assert_eq!(parse_svg_size(""), None);
        assert_eq!(parse_svg_size("<html>"), None);
        assert_eq!(parse_svg_size("<svg"), None);
        assert_eq!(parse_svg_size(r#"<svg viewBox="0 0 5">"#), None);
    }

    #[test]
    fn should_convert_tex_points_when_parsing_dimensions() {
        let px = tex_dimension_to_px("72.27pt").unwrap();
        assert!((px - 96.0).abs() < 1e-3);
        assert_eq!(tex_dimension_to_px("3sp"), None);
    }

    #[test]
    fn should_parse_one_size_per_page_when_reading_dvisvgm_output() {
        let output = "pre-processing DVI file\n\
            processing page 1\n  graphic size: 10.5pt x 7.2pt (3.69mm x 2.53mm)\n\
            processing page 2\n  graphic size: 72pt x 36pt (25.4mm x 12.7mm)\n\
            graphic size: broken\n";
        let sizes = parse_dvisvgm_sizes(output);
        assert_eq!(sizes.len(), 2);
        assert_eq!(
            sizes[1],
            SvgSize {
                width: 96.0,
                height: 48.0
            }
        );
    }
}
