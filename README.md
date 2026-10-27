# limpid

Limpid renders Markdown as clean, clear, keyboard-navigable pages in a small
native window.

You can also comment on any rendered block and send a compact
reply back to your agent.

Limpid is built for working with coding agent artifacts and transcripts, but it's
also a snappy markdown reader.


## Use Case
* You want to use coding agents from a harness in the terminal, but sometimes you
want a nicer reading environment for long agent responses that include tables, 
codeblocks, latex, and diagrams.
* You want to navigate agent responses quickly using a keyboard-first, vim-inspired
workflow.
* Leave comments to steer the agent and ask questions *in situ* in the agent's 
response as you read it.

## Install

The default build is the native viewer, the `limpid` command and the helpers.
It needs a Rust toolchain, Git and a C compiler (for the tree-sitter grammars).

```sh
./install.sh                   # native viewer
LIMPID_WEBKIT=1 ./install.sh   # also the WebKit host
./install.sh --dry-run         # print what would run, change nothing
```

The script installs the binaries into `${PREFIX:-$HOME/.local}/bin` and
compiles the configured tree-sitter grammars. Cargo builds never invoke Node
or npm.

### Runtime requirements

Native viewer (default):

| Needed | For |
|---|---|
| Wayland or X11 session | the window |
| `nvim` | reply and comment editing; `editor.command` names another binary |
| `wl-copy` | the default reply command; `wezterm` only if `reply.command = "limpid-core send"` |
| a TeX install (with `dvisvgm` or `pdftocairo`) or `typst` | optional: better math typesetting. `math.backend = "auto"` tries them in that order and falls back to the built-in MathJax, so math works with none installed |

With `LIMPID_WEBKIT=1` the build also produces `limpid-host`, which shows
HTML pages and renders Mermaid diagrams. It needs the GTK4 and WebKitGTK 6.0
development packages and `pkg-config` to build, and GTK4 and WebKitGTK 6.0 at
runtime. Without it, `.html` files cannot be opened and Mermaid fences show
their source.

## Open a page

```sh
limpid notes.md
cat response.md | limpid -
limpid page.html               # needs limpid-host
limpid --out response.html response.md
limpid --separate notes.md
limpid --viewer host notes.md  # force the WebKit host
```

Markdown files, standard input and agent pages open in the native viewer.
Plain `.html` files open in the WebKit host. An HTML page that sits beside the
`.md` and `.meta.json` that `limpid-core <agent>` writes is an agent page and
opens in the native viewer. `--viewer view|host` or `LIMPID_VIEWER` overrides
the choice.

Open files are watched: save the file and the page reloads in place, keeping
your place by the block at the top of the window. `--out` writes a
self-contained HTML page without opening a window. A running viewer shares one
process among its windows; `--separate` opens a dedicated process. `--dark`
and `--light` select separate viewers. The native viewer listens on
`limpid-view-<scheme>.sock` and the host on `limpid-<scheme>.sock` in
`$XDG_RUNTIME_DIR`.

Math is `$x^2$` (inline) and `$$…$$` (display). A dollar sign followed by a
digit is treated as currency, so prices such as `~$0.12` need no escaping. For
numeric formulas, use `$$5$$` or start inline math with a LaTeX command, such
as `$\mathrm{5}$`. Single tildes stay literal; use `~~removed~~` for
strikethrough. Code spans and fences keep their contents.

Formulas and Mermaid fences show their source at first. After the page opens,
the viewer runs `limpid-core math FILE` for formulas and
`limpid-host --render-mermaid FILE` for diagrams as child processes, and the
page redraws when the results are cached in `~/.cache/limpid/`. Neither blocks
the window, and a helper that fails or is not installed leaves the source in
place.

Nothing opens automatically. The pi extension adds `/read` and `Ctrl-R`, and
the Claude Code plugin adds `/read`; both render the current turn with
`limpid-core` and then open it with `limpid`, so the window takes focus like
any other.

For a Codex session, run `limpid codex` or `limpid codex 0..2`. The core reader
also accepts `limpid-core claude [spec]` and `limpid-core pi [spec]`, with their
transcript data as JSON on stdin. Each prints a cached page path that can be
passed to `limpid`, and writes the Markdown it rendered from beside it as
`<page>.md`.

Turn specs include `0`, `3`, `0..2`, `1..=3`, `2..`, `..4`, and `..`. Ranges are
limited to ten turns.

## Answer from the keyboard

