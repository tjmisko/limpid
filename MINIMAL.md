# Minimal limpid

Investigation and plan, 2026-10-26. The question: why does a static page cost
tens of MiB, what does limpid actually need from a web engine, and what is the
smallest thing that still does the job. Numbers are from this machine (Asahi
Linux, aarch64, Hyprland at 2× scale, GTK 4.22.5, WebKitGTK 6.0 2.54.1) and were
taken with `bench/measure_memory.py` and `bench/measure_startup.py`.

"Private" below is `Pss_Anon` from `smaps_rollup`: memory this process alone
owns. "PSS" adds each process's share of mapped files (libraries, fonts). Both
are read ~3 s after launch; run-to-run noise is about ±8 MiB for the WebKit
stack and ±3 MiB for everything else.

## Summary

- A page in limpid costs **~95 MiB private, ~240 MiB PSS, 8 processes**, and
  the page content is almost none of it: `<p>hi</p>` costs the same as the
  91 KB benchmark page. The cost is three browser runtimes starting
  (web, network, UI) plus GTK, plus four sandbox helpers.
- **No WebKit knob gets below ~65 MiB private.** JIT is already off; the
  cairo renderer, disabling compositing, X11 and the sandbox each move a few
  MiB or nothing. WebKitGTK 6.0 has no single-process mode. Rebuilding WebKit
  with features off trims disk and shared pages, not the private floor.
- **A pure-Rust window that shapes and draws a 2,000-line file costs
  9–14 MiB private, 27–32 MiB PSS, links 5 shared libraries, and is on screen
  in 25 ms.** That is the floor for "from scratch" and it is about a tenth of
  today's memory and a tenth of today's cold start.
- **Blitz** (Stylo + Taffy + Parley + vello_cpu, no JavaScript) renders our
  existing HTML headlessly in 16–41 MiB private including a 12.8 MB raster
  buffer. It is a credible "vendor an engine" option but still needs every
  interaction ported to Rust, has no MathML, and pulls in 311 crates for CSS
  we mostly do not use.
- **Recommendation:** build a native reader (`limpid-view`) from the markdown
  document model we already own. Everything that needs a big runtime (TeX, a
  JavaScript engine, a browser for Mermaid) runs in a short-lived helper that
  writes SVG to the cache and exits; the viewer draws SVG with `resvg`, so
  nothing heavy stays resident. The reply editor is an embedded `nvim`
  (2 MiB private with `--clean`), not CodeMirror. `limpid-host` (WebKit)
  stays as an optional fallback for raw HTML during the transition, then
  becomes a cargo feature. The same stack runs on macOS unchanged.

Constraints added 2026-10-26 after the first draft: no loss of function. Code
highlighting, typeset math, SVG and Mermaid all stay. CodeMirror goes.

## Where the memory goes

Empty page (`<p>hi</p>`), Wayland, defaults:

| Process | Private | PSS | What it is |
|---|---|---|---|
| WebKitWebProcess | 47–49 | 125–138 | JavaScriptCore VM and heap, DOM, style, fonts, ICU, compositor |
| limpid-host | 30–38 | 80–95 | GTK4 (16 alone, see below) plus the WebKit UI side: IPC, process pool, view textures |
| WebKitNetworkProcess | 15 | 28 | Exists for `file://` too; cannot be disabled |
| bwrap ×4, xdg-dbus-proxy | 2 | 2 | Sandbox |
| **Total** | **93–102** | **230–265** | |

Page content on top of that: `bench/small.html` (91 KB) adds ~5 MiB private;
`bench/sample.html` (315 KB, 2,004 lines) adds ~35 MiB, mostly in the web
process.

On disk, `limpid-host` links **167 shared libraries, 268 MB** mapped:
`libwebkitgtk` 96 MB, JavaScriptCore 33 MB, ICU data 31 MB, GTK 12 MB, and
then things a markdown reader has no use for (three `flite` speech-synthesis
voices, AV1 encoders, JPEG XL, GnuTLS, OpenSSL). These are shared with other
GTK/WebKit processes when any are running, and charged to limpid when none are.

### Knobs tried on the empty page

| Setting | Private | Note |
|---|---|---|
| default | 93–102 | |
| `GDK_BACKEND=x11` | 96 | no change |
| `GSK_RENDERER=cairo` | 102 (one run 94) | saves ~6 MiB in the host, noise elsewhere |
| `WEBKIT_DISABLE_COMPOSITING_MODE=1` | 93 | no change |
| cairo + no compositing | 65 | best single run; not reproducible on repeat |
| + `LIBGL_ALWAYS_SOFTWARE=1` | 250 | llvmpipe allocates its own world |

