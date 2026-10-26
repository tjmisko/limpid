//! The placeholder page for this wave: the target file as styled plain text in
//! the content column, scrolled with `j`/`k`/`Ctrl-d`/`Ctrl-u`. The real
//! block layout replaces it in a later work unit.

use crate::{
    paint::{Canvas, Rect},
    text::{Fonts, TextBlock, TextStyle},
    theme::Theme,
    window::Page,
};
use std::{cell::RefCell, ops::Range, path::Path, rc::Rc};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

pub type SharedFonts = Rc<RefCell<Fonts>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkKind {
    Plain,
    Heading(u8),
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    range: Range<usize>,
    kind: ChunkKind,
}

struct LaidChunk {
    block: TextBlock,
    top: f32,
    height: f32,
    kind: ChunkKind,
}

/// Inputs the layout was built for; a change throws the layout away.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutKey {
    column_width: f32,
    scale: f32,
}

pub struct TextPage {
    title: String,
    source: String,
    chunks: Vec<Chunk>,
    theme: Theme,
    fonts: SharedFonts,
    laid: Vec<LaidChunk>,
    laid_for: Option<LayoutKey>,
    scale: f32,
    scroll: f32,
    viewport_height: f32,
    modifiers: ModifiersState,
}

impl TextPage {
    pub fn open(path: &Path, theme: Theme, fonts: SharedFonts) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "limpid".to_owned());
        Ok(Self::from_source(
            &String::from_utf8_lossy(&bytes),
            title,
            theme,
            fonts,
        ))
    }

    pub fn from_source(source: &str, title: String, theme: Theme, fonts: SharedFonts) -> Self {
        let source = source.replace("\r\n", "\n");
        Self {
            title,
            chunks: split_chunks(&source),
            source,
            theme,
            fonts,
            laid: Vec::new(),
            laid_for: None,
            scale: 1.0,
            scroll: 0.0,
            viewport_height: 0.0,
            modifiers: ModifiersState::empty(),
        }
    }

    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// How many chunks have been laid out so far (the rest wait for scrolling).
    pub fn laid_out_chunks(&self) -> usize {
        self.laid.len()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    fn page_padding(&self) -> f32 {
        self.theme.margin * self.scale
    }

    fn column(&self, canvas_width: f32) -> (f32, f32) {
        let gutter = self.page_padding();
        let width = (self.theme.content_width * self.scale)
            .min(canvas_width - 2.0 * gutter)
            .max(1.0);
        ((canvas_width - width).max(0.0) / 2.0, width)
    }

    fn block_gap(&self) -> f32 {
        self.theme.font_size * self.scale * 0.9
    }

    fn code_padding(&self) -> f32 {
        10.0 * self.scale
    }

    fn all_laid(&self) -> bool {
        self.laid.len() == self.chunks.len()
    }

    fn content_height(&self) -> f32 {
        self.laid.last().map_or(0.0, |last| last.top + last.height) + self.page_padding() * 2.0
    }

    fn max_scroll(&self) -> Option<f32> {
        if !self.all_laid() {
            return None;
        }
        Some((self.content_height() - self.viewport_height).max(0.0))
    }

    fn lay_out_next(&mut self, column_width: f32) {
        let index = self.laid.len();
        let Some(chunk) = self.chunks.get(index) else {
            return;
        };
        let theme = &self.theme;
        let scale = self.scale;
        let top = self
            .laid
            .last()
            .map_or(0.0, |last| last.top + last.height + self.block_gap());
        let text = self.source[chunk.range.clone()].trim_end_matches('\n');
        let kind = chunk.kind;
        let text = match kind {
            ChunkKind::Heading(_) => text.trim_start_matches('#').trim_start(),
            _ => text,
        };
        let (size, style, inset) = match kind {
            ChunkKind::Heading(level) => {
                let factor = match level {
                    1 => 1.9,
                    2 => 1.5,
                    3 => 1.25,
                    _ => 1.1,
                };
                (
                    theme.font_size * factor,
                    TextStyle::sans(theme.ink).bold(),
                    0.0,
                )
            }
            ChunkKind::Code => (
                theme.font_size * 0.9,
                TextStyle::mono(theme.ink),
                self.code_padding(),
            ),
            ChunkKind::Plain => (theme.font_size, TextStyle::sans(theme.ink), 0.0),
        };
        let mut fonts = self.fonts.borrow_mut();
        let mut block = TextBlock::new(&mut fonts, size * scale, size * theme.line_height * scale);
        block.set_text(&mut fonts, text, style, &theme.families);
        block.set_width(&mut fonts, Some((column_width - 2.0 * inset).max(1.0)));
        let height = block.height() + 2.0 * inset;
        drop(fonts);
        self.laid.push(LaidChunk {
            block,
            top,
            height,
            kind,
        });
    }

    /// Lay out chunks until the document is covered down to `y`.
    fn ensure_to(&mut self, y: f32, column_width: f32) {
        while !self.all_laid() {
            let covered = self.laid.last().map_or(0.0, |last| last.top + last.height);
            if covered >= y {
                return;
            }
            self.lay_out_next(column_width);
        }
    }

    fn scroll_to(&mut self, target: f32) {
        let mut target = target.max(0.0);
        if let Some(max) = self.max_scroll() {
            target = target.min(max);
        }
        self.scroll = target;
    }

    /// Handle one key press. `ctrl` is the Control modifier state.
    pub fn handle_key(&mut self, key: &Key, ctrl: bool) -> bool {
        let line = self.theme.font_size * self.theme.line_height * self.scale;
        let half = self.viewport_height * self.theme.scroll_fraction;
        let delta = match key {
            Key::Character(text) if ctrl => match text.as_str() {
                "d" => half,
                "u" => -half,
                _ => return false,
            },
            Key::Character(text) => match text.as_str() {
                "j" => line,
                "k" => -line,
                _ => return false,
            },
            Key::Named(NamedKey::ArrowDown) => line,
            Key::Named(NamedKey::ArrowUp) => -line,
            Key::Named(NamedKey::PageDown) => self.viewport_height - line,
            Key::Named(NamedKey::PageUp) => -(self.viewport_height - line),
            _ => return false,
        };
        let target = self.scroll + delta;
        // Lay out ahead of the move so the clamp knows whether the end is near.
        if let Some(key) = self.laid_for {
            self.ensure_to(target + self.viewport_height * 2.0, key.column_width);
        }
        self.scroll_to(target);
        true
    }
}

