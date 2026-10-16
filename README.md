# limpid

limpid opens markdown files and agent responses as readable pages in a small
GTK4/WebKit window. Questions under a `## Questions` heading become keyboard
answer fields. You can also comment on any rendered block and send a compact
reply back to your agent.

## Install

The Rust build needs GTK4 and WebKitGTK 6.0 development packages, `pkg-config`,
Git, and a C compiler. At runtime, GTK4 and WebKitGTK 6.0 are required;
`wl-copy` is used by the default reply command. `wezterm` is needed only if
you configure `reply.command = "limpid-core send"`.

```sh
./install.sh
```

The script installs the three Rust binaries into `${PREFIX:-$HOME/.local}/bin`
and compiles the configured tree-sitter grammars. Cargo builds never invoke
Node or npm.

## Open a page

```sh
limpid notes.md
limpid page.html
cat response.md | limpid -
limpid --out response.html response.md
limpid --separate notes.md
```

Markdown files are watched for changes. `--out` writes a self-contained page
without opening a window. A running host shares one WebKit context among its
windows; `--separate` opens a dedicated host process. `--dark` and `--light`
select separate hosts.

Math renders as MathML. Mermaid fences show their source immediately; the
diagram script loads after first paint, and completed SVGs are cached for the
next render.

Nothing opens automatically. The pi extension adds `/read` and `Ctrl-R`, and
the Claude Code plugin adds `/read`; both render the current turn with
`limpid-core` and then open it with `limpid`, so the window takes focus like
any other.

For a Codex session, run `limpid codex` or `limpid codex 0..2`. The core reader
also accepts `limpid-core claude [spec]` and `limpid-core pi [spec]`, with their
transcript data as JSON on stdin. Each prints a cached page path that can be
passed to `limpid`.

Turn specs include `0`, `3`, `0..2`, `1..=3`, `2..`, `..4`, and `..`. Ranges are
limited to ten turns.

## Answer from the keyboard

A block cursor selects one block at a time: a paragraph, code block, diagram,
table, blockquote, list item or question. `j` / `k` and the arrow keys move it;
PageUp, PageDown, `Ctrl-u` and `Ctrl-d` scroll; `{` / `}` or `Ctrl-Up` /
`Ctrl-Down` jump sections; `gg` / `G` go to the ends; `zt`, `zz` and `zb` put
the focused block at the top, center or bottom. On a focused question, `1`–`9`
choose an option and `Enter` or `i` edits a reply. `c` comments on the focused
block, or reopens a focused comment for editing, and `x` deletes it. The editor
uses CodeMirror with vim keys: `Esc` then `:w` saves, `:q` cancels, and
`Ctrl-Enter` saves directly. `S` opens the composed message for review;
`Ctrl-Enter` there runs the configured reply command.

Edits stay on the page until you save. `:` opens a page command line: `:w`
saves, `:q` quits and refuses while unsaved changes remain, `:wq` saves, sends
the answers, and quits, `:q!` quits and discards, and `:qa` closes every limpid
window. `S` previews the pending buffer.

Other keys include `/`, `n`, `N`, `t`, `F1`, `r`, `+`, `-`, and `0`. `n` and
`N` center the current match like Vim's `nzz`. Hover a code block for a copy
button, and `F1` toggles the contents rail.

Agent pages save replies beside their cached HTML. Replies to a markdown file
are stored as HTML comments in that file. limpid refuses to write an annotation
if the file changed since it was rendered.

## Send answers back

Saved answers live beside the page. `limpid-core feedback <page>` prints them
in the compact reply form, `--json` dumps the sidecar, and `limpid-core
feedback --session <id>` finds the newest page for a session (with `--spec` to
narrow it). An agent can pull at any time; it never has to wait.

The pi extension binds a per-session socket and `/read` leaves a note with the
page path and the pull command, without starting a turn. `:wq` writes the
answers and wakes that session. Without a socket the host falls back to the
configured `reply.command`.

## Configure

All keys are optional in `~/.config/limpid/config.toml`:

```toml
[highlight]
grammars = ["rust", "python", "typescript", "bash", "json", "toml"]
theme = "default"            # page palette; code tokens use Catppuccin Mocha

[questions]
headings = ["Questions", "Open questions", "Questions for you"]

[reply]
command = "wl-copy"
codex = false
```

Run `limpid-core grammars list` to see the catalog and installed grammars, or
`limpid-core grammars sync` after changing the grammar list. Missing grammars
render as plain escaped code. `limpid-core instructions` prints the one-line
question convention for agent instruction files.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The Rust workspace is in `crates/`; `catalog.toml` pins grammar sources, and
`vendor/` contains the lazy CodeMirror bundle and its license notices. The
renderer, CLI and host are Rust. The only JavaScript left is the thin pi and
Claude Code integration wrappers, which run inside those agents and shell out
to the binaries.

## License

MIT
