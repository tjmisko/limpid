# limpid

Open an AI agent's response as a clean, styled HTML page.

*Limpid* means clear and transparent. It is also what the human does while
limping along behind the AI, trying to catch up with what it just wrote.

Agent output is long and terminals wrap it badly. `limpid` takes the turns of a
conversation, renders them as one self-contained HTML file, and opens it in a
viewer.

## Page layout

Content comes first. Metadata (turn, model, time) is one faint line at the top.
A table of contents sits in a left rail on wide screens and in a collapsed
`<details>` on narrow ones. Code blocks are syntax highlighted when `nvim` is
installed; otherwise they are plain escaped text.

## Install

Needs Node >= 22.6 (it runs TypeScript natively).

```
git clone https://github.com/tjmisko/limpid
cd limpid
./install.sh
```

This runs `npm ci --omit=dev` and links `limpid` into `${PREFIX:-$HOME/.local}/bin`.
It uses no sudo and edits none of your config. Make sure that directory is on `PATH`.

## Agents

### Claude Code

```
/plugin marketplace add tjmisko/limpid
/plugin install limpid@limpid
```

Then `/read [spec]` in a session.

### pi

```
pi install git:github.com/tjmisko/limpid
```

pi installs the package's dependencies. Then use `/read [spec]`, or press
`Ctrl-R` to open the latest response.

### Codex

From inside a Codex session:

```
!limpid codex
!limpid codex 0..2
```

`limpid codex` finds the current directory's session and opens it. The
command and its one-line output (`limpid: opened turn 0`) enter the model's
context, so the output is kept tiny.

### Other agents

Pipe JSON to `limpid`, oldest turn first:

```
echo '{"model": "my-model", "turns": [{"prompt": "hi", "response": "# Hello"}]}' | limpid --spec 0
```

```
limpid [--spec <spec>] [--no-open] [--out <dir>]
stdin:  {"model": "<string, optional>", "turns": [{"prompt": "...", "response": "..."}]}
stdout (exit 0): {"file": "<absolute path>", "context": "turn 0"}
stderr (exit 1): one short human line (bad spec, missing turn, no response, malformed stdin, no viewer)
exit 2: usage error, with usage on stderr
```

Turns with an empty or whitespace-only response are ignored. `limpid` assigns
indices itself, newest first.

## Turn specs

| Spec | Shows |
|------|-------|
| `0` | the newest response (default) |
| `3` | one response, three back |
| `0..2` | turns 0 and 1, with prompts, oldest first |
| `1..=3` | turns 1 through 3, inclusive |
| `2..` | turn 2 and everything older |
| `..4` | turns 0 through 3 |
| `..` | every turn |

A range shows at most 10 turns.

## Output and viewer

Pages are written to `~/.cache/limpid/turns-<timestamp>.html`; only the newest 50
are kept. `--out <dir>` writes elsewhere and disables pruning. `--no-open` skips
opening.

The viewer is chosen in this order, and is always launched detached:

1. `$LIMPID_VIEWER`, a command (split on whitespace) with the file appended
2. `htmlview`, on `PATH` or in `~/.local/bin`
3. `xdg-open` (Linux) or `open` (macOS)

## Optional dependencies

- `nvim`: tree-sitter syntax highlighting for code blocks.
- [`htmlview`](https://github.com/tjmisko/htmlview): the recommended viewer, with vim keys.

## Development

```
npm install
npm test
```

## License

MIT
