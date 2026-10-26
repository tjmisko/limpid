//! The one-line status bar: the state name, flashes, the `/` and `:` prompts
//! and the keys typed so far.

use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::Theme,
};

/// How long a flash stays up unless configured otherwise.
pub const DEFAULT_FLASH_MS: u64 = 1600;

/// Who owns the keys right now, as the status bar names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateName {
    Page,
    Visual,
    Search,
    Command,
}

impl StateName {
    pub fn label(self) -> &'static str {
        match self {
            StateName::Page => "PAGE",
            StateName::Visual => "VISUAL",
            StateName::Search => "SEARCH",
            StateName::Command => "COMMAND",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Flash {
    text: String,
    until_ms: u64,
}

/// The transient message on the status bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    flash: Option<Flash>,
}

impl Status {
    pub fn flash(&mut self, text: impl Into<String>, now_ms: u64, flash_ms: u64) {
        self.flash = Some(Flash {
            text: text.into(),
            until_ms: now_ms.saturating_add(flash_ms),
        });
    }

    /// The flash still showing at `now_ms`.
    pub fn current(&self, now_ms: u64) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|flash| now_ms < flash.until_ms)
            .map(|flash| flash.text.as_str())
    }

    pub fn clear(&mut self) {
        self.flash = None;
    }
}

/// The prompt being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub sigil: char,
    pub text: String,
    /// Shown dimmed while `text` is empty.
    pub placeholder: &'static str,
}

/// Everything the bar shows for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    pub state: StateName,
    pub message: String,
    pub prompt: Option<Prompt>,
    /// Right-aligned: the active search, or keys typed so far.
    pub right: String,
}

/// Height of the bar in device pixels.
pub fn height(theme: &Theme, scale: f32) -> f32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (font_size(theme) * 1.6 + 8.0).round() * scale
}

fn font_size(theme: &Theme) -> f32 {
    (theme.font_size * 0.8).round().max(8.0)
}

fn text_width(block: &TextBlock) -> f32 {
    block
        .glyphs()
        .map(|glyph| glyph.x + glyph.width)
        .fold(0.0, f32::max)
}

fn shape(fonts: &mut Fonts, theme: &Theme, scale: f32, text: &str, style: TextStyle) -> TextBlock {
    let size = font_size(theme) * scale;
    let mut block = TextBlock::new(fonts, size, size * 1.3);
    block.set_text(fonts, text, style, &theme.families);
    block.set_width(fonts, None);
    block
}

