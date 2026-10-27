# Native viewer work units

Execution plan for [MINIMAL.md](MINIMAL.md). Each work unit (WU) is one
implementer's job: a fixed set of files, an API contract other units depend
on, tests written alongside, and a definition of done. Units in the same wave
run in parallel in separate worktrees and must stay inside their file lists.
Everything else in the repo is read-only to them.

Rules for every unit:

- Rust edition 2024; `cargo clippy --all-targets -- -D warnings` clean for the
  crates you touch; `cargo test -p <crate>` green. Tests live beside the code
  (`#[cfg(test)]`) or in the crate's `tests/`.
- Any test that opens a window sets `LIMPID_WINDOW_CLASS=limpid-test` on the
  child process and skips when neither `WAYLAND_DISPLAY` nor `DISPLAY` is set
  (see `crates/limpid-host/tests/window.rs`). Nothing opens a window outside
  tests.
- Early returns over nesting; descriptive names; no emoji; conventional
  commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`), one logical
  change per commit, staged by explicit path so stray files never ride along.
- Every new crate dependency gets one line in the commit body saying why.
- Settings: every new key is optional, has a default, and an unknown key is
  an error naming it (existing behaviour of `settings.rs`).
- Configurability is a goal: constants that a user could reasonably want to
  change (sizes, fonts, keys, timeouts, backends) are settings with defaults,
  not literals.

## Layout of the workspace after this plan

```
crates/limpid-core      parsing, Doc model, HTML emitter, highlighting, math helper, replies, transport
crates/limpid           the command; routes .md and agent pages to limpid-view, .html to limpid-host
crates/limpid-view      the native viewer (new)
crates/limpid-host      WebKit host, kept for .html files and the Mermaid helper, behind a feature
```

## Settings added by this plan (WU-C1 owns `settings.rs`)

```toml
[view]
font = "sans-serif"        # family name, or sans-serif / serif / monospace
mono_font = "monospace"
font_size = 17             # CSS px at scale 1
line_height = 1.65
content_width = 860        # px, the page column
rail_width = 220           # px
rail = "auto"              # auto (when the window is wide enough) | on | off
code_wrap = false          # wrap long code lines instead of scrolling the block
theme = "auto"             # auto (follow --dark/--light, default dark) | dark | light
scroll_fraction = 0.5      # Ctrl-d / Ctrl-u move this fraction of the viewport

[view.keys]                # optional remaps; values are key names (winit logical keys,
next_block = "j"           # with modifiers as "ctrl+d", "shift+g")
prev_block = "k"
# ... one entry per Action in limpid-view's nav::Action; defaults match DESIGN.md

[editor]
command = "nvim"           # binary; may be a path
init = "user"              # user (your config) | clean (limpid's init only)
idle_seconds = 300         # kill the idle nvim after this long with no box open

[math]
backend = "auto"           # auto | latex | typst | mathjax | none
latex = "auto"             # auto | pdflatex | latex
svg = "auto"               # auto | dvisvgm | pdftocairo
typst = "typst"            # binary; may be a path