The sandbox helpers are 2 MiB; disabling the sandbox is not worth the loss.
`JSC_useJIT=0` is already set (about 10 MiB).

### Reference floors

| Program | Private | PSS | Window | Stack |
|---|---|---|---|---|
| `foot` | 2 | 10 | | C, Wayland, fcft, pixman |
| **protobase** (prototype) | **9–14** | **27–32** | **25 ms** | Rust: winit, softbuffer, cosmic-text, 5 DSOs |
| bare GTK4 window, Rust, cairo | 10 | 39 | | |
| bare GTK4 window, Rust, default renderer | 16 | 62 | 100 ms | |
| Blitz headless, empty page | 16 | 24 | | includes a 12.8 MB raster buffer |
| Blitz headless, `small.html` | 21 | 30 | | parse 19 ms, layout 4 ms, paint 11 ms |
| alacritty | 24 | 68 | | Rust, OpenGL |
| Blitz headless, `sample.html` | 41 | 50 | | parse 27 ms, layout 31 ms, paint 10 ms |
| limpid, empty page | 93–102 | 230–265 | ~300 ms | GTK4 + WebKitGTK |

The prototype is 120 lines: a winit window, a softbuffer CPU surface, and
cosmic-text laying out `bench/sample.md` as wrapped sans-serif text. It lives in
`~/.cache/limpid-proto/scratch-a` (the Blitz probe is in `scratch-b`; together
their target directories are 1.1 GB and can be deleted).

Why GTK alone is 16 MiB: the default GSK renderer creates a GL (or Vulkan)
context on Asahi, which costs ~6 MiB over cairo. The WebKit web process does the
same again for its compositor. A CPU-only path avoids both.

## What limpid actually needs

Everything the page does today, from `page.js`, `page.css` and `DESIGN.md`:

**Display**
- Headings with ids, a rail with the TOC and scroll-spy, a compact TOC on
  narrow windows.
- Paragraphs with bold, italic, code spans, links, strikethrough.
- Lists (nested, ordered, task), blockquotes, GitHub-style callouts, rules.
- Code blocks with tree-sitter token colours and a copy action.
- Tables.
- Images, and SVG for cached Mermaid diagrams.
- Math (`pulldown-latex` → MathML, drawn natively by WebKit).
- Mermaid (5 MB of JavaScript, run lazily, SVG cached by the host).
- Question fields with numbered options; saved replies and comments shown as
  annotations under their block.
- A focus band on the current block, a visual-mode band, search highlights, a
  status line, a command line, a review dialog.

**Interaction**: the key table in `DESIGN.md` (block cursor, half-page and
section moves, `gg`/`G`, `z` aligns, visual + yank, `/ n N`, `1`–`9`, the
reply and comment editors with vim keys, `:w :q :wq :q!`, `S`, `t`, zoom,
reload). Mouse is optional by design, which matters below: there is no text
selection to reimplement.

**Already engine-free** and reusable as is: everything in `limpid-core`
(markdown parsing, highlighting, grammars, question detection, replies and
sidecars, feedback composition, the wake socket, the handoff socket, agent
readers, settings, watch logic) and the `limpid` front end.

**What JavaScript is really used for**: Mermaid, and the CodeMirror editor
(512 KB vendored). Everything else in `page.js` (1,100 lines) is DOM
bookkeeping for the cursor, search, yank and annotations, which is simpler in
Rust against our own document model than it is against the DOM. Yank in
particular reconstructs markdown from HTML today; natively it is a slice of the
source by `data-src` range.

So the actual requirement is: **block layout, inline rich text, tables, a
little vector drawing, keyboard input, one window per page, and a text
editor.** Not an HTML engine.

## Keeping the heavy features without keeping them resident

The rule: anything that needs a large runtime runs in a **short-lived helper
process**, writes **SVG to `~/.cache/limpid/` keyed by a hash of the source**,
and exits. The viewer draws SVG with `resvg` (pure Rust, already needed for
Mermaid). The resident cost of a formula or a diagram is the size of its SVG.
A page shows the source until the SVG lands and then swaps it in, which is
how Mermaid already behaves. Nothing below adds to the 10–20 MiB floor.

### Math