/// Paint the bar along the bottom edge of `canvas`.
pub fn paint(canvas: &mut Canvas, fonts: &mut Fonts, theme: &Theme, scale: f32, view: &StatusView) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let bar_height = height(theme, scale);
    let (width, total) = (canvas.width() as f32, canvas.height() as f32);
    if bar_height >= total {
        return;
    }
    let top = total - bar_height;
    canvas.fill_rect(Rect::new(0.0, top, width, bar_height), theme.surface);
    canvas.fill_rect(
        Rect::new(0.0, top, width, scale.round().max(1.0)),
        theme.rule,
    );

    let pad = 12.0 * scale;
    let size = font_size(theme) * scale;
    let text_y = top + (bar_height - size * 1.3) / 2.0;

    let tag_color = match view.state {
        StateName::Page => theme.muted,
        StateName::Visual => theme.purple,
        StateName::Search | StateName::Command => theme.amber,
    };
    let tag = shape(
        fonts,
        theme,
        scale,
        view.state.label(),
        TextStyle::mono(tag_color).bold(),
    );
    canvas.draw_text(fonts, &tag, pad, text_y, tag_color);
    let x = pad + text_width(&tag) + 14.0 * scale;

    let right = (!view.right.is_empty()).then(|| {
        shape(
            fonts,
            theme,
            scale,
            &view.right,
            TextStyle::mono(theme.muted),
        )
    });
    let right_left = right
        .as_ref()
        .map_or(width, |block| width - pad - text_width(block));

    if let Some(prompt) = &view.prompt {
        let line = format!("{}{}", prompt.sigil, prompt.text);
        let typed = shape(fonts, theme, scale, &line, TextStyle::mono(theme.ink));
        canvas.draw_text(fonts, &typed, x, text_y, theme.ink);
        let mut caret_x = x + text_width(&typed);
        if prompt.text.is_empty() && !prompt.placeholder.is_empty() {
            let hint = shape(
                fonts,
                theme,
                scale,
                &format!("  {}", prompt.placeholder),
                TextStyle::mono(theme.faint),
            );
            canvas.draw_text(fonts, &hint, caret_x, text_y, theme.faint);
        }
        caret_x = caret_x.min(right_left - size * 0.6);
        canvas.fill_rect(
            Rect::new(
                caret_x,
                text_y + size * 0.1,
                (size * 0.55).round().max(2.0),
                size * 1.1,
            ),
            theme.ink,
        );
    } else if !view.message.is_empty() {
        let message = shape(
            fonts,
            theme,
            scale,
            &view.message,
            TextStyle::mono(theme.ink),
        );
        canvas.draw_text(fonts, &message, x, text_y, theme.ink);
    }

    if let Some(right) = right {
        canvas.draw_text(fonts, &right, right_left, text_y, theme.muted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_show_a_flash_until_its_deadline_when_flashing() {
        let mut status = Status::default();
        assert_eq!(status.current(0), None);
        status.flash("Yanked 2 blocks", 100, 1600);
        assert_eq!(status.current(100), Some("Yanked 2 blocks"));
        assert_eq!(status.current(1699), Some("Yanked 2 blocks"));
        assert_eq!(status.current(1700), None);
    }

    #[test]
    fn should_replace_the_flash_when_a_newer_one_arrives() {
        let mut status = Status::default();
        status.flash("first", 0, 1000);
        status.flash("second", 500, 1000);
        assert_eq!(status.current(1200), Some("second"));
        status.clear();
        assert_eq!(status.current(1200), None);
    }

    #[test]
    fn should_not_overflow_when_the_deadline_is_astronomical() {
        let mut status = Status::default();
        status.flash("late", u64::MAX - 1, u64::MAX);
        assert_eq!(status.current(u64::MAX - 1), Some("late"));
    }

    #[test]
    fn should_name_each_state_when_labelling() {
        assert_eq!(StateName::Page.label(), "PAGE");
        assert_eq!(StateName::Search.label(), "SEARCH");
        assert_eq!(StateName::Command.label(), "COMMAND");
        assert_eq!(StateName::Visual.label(), "VISUAL");
    }

    #[test]
    fn should_scale_the_bar_height_when_the_scale_changes() {
        let theme = Theme::dark();
        let one = height(&theme, 1.0);
        assert!(one > 10.0);
        assert_eq!(height(&theme, 2.0), one * 2.0);
        assert_eq!(height(&theme, f32::NAN), one);
        assert_eq!(height(&theme, -1.0), one);
    }

    fn view(state: StateName) -> StatusView {
        StatusView {
            state,
            message: "hello".into(),
            prompt: None,
            right: "[1/3]".into(),
        }
    }

    #[test]
    fn should_paint_the_bar_background_at_the_bottom_when_painting() {
        let theme = Theme::dark();
        let (width, bar) = (300u32, height(&theme, 1.0) as u32);
        let canvas_height = 200u32;
        let mut pixels = vec![0u32; (width * canvas_height) as usize];
        let mut canvas = Canvas::new(&mut pixels, width, canvas_height);
        paint(
            &mut canvas,
            &mut Fonts::new(),
            &theme,
            1.0,
            &view(StateName::Page),
        );
        let inside = canvas.pixel(2, (canvas_height - 3) as i32).unwrap();
        assert_eq!(inside, theme.surface.to_pixel());
        let above = canvas.pixel(2, (canvas_height - bar - 3) as i32).unwrap();
        assert_eq!(above, 0, "nothing above the bar is touched");
    }

    #[test]
    fn should_draw_a_caret_when_a_prompt_is_open() {
        let theme = Theme::dark();
        let (width, canvas_height) = (400u32, 120u32);
        let mut with_prompt = vec![0u32; (width * canvas_height) as usize];
        let mut without = with_prompt.clone();
        let mut fonts = Fonts::new();
        let mut prompt_view = view(StateName::Search);
        prompt_view.message.clear();
        prompt_view.prompt = Some(Prompt {
            sigil: '/',
            text: String::new(),
            placeholder: "",
        });
        paint(
            &mut Canvas::new(&mut with_prompt, width, canvas_height),
            &mut fonts,
            &theme,
            1.0,
            &prompt_view,
        );
        prompt_view.prompt = None;
        paint(
            &mut Canvas::new(&mut without, width, canvas_height),
            &mut fonts,
            &theme,
            1.0,
            &prompt_view,
        );
        assert_ne!(with_prompt, without);
    }

    #[test]
    fn should_skip_painting_when_the_canvas_is_shorter_than_the_bar() {
        let theme = Theme::dark();
        let mut pixels = vec![0u32; 50 * 10];
        let mut canvas = Canvas::new(&mut pixels, 50, 10);
        paint(
            &mut canvas,
            &mut Fonts::new(),
            &theme,
            1.0,
            &view(StateName::Page),
        );
        assert!(pixels.iter().all(|&pixel| pixel == 0));
    }

    #[test]
    fn should_survive_a_zero_sized_canvas_and_long_text_when_painting() {
        let theme = Theme::dark();
        let mut empty: Vec<u32> = Vec::new();
        paint(
            &mut Canvas::new(&mut empty, 0, 0),
            &mut Fonts::new(),
            &theme,
            1.0,
            &view(StateName::Page),
        );
        let mut long = view(StateName::Command);
        long.message = "x".repeat(5000);
        long.right = "y".repeat(5000);
        let mut pixels = vec![0u32; 200 * 80];
        paint(
            &mut Canvas::new(&mut pixels, 200, 80),
            &mut Fonts::new(),
            &theme,
            2.0,
            &long,
        );
    }
}
