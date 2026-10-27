//! The document model: Markdown parsed once into blocks and inlines that both
//! the HTML emitter and the native viewer read.

use crate::render::{self, RenderMode};
use crate::settings::Settings;
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A Markdown file on disk: raw HTML is sanitized and saved annotations
    /// become [`BlockKind::Annotation`] blocks.
    File,
    /// Agent output: raw HTML is shown as text.
    Agent,
}

impl From<Mode> for RenderMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::File => RenderMode::File,
            Mode::Agent => RenderMode::Agent,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub title: Option<String>,
    /// The Markdown. Every `src` range indexes into it.
    pub source: String,
    pub blocks: Vec<Block>,
    pub toc: Vec<TocEntry>,
    /// Question fields in document order.
    pub fields: Vec<FieldRef>,
    /// How `source` was parsed; `html::emit` renders it the same way.
    pub(crate) mode: Mode,
    /// The question headings in force when `source` was parsed.
    pub(crate) question_headings: Vec<String>,
}

impl Doc {
    /// The number of fenced or indented code blocks anywhere in the document
    /// (Mermaid fences are not code blocks).
    pub fn code_block_count(&self) -> usize {
        fn count(blocks: &[Block]) -> usize {
            blocks
                .iter()
                .map(|block| match &block.kind {
                    BlockKind::Code { .. } => 1,
                    BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
                        count(blocks)
                    }
                    BlockKind::List { items, .. } => {
                        items.iter().map(|item| count(&item.blocks)).sum()
                    }
                    _ => 0,
                })
                .sum()
        }
        count(&self.blocks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub level: u8,
    pub text: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRef {
    pub id: String,
    /// Index into `Doc::blocks` of the top-level block holding the field.
    pub block_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    /// Byte range of the block in `Doc::source`.
    pub src: Range<usize>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlSpan {
    /// Byte range into the code text.
    pub range: Range<usize>,
    /// The highlight name without the `hl-` prefix, e.g. `keyword-function`.
    /// Spans nest; an inner span follows its outer one.
    pub class: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callout {
    pub kind: CalloutKind,
    /// The label shown on the callout, e.g. `Note`.
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Reply,
    Comment,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        id: String,
    },
    Paragraph(Vec<Inline>),
    /// `spans` is empty after [`parse`]; fill it on demand with
    /// [`crate::highlight::highlight_spans`] over `text`.
    Code {
        lang: Option<String>,
        text: String,
        spans: Vec<HlSpan>,
    },
    /// Display math.
    Math {
        latex: String,
        hash: String,
    },
    Mermaid {
        source: String,
        hash: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Quote {
        callout: Option<Callout>,
        blocks: Vec<Block>,
    },
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    Image {
        src: String,
        alt: String,
        title: Option<String>,
    },
    /// Sanitized by ammonia in file mode. In agent mode the raw text exactly as
    /// written (not HTML-escaped): the viewer shows it as text and `html::emit`
    /// escapes it.
    Html(String),
    /// A question under a questions heading: the prompt and its suggested
    /// answers. Replaces the numbered list item it came from.
    Field {
        id: String,
        prompt: Vec<Inline>,
        options: Vec<Vec<Inline>>,
    },
    /// A saved reply or comment. `target` is the byte range of the block it
    /// follows (its own range when nothing precedes it). `field` is the
    /// question id a reply belongs to; `None` for comments.
    Annotation {
        target: Range<usize>,
        kind: AnnotationKind,
        text: String,
        field: Option<String>,
    },
    FootnoteDef {
        label: String,
        blocks: Vec<Block>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    SoftBreak,
    HardBreak,
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link {
        href: String,
        title: Option<String>,
        inlines: Vec<Inline>,
    },
    Image {
        src: String,
        alt: String,
    },
    Math {
        latex: String,
        hash: String,
    },
    FootnoteRef(String),
    /// Sanitized by ammonia in file mode; the raw text, not HTML-escaped, in
    /// agent mode.
    Html(String),
}

/// The cache key of a math expression: sixteen hex digits of
/// `blake3(latex + "\0" + "display" | "inline")`.
pub fn math_hash(latex: &str, display: bool) -> String {
    let mut input = String::with_capacity(latex.len() + 8);
    input.push_str(latex);
    input.push('\0');
    input.push_str(if display { "display" } else { "inline" });
    blake3::hash(input.as_bytes()).to_hex()[..16].to_owned()
}

/// Parse `markdown` into a [`Doc`]. Never fails: malformed input yields the
/// closest readable structure.
pub fn parse(markdown: &str, mode: Mode, settings: &Settings) -> Doc {
    let headings = &settings.questions.headings;
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let (protected, offsets) = render::protect_prose(markdown, options);
    let events: Vec<_> = Parser::new_ext(&protected, options)
        .into_offset_iter()
        .map(|(event, range)| (event, offsets[range.start]..offsets[range.end]))
        .collect();
    let mut builder = Builder {
        events,
        position: 0,
        mode,
        question_headings: headings,
        used: HashMap::new(),
        question_number: 0,
        questions_level: None,
        list_depth: 0,
        block_depth: 0,
        inline_depth: 0,
        toc: Vec::new(),
        title: None,
    };
    let blocks = builder.blocks();
    let fields = collect_fields(&blocks);
    Doc {
        title: builder.title,
        source: markdown.to_owned(),
        blocks,
        toc: builder.toc,
        fields,
        mode,
        question_headings: headings.clone(),
    }
}

fn collect_fields(blocks: &[Block]) -> Vec<FieldRef> {
    fn walk(blocks: &[Block], top: Option<usize>, out: &mut Vec<FieldRef>) {
        for (index, block) in blocks.iter().enumerate() {
            let block_index = top.unwrap_or(index);
            match &block.kind {
                BlockKind::Field { id, .. } => out.push(FieldRef {
                    id: id.clone(),
                    block_index,
                }),
                BlockKind::Quote { blocks, .. } | BlockKind::FootnoteDef { blocks, .. } => {
                    walk(blocks, Some(block_index), out)
                }
                BlockKind::List { items, .. } => {
                    for item in items {
                        walk(&item.blocks, Some(block_index), out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, None, &mut out);
    out
}

/// Deepest container nesting the builder recurses into. Deeper containers are
/// flattened to plain text so hostile input cannot overflow the stack.
pub const MAX_NESTING_DEPTH: usize = 48;

type Item<'a> = (Event<'a>, Range<usize>);

struct Builder<'a> {
    events: Vec<Item<'a>>,
    position: usize,
    mode: Mode,
    question_headings: &'a [String],
    used: HashMap<String, usize>,
    question_number: usize,
    questions_level: Option<u8>,
    list_depth: usize,
    block_depth: usize,
    inline_depth: usize,
    toc: Vec<TocEntry>,
    title: Option<String>,
}

impl<'a> Builder<'a> {
    fn peek(&self) -> Option<&Item<'a>> {
        self.events.get(self.position)
    }

    fn peek_event(&self, ahead: usize) -> Option<&Event<'a>> {
        self.events
            .get(self.position + ahead)
            .map(|(event, _)| event)
    }

    /// Consume the next event when it closes a container.
    fn skip_end(&mut self) -> Option<usize> {
        let (event, range) = self.peek()?;
        if !matches!(event, Event::End(_)) {
            return None;
        }
        let end = range.end;
        self.position += 1;
        Some(end)
    }

    /// Blocks up to and including the end of the enclosing container.
    fn blocks(&mut self) -> Vec<Block> {
        if self.block_depth >= MAX_NESTING_DEPTH {
            return self.flatten_container();
        }
        self.block_depth += 1;
        let blocks = self.blocks_unbounded();
        self.block_depth -= 1;
        blocks
    }

    /// Consume events through the end of the container just opened and keep
    /// only their text, without recursing.
    fn flatten_container_text(&mut self) -> (String, usize) {
        let mut text = String::new();
        let mut level = 1usize;
        let mut end = self.events.get(self.position).map_or(0, |(_, r)| r.start);
        while let Some((event, range)) = self.peek().cloned() {
            self.position += 1;
            end = range.end;
            match event {
                Event::Start(_) => level += 1,
                Event::End(_) => {
                    level -= 1;
                    if level == 0 {
                        break;
                    }
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                }
                Event::Text(part) | Event::Code(part) => text.push_str(&part),
                Event::InlineMath(part) | Event::DisplayMath(part) => text.push_str(&part),
                Event::SoftBreak | Event::HardBreak => text.push(' '),
                _ => {}
            }
        }
        (text.trim().to_owned(), end)
    }

    fn flatten_container(&mut self) -> Vec<Block> {
        let start = self.events.get(self.position).map_or(0, |(_, r)| r.start);
        let (text, end) = self.flatten_container_text();
        if text.is_empty() {
            return Vec::new();
        }
        vec![Block {
            kind: BlockKind::Paragraph(vec![Inline::Text(text)]),
            src: start..end.max(start),
            id: None,
        }]
    }

    fn blocks_unbounded(&mut self) -> Vec<Block> {
        let mut blocks: Vec<Block> = Vec::new();
        while let Some((event, range)) = self.peek().cloned() {
            if matches!(event, Event::End(_)) {
                self.position += 1;
                break;
            }
            let before = self.position;
            self.block(event, range, &mut blocks);
            if self.position == before {
                // A handler that consumed nothing would loop forever.
                self.position += 1;
            }
        }
        blocks
    }

    fn block(&mut self, event: Event<'a>, range: Range<usize>, blocks: &mut Vec<Block>) {
        match event {
            Event::Start(tag) => {
                self.position += 1;
                self.start_block(tag, range, blocks);
            }
            Event::Rule => {
                self.position += 1;
                blocks.push(Block {
                    kind: BlockKind::Rule,
                    src: range,
                    id: None,
                });
            }
            Event::Html(raw) => {
                self.position += 1;
                self.push_html(raw.to_string(), range, blocks);
            }
            Event::TaskListMarker(_) => self.position += 1,
            _ => {
                // Inline events straight inside a tight list item.
                let start = range.start;
                let inlines = self.inlines();
                if inlines.is_empty() {
                    return;
                }
                let end = self.events[self.position - 1].1.end;
                blocks.push(Block {
                    kind: BlockKind::Paragraph(inlines),
                    src: start..end.max(start),
                    id: None,
                });
            }
        }
    }

    fn start_block(&mut self, tag: Tag<'a>, range: Range<usize>, blocks: &mut Vec<Block>) {
        match tag {
            Tag::MetadataBlock(_) => self.metadata(),
            Tag::Heading { level, .. } => self.heading(level as u8, range, blocks),
            Tag::Paragraph => self.paragraph(range, blocks),
            Tag::CodeBlock(kind) => self.code_block(kind, range, blocks),
            Tag::BlockQuote(kind) => self.quote(kind, range, blocks),
            Tag::List(start) => self.list(start, range, blocks),
            Tag::Table(aligns) => self.table(aligns, range, blocks),
            Tag::HtmlBlock => self.html_block(range, blocks),
            Tag::FootnoteDefinition(label) => {
                let inner = self.blocks();
                blocks.push(Block {
                    kind: BlockKind::FootnoteDef {
                        label: label.to_string(),
                        blocks: inner,
                    },
                    src: range,
                    id: None,
                });
            }
            Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Link { .. }
            | Tag::Image { .. } => {
                // An inline run at block level (tight list items).
                self.position -= 1;
                let start = range.start;
                let inlines = self.inlines();
                let end = self.events[self.position - 1].1.end;
                if !inlines.is_empty() {
                    blocks.push(Block {
                        kind: BlockKind::Paragraph(inlines),
                        src: start..end.max(start),
                        id: None,
                    });
                }
            }
            _ => {
                // Containers limpid has no model for keep their blocks.
                let inner = self.blocks();
                blocks.extend(inner);
            }
        }
    }

    fn metadata(&mut self) {
        while let Some((event, _)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::End(_) => break,
                Event::Text(value) => {
                    for line in value.lines() {
                        if let Some(value) = line.strip_prefix("title:") {
                            let title = value.trim().trim_matches(['\'', '"']);
                            self.title = Some(title.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        if self.title.as_deref() == Some("") {
            self.title = None;
        }
    }

    fn heading(&mut self, level: u8, range: Range<usize>, blocks: &mut Vec<Block>) {
        let inlines = self.inlines();
        let end = self.skip_end().unwrap_or(range.end);
        let text = plain_text(&inlines);
        if self.questions_level.is_some_and(|open| level <= open) {
            self.questions_level = None;
        }
        if self
            .question_headings
            .iter()
            .any(|heading| heading.eq_ignore_ascii_case(text.trim()))
        {
            self.questions_level = Some(level);
        }
        let id = render::unique_heading_id(&text, &mut self.used);
        if self.title.as_deref().is_none_or(str::is_empty) && !text.is_empty() {
            self.title = Some(text.clone());
        }
        if level <= 3 {
            self.toc.push(TocEntry {
                level,
                text,
                id: id.clone(),
            });
        }
        blocks.push(Block {
            kind: BlockKind::Heading {
                level,
                inlines,
                id: id.clone(),
            },
            src: range.start..end.max(range.start),
            id: Some(id),
        });
    }

    fn paragraph(&mut self, range: Range<usize>, blocks: &mut Vec<Block>) {
        let first = self.peek_event(0).cloned();
        let only_child = matches!(self.peek_event(1), Some(Event::End(_)));
        let inlines = self.inlines();
        let end = self.skip_end().unwrap_or(range.end);
        let src = range.start..end.max(range.start);
        let kind = match (first, only_child) {
            (Some(Event::DisplayMath(latex)), true) => BlockKind::Math {
                hash: math_hash(&latex, true),
                latex: latex.to_string(),
            },
            (Some(Event::Start(Tag::Image { title, .. })), _) if inlines.len() == 1 => {
                match inlines.first() {
                    Some(Inline::Image { src, alt }) => BlockKind::Image {
                        src: src.clone(),
                        alt: alt.clone(),
                        title: (!title.is_empty()).then(|| title.to_string()),
                    },
                    _ => BlockKind::Paragraph(inlines),
                }
            }
            _ => BlockKind::Paragraph(inlines),
        };
        blocks.push(Block {
            kind,
            src,
            id: None,
        });
    }

    fn code_block(
        &mut self,
        kind: CodeBlockKind<'a>,
        range: Range<usize>,
        blocks: &mut Vec<Block>,
    ) {
        let lang = match &kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
            CodeBlockKind::Indented => "",
        };
        let lang: String = lang
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || "_+#.-".contains(*ch))
            .collect();
        let mut text = String::new();
        let mut end = range.end;
        while let Some((event, event_range)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::End(_) => {
                    end = event_range.end;
                    break;
                }
                Event::Text(part) | Event::Code(part) => text.push_str(&part),
                Event::SoftBreak | Event::HardBreak => text.push('\n'),
                _ => {}
            }
        }
        let src = range.start..end.max(range.start);
        if lang == "mermaid" {
            blocks.push(Block {
                kind: BlockKind::Mermaid {
                    hash: crate::replies::source_hash(text.as_bytes()),
                    source: text,
                },
                src,
                id: None,
            });
            return;
        }
        blocks.push(Block {
            kind: BlockKind::Code {
                lang: (!lang.is_empty()).then_some(lang),
                text,
                spans: Vec::new(),
            },
            src,
            id: None,
        });
    }

    fn quote(
        &mut self,
        kind: Option<BlockQuoteKind>,
        range: Range<usize>,
        blocks: &mut Vec<Block>,
    ) {
        let mut inner = self.blocks();
        let mut callout = kind.map(|kind| {
            let (kind, title) = match kind {
                BlockQuoteKind::Note => (CalloutKind::Note, "Note"),
                BlockQuoteKind::Tip => (CalloutKind::Tip, "Tip"),
                BlockQuoteKind::Important => (CalloutKind::Important, "Important"),
                BlockQuoteKind::Warning => (CalloutKind::Warning, "Warning"),
                BlockQuoteKind::Caution => (CalloutKind::Caution, "Caution"),
            };
            Callout {
                kind,
                title: title.into(),
            }
        });
        if callout.is_none() && strip_ask_marker(&mut inner) {
            callout = Some(Callout {
                kind: CalloutKind::Ask,
                title: "Ask".into(),
            });
        }
        blocks.push(Block {
            kind: BlockKind::Quote {
                callout,
                blocks: inner,
            },
            src: range,
            id: None,
        });
    }

    fn list(&mut self, start: Option<u64>, range: Range<usize>, blocks: &mut Vec<Block>) {
        self.list_depth += 1;
        let ordered = start.is_some();
        let is_question_list = self.questions_level.is_some() && self.list_depth == 1 && ordered;
        let mut items = Vec::new();
        let mut fields = Vec::new();
        while let Some((event, item_range)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::End(_) => break,
                Event::Start(Tag::Item) => {
                    let checked = match self.peek_event(0) {
                        Some(Event::TaskListMarker(checked)) => Some(*checked),
                        _ => None,
                    };
                    let inner = self.blocks();
                    if is_question_list {
                        self.question_number += 1;
                        fields.push(field_from_item(
                            format!("q{}", self.question_number),
                            inner,
                            item_range,
                        ));
                        continue;
                    }
                    items.push(ListItem {
                        checked,
                        blocks: inner,
                    });
                }
                _ => {}
            }
        }
        self.list_depth -= 1;
        if is_question_list {
            blocks.extend(fields);
            return;
        }
        blocks.push(Block {
            kind: BlockKind::List {
                ordered,
                start: start.unwrap_or(0),
                items,
            },
            src: range,
            id: None,
        });
    }

    fn table(&mut self, aligns: Vec<Alignment>, range: Range<usize>, blocks: &mut Vec<Block>) {
        let mut head = Vec::new();
        let mut rows = Vec::new();
        while let Some((event, _)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::End(pulldown_cmark::TagEnd::Table) => break,
                Event::Start(Tag::TableHead) => head = self.cells(),
                Event::Start(Tag::TableRow) => rows.push(self.cells()),
                Event::End(_) => break,
                _ => {}
            }
        }
        blocks.push(Block {
            kind: BlockKind::Table {
                align: aligns
                    .into_iter()
                    .map(|align| match align {
                        Alignment::None => Align::None,
                        Alignment::Left => Align::Left,
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                    })
                    .collect(),
                head,
                rows,
            },
            src: range,
            id: None,
        });
    }

    /// Cells up to and including the end of the row or head.
    fn cells(&mut self) -> Vec<Vec<Inline>> {
        let mut cells = Vec::new();
        while let Some((event, _)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::Start(Tag::TableCell) => {
                    cells.push(self.inlines());
                    self.skip_end();
                }
                Event::End(_) => break,
                _ => {}
            }
        }
        cells
    }

    fn html_block(&mut self, range: Range<usize>, blocks: &mut Vec<Block>) {
        let mut raw = String::new();
        let mut end = range.end;
        while let Some((event, event_range)) = self.peek().cloned() {
            self.position += 1;
            match event {
                Event::End(_) => {
                    end = event_range.end;
                    break;
                }
                Event::Html(value) | Event::Text(value) => raw.push_str(&value),
                _ => {}
            }
        }
        self.push_html(raw, range.start..end.max(range.start), blocks);
    }

    fn push_html(&mut self, raw: String, range: Range<usize>, blocks: &mut Vec<Block>) {
        let kind = match self.mode {
            Mode::Agent => BlockKind::Html(raw),
            Mode::File => match render::parse_saved_annotation(&raw) {
                Some((field, text)) => BlockKind::Annotation {
                    target: blocks
                        .last()
                        .map_or(range.clone(), |block| block.src.clone()),
                    field: field.clone(),
                    kind: if field.is_some() {
                        AnnotationKind::Reply
                    } else {
                        AnnotationKind::Comment
                    },
                    text,
                },
                None => BlockKind::Html(ammonia::clean(&raw)),
            },
        };
        blocks.push(Block {
            kind,
            src: range,
            id: None,
        });
    }

    /// A run of inline events; stops before the first event that is not
    /// inline, without consuming it.
    fn inlines(&mut self) -> Vec<Inline> {
        let mut out: Vec<Inline> = Vec::new();
        while let Some((event, _)) = self.peek().cloned() {
            let inline = match event {
                Event::Text(text) => {
                    self.position += 1;
                    if let Some(Inline::Text(last)) = out.last_mut() {
                        last.push_str(&text);
                        continue;
                    }
                    Inline::Text(text.to_string())
                }
                Event::Code(text) => {
                    self.position += 1;
                    Inline::Code(text.to_string())
                }
                Event::SoftBreak => {
                    self.position += 1;
                    Inline::SoftBreak
                }
                Event::HardBreak => {
                    self.position += 1;
                    Inline::HardBreak
                }
                Event::InlineMath(latex) => {
                    self.position += 1;
                    Inline::Math {
                        hash: math_hash(&latex, false),
                        latex: latex.to_string(),
                    }
                }
                Event::DisplayMath(latex) => {
                    self.position += 1;
                    Inline::Math {
                        hash: math_hash(&latex, true),
                        latex: latex.to_string(),
                    }
                }
                Event::FootnoteReference(label) => {
                    self.position += 1;
                    Inline::FootnoteRef(label.to_string())
                }
                Event::InlineHtml(raw) => {
                    self.position += 1;
                    Inline::Html(match self.mode {
                        Mode::Agent => raw.to_string(),
                        Mode::File => ammonia::clean(&raw),
                    })
                }
                Event::Start(Tag::Emphasis) => Inline::Emph(self.inline_container()),
                Event::Start(Tag::Strong) => Inline::Strong(self.inline_container()),
                Event::Start(Tag::Strikethrough) => Inline::Strike(self.inline_container()),
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => {
                    let inlines = self.inline_container();
                    Inline::Link {
                        href: safe_href(&dest_url),
                        title: (!title.is_empty()).then(|| title.to_string()),
                        inlines,
                    }
                }
                Event::Start(Tag::Image { dest_url, .. }) => {
                    let inlines = self.inline_container();
                    Inline::Image {
                        src: safe_href(&dest_url),
                        alt: plain_text(&inlines),
                    }
                }
                _ => break,
            };
            out.push(inline);
        }
        out
    }

    /// The inlines of the container started at the current position.
    fn inline_container(&mut self) -> Vec<Inline> {
        self.position += 1;
        if self.inline_depth >= MAX_NESTING_DEPTH {
            let (text, _) = self.flatten_container_text();
            return if text.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Text(text)]
            };
        }
        self.inline_depth += 1;
        let inner = self.inlines();
        self.inline_depth -= 1;
        self.skip_end();
        inner
    }
}

fn safe_href(url: &str) -> String {
    if render::safe_url(url) {
        url.to_owned()
    } else {
        "#".into()
    }
}

/// The text of `inlines` the way heading ids see it.
fn plain_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(value) | Inline::Code(value) => text.push_str(value),
            Inline::SoftBreak | Inline::HardBreak => text.push(' '),
            Inline::Emph(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                text.push_str(&plain_text(inner))
            }
            Inline::Link { inlines, .. } => text.push_str(&plain_text(inlines)),
            Inline::Image { alt, .. } => text.push_str(alt),
            Inline::Math { .. } | Inline::FootnoteRef(_) | Inline::Html(_) => {}
        }
    }
    text
}