A block cursor selects one block at a time: a paragraph, code block, diagram,
table, blockquote, list item or question. `j` / `k` and the arrow keys move it;
PageUp, PageDown, `Ctrl-u` and `Ctrl-d` scroll; `{` / `}` or `Ctrl-Up` /
`Ctrl-Down` jump sections; `gg` / `G` go to the ends; `zt`, `zz` and `zb` put
the focused block at the top, center or bottom. On a focused question, `1`–`9`
choose an option and `Enter` or `i` edits a reply. `c` comments on the focused
block, or reopens a focused comment for editing, and `x` deletes it; `c` on a
question is an answer to it.

Replies and comments are written in your own Neovim, embedded in the box
(`editor.init = "user"` loads your config, `"clean"` loads limpid's small
one). Inside a box every key is Neovim's: `:w` saves, `:q` cancels, and
`Ctrl-Enter` saves directly. `S` opens the composed message for review;
`Ctrl-Enter` there runs the configured reply command.

Edits stay on the page until you save. `:` on the page opens limpid's command
line: `:w` saves, `:q` quits and refuses while unsaved changes remain, `:wq`
saves, sends the answers, and quits, `:q!` quits and discards, and `:qa`
closes every limpid window. `/` searches; `n` and `N` center the current
match like Vim's `nzz`. `v` and `y` select and copy, `Y` copies the page,
`t` or `F1` toggles the contents rail, `r` reloads, and `+`, `-` and `0` zoom.
`h` and `l` scroll a focused table or code block sideways. Every key can be
remapped under `[view.keys]`.

Agent pages save replies beside their cached page. Replies to a markdown file
are stored as HTML comments in that file. limpid refuses to write an annotation
if the file changed since it was rendered.

## Send answers back

Saved answers live beside the page. `limpid-core feedback <page>` prints them
in the compact reply form, `--json` dumps the sidecar, and `limpid-core
feedback --session <id>` finds the newest page for a session (with `--spec` to
narrow it). The compact form is self-contained: an agent answers from it and
only pulls the page when something is unclear, so it never has to load the
whole document.

The pi extension and the Claude Code plugin each bind a per-session socket.
`/read` leaves a note with the page path and a fallback pull command, without
starting a turn. `:wq` writes the answers and wakes that session. Without a socket the viewer falls back to the
configured `reply.command`.

## Configure

All keys are optional in `~/.config/limpid/config.toml`; an unknown key is an
error that names it.

```toml
[highlight]
grammars = ["rust", "python", "typescript", "bash", "json", "toml"]
theme = "default"            # page palette; code tokens use Catppuccin Mocha

[questions]
headings = ["Questions", "Open questions", "Questions for you"]

[reply]
command = "wl-copy"
codex = false

[view]
font = "sans-serif"          # family name, or sans-serif / serif / monospace
mono_font = "monospace"
font_size = 17               # CSS px at scale 1
line_height = 1.65
content_width = 860          # px, the page column
rail_width = 220             # px
rail = "auto"                # auto | on | off
code_wrap = false            # wrap long code lines instead of scrolling
theme = "auto"               # auto | dark | light
scroll_fraction = 0.5        # Ctrl-d / Ctrl-u move this fraction of the window
watch_interval_ms = 300      # how often an open file is checked; 0 turns watching off

[view.keys]                  # optional remaps, e.g. next_block = "n", half_page_down = "ctrl+f"

[editor]
command = "nvim"             # binary; may be a path
init = "user"                # user | clean
idle_seconds = 300           # stop the idle nvim after this long with no box open

[math]
backend = "auto"             # auto | latex | typst | mathjax | none
latex = "auto"               # auto | pdflatex | latex
svg = "auto"                 # auto | dvisvgm | pdftocairo
typst = "typst"              # binary; may be a path

[mermaid]
enabled = true               # false: diagrams always show their source
timeout_seconds = 20         # per page, for the helper
helper_window = "auto"       # auto | never | always: a 1x1 window when layout needs one
```

Run `limpid-core grammars list` to see the catalog and installed grammars, or
`limpid-core grammars sync` after changing the grammar list. Missing grammars
render as plain escaped code. `limpid-core instructions` prints the one-line
question convention for agent instruction files.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# with the WebKit host
cargo test --workspace --features webkit
```

The Rust workspace is in `crates/`: `limpid-core` (parsing, the document
model, math, replies), `limpid-view` (the native viewer), `limpid` (the
command) and `limpid-host` (WebKit, behind the `webkit` feature). Tests that
open a window set `LIMPID_WINDOW_CLASS=limpid-test`, so a compositor rule can
keep them off your desktop, and skip themselves without a display.
`limpid-view --snapshot WxH --out FILE.png FILE.md` paints a page to a PNG
without a display. `catalog.toml` pins grammar sources, and `vendor/` holds the
MathJax, Mermaid and CodeMirror bundles, with license notices. The only JavaScript left is the thin pi and Claude Code integration
wrappers, which run inside those agents and shell out to the binaries.

## License

MIT
