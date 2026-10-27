//! The review overlay (`S`): the message that would be sent, shown over the
//! page before anything is delivered.

use crate::{
    nav::keymap::KeyInput,
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::Theme,
};

/// What a key did to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewKey {
    /// Nothing changed.
    Ignored,
    /// The overlay moved; repaint.
    Moved,
    /// Close it and send the message.
    Send,
    Dismiss,
}

/// The message under review and how far it is scrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub message: String,
    /// First visible line.
    pub scroll: usize,
}

/// Lines moved by `Ctrl-d` and `Ctrl-u`.
const PAGE_LINES: usize = 8;

impl Review {
    pub fn new(message: String) -> Self {
        Self { message, scroll: 0 }
    }

    pub fn lines(&self) -> Vec<&str> {
        self.message.lines().collect()
    }

    fn last_scroll(&self) -> usize {
        self.lines().len().saturating_sub(1)
    }

    /// `Ctrl-Enter` sends, `Esc` and `q` dismiss, `j`/`k` and `Ctrl-d`/`Ctrl-u`
    /// scroll. Every other key is swallowed: the page below does not move.
    pub fn handle(&mut self, input: &KeyInput) -> ReviewKey {
        let chord = &input.chord;
        let before = self.scroll;
        match (chord.key.as_str(), chord.ctrl) {
            ("enter", true) => return ReviewKey::Send,
            ("escape", _) | ("q", false) => return ReviewKey::Dismiss,
            ("c", true) => return ReviewKey::Dismiss,
            ("j" | "down", false) => self.scroll = (self.scroll + 1).min(self.last_scroll()),
            ("k" | "up", false) => self.scroll = self.scroll.saturating_sub(1),
            ("d", true) => self.scroll = (self.scroll + PAGE_LINES).min(self.last_scroll()),
            ("u", true) => self.scroll = self.scroll.saturating_sub(PAGE_LINES),
            ("g", false) if !chord.shift => self.scroll = 0,
            ("g", false) => self.scroll = self.last_scroll(),
            _ => {}
        }
        if self.scroll == before {
            ReviewKey::Ignored
        } else {
            ReviewKey::Moved
        }
    }
}

fn shape(fonts: &mut Fonts, theme: &Theme, size: f32, text: &str, width: f32) -> TextBlock {
    let mut block = TextBlock::new(fonts, size, size * 1.5);
    block.set_text(fonts, text, TextStyle::mono(theme.ink), &theme.families);
    block.set_width(fonts, Some(width));
    block
}

