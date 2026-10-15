# limpid

Renders AI-agent conversation turns as a styled, self-contained HTML page and
opens it in `htmlview`. Content first; metadata is one faint line at the top;
a table of contents sits in a left rail (wide) or a collapsed `<details>` (narrow).

## Contract

```
limpid [--spec <spec>] [--no-open] [--out <dir>]
stdin:  {"model": "<string, optional>", "turns": [{"prompt": "...", "response": "..."}]}   # oldest first
stdout (exit 0): {"file": "<absolute path>", "context": "turn 0"}
stderr (exit 1): one short human line (bad spec, missing turn, no response, malformed stdin, no htmlview)
exit 2: usage error (unknown flag) with usage on stderr
```

- `--spec`: `0` = newest (default). A number selects one response; a Rust-style
  range (`0..2`, `1..=3`, `2..`, `..4`, `..`) shows prompts too, oldest first,
  at most 10 turns.
- Turns with an empty or whitespace-only response are ignored; `limpid` assigns
  indices itself (newest = 0).
- Output goes to `~/.cache/limpid/turns-<iso stamp>.html`; with the default dir
  only the newest 50 `*.html` files are kept. `--out <dir>` disables pruning.
- Unless `--no-open`, `htmlview` (`~/.local/bin/htmlview`, then `PATH`) is
  launched detached via `setsid -f` and `limpid` returns immediately.
- Code blocks are highlighted with headless Neovim (tree-sitter) when `nvim` is
  available; otherwise they are shown as escaped plain text.

## Install

Needs Node >= 22.6 (runs TypeScript natively) and `npm install` for `marked`.

```
cd ~/Tools/limpid && npm install
ln -s ~/Tools/limpid/limpid ~/.local/bin/limpid
npm test
```