impl Page for TextPage {
    fn title(&self) -> String {
        self.title.clone()
    }

    fn paint(&mut self, canvas: &mut Canvas, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.scale = scale;
        self.viewport_height = canvas.height() as f32;
        let (column_x, column_width) = self.column(canvas.width() as f32);
        let key = LayoutKey {
            column_width,
            scale,
        };
        if self.laid_for != Some(key) {
            self.laid.clear();
            self.laid_for = Some(key);
        }
        self.ensure_to(self.scroll + self.viewport_height * 2.0, column_width);
        self.scroll_to(self.scroll);

        canvas.clear(self.theme.bg);
        let pad = self.page_padding();
        let viewport = self.viewport_height;
        let mut fonts = self.fonts.borrow_mut();
        for laid in &self.laid {
            let top = pad + laid.top - self.scroll;
            if top > viewport {
                break;
            }
            if top + laid.height < 0.0 {
                continue;
            }
            match laid.kind {
                ChunkKind::Code => {
                    let inset = 10.0 * scale;
                    canvas.fill_rect(
                        Rect::new(column_x, top, column_width, laid.height),
                        self.theme.code_bg,
                    );
                    canvas.draw_text(
                        &mut fonts,
                        &laid.block,
                        column_x + inset,
                        top + inset,
                        self.theme.ink,
                    );
                }
                _ => canvas.draw_text(&mut fonts, &laid.block, column_x, top, self.theme.ink),
            }
        }
    }

    fn key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        self.handle_key(&event.logical_key, self.modifiers.control_key())
    }

    fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }
}