/// A quote whose first paragraph starts with `[!ASK]` is an ask callout.
/// Removes the marker and reports whether it was there.
fn strip_ask_marker(blocks: &mut Vec<Block>) -> bool {
    let Some(Block {
        kind: BlockKind::Paragraph(inlines),
        ..
    }) = blocks.first_mut()
    else {
        return false;
    };
    let Some(Inline::Text(first)) = inlines.first_mut() else {
        return false;
    };
    let Some(rest) = first.strip_prefix("[!ASK]") else {
        return false;
    };
    let rest = rest.trim_start().to_owned();
    if rest.is_empty() {
        inlines.remove(0);
    } else {
        *first = rest;
    }
    if matches!(inlines.first(), Some(Inline::SoftBreak | Inline::HardBreak)) {
        inlines.remove(0);
    }
    if inlines.is_empty() {
        blocks.remove(0);
    }
    true
}

/// Build the field for a numbered item under a questions heading: the leading
/// paragraphs are the prompt and the first bullet list holds the options.
fn field_from_item(id: String, blocks: Vec<Block>, src: Range<usize>) -> Block {
    let mut prompt: Vec<Inline> = Vec::new();
    let mut options = Vec::new();
    let mut in_prompt = true;
    for block in blocks {
        match block.kind {
            BlockKind::Paragraph(inlines) if in_prompt => {
                if !prompt.is_empty() {
                    prompt.push(Inline::HardBreak);
                }
                prompt.extend(inlines);
            }
            BlockKind::List {
                ordered: false,
                items,
                ..
            } if options.is_empty() => {
                in_prompt = false;
                for item in items {
                    let mut option = Vec::new();
                    for inner in item.blocks {
                        if let BlockKind::Paragraph(inlines) = inner.kind {
                            if !option.is_empty() {
                                option.push(Inline::HardBreak);
                            }
                            option.extend(inlines);
                        }
                    }
                    options.push(option);
                }
            }
            _ => in_prompt = false,
        }
    }
    Block {
        kind: BlockKind::Field {
            id: id.clone(),
            prompt,
            options,
        },
        src,
        id: Some(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(markdown: &str) -> Doc {
        parse(markdown, Mode::File, &Settings::default())
    }

    fn agent(markdown: &str) -> Doc {
        parse(markdown, Mode::Agent, &Settings::default())
    }

    fn text(value: &str) -> Inline {
        Inline::Text(value.into())
    }

    #[test]
    fn should_nest_blocks_when_lists_are_nested() {
        let doc = file("- one\n  - inner\n    - deep\n- two\n\n1. a\n2. b\n");
        let BlockKind::List { ordered, items, .. } = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks[0]);
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].blocks[0].kind,
            BlockKind::Paragraph(vec![text("one")])
        );
        let BlockKind::List { items: inner, .. } = &items[0].blocks[1].kind else {
            panic!("{:?}", items[0].blocks);
        };
        assert_eq!(
            inner[0].blocks[0].kind,
            BlockKind::Paragraph(vec![text("inner")])
        );
        assert!(matches!(inner[0].blocks[1].kind, BlockKind::List { .. }));
        let BlockKind::List {
            ordered,
            start,
            items,
        } = &doc.blocks[1].kind
        else {
            panic!();
        };
        assert!(*ordered);
        assert_eq!(*start, 1);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn should_record_checked_state_when_items_are_tasks() {
        let doc = file("- [x] done\n- [ ] todo\n- plain\n");
        let BlockKind::List { items, .. } = &doc.blocks[0].kind else {
            panic!();
        };
        let checked: Vec<_> = items.iter().map(|item| item.checked).collect();
        assert_eq!(checked, [Some(true), Some(false), None]);
    }

    #[test]
    fn should_start_ordered_lists_at_their_number_when_not_one() {
        let doc = file("7. seven\n8. eight\n");
        let BlockKind::List { start, .. } = &doc.blocks[0].kind else {
            panic!();
        };
        assert_eq!(*start, 7);
    }

    #[test]
    fn should_tag_each_callout_kind_when_quote_has_alert_marker() {
        for (marker, kind, title) in [
            ("NOTE", CalloutKind::Note, "Note"),
            ("TIP", CalloutKind::Tip, "Tip"),
            ("IMPORTANT", CalloutKind::Important, "Important"),
            ("WARNING", CalloutKind::Warning, "Warning"),
            ("CAUTION", CalloutKind::Caution, "Caution"),
            ("ASK", CalloutKind::Ask, "Ask"),
        ] {
            let doc = file(&format!("> [!{marker}]\n> body text\n"));
            let BlockKind::Quote { callout, blocks } = &doc.blocks[0].kind else {
                panic!("{marker}: {:?}", doc.blocks);
            };
            let callout = callout
                .as_ref()
                .unwrap_or_else(|| panic!("{marker}: no callout"));
            assert_eq!(callout.kind, kind, "{marker}");
            assert_eq!(callout.title, title, "{marker}");
            assert_eq!(
                blocks[0].kind,
                BlockKind::Paragraph(vec![text("body text")]),
                "{marker}"
            );
        }
    }

    #[test]
    fn should_not_be_a_callout_when_quote_is_plain() {
        let doc = file("> just a quote\n");
        let BlockKind::Quote { callout, .. } = &doc.blocks[0].kind else {
            panic!();
        };
        assert!(callout.is_none());
    }

    #[test]
    fn should_keep_ask_text_when_marker_shares_the_line() {
        let doc = file("> [!ASK] which one?\n");
        let BlockKind::Quote { callout, blocks } = &doc.blocks[0].kind else {
            panic!();
        };
        assert_eq!(callout.as_ref().map(|c| c.kind), Some(CalloutKind::Ask));
        assert_eq!(
            blocks[0].kind,
            BlockKind::Paragraph(vec![text("which one?")])
        );
    }

    #[test]
    fn should_carry_alignment_when_table_has_aligned_columns() {
        let doc = file("| a | b | c | d |\n|:--|:-:|--:|---|\n| 1 | **2** | 3 | 4 |\n");
        let BlockKind::Table { align, head, rows } = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(
            align,
            &[Align::Left, Align::Center, Align::Right, Align::None]
        );
        assert_eq!(head.len(), 4);
        assert_eq!(head[0], vec![text("a")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1], vec![Inline::Strong(vec![text("2")])]);
    }

    #[test]
    fn should_hash_inline_and_display_math_differently_when_latex_matches() {
        let doc = file("Inline $x^2$ here.\n\n$$x^2$$\n");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!();
        };
        let Inline::Math {
            latex,
            hash: inline_hash,
        } = &inlines[1]
        else {
            panic!("{inlines:?}");
        };
        assert_eq!(latex, "x^2");
        assert_eq!(inline_hash, &math_hash("x^2", false));
        let BlockKind::Math { latex, hash } = &doc.blocks[1].kind else {
            panic!("{:?}", doc.blocks[1]);
        };
        assert_eq!(latex, "x^2");
        assert_eq!(hash, &math_hash("x^2", true));
        assert_ne!(inline_hash, hash);
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn should_hash_math_the_documented_way_when_computing_cache_keys() {
        let expected = blake3::hash(b"a+b\0display").to_hex()[..16].to_owned();
        assert_eq!(math_hash("a+b", true), expected);
        let expected = blake3::hash(b"a+b\0inline").to_hex()[..16].to_owned();
        assert_eq!(math_hash("a+b", false), expected);
    }

    #[test]
    fn should_make_fields_with_options_when_questions_section_is_numbered() {
        let doc = agent(
            "## Questions\n\n1. Which **one**?\n   - A\n   - B\n2. Why?\n\n## Other\n\n1. Not a question\n",
        );
        assert_eq!(
            doc.fields,
            vec![
                FieldRef {
                    id: "q1".into(),
                    block_index: 1
                },
                FieldRef {
                    id: "q2".into(),
                    block_index: 2
                },
            ]
        );
        let BlockKind::Field {
            id,
            prompt,
            options,
        } = &doc.blocks[1].kind
        else {
            panic!("{:?}", doc.blocks[1]);
        };
        assert_eq!(id, "q1");
        assert_eq!(
            prompt,
            &vec![text("Which "), Inline::Strong(vec![text("one")]), text("?")]
        );
        assert_eq!(options, &vec![vec![text("A")], vec![text("B")]]);
        let BlockKind::Field { options, .. } = &doc.blocks[2].kind else {
            panic!();
        };
        assert!(options.is_empty());
        assert!(matches!(
            doc.blocks[4].kind,
            BlockKind::List { ordered: true, .. }
        ));
    }

    #[test]
    fn should_use_configured_headings_when_settings_name_them() {
        let mut settings = Settings::default();
        settings.questions.headings = vec!["Decisions".into()];
        let doc = parse(
            "# DECISIONS\n\n1. Pick?\n\n# Questions\n\n1. Ignored\n",
            Mode::Agent,
            &settings,
        );
        assert_eq!(doc.fields.len(), 1);
        assert_eq!(doc.fields[0].id, "q1");
    }

    #[test]
    fn should_not_make_fields_when_questions_have_only_bullets_or_nesting() {
        let doc = agent("## Questions\n\n- bullet\n\n1. Main?\n   1. nested numbered\n");
        assert_eq!(doc.fields.len(), 1);
    }

    #[test]
    fn should_attach_annotation_to_preceding_block_when_file_has_saved_reply() {
        let source = "## Questions\n\n1. Pick?\n\n<!-- limpid reply q1\nA --&gt; B\n-->\n\nTail.\n";
        let doc = file(source);
        let annotation = doc
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Annotation { .. }))
            .expect("annotation block");
        let BlockKind::Annotation {
            target,
            kind,
            text,
            field,
        } = &annotation.kind
        else {
            unreachable!();
        };
        assert_eq!(*kind, AnnotationKind::Reply);
        assert_eq!(field.as_deref(), Some("q1"));
        assert_eq!(text, "A --> B");
        let field = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Field { .. }))
            .unwrap();
        assert_eq!(target, &field.src);
        assert!(source[annotation.src.clone()].starts_with("<!-- limpid reply"));
    }

    #[test]
    fn should_read_comment_annotations_when_file_has_saved_comment() {
        let doc = file("Body.\n\n<!-- limpid comment\nNote -->\n");
        let BlockKind::Annotation { kind, text, .. } = &doc.blocks[1].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(*kind, AnnotationKind::Comment);
        assert_eq!(text, "Note");
    }

    #[test]
    fn should_escape_html_when_mode_is_agent_and_sanitize_when_file() {
        let source = "<script>alert(1)</script>\n";
        let doc = agent(source);
        let BlockKind::Html(html) = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(html, "<script>alert(1)</script>\n");
        let doc = file(source);
        let BlockKind::Html(html) = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("alert"), "{html}");
    }

    #[test]
    fn should_keep_inline_html_raw_when_mode_is_agent() {
        let doc = agent("a <b>bold</b> & c\n");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert!(inlines.contains(&Inline::Html("<b>".into())), "{inlines:?}");
        let doc = file("a <b onclick=\"x()\">bold</b>\n");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!();
        };
        assert!(
            inlines.contains(&Inline::Html("<b></b>".into()))
                || inlines
                    .iter()
                    .any(|i| matches!(i, Inline::Html(h) if !h.contains("onclick"))),
            "{inlines:?}"
        );
    }

    #[test]
    fn should_leave_the_field_empty_when_annotation_is_a_comment() {
        let doc = file("Body.\n\n<!-- limpid comment\nNote -->\n");
        let BlockKind::Annotation { field, .. } = &doc.blocks[1].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(*field, None);
    }

    #[test]
    fn should_parse_crlf_input_when_lines_end_with_carriage_returns() {
        let doc = file("# Title\r\n\r\nFirst line\r\nsecond line\r\n\r\n- a\r\n- b\r\n");
        assert_eq!(doc.title.as_deref(), Some("Title"));
        assert!(matches!(
            doc.blocks[0].kind,
            BlockKind::Heading { level: 1, .. }
        ));
        let BlockKind::Paragraph(inlines) = &doc.blocks[1].kind else {
            panic!("{:?}", doc.blocks[1]);
        };
        assert!(inlines.contains(&Inline::SoftBreak));
        let BlockKind::List { items, .. } = &doc.blocks[2].kind else {
            panic!();
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn should_keep_remaining_text_as_code_when_fence_is_unclosed() {
        let doc = file("```rust\nfn main() {}\nstill code");
        let BlockKind::Code { lang, text, .. } = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(lang.as_deref(), Some("rust"));
        assert_eq!(text, "fn main() {}\nstill code");
        assert_eq!(doc.blocks[0].src.end, doc.source.len());
    }

    #[test]
    fn should_keep_language_and_plain_text_when_fence_language_is_unknown() {
        let doc = file("```nosuchlang\n<b>x</b>\n```\n");
        let BlockKind::Code { lang, text, spans } = &doc.blocks[0].kind else {
            panic!();
        };
        assert_eq!(lang.as_deref(), Some("nosuchlang"));
        assert_eq!(text, "<b>x</b>\n");
        assert!(spans.is_empty());
    }

    #[test]
    fn should_make_no_language_when_fence_has_no_info_string() {
        let doc = file("```\nplain\n```\n");
        let BlockKind::Code { lang, .. } = &doc.blocks[0].kind else {
            panic!();
        };
        assert_eq!(*lang, None);
    }

    #[test]
    fn should_make_mermaid_block_when_fence_language_is_mermaid() {
        let doc = file("```mermaid\ngraph TD; A-->B\n```\n");
        let BlockKind::Mermaid { source, hash } = &doc.blocks[0].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(source, "graph TD; A-->B\n");
        assert_eq!(hash, &crate::replies::source_hash(source.as_bytes()));
    }

    #[test]
    fn should_give_unique_ids_and_toc_when_headings_repeat() {
        let doc = file("---\ntitle: Notes\n---\n# One\n\n## Same\n\n## Same\n\n#### Deep\n");
        assert_eq!(doc.title.as_deref(), Some("Notes"));
        let ids: Vec<_> = doc.toc.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["one", "same", "same-2"]);
        assert_eq!(doc.toc[1].level, 2);
        assert_eq!(doc.toc[1].text, "Same");
        let heading = doc
            .blocks
            .iter()
            .find(|b| b.id.as_deref() == Some("same-2"))
            .unwrap();
        assert!(matches!(heading.kind, BlockKind::Heading { level: 2, .. }));
    }

    #[test]
    fn should_not_panic_when_highlighted_code_starts_with_multibyte_character() {
        let doc = file("```python\né = 1\n```\n");
        assert!(!doc.blocks.is_empty());
        let doc = file("```rust\nlet 日本 = \"é\";\n```\n");
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn should_use_later_heading_as_title_when_first_heading_is_empty() {
        let markdown = "#\n\n# Real\n";
        let doc = file(markdown);
        assert_eq!(doc.title.as_deref(), Some("Real"));
        let rendered = render::render_markdown(markdown);
        assert_eq!(rendered.title, "Real");
        let emitted = crate::html::emit_rendered(&doc, &crate::html::EmitOptions::default());
        assert_eq!(emitted.title, rendered.title);
    }

    #[test]
    fn should_take_title_from_first_heading_when_there_is_no_metadata() {
        assert_eq!(file("## Hello\n\ntext").title.as_deref(), Some("Hello"));
        assert_eq!(file("just text").title, None);
    }

    #[test]
    fn should_rewrite_href_when_link_scheme_is_unsafe() {
        let doc = file("[bad](javascript:alert(1)) [good](https://example.org \"t\")");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!();
        };
        let Inline::Link { href, .. } = &inlines[0] else {
            panic!("{inlines:?}");
        };
        assert_eq!(href, "#");
        let Inline::Link { href, title, .. } = &inlines[2] else {
            panic!();
        };
        assert_eq!(href, "https://example.org");
        assert_eq!(title.as_deref(), Some("t"));
    }

    #[test]
    fn should_make_image_block_when_paragraph_is_only_an_image() {
        let doc = file("![alt text](pic.png \"caption\")\n\ntext ![inline](a.png) more\n");
        assert_eq!(
            doc.blocks[0].kind,
            BlockKind::Image {
                src: "pic.png".into(),
                alt: "alt text".into(),
                title: Some("caption".into()),
            }
        );
        let BlockKind::Paragraph(inlines) = &doc.blocks[1].kind else {
            panic!();
        };
        assert!(matches!(inlines[1], Inline::Image { .. }));
    }

    #[test]
    fn should_keep_footnotes_when_document_defines_them() {
        let doc = file("Claim[^a].\n\n[^a]: The note.\n");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!();
        };
        assert_eq!(inlines[1], Inline::FootnoteRef("a".into()));
        let BlockKind::FootnoteDef { label, blocks } = &doc.blocks[1].kind else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(label, "a");
        assert_eq!(
            blocks[0].kind,
            BlockKind::Paragraph(vec![text("The note.")])
        );
    }

    #[test]
    fn should_make_rule_block_when_thematic_break_appears() {
        let doc = file("above\n\n---\n\nbelow\n");
        assert_eq!(doc.blocks[1].kind, BlockKind::Rule);
    }

    #[test]
    fn should_point_src_at_the_markdown_when_blocks_are_parsed() {
        let source = "# Head\n\nPara one.\n\n```rust\nfn x() {}\n```\n";
        let doc = file(source);
        assert!(source[doc.blocks[1].src.clone()].starts_with("Para one."));
        assert!(source[doc.blocks[2].src.clone()].starts_with("```rust"));
        assert_eq!(doc.source, source);
        for block in &doc.blocks {
            assert!(block.src.end <= source.len());
        }
    }

    fn parse_on_small_stack(markdown: String) -> Doc {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let doc = file(&markdown);
                // Dropping and walking the result must also be safe.
                let _ = doc.fields.len();
                doc
            })
            .unwrap()
            .join()
            .expect("parse must not overflow the stack")
    }

    #[test]
    fn should_not_overflow_when_blockquotes_are_nested_deeply() {
        let doc = parse_on_small_stack("> ".repeat(20000) + "deep text");
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn should_not_overflow_when_lists_are_nested_deeply() {
        let mut source = String::new();
        for level in 0..3000 {
            source.push_str(&"  ".repeat(level));
            source.push_str("- item\n");
        }
        let doc = parse_on_small_stack(source);
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn should_not_overflow_when_emphasis_is_nested_deeply() {
        let source = format!("{}word{}", "*".repeat(20000), "*".repeat(20000));
        let doc = parse_on_small_stack(source);
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn should_keep_text_when_nesting_exceeds_the_cap() {
        let doc = parse_on_small_stack("> ".repeat(200) + "kept words");
        let mut text = String::new();
        fn gather(blocks: &[Block], out: &mut String) {
            for block in blocks {
                match &block.kind {
                    BlockKind::Quote { blocks, .. } => gather(blocks, out),
                    BlockKind::Paragraph(inlines) => out.push_str(&plain_text(inlines)),
                    _ => {}
                }
            }
        }
        gather(&doc.blocks, &mut text);
        assert!(text.contains("kept words"), "got {text:?}");
    }

    #[test]
    fn should_not_panic_when_input_is_empty_or_hostile() {
        assert!(file("").blocks.is_empty());
        assert!(file("\n\n\n").blocks.is_empty());
        for source in [
            "> > > > nested\n",
            "- - - - x\n",
            "| a |\n|---|\n",
            "$$\n",
            "```\n",
            "[^1]\n",
            "<!-- limpid reply q9\n-->",
            "<!-- limpid reply\nx\n-->",
            "---\ntitle:\n---\n",
            "\u{0}\u{1} $ ~ \\",
        ] {
            let _ = file(source);
            let _ = agent(source);
        }
    }

    #[test]
    fn should_keep_prices_as_text_when_dollar_signs_are_not_math() {
        let doc = file("It costs $5 and $6 total.\n");
        let BlockKind::Paragraph(inlines) = &doc.blocks[0].kind else {
            panic!();
        };
        assert!(
            inlines
                .iter()
                .all(|inline| !matches!(inline, Inline::Math { .. }))
        );
        assert_eq!(plain_text(inlines), "It costs $5 and $6 total.");
    }
}