/// Paint the overlay over a `canvas` whose bottom `reserved` pixels belong to
/// the status bar.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    theme: &Theme,
    scale: f32,
    reserved: f32,
    review: &Review,
) {
    let (width, total) = (canvas.width() as f32, canvas.height() as f32 - reserved);
    if width < 40.0 || total < 40.0 {
        return;
    }
    let margin = (24.0 * scale).min(width / 8.0);
    let panel = Rect::new(margin, margin, width - 2.0 * margin, total - 2.0 * margin);
    canvas.fill_rect(panel, theme.surface);
    canvas.stroke_rect(panel, scale.round().max(1.0), theme.teal);
    let pad = 16.0 * scale;
    let size = (theme.font_size * 0.9 * scale).round().max(8.0);
    let text_width = (panel.w - 2.0 * pad).max(1.0);
    let heading = shape(
        fonts,
        theme,
        size,
        "Review: Ctrl-Enter sends, Esc closes",
        text_width,
    );
    canvas.draw_text(fonts, &heading, panel.x + pad, panel.y + pad, theme.muted);
    let body_top = panel.y + pad + size * 2.2;
    // Each source line may wrap, so the budget is rows of the panel, spent
    // line by line; whatever does not fit scrolls.
    let line_height = size * 1.5;
    let capacity = ((panel.bottom() - body_top - pad) / line_height)
        .floor()
        .max(1.0) as usize;
    let visible: Vec<&str> = review
        .lines()
        .into_iter()
        .skip(review.scroll)
        .take(capacity)
        .collect();
    let text = if visible.is_empty() {
        "(nothing to send)".to_owned()
    } else {
        visible.join("\n")
    };
    let body = shape(fonts, theme, size, &text, text_width);
    canvas.draw_text(fonts, &body, panel.x + pad, body_top, theme.ink);
    // Wrapped lines can still run past the panel: put the frame back over them.
    let below = panel.bottom();
    canvas.fill_rect(
        Rect::new(
            panel.x,
            below,
            panel.w,
            (total - below).max(0.0).min(margin),
        ),
        theme.bg,
    );
    canvas.stroke_rect(panel, scale.round().max(1.0), theme.teal);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(lines: usize) -> Review {
        Review::new((0..lines).map(|n| format!("line {n}\n")).collect())
    }

    #[test]
    fn should_send_when_ctrl_enter_is_pressed() {
        assert_eq!(
            review(2).handle(&KeyInput::named("ctrl+enter")),
            ReviewKey::Send
        );
    }

    #[test]
    fn should_not_send_when_plain_enter_is_pressed() {
        assert_eq!(
            review(2).handle(&KeyInput::named("enter")),
            ReviewKey::Ignored
        );
    }

    #[test]
    fn should_dismiss_on_escape_q_and_ctrl_c() {
        for key in ["escape", "q", "ctrl+c"] {
            assert_eq!(
                review(2).handle(&KeyInput::named(key)),
                ReviewKey::Dismiss,
                "{key}"
            );
        }
    }

    #[test]
    fn should_scroll_within_the_message_when_moving() {
        let mut review = review(5);
        assert_eq!(review.handle(&KeyInput::named("j")), ReviewKey::Moved);
        assert_eq!(review.scroll, 1);
        assert_eq!(review.handle(&KeyInput::named("k")), ReviewKey::Moved);
        assert_eq!(review.handle(&KeyInput::named("k")), ReviewKey::Ignored);
        assert_eq!(review.scroll, 0);
        for _ in 0..20 {
            review.handle(&KeyInput::named("j"));
        }
        assert_eq!(review.scroll, 4, "never past the last line");
        assert_eq!(review.handle(&KeyInput::named("ctrl+u")), ReviewKey::Moved);
        assert_eq!(review.scroll, 0);
        assert_eq!(review.handle(&KeyInput::named("shift+g")), ReviewKey::Moved);
        assert_eq!(review.scroll, 4);
    }

    #[test]
    fn should_swallow_unrelated_keys_when_reviewing() {
        let mut review = review(3);
        assert_eq!(review.handle(&KeyInput::named("x")), ReviewKey::Ignored);
        assert_eq!(review.handle(&KeyInput::named("i")), ReviewKey::Ignored);
        assert_eq!(review.scroll, 0);
    }

    #[test]
    fn should_not_scroll_when_the_message_is_empty() {
        let mut empty = Review::new(String::new());
        assert_eq!(empty.handle(&KeyInput::named("j")), ReviewKey::Ignored);
        assert_eq!(empty.scroll, 0);
    }

    #[test]
    fn should_draw_inside_the_panel_when_painting() {
        let theme = Theme::dark();
        let mut pixels = vec![theme.bg.to_pixel(); 400 * 300];
        let mut canvas = Canvas::new(&mut pixels, 400, 300);
        let mut fonts = Fonts::new();
        paint(&mut canvas, &mut fonts, &theme, 1.0, 0.0, &review(3));
        assert_eq!(
            canvas.pixel(1, 1),
            Some(theme.bg.to_pixel()),
            "outside the panel"
        );
        assert!(
            canvas
                .pixel(200, 150)
                .is_some_and(|p| p != theme.bg.to_pixel()),
            "the panel is drawn"
        );
    }

    #[test]
    fn should_not_panic_when_the_canvas_is_tiny() {
        let mut pixels = vec![0u32; 10 * 10];
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        paint(
            &mut canvas,
            &mut Fonts::new(),
            &Theme::dark(),
            1.0,
            4.0,
            &review(1),
        );
    }
}