`pulldown-latex` to MathML stays for the `--out` HTML page only; the viewer
needs SVG. Backends, selected by `math.backend` in config, `auto` trying them
in this order:

| Backend | Needs | Quality | First render of a page | Resident |
|---|---|---|---|---|
| `latex` | `latex` or `pdflatex` plus `dvisvgm` or `pdftocairo` | TeX | 0.5–1.5 s (one run per page, all formulas batched with the `preview` package, one SVG per page) | none |
| `typst` | the `typst` binary; LaTeX→Typst via the `mitex` crate in `limpid-core` | very good | ~50–100 ms per page | none |
| `mathjax` | nothing installed: vendored MathJax `tex2svg` with its lite DOM adaptor, run under QuickJS (`rquickjs`, ~1 MB) in the helper | very good | ~200 ms per page; the helper peaks at tens of MiB and exits | none |
| `none` | | LaTeX source in monospace with a tint | | |

This machine has `latex`, `pdflatex` and `pdftocairo` but not `dvisvgm`, so the
chain here is `pdflatex` → `pdftocairo -svg`. Every backend produces SVG with
glyphs as paths, so `resvg` needs no math fonts. `auto` means local TeX when it
exists, `typst` when it exists, else the vendored MathJax, which is the
"web-distributed" option the user asked for as a fallback: same JavaScript as
a browser would run, but under a 1 MB engine in a process that exits, not
under WebKit, and never resident.

### Mermaid

There is no Mermaid without a browser: layout calls `getBBox` on live SVG text,
and the fake-DOM attempts do not hold up. So the helper is a headless WebKit
web view (no window) built behind the `webkit` cargo feature. It renders every
uncached diagram on a page with `htmlLabels: false` (otherwise Mermaid emits
`foreignObject`, which `resvg` cannot draw), caches the SVGs, and exits. It
runs only when a page has a diagram that is not cached, costs about a second
that first time, and nothing afterwards. On macOS the same helper over
WKWebView (through `wry`) is a later phase. Without the feature installed, a
diagram shows its source and says why.

### SVG

`resvg` + `usvg` in the viewer, text through `fontdb`. Covers cached Mermaid,
math, and inline or linked SVG in markdown (through the existing sanitizer).

### Editor: embedded nvim

Replace CodeMirror and its vim emulation with the real thing. Neovim has been
embeddable since 0.2: `nvim --embed` speaks msgpack-rpc on stdio, and a client
calls `nvim_ui_attach(cols, rows, {ext_linegrid, ext_multigrid})` and
receives `redraw` events (`grid_line`, `grid_cursor_goto`, `mode_change`,
`win_pos`) that it paints however it likes. That is how Neovide and nvim-qt
work; here each reply or comment box is one grid painted with the viewer's own
glyph pipeline, and keys are forwarded with `nvim_input`.

- Spawned lazily on the first `i` or `c`, one per view process, reused for
  every editor on every window (`ext_multigrid` gives one grid per box),
  killed after idle.
- Measured here: `nvim --headless --clean` is **2 MiB private**; with the
  user's full config, 17 MiB. `editor.init` chooses `clean` (default: a small
  init shipped by limpid, insert mode on open, wrap, spell off) or `user`.
- Buffers are `buftype=acwrite`; `BufWriteCmd` sends `rpcnotify` with the
  buffer text, so `:w`, `:wq`, `:q` and `:q!` keep today's meaning with zero
  emulation, and `Ctrl-Enter` maps to `:wq`.
- Crate: `nvim-rs`, or hand-rolled msgpack-rpc over stdio with `rmpv` (the
  protocol surface used is small).
- nvim is on this machine (0.12.5) and a `brew install` on macOS.

**The boxes keep their look.** nvim supplies cell contents and highlight ids;
limpid paints them. The box chrome (tint, border, focus band, rounded
corners, mode line) is unchanged. The init sets `laststatus=0`,
`number` off, `signcolumn=no`, `showmode` off, and attaches with
`ext_cmdline` and `ext_messages`, so `:` prompts and messages are drawn in
limpid's own command line and status style, not as a vim status bar. Only
cursor, visual, search and spell highlights are taken from nvim; text colour
and font come from the page palette. Cursor shape follows `mode_info_set`.

**Config.** `editor.init = "user"` is the default: the user's keymaps and
plugins work as they do in a terminal. Measured cost here is 17 MiB private
while an editor process is alive; the process is killed after
`editor.idle_seconds` (default 300) with no box open and respawned on the
next `i`/`c`, so the cost is paid only while writing. `editor.init = "clean"`
loads limpid's init only (2 MiB).

