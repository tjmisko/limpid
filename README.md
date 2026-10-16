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
limpid --slot session-1 notes.md
limpid --hook --slot session-1 notes.md
limpid --separate notes.md
```

Markdown files are watched for changes. `--out` writes a self-contained page
without opening a window. A running host shares one WebKit context among its
windows; `--separate` opens a dedicated host process. `--dark` and `--light`
select separate hosts.

Math renders as MathML. Mermaid fences show their source immediately; the
diagram script loads after first paint, and completed SVGs are cached for the
next render.

Hook windows (`--hook`) never take focus. limpid is Wayland-only and asks GDK
for the Wayland backend, so the compositor sees the app id
`io.quasioptimal.limpid`. The rule that keeps hook windows unfocused lives in
your compositor config; [`integrations/hyprland/limpid.conf`](integrations/hyprland/limpid.conf)
is the canonical copy.

For a Codex session, run `limpid codex` or `limpid codex 0..2`. The core reader
also accepts `limpid-core claude [spec]` and `limpid-core pi [spec]`, with their
transcript data as JSON on stdin. Each prints a cached page path that can be
passed to `limpid`.

Auto-open is off by default. Set `autoopen.claude = true` and merge
[`integrations/claude-code/stop-hook.settings.json`](integrations/claude-code/stop-hook.settings.json)
into Claude Code's settings to open turns with numbered Questions without
taking focus. The pi extension uses its `agent_end` event when `autoopen.pi` is
true. Both reuse a window by session ID. `limpid hook codex '<json>'` accepts a
Codex turn-complete payload as the final command argument; this repository does
not enable a Codex notification command.

Turn specs include `0`, `3`, `0..2`, `1..=3`, `2..`, `..4`, and `..`. Ranges are
limited to ten turns.

## Answer from the keyboard

Use `Tab` and `Shift-Tab` to move between question fields, `1`–`9` to choose an
option, and `Enter` or `i` to edit a reply. `c` comments on the block near the
top of the view. The editor uses CodeMirror with vim keys: `Esc` then `:w`
saves, `:q` cancels, and `Ctrl-Enter` saves directly. `S` opens the composed
message for review; `Ctrl-Enter` there runs the configured reply command.

Navigation keys include `j`, `k`, `gg`, `G`, PageUp, PageDown, `/`, `n`, `N`,
`{`, `}`, `t`, `r`, `+`, `-`, and `0`.

Agent pages save replies beside their cached HTML. Replies to a markdown file
are stored as HTML comments in that file. limpid refuses to write an annotation
if the file changed since it was rendered.

## Configure

All keys are optional in `~/.config/limpid/config.toml`:

```toml
[highlight]
grammars = ["rust", "python", "typescript", "bash", "json", "toml"]
theme = "default"            # Catppuccin Mocha (dark) / Latte (light), built in

[questions]
headings = ["Questions", "Open questions", "Questions for you"]

[reply]
command = "wl-copy"

[autoopen]
claude = false
pi = false
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