/// Split the source into blank-line separated chunks. Fenced code stays whole
/// and each heading line is its own chunk.
fn split_chunks(source: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut start: Option<usize> = None;
    let mut kind = ChunkKind::Plain;
    let mut fence: Option<String> = None;
    let mut offset = 0;
    let flush = |chunks: &mut Vec<Chunk>, start: &mut Option<usize>, end: usize, kind| {
        if let Some(begin) = start.take() {
            chunks.push(Chunk {
                range: begin..end,
                kind,
            });
        }
    };
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if let Some(marker) = &fence {
            if trimmed.starts_with(marker.as_str()) {
                fence = None;
                flush(&mut chunks, &mut start, offset, ChunkKind::Code);
            }
            continue;
        }
        if let Some(marker) = fence_marker(trimmed) {
            flush(&mut chunks, &mut start, line_start, kind);
            fence = Some(marker);
            start = Some(line_start);
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut chunks, &mut start, line_start, kind);
            continue;
        }
        if let Some(level) = heading_level(trimmed) {
            flush(&mut chunks, &mut start, line_start, kind);
            chunks.push(Chunk {
                range: line_start..offset,
                kind: ChunkKind::Heading(level),
            });
            continue;
        }
        if start.is_none() {
            start = Some(line_start);
            kind = ChunkKind::Plain;
        }
    }
    // An unclosed fence runs to the end of the file.
    let final_kind = if fence.is_some() {
        ChunkKind::Code
    } else {
        kind
    };
    flush(&mut chunks, &mut start, source.len(), final_kind);
    chunks
}

fn fence_marker(trimmed: &str) -> Option<String> {
    let character = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = trimmed.chars().take_while(|c| *c == character).count();
    (run >= 3).then(|| character.to_string().repeat(run))
}