### Key routing: page versus editor

Keys mean different things on the page and inside a box, so ownership is a
single explicit state, shown in the status line, never guessed per key.

| State | Who gets every key | How you enter | How you leave |
|---|---|---|---|
| Page | limpid (`keys.rs` dispatcher) | default | `i`/`Enter` on a field, `c` on a block, `/`, `:` |
| Editor | nvim, without exception | `i`/`Enter`/`c` | nvim closes the buffer: `:w`, `:wq`, `:q`, `:q!`, `Ctrl-Enter` |
| Command | limpid's `:` line | `:` on the page | `Enter`, `Esc` |
| Search | limpid's `/` line | `/` on the page | `Enter`, `Esc` |

- In Editor state, limpid converts each winit key event to nvim notation
  (`<C-x>`, `<S-Tab>`, `<CR>`, `<Esc>`, `<BS>`, `<F1>`; printable text as
  is with `<` sent as `<lt>`; IME commits through `nvim_input`) and sends it
  with `nvim_input`. Key repeat is forwarded. Modifier-only events are
  dropped. limpid interprets nothing, so `j`, `k`, `/`, `:` and `Esc` are
  vim's. `Esc` goes to normal mode and never leaves the box; leaving is a
  vim command, as it is today.
- Leaving is observed, not intercepted: `BufWriteCmd` delivers the text
  (save), `QuitPre`/`WinClosed` closes the box, and focus returns to the
  page with the cursor on the box's block. `Ctrl-Enter` is a buffer-local
  map to `:wq` set after the user's config loads, so it wins over user maps.
- Two `:wq`s exist on purpose: in a box it saves that reply or comment; on
  the page it saves everything and sends. The status line says which state
  you are in, and the page command line is unreachable while a box is open.
- One box is open at a time. Opening another while one is open flashes
  `editor open: :wq or :q!`. (Multiple boxes with `ext_multigrid` can come
  later; the routing rule does not change.)
- One escape hatch at the host level, outside nvim: `Ctrl-Shift-q`
  force-closes the box and discards its buffer, for a user config that hangs
  nvim. Nothing else bypasses the editor.
- Window-manager keys (compositor binds) never reach either side; that is
  unchanged.

## Options

### A. Keep WebKit and trim

Knobs (above) are worth at most 10%. A custom WebKitGTK build with media,
WebGL, speech, WebRTC and most of ICU disabled would cut the 268 MB of
libraries and shared pages, but not the three-process private floor, and it
adds a one-hour C++ build to `install.sh`. `wry` would give a WKWebView
backend on macOS with the same architecture and roughly the same cost.

Verdict: no. Keep as the fallback for arbitrary HTML, not as the default.

### B. Vendor an HTML/CSS engine without JavaScript (Blitz)