[mermaid]
enabled = true             # false: diagrams always show their source
```

## Cache layout

```
~/.cache/limpid/math/<hash>.svg        hash = blake3(latex + "\0" + "display"|"inline"), 16 hex
~/.cache/limpid/math/<hash>.json       { "width": px, "height": px, "baseline": px, "backend": "latex"|... }
~/.cache/limpid/mermaid/<hash>.svg     unchanged from today
```

---

## Wave 1

### WU-C1: Document model and HTML emitter (`limpid-core`)

**Owns:** `crates/limpid-core/src/doc.rs` (new), `crates/limpid-core/src/html.rs`
(new), `crates/limpid-core/src/render.rs`, `crates/limpid-core/src/settings.rs`,
`crates/limpid-core/src/lib.rs`, `crates/limpid-core/tests/**`.

**Goal:** the renderer builds a `Doc` first and emits HTML from it. The HTML
output stays byte-identical, proven by the existing golden and fixture tests.

**Contract (`limpid_core::doc`):**

```rust
pub struct Doc {
    pub title: Option<String>,
    pub source: String,            // the markdown
    pub blocks: Vec<Block>,
    pub toc: Vec<TocEntry>,        // { level, text, id }
    pub fields: Vec<FieldRef>,     // question fields in document order: { id: "q1", block_index }
}
pub struct Block { pub kind: BlockKind, pub src: Range<usize>, pub id: Option<String> }
pub enum BlockKind {
    Heading { level: u8, inlines: Vec<Inline>, id: String },
    Paragraph(Vec<Inline>),
    Code { lang: Option<String>, text: String, spans: Vec<HlSpan> }, // HlSpan { range, class: String }
    Math { latex: String, hash: String },                             // display math
    Mermaid { source: String, hash: String },
    List { ordered: bool, start: u64, items: Vec<ListItem> },         // ListItem { checked: Option<bool>, blocks: Vec<Block> }
    Quote { callout: Option<Callout>, blocks: Vec<Block> },           // Callout { kind: Note|Tip|Important|Warning|Caution|Ask, title }
    Table { align: Vec<Align>, head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
    Image { src: String, alt: String, title: Option<String> },
    Html(String),                                                     // sanitized (file mode) or escaped (agent mode)
    Field { id: String, prompt: Vec<Inline>, options: Vec<Vec<Inline>> },
    Annotation { target: Range<usize>, kind: Reply|Comment, text: String },
    FootnoteDef { label: String, blocks: Vec<Block> },
}
pub enum Inline {
    Text(String), Code(String), SoftBreak, HardBreak,
    Emph(Vec<Inline>), Strong(Vec<Inline>), Strike(Vec<Inline>),
    Link { href: String, title: Option<String>, inlines: Vec<Inline> },
    Image { src: String, alt: String },
    Math { latex: String, hash: String },
    FootnoteRef(String), Html(String),
}
pub fn parse(markdown: &str, mode: Mode, settings: &Settings) -> Doc;   // Mode::File | Mode::Agent
```

`html::emit(&Doc, options) -> String` produces what `render.rs` produces
today, including `data-src`, heading ids, the TOC, highlight classes, MathML
(still via `pulldown-latex`), the Mermaid `<pre>`, fields and annotations. If
an exact AST would force a golden change, keep whatever intermediate shape is
needed, but the public `Doc` must exist with the fields above and the goldens
must not change.

Also in this unit: add every settings key from the table above with defaults
and tests (unknown key error, defaults when absent, each key parsed). Expose
`Settings::view`, `::editor`, `::math`, `::mermaid` structs.

**Tests:** existing goldens unchanged; new `doc` tests: nested lists, callouts
of each kind, a table with alignment, inline and display math hashes, a
`## Questions` section with options, an annotation, CRLF input, an unclosed
fence, a fence with an unknown language.

**Done when:** `cargo test -p limpid-core` passes with the goldens untouched;
`limpid_core::doc::parse` and `limpid_core::html::emit` are public; settings
cover the table.

### WU-V1: `limpid-view` skeleton and snapshot renderer

**Owns:** `crates/limpid-view/**` (new crate), `Cargo.toml` (workspace members
only), `install.sh` (add the binary).

**Goal:** a winit window that paints text with cosmic-text into a softbuffer
surface, serves the handoff socket, and can render headlessly to PNG for
tests and evaluators.

Dependencies: `winit 0.30`, `softbuffer 0.4`, `cosmic-text 0.12`,
`tiny-skia 0.11`, `png 0.17` (snapshot output), `limpid-core`, `serde_json`.
No GPU, no GTK.

Modules and contract:

- `main.rs`: args `[--dark|--light] [--separate] [--snapshot WxH [--scale S] --out FILE.png] TARGET`.
  Without `--snapshot`: bind `transport::socket_path` for the scheme (a
  different socket name from the host: `limpid-view-<scheme>.sock`; add a
  `socket_path_view` helper in `transport.rs` is NOT allowed here, so take a
  `--socket PATH` arg for now and let WU-V6 unify), serve handoffs with
  `transport::serve`, open one window per request, quit when the last closes.
  `LIMPID_WINDOW_CLASS` sets the Wayland app id (`WindowAttributesExtWayland::with_name`).
- `theme.rs`: `Theme { bg, surface, ink, muted, faint, teal, blue, amber, purple, code_bg, rule, focus, hl: HighlightPalette, font_size, line_height, content_width, rail_width, families }` with `Theme::dark()` / `::light()` from the values in `page.css` and `Theme::from_settings(&Settings, Scheme)`.
- `text.rs`: `Fonts` (one `FontSystem` + `SwashCache` per process), `Attrs` helpers for sans/mono/weight/style/colour, a `TextBlock` wrapper around `cosmic_text::Buffer` (set width, metrics, spans; height; glyph iteration).
- `paint.rs`: `Canvas` over a `&mut [u32]` of `width × height`: `fill_rect`, `stroke_rect`, `hline`, `draw_text(&TextBlock, x, y, colour)`, `draw_pixmap(tiny_skia::Pixmap, x, y)`; all clipped; premultiplied-alpha blending.
- `window.rs`: window lifecycle, scale factor, resize, redraw request coalescing, a `Page` trait: `fn paint(&mut self, canvas: &mut Canvas, scale: f32)` and `fn key(&mut self, event: &KeyEvent) -> bool`.
- `page.rs`: for this wave, a placeholder page that reads the target file as text, wraps it in the content column with the theme, scrolls with `j`/`k`/`Ctrl-d`/`Ctrl-u`, and sets the window title to the file stem. Replaced by the real layout in WU-V2a.
- `snapshot.rs`: lay out and paint the page at the given size and scale into a buffer and write a PNG. This is the test path: no display needed.

**Tests:** theme from settings (defaults, overrides); `Canvas` fills and
clipping (unit, pixel asserts); snapshot of a fixture `.md` at 800×600 scale 1
and 2 produces a PNG whose dimensions match and whose top-left pixel is the
theme background; a display-gated test that starts the binary with
`LIMPID_WINDOW_CLASS=limpid-test`, sends a handoff, sees the socket, and exits
when the window is closed via a `close` request on the socket (add
`{"close":true}` handling to the view's handoff loop for tests only, gated by
`LIMPID_WINDOW_CLASS`).

**Done when:** `limpid-view --snapshot 800x600 --out /tmp/x.png bench/sample.md`
writes readable text; the window test passes; `bench/measure_memory.py`
against `limpid-view bench/sample.md` records private memory in
`bench/RESULTS.md` under a new "limpid-view" heading (target ≤ 20 MiB).

### WU-C2: Math helper with `latex` and `typst` backends (`limpid-core`)

**Owns:** `crates/limpid-core/src/math/**` (new: `mod.rs`, `cache.rs`,
`latex.rs`, `typst.rs`, `svgmeta.rs`), the `math` subcommand in
`crates/limpid-core/src/main.rs` (add only the match arm and a `mod math;`
line), `crates/limpid-core/Cargo.toml` (add `mitex`).

**Contract (`limpid_core::math`):**

```rust
pub struct Formula { pub latex: String, pub display: bool, pub hash: String }
pub fn collect(markdown: &str) -> Vec<Formula>;              // pulldown-cmark math events, same hash rule as doc.rs
pub struct Rendered { pub svg: PathBuf, pub width: f32, pub height: f32, pub baseline: f32, pub backend: String }
pub fn cached(hash: &str) -> Option<Rendered>;
pub fn render_missing(formulas: &[Formula], settings: &MathSettings) -> Report; // { rendered, cached, failed: Vec<(hash, String)> }
pub trait Backend { fn name(&self) -> &str; fn available(&self, s: &MathSettings) -> bool; fn render(&self, batch: &[&Formula], out: &Path) -> Result<Vec<Rendered>, BackendError>; }
pub fn select(settings: &MathSettings) -> Option<Box<dyn Backend>>;   // auto: latex, typst, (mathjax in WU-C3), else None
```

- `latex.rs`: one document per batch using `\documentclass{article}`,
  `\usepackage[active,tightpage]{preview}`, `\usepackage{amsmath,amssymb}`,
  each formula in a `\begin{preview}` with `\(` or `\[`. Run `pdflatex` (or
  `latex` when `math.latex = "latex"`) in a temp dir, then `pdftocairo -svg`
  per page or `dvisvgm --page=1- --exact-bbox`; `svg = "auto"` prefers
  `dvisvgm` when present. On a compile failure, retry each formula alone and
  report the bad ones. Baseline: with `preview`, use `\PreviewBorder` 0 and
  read the depth from the `.log` (preview prints `Preview: Snippet N x y z`);
  with `dvisvgm`, read its bounding box output. Store width/height/baseline in
  px at 96 dpi in the `.json`.
- `typst.rs`: convert with the `mitex` crate to Typst math, write one `.typ`
  with `#set page(width: auto, height: auto, margin: 0pt)` and a `#pagebreak()`
  between formulas, run `typst compile --format svg out-{p}.svg`, parse
  `viewBox` for width/height; baseline from a zero-width `\rule` marker
  technique or Typst's `measure`, documented in the module.
- `cache.rs`: paths under `~/.cache/limpid/math`, atomic writes (temp +
  rename), `cached()`.
- CLI: `limpid-core math FILE.md [--json]` prints counts or the report.

**Tests:** `collect` on inline/display/currency/code-span cases (reuse the
rules documented in README); the batch document builder (snapshot test of the
generated `.tex` and `.typ`); baseline parsing from a fixture `.log` and a
fixture `dvisvgm` output; a backend-availability test with `PATH` emptied; an
integration test that runs the real `pdflatex` chain and is skipped when
`pdflatex` or `pdftocairo` is missing; an end-to-end cache test with a fake
backend that writes a fixed SVG.

**Done when:** `limpid-core math bench/sample.md` renders every formula on
this machine through `pdflatex` + `pdftocairo`, the `.json` sidecars carry a
plausible baseline, and a second run reports everything cached.

---

## Wave 2

### WU-V2a: Layout and paint for the document (`limpid-view`)

**Owns:** `crates/limpid-view/src/layout/**` (new: `mod.rs`, `block.rs`,
`inline.rs`, `code.rs`, `list.rs`, `quote.rs`, `rail.rs`),
`crates/limpid-view/src/page.rs`, `crates/limpid-view/src/main.rs`,
`crates/limpid-view/tests/**`, `crates/limpid-view/tests/fixtures/**`.

**Goal:** `page.rs` renders a `limpid_core::doc::Doc` with the look of
`page.css`: content column, rail with TOC, headings, paragraphs with inline
styles, code with highlight colours, lists, quotes and callouts, rules.
Tables, images, SVG, math and Mermaid boxes are WU-V2b; leave typed hooks.

Contract (`layout`):

```rust
pub struct Layout { pub blocks: Vec<LaidBlock>, pub height: f32, pub rail: Option<RailLayout> }
pub struct LaidBlock { pub index: usize, pub src: Range<usize>, pub rect: Rect, pub stop: bool, pub content: Content, pub depth: u8 }
pub enum Content { Text(TextBlock), Code { text: TextBlock, gutter: f32, scroll_x: f32, overflow: bool }, Rule, Children(Vec<LaidBlock>), Pending(PendingKind) /* table/image/svg/math/mermaid, filled by V2b */ }
pub fn lay_out(doc: &Doc, width: f32, theme: &Theme, scale: f32, fonts: &mut Fonts) -> Layout;
impl Layout { pub fn ensure_to(&mut self, y: f32, ...) /* lazy: lay out further blocks on demand */; pub fn block_at(&self, y: f32) -> Option<usize>; pub fn stops(&self) -> impl Iterator<Item=&LaidBlock>; }
```

- Lazy layout: lay out blocks until the viewport plus one screen is covered;
  `ensure_to` extends on scroll. A 10 MB file must paint its first screen in
  the same time as a small one.
- Inline runs map to cosmic-text attrs: strong → weight 700, emph → italic,
  code → mono on `code_bg`, link → teal, strike → strikethrough flag (draw
  the line in paint if cosmic-text lacks it), inline math → `Pending` box
  sized from the cache when present else the monospace source.
- Code: highlight spans map `hl-*` classes to `theme.hl` colours; `code_wrap`
  from settings; a wider block scrolls horizontally with `h`/`l` while
  focused (store `scroll_x`).
- Lists: markers, hanging indent, task checkboxes drawn as glyphs or boxes;
  nested lists indent by a theme value.
- Quotes and callouts: left bar and tint per `Callout` kind, matching
  `page.css`.
- Rail: when `rail` is on and the window is at least `content_width +
  rail_width + 2 × margin` wide, a left pane with the TOC; the active entry
  follows the block at the top of the viewport.
- `page.rs` owns scroll state, repaint on scroll/resize/scale, title from
  `doc.title` or the file stem, and parses `.md` via `limpid_core::doc::parse`
  (agent `.html` pages are not supported by the view in this wave; route only
  markdown).

**Tests:** snapshot tests at 1000×800 scale 1 and 2 for a fixture covering
every block kind in scope, with a pixel-diff helper that compares against
committed PNGs under a tolerance and writes the actual image next to the
expected one on failure; a lazy-layout test proving that laying out a
200,000-line fixture to y = 2000 touches fewer than 200 blocks; a rail
threshold test; a `code_wrap` test.

**Done when:** `limpid-view --snapshot 1000x800 --scale 2 --out shot.png
bench/sample.md` is visually comparable to the WebKit rendering of
`bench/sample.html` for the block kinds in scope (the evaluator compares the
two images), and the snapshot tests pass.

### WU-V4: Embedded nvim editor (`limpid-view`)

**Owns:** `crates/limpid-view/src/editor/**` (new: `mod.rs`, `rpc.rs`,
`grid.rs`, `keys.rs`, `paint.rs`, `init.lua`), `crates/limpid-view/Cargo.toml`
(add `rmpv`). Must not touch `main.rs`, `page.rs` or `layout/`.

**Goal:** spawn `nvim --embed`, attach a UI, mirror its grid, forward keys,
observe saves and quits. Integration into the page happens in WU-V5; this
unit ships a self-contained module with its own tests.

Contract:

```rust
pub struct Editor { /* child, reader thread, event queue */ }
pub struct EditorConfig { pub command: String, pub init: InitMode /* User | Clean */, pub cols: u16, pub rows: u16 }
impl Editor {
    pub fn spawn(config: &EditorConfig) -> Result<Editor, EditorError>;
    pub fn open(&mut self, text: &str, kind: BoxKind /* Reply | Comment */) -> Result<(), EditorError>; // sets the buffer, buftype=acwrite, insert mode on empty text
    pub fn input(&mut self, keys: &str);            // nvim notation
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn poll(&mut self) -> Vec<EditorEvent>;      // non-blocking
    pub fn grid(&self) -> &Grid;
    pub fn shutdown(self);
}
pub enum EditorEvent { Redraw, Mode(String), Save(String), Quit { discard: bool }, Message(String), CmdlineShow(String), CmdlineHide, Error(String) }
pub struct Grid { pub cols: u16, pub rows: u16, pub cells: Vec<Cell> /* ch, hl id */, pub cursor: (u16, u16), pub cursor_shape: CursorShape, pub highlights: HashMap<u64, HlAttr> }
pub fn key_to_nvim(event: &winit::event::KeyEvent, modifiers: ModifiersState) -> Option<String>; // keys.rs
pub fn paint_grid(canvas: &mut Canvas, grid: &Grid, rect: Rect, theme: &Theme, fonts: &mut Fonts); // paint.rs
```

- `rpc.rs`: msgpack-rpc over the child's stdin/stdout with `rmpv`; request
  ids; a reader thread pushing notifications to a channel; `nvim_ui_attach`
  with `ext_linegrid`, `ext_cmdline`, `ext_messages`; `nvim_input`;
  `nvim_buf_set_lines`; `nvim_exec_lua` for the init.
- `init.lua` (embedded with `include_str!`, written to the cache like page
  assets, loaded with `-u` in `clean` mode or sourced after the user's config
  in `user` mode): `laststatus=0`, `number` off, `signcolumn=no`, `showmode`
  off, `wrap`, `linebreak`; a buffer-local `BufWriteCmd` that calls
  `rpcnotify(chan, "limpid", "save", lines)` and clears `modified`;
  `QuitPre`/`VimLeavePre` → `rpcnotify(chan, "limpid", "quit", discard)`;
  buffer-local `<C-CR>` mapped to `:wq`. In `user` mode the user's init runs
  first, then this file, so these maps win.
- `grid.rs`: apply `redraw` batches (`grid_resize`, `grid_line`,
  `grid_clear`, `grid_cursor_goto`, `grid_scroll`, `hl_attr_define`,
  `mode_info_set`, `mode_change`, `cmdline_show/hide`, `msg_show`, `flush`).
- `keys.rs`: winit logical key + modifiers → `<C-x>`, `<S-Tab>`, `<CR>`,
  `<Esc>`, `<BS>`, `<Del>`, `<Up>`…, `<F1>`…; printable text as is with `<`
  as `<lt>`; `None` for modifier-only events.
- `paint.rs`: cells in the mono font on the box background; cursor as block,
  bar or underline by mode; only cursor/visual/search/spell attrs are honoured
  from nvim, text colour from the theme.

**Tests:** `key_to_nvim` table; `Grid` applying recorded `redraw` fixtures;
an integration test (skipped if `nvim` is missing) that spawns `--clean`,
opens "hello", sends `A world<Esc>:w<CR>`, expects `Save("hello world")`,
then `:q<CR>` expects `Quit`; an `idle` test is not needed here (WU-V5 owns
the timer).

**Done when:** the integration test passes against the installed nvim and the
module compiles with no use from `main.rs`.

### WU-C3: MathJax backend under QuickJS (`limpid-core`)

**Owns:** `crates/limpid-core/src/math/mathjax.rs` (new), one `mod mathjax;`
line and the `select` arm in `crates/limpid-core/src/math/mod.rs`,
`crates/limpid-core/Cargo.toml` (add `rquickjs`), `vendor/mathjax.min.js`,
`vendor/mathjax-entry.js`, `vendor/README.md` and `vendor/MATHJAX_NOTICES.md`
(new).

**Goal:** typeset math with no external install. Build the bundle once
outside Cargo (document the exact `npm`/`esbuild` steps in `vendor/README.md`
like Mermaid): `mathjax-full` `tex2svg` with `liteAdaptor`, `AllPackages`,
`fontCache: 'none'` so every SVG is self-contained, exposing
`tex2svg(latex, display) -> { svg, widthEx, heightEx, baselineEx }` (read
from the `<svg>` `style="vertical-align"` and width/height in `ex`; convert
to px with 1 ex = 8.5 px at 17 px font, taken from `view.font_size`). Run it
under `rquickjs` in the helper process; evaluate the bundle once per run and
loop over the batch.

**Tests:** bundle loads under QuickJS (test asset is the vendored file);
`x^2` renders to an SVG containing `<path`; a malformed formula reports an
error without aborting the batch; `select` returns `mathjax` when `latex` and
`typst` are unavailable.

**Done when:** `math.backend = "mathjax"` renders `bench/sample.md` with no
TeX or Typst on `PATH`.

### WU-H1: Mermaid helper (`limpid-host`)

**Owns:** `crates/limpid-host/src/mermaid.rs` (new), the `--render-mermaid
FILE` arg and dispatch in `crates/limpid-host/src/main.rs` (small, additive),
`crates/limpid-host/tests/mermaid.rs` (new).

**Goal:** `limpid-host --render-mermaid FILE.md` collects Mermaid fences,
skips cached hashes, renders the rest with the vendored `mermaid.min.js` using
`htmlLabels: false` (and `securityLevel: 'strict'`), writes
`~/.cache/limpid/mermaid/<hash>.svg` through the existing `cache_mermaid`
path, prints `rendered N cached M failed K`, and exits with a non-zero status
only when nothing could be rendered.

- Spike first: can a `WebKitWebView` in an unrealized window run Mermaid and
  return correct `getBBox` sizes? If yes, never show a window. If not, map a
  1×1 window with `LIMPID_WINDOW_CLASS` set to `limpid-helper` and document
  the Hyprland rule needed (the same shape as `limpid-test`), and say so in
  the structured report.
- A hard timeout (`mermaid.timeout_seconds`, default 20, add the key to the
  `[mermaid]` settings table in `settings.rs` is NOT yours; read it with a
  default and note it for WU-V6).
- Verify the output contains no `foreignObject` and that `resvg` can parse it
  (add `usvg` as a dev-dependency for the test only).

**Tests:** a display-gated integration test rendering a fixture with two
diagrams, asserting both SVGs land in a private `XDG_CACHE_HOME`, contain
`<svg`, no `foreignObject`, and parse with `usvg`; a second run reports
`cached 2`.

**Done when:** the test passes and the helper leaves no process behind.

---

## Wave 3

### WU-V2b: Tables, images, SVG, math and Mermaid boxes (`limpid-view`)

**Owns:** `crates/limpid-view/src/layout/table.rs`, `layout/image.rs`,
`layout/svg.rs`, `layout/boxes.rs` (new), the `Pending` arms in
`layout/mod.rs`, `crates/limpid-view/Cargo.toml` (add `resvg`, `usvg`,
`image` with `png` and `jpeg` features only), new fixtures and snapshots
under `crates/limpid-view/tests/`.

- Tables: min-content and max-content widths per column from the cell
  `TextBlock`s; distribute the content width (CSS auto-layout rules: shrink
  proportionally when max-content overflows, never below min-content); header
  row in bold on `surface`; rule lines; alignment from `Align`; a table wider
  than the column scrolls horizontally with `h`/`l` while focused.
- Images: decode with `image`, cache the decoded pixmap by path and size,
  scale to fit the column, alt text when missing.
- SVG: `usvg` + `resvg` into a `tiny_skia::Pixmap` at the device scale,
  cached by (source hash, width, scale).
- Math: display math is a centred SVG box from `math::cached(hash)`; inline
  math is an inline box whose baseline aligns to the text baseline using the
  cached `baseline`; when not cached, show the LaTeX source in the mono font
  on `code_bg`, and let `page.rs` re-lay out when the cache gains the hash
  (expose `Layout::refresh_pending(&mut self, hashes: &[String])`).
- Mermaid: same shape as display math with `~/.cache/limpid/mermaid/<hash>.svg`,
  source shown until cached.

**Tests:** snapshot tests for a tables fixture (narrow, wide, aligned,
wrapped cells), an image fixture (PNG and a missing file), an inline SVG
fixture, a math fixture with pre-seeded cache files, and a Mermaid fixture
with a pre-seeded SVG; the column-width algorithm as unit tests with explicit
numbers.

### WU-V3: Navigation, search, yank, command line (`limpid-view`)

**Owns:** `crates/limpid-view/src/nav/**` (new: `mod.rs`, `cursor.rs`,
`search.rs`, `yank.rs`, `command.rs`, `status.rs`, `keymap.rs`),
`crates/limpid-view/src/page.rs`, `crates/limpid-view/src/main.rs`.

Implements the key table in `DESIGN.md` for the Page, Command and Search
states of the routing table in `MINIMAL.md`:

- `keymap.rs`: `Action` enum (one variant per key in `DESIGN.md`), defaults,
  overrides from `[view.keys]`, multi-key sequences (`gg`, `zt`, `zz`, `zb`)
  with the same timeout as `keys.rs` in the host; counts (`3j`).
- `cursor.rs`: block cursor over `Layout::stops()`, focus band painted
  behind the block, reveal only when off screen, `zt`/`zz`/`zb`, section
  jumps from the TOC, `gg`/`G`, half-page scrolls by `scroll_fraction`.
- `search.rs`: `/` with smartcase over the `Doc` text, highlights in the
  runs, `n`/`N` centre the match.
- `yank.rs`: visual mode band (`v`, motions, `y`), `yy`, `yip`-style section
  yank as today; the yanked text is the markdown source slice by `src`
  ranges, delivered through `reply.command` (via `limpid_core::replies`).
- `command.rs`: `:` line with `:q`, `:q!`, `:qa`, `:w`, `:wq` parsed here but
  `:w`/`:wq` delegate to a `PageActions` trait that WU-V5 implements; until
  then they flash `nothing to save`.
- `status.rs`: one-line status and flashes (`flash_ms` theme value), the
  current state name (`PAGE`, `SEARCH`, `COMMAND`, later `EDITOR`).
- Zoom `+ - 0` changes the effective `font_size` and re-lays out; `r`
  re-reads the file.

**Tests:** keymap parsing and sequence resolution; cursor movement over a
fixture layout (unit, no display); search smartcase cases; yank slices for
block, visual range and section; command parsing; a snapshot with the focus
band on the third block.

---

## Wave 4

### WU-V5: Fields, annotations, editor integration, delivery (`limpid-view`)

**Owns:** `crates/limpid-view/src/feedback/**` (new: `mod.rs`, `fields.rs`,
`boxes.rs`, `review.rs`), the `Editor` wiring in `page.rs` and `main.rs`,
`layout/field.rs` (new) and the `Field`/`Annotation` arms in `layout/mod.rs`.

- Question fields render the prompt and numbered options; `1`–`9` on a
  focused field selects; `Enter`/`i` opens a reply box; `c` opens a comment
  box on any stop; `x` deletes a focused saved comment.
- Boxes: the Editor state from the routing table. One box at a time; spawn
  the `Editor` lazily with the `[editor]` settings, size the box to the
  buffer (cap at a theme value), paint with `editor::paint_grid`, route every
  key through `key_to_nvim`, `Ctrl-Shift-q` force-closes. `Save` writes the
  pending buffer; `Quit` closes the box. Idle timer kills the editor after
  `idle_seconds`.
- Pending state, `:w`, `:wq`, `:q` guard and the review overlay (`S`) call
  `limpid_core::replies` exactly as `limpid-host/src/main.rs` does today
  (`save_pending`, `compose_pending`, `compose_review`, `deliver_or_wake`,
  `has_feedback`); move any of those helpers that live in the host into
  `limpid-core::replies` if they are not already there (that is the one
  allowed edit outside this unit's files: moving, not changing).
- Saved annotations render under their block from the sidecar or in-file
  comments, as today.

**Tests:** field selection and numbering on a fixture; the pending buffer
round trip through `save_pending`; the `:q` guard; a display-free test of
box sizing; an nvim-gated test that opens a comment box, types, `:wq`, and
sees the sidecar written.

---

## Wave 5

### WU-V6: Host behaviours, routing, helpers, install (`limpid-view`, `limpid`, `limpid-host`, docs)

**Owns:** `crates/limpid-view/src/watch.rs`, `src/helpers.rs` (new),
`crates/limpid-view/src/main.rs`, `crates/limpid/src/main.rs`,
`crates/limpid-core/src/transport.rs` (socket naming only),
`crates/limpid-core/src/settings.rs` (add `mermaid.timeout_seconds`),
`crates/limpid-host/Cargo.toml` and the workspace `Cargo.toml` (feature),
`install.sh`, `README.md`, `DESIGN.md`, `TASKS.md`, `.github/workflows/*.yml`.
Added after review, because the unit cannot work without them:
`crates/limpid-view/src/lib.rs` (register `helpers` and `watch`),
`crates/limpid-view/src/page.rs` (`reload_quietly` public, `refresh_pending`
on `DocPage`), `crates/limpid-view/src/window.rs` (`Page::tick_period`, so an
idle watched page sleeps for the watch interval instead of waking every
16 ms), `crates/limpid-core/src/agent_page.rs` and
`crates/limpid-core/src/main.rs` (the `.md` sidecar for agent pages), and the
tests `crates/limpid-core/tests/cli.rs`, `crates/limpid/tests/cli.rs`,
`crates/limpid/tests/install.rs`.

- Watch: poll the source as the host does; on change re-parse, re-lay out,
  and restore the viewport by the `src` range at the top.
- Helpers: after a page opens, if it has uncached math run `limpid-core math`
  and if it has uncached Mermaid run `limpid-host --render-mermaid` (when
  installed and `mermaid.enabled`), each as a child process; when a child
  exits, call `Layout::refresh_pending` and repaint. Never block the UI.
- Routing: `limpid` sends `.md`, `-` and agent pages to `limpid-view`
  (starting it when its socket is absent) and `.html` to `limpid-host`.
  Socket names: `limpid-view-<scheme>.sock`, `limpid-<scheme>.sock` stays
  for the host. `--separate` works for both.
- Agent pages: `limpid-core <agent>` keeps writing the HTML page for `--out`
  and the host, and also writes the markdown it rendered from as
  `<page>.md` next to it, so the view opens the markdown and the sidecar
  logic (keyed by page id) keeps working. `limpid-core feedback` is
  unchanged.
- Feature gate: `limpid-host` builds only with `--features webkit` at the
  workspace level; `install.sh` builds without it by default and with it when
  `LIMPID_WEBKIT=1`; README lists runtime requirements for both.
- Docs: README (commands, requirements, settings table), DESIGN.md
  (architecture diagram and the Windows/Keys/Replies sections now describe
  the view; the Non-goals entry about leaving WebKit is replaced), TASKS.md
  (link to this file and mark the P-phases that the view supersedes).
- CI: build the workspace without the feature on Linux and macOS; with the
  feature on Linux.

**Tests:** watch reload with anchor (display-free, on the `Layout`); routing
decisions in `limpid` as unit tests; a display-gated end-to-end test that
runs `limpid bench/small.md` with `LIMPID_WINDOW_CLASS=limpid-test` and sees
the view's socket; `install.sh --dry-run` prints the commands for both modes.

**Done when:** `limpid notes.md` opens the native viewer by default on this
machine, `bench/RESULTS.md` has the final numbers for the view (one and three
windows, cold and warm), and `cargo test --workspace` passes without the
`webkit` feature.