fn heading_level(trimmed: &str) -> Option<u8> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    (rest.is_empty() || rest.starts_with(' ')).then_some(hashes as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(ChunkKind, String)> {
        split_chunks(source)
            .into_iter()
            .map(|chunk| (chunk.kind, source[chunk.range].trim_end().to_owned()))
            .collect()
    }

    fn page(source: &str) -> TextPage {
        TextPage::from_source(
            source,
            "t".into(),
            Theme::dark(),
            Rc::new(RefCell::new(Fonts::new())),
        )
    }

    fn paint(page: &mut TextPage, width: u32, height: u32, scale: f32) -> Vec<u32> {
        let mut pixels = vec![0; (width * height) as usize];
        page.paint(&mut Canvas::new(&mut pixels, width, height), scale);
        pixels
    }

    #[test]
    fn should_split_paragraphs_headings_and_code_when_chunking_source() {
        let source = "# Title\n\nfirst\nsecond\n\n```rs\nlet a = 1;\n\nlet b = 2;\n```\nafter\n";
        let chunks = kinds(source);
        assert_eq!(chunks.len(), 4, "{chunks:?}");
        assert_eq!(chunks[0].0, ChunkKind::Heading(1));
        assert_eq!(chunks[1], (ChunkKind::Plain, "first\nsecond".into()));
        assert_eq!(chunks[2].0, ChunkKind::Code);
        assert!(chunks[2].1.contains("let b = 2;"));
        assert_eq!(chunks[3], (ChunkKind::Plain, "after".into()));
    }

    #[test]
    fn should_run_code_to_the_end_when_a_fence_is_unclosed() {
        let chunks = kinds("```\nstill code\n\nmore code");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, ChunkKind::Code);
        assert!(chunks[0].1.contains("more code"));
    }

    #[test]
    fn should_not_treat_hashes_as_headings_when_there_is_no_space_or_too_many() {
        for line in ["#nospace", "####### seven", "#!shebang"] {
            let chunks = kinds(line);
            assert_eq!(chunks[0].0, ChunkKind::Plain, "{line}");
        }
        assert_eq!(kinds("###### six")[0].0, ChunkKind::Heading(6));
    }

    #[test]
    fn should_produce_no_chunks_when_source_is_blank() {
        assert!(split_chunks("").is_empty());
        assert!(split_chunks("\n\n  \n").is_empty());
    }

    #[test]
    fn should_match_fence_length_when_a_longer_fence_contains_a_shorter_one() {
        let chunks = kinds("````\n```\ninner\n```\n````\ntail");
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0].0, ChunkKind::Code);
    }

    #[test]
    fn should_treat_crlf_like_lf_when_chunking() {
        let page = page("a\r\nb\r\n\r\nc\r\n");
        assert_eq!(page.chunk_count(), 2);
        assert!(!page.source.contains('\r'));
    }

    #[test]
    fn should_paint_background_in_the_corner_and_ink_somewhere_when_painting() {
        let mut page = page("# Hello\n\nsome body text\n");
        let pixels = paint(&mut page, 400, 300, 1.0);
        assert_eq!(pixels[0], Theme::dark().bg.to_pixel());
        assert!(pixels.iter().any(|&p| p != Theme::dark().bg.to_pixel()));
    }

    #[test]
    fn should_scroll_by_lines_and_clamp_at_the_top_when_pressing_j_and_k() {
        let body = (0..200)
            .map(|n| format!("line {n}\n\n"))
            .collect::<String>();
        let mut page = page(&body);
        paint(&mut page, 400, 300, 1.0);
        assert!(page.handle_key(&Key::Character("j".into()), false));
        let after_j = page.scroll();
        assert!(after_j > 0.0);
        page.handle_key(&Key::Character("k".into()), false);
        page.handle_key(&Key::Character("k".into()), false);
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_scroll_half_a_viewport_when_pressing_ctrl_d_and_ctrl_u() {
        let body = (0..200)
            .map(|n| format!("line {n}\n\n"))
            .collect::<String>();
        let mut page = page(&body);
        paint(&mut page, 400, 300, 1.0);
        assert!(page.handle_key(&Key::Character("d".into()), true));
        assert_eq!(page.scroll(), 150.0);
        assert!(page.handle_key(&Key::Character("u".into()), true));
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_ignore_keys_when_they_are_not_bound() {
        let mut page = page("text");
        paint(&mut page, 200, 100, 1.0);
        assert!(!page.handle_key(&Key::Character("q".into()), false));
        assert!(!page.handle_key(&Key::Character("d".into()), false));
        assert!(!page.handle_key(&Key::Named(NamedKey::Escape), false));
    }

    #[test]
    fn should_not_scroll_past_the_end_when_the_document_is_short() {
        let mut page = page("short\n");
        paint(&mut page, 400, 300, 1.0);
        page.handle_key(&Key::Character("d".into()), true);
        page.handle_key(&Key::Named(NamedKey::PageDown), false);
        assert_eq!(page.scroll(), 0.0);
    }

    #[test]
    fn should_stop_at_the_bottom_when_scrolling_far_past_a_long_document() {
        let body = (0..60).map(|n| format!("line {n}\n\n")).collect::<String>();
        let mut page = page(&body);
        paint(&mut page, 400, 300, 1.0);
        for _ in 0..500 {
            page.handle_key(&Key::Character("j".into()), false);
        }
        paint(&mut page, 400, 300, 1.0);
        let max = page.max_scroll().expect("whole document is laid out");
        assert_eq!(page.scroll(), max);
        assert!(max > 0.0);
    }

    #[test]
    fn should_lay_out_lazily_when_the_document_is_large() {
        let body = "paragraph of text\n\n".repeat(20_000);
        let mut page = page(&body);
        paint(&mut page, 800, 600, 1.0);
        assert_eq!(page.chunk_count(), 20_000);
        assert!(page.laid_out_chunks() < 200, "{}", page.laid_out_chunks());
    }

    #[test]
    fn should_relayout_when_the_scale_changes() {
        let mut page = page("# Title\n\nbody\n");
        paint(&mut page, 400, 300, 1.0);
        let one = page.content_height();
        paint(&mut page, 800, 600, 2.0);
        assert!(page.content_height() > one * 1.5);
    }

    #[test]
    fn should_survive_a_zero_sized_canvas_and_a_bad_scale() {
        let mut page = page("text\n\nmore");
        paint(&mut page, 0, 0, 1.0);
        paint(&mut page, 10, 10, f32::NAN);
        paint(&mut page, 10, 10, -2.0);
    }

    #[test]
    fn should_report_the_file_stem_when_opening_a_path() {
        let dir = std::env::temp_dir().join(format!("limpid-view-page-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, b"# hi\n\xff\xfe bad utf8\n").unwrap();
        let page =
            TextPage::open(&path, Theme::dark(), Rc::new(RefCell::new(Fonts::new()))).unwrap();
        assert_eq!(page.title(), "notes");
        assert_eq!(page.chunk_count(), 2);
        assert!(
            TextPage::open(
                &dir.join("missing.md"),
                Theme::dark(),
                Rc::new(RefCell::new(Fonts::new()))
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