`blitz-html` + `blitz-dom` + `blitz-paint` + `anyrender_vello_cpu`. Pure Rust,
html5ever parsing, Stylo (Firefox's CSS engine), Taffy (block/flex/grid),
Parley (inline), vello_cpu raster. Our rendered HTML and CSS load mostly as is.

- Measured: 16 / 21 / 41 MiB private for empty / small / large, headless, of
  which 12.8 MB is the raster buffer. With a window, roughly 20–45 MiB.
- 311 crates, 18 MB binary, 71 s clean release build.
- No JavaScript, so `page.js` must be ported to Rust against the Blitz DOM
  regardless. The editor must be ours (Parley has a plain-text editor).
- No MathML. `position: sticky` and CSS `columns` (both used by the rail) are
  not supported; the rail becomes a second pane.
- 0.1.x API; expect churn on every upgrade. Stylo pins its own html5ever,
  selectors and `markup5ever` versions.

Verdict: a reasonable fallback if our own inline or table layout proves harder
than budgeted. It keeps the HTML pipeline and the page's look, at 2–4× the
memory of option C and with a large dependency we would be debugging rather
than owning.

### C. From scratch on our own document model

`pulldown-cmark` events → a `Doc` of blocks and inline runs (which
`render.rs` already walks to emit HTML) → our own block and inline layout →
cosmic-text for shaping and glyphs, tiny-skia for rects, lines and SVG paths →
softbuffer into a winit window. CPU only. No HTML on the display path; HTML
remains the `--out` export.

- Measured floor: 9–14 MiB private, 27–32 MiB PSS, 25 ms to a window.
- Dependencies: winit, softbuffer, cosmic-text (fontdb, rustybuzz, swash),
  tiny-skia, resvg/usvg (cached Mermaid SVG), image (PNG/JPEG), plus what
  `limpid-core` has now. About 150 crates, no C libraries beyond libc. The
  prototype binary is 7.8 MB.
- Layout we must write: block stacking, inline wrapping with mixed runs
  (cosmic-text does the wrapping given attributed spans), lists and
  indentation, blockquotes and callouts, code blocks, tables with auto column
  widths, images and SVG boxes, the rail. Estimated 2,500 lines.
- Interaction we must write: cursor, search, yank, command line, fields,
  annotations, editor. Estimated 2,000 lines, replacing 1,100 lines of
  `page.js`, 1,900 lines of host, 512 KB of CodeMirror and 5.2 MB of Mermaid.
- Math, Mermaid and the editor are covered by the helper and nvim sections
  above. No mouse text selection (not in the design). No inspector.
- macOS: winit, softbuffer and cosmic-text run on AppKit unchanged.

Verdict: recommended. The work is a document layout engine for one document
format we control, and the measured floor is where the user wants to be.

### Own layout versus Blitz, concretely

Both are an order of magnitude under today. The difference is where the
remaining cost sits and who owns the bugs.

| | Own layout (C) | Blitz (B) |
|---|---|---|
| New code in N2 | ~2,500 lines (block, inline via cosmic-text, lists, quotes, code, tables, images, SVG boxes, rail) | ~600 lines (embed, scroll, overlays, reserved boxes for nvim grids) |
| Crates / shared libs / binary | ~150 / 5 / ~8 MB | ~310 / 5 / ~18 MB |
| Clean release build | ~40 s | ~75 s (Stylo) |
| Private memory, empty page | ~10 (measured prototype) | 16 (measured, headless) |
| Private memory, `small.html` | ~12 (estimate) | 21 (measured) |
| Private memory, 2,000-line page | ~15 (plain text measured 9–14) | 41 (measured): Stylo keeps a computed style per node |
| Scaling | lazy: lay out blocks until the viewport is full, the rest on scroll; a 10-turn transcript or a 10 MB file opens in the same time as a small one | whole document every time: 58 ms for 2,000 lines (parse 27 + layout 31) and roughly linear, so a 1 MB transcript page is ~200 ms and ~100 MiB |
| Path to pixels | Doc → layout → paint | Doc → HTML string → html5ever → Stylo → Taffy → paint: two extra serializations on the warm path |
| Fidelity to the current look | `page.css` re-expressed as a theme struct; some drift in margins and line heights to tune by eye | the same CSS; near-exact, minus `position: sticky` and `columns` (rail becomes a pane) |
| Tables | ours: auto column widths, wrapping cells. Markdown tables are plain grids, no spans | done |
| Raw HTML inside markdown | a subset (`b i code a br details`); the rest shows as text | full |
| Focus band, search highlight, visual band | first-class in the layout tree; no restyle | overlay painting using box positions from blitz-dom, or DOM mutation plus restyle (ms on big pages) |
| Images, SVG | `image` + `resvg`, ours | done by Blitz (`usvg`) |
| nvim grids, math SVG swap-in | native boxes | reserved `div` painted over after `paint_scene`; re-resolve on size change |
| Upstream risk | none; a fixed document format rarely needs layout changes | 0.1.x API; Stylo bumps every ~2 months; an inline-layout bug blocks us until we fork or patch |
| `--out` HTML and the viewer | two renderers from one `Doc`; golden tests keep them in step | one HTML pipeline feeds both |
| `.html` files opened directly | still need the `webkit` feature (today's host) | render natively, no JS |

What tips it: limpid's core page is an agent transcript, and those are the
pages that get long. Blitz lays out and styles the whole document in memory
and is 3–4× the private memory of own layout at 2,000 lines, growing
linearly, while own layout is lazy and flat. The look is a 10 KB stylesheet we
wrote and can re-express; the hard layout piece is markdown tables, which are
grids without spans. Blitz's real advantage is the ~1,900 lines we would not
write and arbitrary HTML files; the second is covered by keeping the WebKit
host behind a feature for people who want it.

**Decision: own layout.** Keep Blitz written down as the fallback if N2's
table or inline work blows its budget, but do not design for it.

### D. Hybrid transition

Build C as `limpid-view` beside `limpid-host`. `limpid` routes markdown and
agent pages to the native viewer and `.html` to WebKit. When the native viewer
covers the key table, `limpid-host` becomes a cargo feature (`webkit`) that
`install.sh` does not build by default. This is the path, not an alternative.

## Plan

Phases are numbered N0–N7 so they can sit beside the existing P-phases in
`TASKS.md`. Each phase ends in something measurable.

### N0. Spike and gates (1–2 days)

- Promote the prototype into `crates/limpid-view`: winit + softbuffer +
  cosmic-text, reading a `.md` path, rendering headings, paragraphs and code
  blocks as plain styled text, scrolling with `j`/`k`/`Ctrl-d`/`Ctrl-u`.
- Record in `bench/RESULTS.md`: private memory, PSS, time to window and time
  to first paint for `bench/sample.md`, one and three windows.
- Gates to continue: ≤ 20 MiB private for one window of `sample.md`, ≤ 60 ms
  from `limpid` invocation to painted text with a view process running.

### N1. Document model

- Split `render.rs` into `doc.rs` (blocks, inline runs, source ranges,
  highlight spans, question fields, annotations, TOC) and `html.rs` (the
  emitter). Golden tests and agent-page tests are unchanged, which proves the
  split.
- Highlighting attaches token classes to inline runs in code blocks instead
  of emitting `<span class="hl-…">`; the HTML emitter maps them back.
- Math nodes carry their LaTeX source and the MathML string; Mermaid nodes
  carry source, hash and the cached SVG path when present.

### N2. Layout and paint

- Block layout: vertical stacking with margins from a theme struct (the
  values in `page.css`), content column `max-width 860px` centred, rail pane
  220 px on the left when the window is wide enough.
- Inline layout through cosmic-text `Buffer` with attributed spans (family,
  weight, style, colour, monospace for code spans, underline for links).
- Lists with markers and hanging indent; task checkboxes; blockquotes and
  callouts with a bar and tint; rules; headings by level.
- Code blocks: monospace, token colours from the highlight theme, wrap or
  horizontal scroll per block (setting), copy action.
- Tables: min-content / max-content column widths, distribute to the column
  width, header row, zebra rule lines; wider tables scroll horizontally.
- Images via `image`; SVG via `resvg` into a tiny-skia pixmap; both lazy and
  cached by size.
- Theme: dark and light from the two `:root` blocks in `page.css`, selected
  by `--dark`/`--light` or config.
- Paint: per-frame clear, draw only blocks intersecting the viewport, glyphs
  from the cosmic-text swash cache into the softbuffer buffer; invalidate on
  scroll, resize, scale change and focus moves.
- Gate: side-by-side screenshots of `sample.md` in WebKit and native with no
  layout difference a reader would notice. **Decision point:** if inline or
  table layout is over budget here, keep the `Doc` model and put Blitz behind
  it instead of our layout (option B); nothing from N1 or N3+ is lost.

### N3. Cursor, search, yank, command line

- Block cursor over the layout's block list (same stop rules as `page.js`:
  paragraphs, code, tables, quotes, list items, fields; not headings or
  rules). Focus band, viewport follows only when needed, cursor starts on the
  first visible stop.
- `j k ↓ ↑`, `PageDown/Up`, `Ctrl-d/u`, `{ }`, `Ctrl-↑/↓`, `gg G`, `zt zz zb`,
  `+ - 0`, `r`, `t`/`F1`, `Esc`.
- `/` with smartcase, `n`/`N` centring the match; matches highlighted in the
  runs.
- Visual mode and yank: ranges are `data-src` byte ranges, so yank is a slice
  of the source; copy via `reply.command`.
- `:` command line and status flashes drawn as one-line overlays.
- Key dispatch reuses `keys.rs` (the dispatcher is already toolkit-free).

### N4. Fields, annotations, editor, delivery

- Question fields render options with numbers; `1`–`9` picks.
- Reply and comment editors: embedded nvim as described above. Spawn on first
  use, `ext_multigrid`, one grid per box, keys through `nvim_input`, text
  back through `BufWriteCmd`. The limpid init file ships in `limpid-core`
  and is written to the cache like the page assets. Height of a box follows
  its line count up to a cap; `Esc` in normal mode with an empty buffer
  closes it.
- Saved replies and comments appear as annotation blocks after their target,
  from the same sidecar and in-file formats `replies.rs` already handles.
- `S` review overlay, `:w` saves pending, `:wq` saves then delivers through
  `deliver_or_wake`, `:q` refuses with the unsaved-changes line. All of this
  calls into `limpid-core` as the host does now.

### N5. Host behaviours

- One `limpid-view` process per colour scheme, serving the existing handoff
  socket (`transport.rs`) and opening one winit window per request; quit on
  last window; `--separate`.
- Watch: poll the source file as today, re-parse, re-layout, restore the
  viewport by the source range at the top.
- `limpid` starts `limpid-view` instead of `limpid-host` for `.md`, `-` and
  agent pages; `.html` still goes to `limpid-host` when it is installed.
- Per-window memory: one framebuffer per window (12.8 MB at 2× for
  1000×800); drop a hidden window's buffer and re-create on expose.

### N6. Helpers: math and Mermaid

- `limpid-core math <page>`: collects the page's formulas, skips cached
  hashes, runs the configured backend (`latex`, `typst`, `mathjax`), writes
  `~/.cache/limpid/math/<hash>.svg`, exits. The viewer launches it when a
  page has uncached math and re-reads the cache when it exits.
- `latex` backend: one `preview`-package document per page, `pdflatex` or
  `latex`, then `pdftocairo -svg` or `dvisvgm --page=1-`; on a compile error,
  retry formula by formula so one bad formula shows its source and the rest
  render.
- `typst` backend: `mitex` conversion in Rust, one `typst compile --format
  svg` per page with page breaks between formulas.
- `mathjax` backend: vendored `tex2svg` bundle built once outside the build
  (like Mermaid today, with notices), run under `rquickjs` inside the helper.
- `limpid-helper mermaid <page>` behind the `webkit` feature: headless
  `WebKitWebView`, `htmlLabels: false`, writes `mermaid/<hash>.svg`, exits.
  Replaces the in-page Mermaid loader and the `mermaid-svg` message handler.
- Inline math is laid out as an inline SVG box on the text baseline (the SVG
  carries its baseline offset in its metadata; `dvisvgm`, `typst` and MathJax
  all provide it); display math is a centred block.

### N6b. Cut WebKit out of the default build

- `limpid-host` behind a workspace feature; `install.sh` builds without GTK
  and WebKit by default; README lists the trimmed runtime requirements and
  the optional ones (`latex`/`typst`, the `webkit` feature for Mermaid).
- Delete `vendor/editor.min.js` and the CodeMirror entry. Keep
  `vendor/mermaid.min.js` for the helper and for `--out` pages.
- `--out` keeps producing the self-contained HTML page for browsers, with
  MathML and the Mermaid loader as now.

### N7. macOS

- Clipboard default `pbcopy`; `open` for links; `Cmd` as `Ctrl` for the
  host-level keys; Retina scale through winit; fonts through fontdb.
- A macOS CI job that builds `limpid`, `limpid-core` and `limpid-view`
  without the `webkit` feature. No `wezterm`/`wl-copy` assumptions in the
  default config on that platform.

### Size and outcome

Roughly 5,000–6,000 lines of Rust across N1–N5. Expected result for a
2,000-line page: 12–20 MiB private, one process, 5 shared libraries, window in
~30 ms, text painted within ~60 ms warm. Today: 95 MiB, 8 processes, 167
libraries, 300 ms cold and 170 ms warm, where 120 ms of the warm path is
rendering to HTML, writing it, and having WebKit parse it again, which the
native path does not do.

## What will bite us

1. **Inline text.** Shaping, fallback fonts, emoji, bidi and line breaking are
   cosmic-text's job and it does them; colour emoji depends on the installed
   font format (bitmap and COLRv0 work, COLRv1 may not). Hinting at 2× is a
   non-issue.
2. **Tables.** Auto column widths and wrapping cells are the hardest layout
   piece. Budget them explicitly; this is the N2 decision point.
3. **Code blocks.** A 2,000-line fence must highlight lazily (query compile
   and parse off the first-paint path) and either wrap or scroll. Today's
   warm-open cost is already dominated by this.
4. **Math helpers.** Batched LaTeX fails as a batch on one bad formula (retry
   singly). Inline math needs the baseline offset from each backend to sit on
   the text line; `dvisvgm`, `typst` and MathJax provide it differently.
   `pdftocairo` output keeps glyphs as paths, which is what we want; some
   TeX fonts come out as bitmaps under old `dvips` setups. MathJax under
   QuickJS needs a bundle built without Node globals; `mathjax-full` with
   `liteAdaptor` is built for that, but the bundle is ours to maintain.
   `mitex` covers common LaTeX math; exotic packages do not convert.
5. **Mermaid** needs a browser. The helper is the only place WebKit remains,
   it is optional, and it never stays resident. `htmlLabels: false` changes
   the look of node labels slightly (no HTML wrapping inside nodes).
5b. **Embedded nvim.** Painting a grid is easy; the details are `ext_multigrid`
   resize and float handling, converting winit key events to nvim `<…>`
   notation (modifiers, dead keys, IME commits via `nvim_input` vs
   `nvim_paste`), and a user config that opens UI-dependent plugins. The
   `clean` init avoids the last; `user` is opt-in. If nvim is missing the
   editor falls back to a plain insert-only box so replies still work.
6. **CPU paint at 2×.** 12.8 MB per frame; clearing and blitting glyphs for a
   viewport of text is a few ms, fine at 60 Hz for block-wise scrolling. If
   smooth scrolling is wanted later, add damage tracking before reaching for
   a GPU (which costs 15–25 MiB and a shader compile at startup).
7. **Per-window framebuffers** are the dominant per-window cost at HiDPI, as
   they are in WebKit today (~10 MiB per extra window).
8. **Wayland details.** Window title must be set before the first commit so
   `hyprctl` and the benchmark see it; decorations are server-side on
   Hyprland but winit needs a CSD fallback (`sctk-adwaita`) elsewhere; IME
   via winit's IME events for the editor; fractional scale changes trigger
   relayout.
9. **Fonts.** fontdb scans 664 fonts here in a few ms; `Inter` is not
   installed, so the default family falls back to Bitstream Vera. Pick
   families in config and ship nothing.
10. **No inspector.** Add `limpid-view --dump-layout FILE` early; it is the
    debugging tool for N2 and N3.
11. **Two renderers during N1–N5.** The `Doc` model with the existing golden
    tests keeps them consistent; the WebKit path is not touched until N6.
12. **Lost for free:** accessibility tree, mouse text selection, printing,
    arbitrary HTML files. Each is either out of the design or stays on the
    optional WebKit path.

## Harness notes

- `bench/measure_memory.py <label> <settle-seconds> -- <cmd>` launches a
  command under a private `XDG_RUNTIME_DIR` with the Wayland socket
  symlinked in, prints PSS / private / file / RSS per process, and kills the
  tree. `MEASURE_ENV="K=V ..."` adds environment variables.
- `bench/measure_startup.py <title> <runs> -- <cmd>` times spawn to a window
  with that title in `hyprctl clients`.
- `bench/bench_limpid.py` isolates `XDG_RUNTIME_DIR` without symlinking the
  Wayland socket, so GTK fell back to X11 through XWayland in the recorded
  `RESULTS.md` runs. Private memory is about the same either way; PSS and
  timings differ. Fix before re-recording.

## Decisions so far (2026-10-26)

- Code, typeset math, SVG and Mermaid stay in scope. CodeMirror goes.
- Editor: embedded nvim. No `$EDITOR` hand-off; the harness's own `Ctrl-g`
  already covers long-form editing.
- Heavy renderers run in helpers and cache SVG; nothing heavy is resident.
- Prototype build output in `~/.cache/limpid-proto` is to be deleted
  (the delete was blocked by a permission rule; run `rm -rf ~/.cache/limpid-proto`).
- Math: `auto` order is local TeX, then `typst`, then vendored MathJax. All
  three backends are in scope before the native viewer becomes the default.
  External tools are detected, never required: MathJax under QuickJS is
  built in, so a machine with nothing installed still gets typeset math.
  `dvisvgm` is preferred when present, `pdftocairo` otherwise.
- Editor: the user's nvim config by default (`editor.init = "user"`), with
  the key-routing rule above. Boxes keep their current look.
- `--out` HTML, `page.js`, `page.css`, MathML and the Mermaid loader stay
  alive for browsers.
- Layout: own layout, decided now; Blitz is the written-down fallback only.

## Questions

1. nvim idle kill: 300 s after the last box closes, or keep it for the life
   of the view process?
   - 300 s; it respawns in well under 100 ms.
2. Multiple editor boxes open at once in the first version, or one at a
   time?
   - One at a time first; `ext_multigrid` later.
3. Start N0 now on a worktree (`feat/native-view`), or wait for more design
   talk?
   - Start N0; it is the spike that validates everything else.
