# limpid design

limpid is for reading what agents write, quickly, and answering from the
keyboard. It opens agent responses and markdown files as clean pages in a
lightweight window. Agents list what they need from you under a `## Questions`
heading. limpid lets you move between those questions, write replies with vim
keys, comment on any block, and send everything back to the agent in compact
form.

This document describes the target system. [TASKS.md](TASKS.md) orders the
work; [MINIMAL.md](MINIMAL.md) explains the native viewer and
[TASKS-NATIVE.md](TASKS-NATIVE.md) orders its work units.

## Goals

- **Fast to first readable text.** With a host already running, under 150 ms
  from command to visible window for a 2,000-line file. Cold start under
  0.3 s (htmlview today: about 0.4 s).
- **Small.** One Rust workspace and one native viewer process shared by all
  windows. No Python, Node or npm at runtime or build time. Heavy features
  (math, Mermaid, the editor) run as helper processes that are not resident
  while unused.
- **Driven from the keyboard.** Every action has a key. The mouse is optional.
- **Transparent to agents.** Agents write ordinary markdown. Nothing limpid
  asks of them looks odd in a terminal.
- **Read-only except comments.** limpid never edits content. It writes only
  replies and comments.

## Non-goals

- Being a browser. The native viewer lays out limpid's own document model
  (see [MINIMAL.md](MINIMAL.md)); it does not run HTML, CSS or JavaScript.
  WebKit stays as an optional host for `.html` files and for rendering
  Mermaid diagrams, built only with `--features webkit`.
- Remote URLs for markdown and general browsing.
- Editing documents beyond comments.

## Architecture

```
you / agent hook / integration
   │
limpid-core <agent> [spec]  ─▶  ~/.cache/limpid/<page>.html + <page>.md + <page>.meta.json
   │  (page path on stdout)
   ▼
limpid <file.md | page.html | ->      small binary, links neither GTK nor winit
   │   routes: .md, stdin and agent pages → limpid-view
   │           plain .html              → limpid-host
   │   captures $WEZTERM_PANE
   │   Unix socket: {path, theme, pane, source}
   ├──────────────────────────────┐
   ▼                              ▼
limpid-view                       limpid-host  (--features webkit)
 winit + softbuffer, own layout    GTK4 + WebKit 6.0
 one process, many windows         HTML pages; Mermaid helper
 embedded nvim for replies         (--render-mermaid FILE)
   │  child processes, never blocking the UI
   ├─▶ limpid-core math FILE        typeset formulas → ~/.cache/limpid/math
   └─▶ limpid-host --render-mermaid diagrams → ~/.cache/limpid/mermaid
```

One Cargo workspace, four crates:

| Crate | Kind | Responsibility |
|---|---|---|
| `limpid-core` | lib + bin | Settings; reading sources (markdown files, stdin, Claude Code, Codex and pi transcripts with turn specs); the document model; rendering pages to HTML; highlighting and the grammar catalog; the math helper; finding question fields; reply storage and formatting; agent instructions; `send`. No GUI toolkit. |
| `limpid` | bin | The command you type. Chooses the viewer, starts it when its socket is absent, and hands it the page. |
| `limpid-view` | lib + bin | The native viewer: layout and paint, the block cursor and keys, search and the command line, the embedded Neovim for replies and comments, delivery, file watching and the helper processes. |
| `limpid-host` | bin, `webkit` feature | HTML pages in WebKit, and `--render-mermaid`. Not built by default. |

`limpid` must not link a GUI toolkit. The dynamic linker maps every linked
library at startup, so keeping GTK, WebKit and winit out of `limpid` keeps the
handoff path to a few milliseconds.

### Commands

```
limpid notes.md                    open in the native viewer
limpid page.html                   open in the WebKit host (agent pages: the viewer)
limpid -                           read markdown from stdin
limpid --dark | --light <target>   theme (separate viewer per scheme)
limpid --separate <target>         own process, no shared viewer
limpid --viewer view|host <target> choose the viewer (or LIMPID_VIEWER)
limpid --out FILE <target>         write a self-contained page, don't open

limpid-core render FILE.md         render to the cache, print the page path
limpid-core math FILE.md [--json]  typeset the formulas that are not cached
limpid-core claude [spec]          newest Claude Code turn(s) -> page path
limpid-core codex  [spec]
limpid-core pi     [spec]
limpid-core grammars sync | list   install the configured grammars / show catalog
limpid-core instructions           the one-line agent instruction (see Questions)
limpid-core feedback <page.html>   saved answers and comments (--json, --session)
limpid-core send                   reply command: type stdin into $LIMPID_PANE
```

Turn specs keep today's grammar: `0`, `3`, `0..2`, `1..=3`, `2..`, `..4`, `..`,
capped at 10 turns.

Integrations are thin wrappers. `/read` in Claude Code, pi's `Ctrl-R` and
`!limpid codex` all run `limpid "$(limpid-core <agent> <spec>)"`.

### Files

| Path | Contents | Written by |
|---|---|---|
| `~/.config/limpid/config.toml` | Settings (below) | you |
| `~/.cache/limpid/<page>.html` | Rendered pages, newest 500 kept | `limpid`, `limpid-core` |
| `~/.cache/limpid/<page>.md` | The Markdown an agent page was rendered from; what the native viewer opens | `limpid-core` |
| `~/.cache/limpid/<page>.meta.json` | Agent, session id, turn spec, source path | `limpid-core` |
| `~/.cache/limpid/<page>.replies.json` | Replies and comments on agent pages, keyed by the page stem | viewer, host |
| `$XDG_RUNTIME_DIR/limpid/<session>.sock` | Feedback wake socket, one per agent session | pi extension, Claude Code plugin |
| `~/.cache/limpid/assets/<hash>.js` | Shared page scripts and styles | `limpid` |
| `~/.cache/limpid/math/<hash>.svg`, `.json` | Typeset formulas and their size and baseline | `limpid-core math` |
| `~/.cache/limpid/mermaid/<hash>.svg` | Rendered diagrams | `limpid-host --render-mermaid` |
| `~/.local/share/limpid/grammars/` | Compiled grammars (`<lang>.so`) and their queries | `limpid-core grammars sync` |
| `$XDG_RUNTIME_DIR/limpid-view-<scheme>.sock` | Handoff socket of the native viewer, one per color scheme | viewer |
| `$XDG_RUNTIME_DIR/limpid-<scheme>.sock` | Handoff socket of the WebKit host | host |

### Settings

```toml
# ~/.config/limpid/config.toml — every key is optional
[highlight]
grammars = ["rust", "python", "typescript", "tsx", "bash", "json", "toml",
            "yaml", "go", "c", "cpp", "lua", "sql", "html", "css", "markdown"]
theme = "default"

[questions]
headings = ["Questions", "Open questions", "Questions for you"]

[reply]
command = "wl-copy"            # or "limpid-core send"

[view]
font = "sans-serif"            # family, or sans-serif / serif / monospace
mono_font = "monospace"
font_size = 17                 # CSS px at scale 1
line_height = 1.65
content_width = 860            # px
rail_width = 220               # px
rail = "auto"                  # auto | on | off
code_wrap = false
theme = "auto"                 # auto | dark | light
scroll_fraction = 0.5
watch_interval_ms = 300        # 0 turns watching off

[style]                        # beats the matching [view] key when set
# font_size = 17               # older spelling of view.font_size
# line_height = 1.65           # 1 to 3
# content_width = 860          # px
# bg surface raised rule ink muted faint teal blue amber = "#rrggbb"
# focus_alpha = 0.06           # 0 to 1, the focus band (teal)
# selection_alpha = 0.12       # 0 to 1, the visual-mode band
# css is read by the WebKit host only; the native viewer ignores it.

[view.keys]                    # remaps by action name: next_block = "n"

[editor]
command = "nvim"
init = "user"                  # user | clean
idle_seconds = 300

[math]
backend = "auto"               # auto | latex | typst | mathjax | none
latex = "auto"                 # auto | pdflatex | latex
svg = "auto"                   # auto | dvisvgm | pdftocairo
typst = "typst"

[mermaid]
enabled = true
timeout_seconds = 20
helper_window = "auto"         # auto | never | always
```

Unknown keys are an error with the key's name, so typos don't fail silently.

## Rendering

This section describes the HTML pages that `limpid-core` emits for `--out`,
for the WebKit host and for agent pages' `.html` files. The native viewer
parses the same Markdown into the shared `Doc` model (`limpid_core::doc`) and
lays that out itself: tables, code, quotes and question fields as described
here, but math and Mermaid as cached SVG pictures, drawn from the helpers'
output, instead of MathML and `mermaid.js`.

The parser is `pulldown-cmark` with tables, task lists, strikethrough,
footnotes, YAML front matter and math enabled.

| Feature | How it renders | Cost at first paint |
|---|---|---|
| Headings, TOC | `id`s with slug deduplication; the rail and the narrow-screen `<details>` stay as they are today | none |
| Code | tree-sitter with injections (see Highlighting); plain escaped text for languages with no grammar installed | one query compile per language per page, measured in phase 0 |
| Math | LaTeX turned into MathML when the page renders (`pulldown-latex`); WebKit draws MathML natively | microseconds |
| Mermaid | `<pre class="mermaid">` source first, then `mermaid.js` loads after first paint. The page sends the SVG to the host, which caches it under a hash of the source; later renders inline the cached SVG | none; the diagram appears later |
| Front matter | `title` becomes the window and page title; the block is otherwise hidden | none |
| Raw HTML | Escaped in agent transcripts. Sanitized with `ammonia` (allowlist, no scripts) in files you open yourself | small |
| Questions | Fields with numbered options and a reply area (see Questions) | none |
| Saved replies | Shown as annotations under the block they belong to | none |

Every block element carries `data-src="start-end"`, its byte range in the
markdown source. Comments, saved replies and the watch scroll anchor all use
these ranges.

Page scripts (rail, scroll-spy, search, fields, the Mermaid loader) and styles
are compiled in with `include_str!` and written once to
`~/.cache/limpid/assets/<hash>.js`, so pages refer to shared files instead of
inlining them. `--out` inlines everything except lazy bundles, and the page
stays readable without them.

The JIT stays off by default (`JSC_useJIT=0`, about 10 MiB per web process).
`LIMPID_JIT=1` turns it back on.

### Highlighting

Code highlighting uses the `tree-sitter` and `tree-sitter-highlight` crates
directly, with no wrapper crate and no regex grammars. The reason is
injections: macro bodies in Rust, SQL in strings and code in markdown get
re-parsed as their own language. nvim's Rust macro highlighting works the same
way.

- **Grammars are shared libraries loaded at runtime** (`libloading`) from
  `~/.local/share/limpid/grammars/<lang>.so`, the way nvim and Helix load them.
  Only languages that appear on a page get loaded, and each language's queries
  compile on first use.
- **Queries come from Helix** (`highlights.scm`, `injections.scm`), not
  nvim-treesitter, whose queries use nvim-only predicates such as
  `#lua-match?`. limpid can ship its own override for a language when Helix's
  queries fall short.
- **The catalog** is generated from Helix's `languages.toml`, which pins a
  source repository and commit for about 200 grammars, and is committed to the
  repo as `catalog.toml`. It also supplies fence aliases (`rs`, `py`, `sh`,
  `ts`).
- **Which grammars are installed is the `highlight.grammars` setting.**
  `limpid-core grammars sync` fetches and compiles exactly that list (it needs
  `git` and a C compiler). `grammars list` shows the catalog and what is
  installed.
- **Themes are CSS.** Highlight names become classes (`hl-keyword`,
  `hl-function-macro`), and a theme is a stylesheet with light and dark
  variants, so switching scheme needs no re-render. The page keeps its own
  palette; only the code token colours use Catppuccin Mocha.

## Questions

Agents mark what needs your answer with ordinary markdown and no new syntax,
so the response reads normally in a terminal. Questions go under a
`## Questions` heading as a numbered list, with suggested answers as nested
`- ` items:

```markdown
## Questions

1. Which database should the prototype use?
   - SQLite
   - Postgres
2. Should the old endpoint keep working?
```

- Each top-level item under the heading is a field, numbered `q1`, `q2`, … in
  document order, so the ids match the agent's own numbering. Nested `- ` items
  are suggested answers. Free text is always allowed.
- The heading may be any level and is matched without regard to case against
  `questions.headings`.
- An agent that forgets the convention breaks nothing: `c` comments on any
  block. Fields only decide which blocks the number keys answer.

`limpid-core instructions` prints the line that goes into each agent's
instructions (CLAUDE.md, AGENTS.md, the pi system prompt):

> Put questions that need the user's answer under a `## Questions` heading as a
> numbered list; give suggested answers as nested `- ` items.

If agents drift from the convention in practice, the fallback is a custom kind
of GitHub alert, `> [!ASK]`, which reads as a quote with a small tag in the
terminal.

## Keys

A block cursor selects one block at a time. Paragraphs, code blocks,
diagrams, tables, blockquotes, list items and question fields are stops;
headings and rules are not. The cursor moves with `j` / `k` and the up and
down arrows, and the viewport follows only when the block would otherwise be
off screen. The cursor starts on the first stop in the restored viewport, so
something is always focused. Editors and saved comments appear at the end of
the heading's section.

The rest of the keys stay: `r`, `+ - 0`, `[ ]` back and forward, `/ n N` with
smartcase, and `Esc`. `n` and `N` center the current match like Vim's `nzz`.
Code blocks carry a hover copy button.

| Key | Action |
|---|---|
| `j` / `k`, `Down` / `Up` | Move the cursor to the next / previous block |
| `PageDown` / `PageUp`, `Ctrl-d` / `Ctrl-u` | Scroll half a page |
| `{` / `}`, `Ctrl-Up` / `Ctrl-Down` | Next / previous section. The cursor moves to that section's first block |
| `gg` / `G` | First / last block |
| `zt` / `zz` / `zb` | Put the focused block at the top / center / bottom |
| `1`–`9` | On a focused question: pick that option |
| `Enter` or `i` | Open the reply editor on the focused question (CodeMirror, vim mode, starts in insert) |
| `c` | Open the comment editor on the focused block, or reopen the focused comment for editing |
| `x` | Delete the focused comment |
| `Esc` then `:w`, or `Ctrl-Enter` | Save the reply and close the editor |
| `:q` | Close the editor without saving |
| `S` | Show the composed reply message for review |
| `Ctrl-Enter` (in review) | Send the reply |
| `t` or `F1` | Toggle the rail |

Key ownership is one explicit state, shown in the status line: the page, an
editor box (every key is Neovim's), the `:` command line, or the `/` search
line (see MINIMAL.md, "Key routing"). Every page key can be remapped under
`[view.keys]`; `limpid-view`'s `nav::Action` names the actions and their
defaults match the table above. `h` / `l` scroll a focused table or code block,
`v` / `y` / `Y` select and copy, `Q` jumps to the first unanswered question,
and `r`, `+`, `-`, `0` reload and zoom.

## Replies

### Storage

Replies never go into agent transcripts.

- **Agent pages:** `~/.cache/limpid/<page>.replies.json`, keyed by field id or
  `data-src` range.
- **Markdown files you opened:** written into the file itself as an HTML comment
  right after the block, so they stay with the document and are invisible in
  other renderers:

  ```markdown
  <!-- limpid reply q1
  SQLite. We can move later.
  -->

  <!-- limpid comment
  This leaks on error.
  -->
  ```

  limpid writes the file atomically (temporary file and rename) and refuses to
  save if the file changed on disk since it was rendered. A `-->` inside a reply
  is escaped so it cannot end the comment early.

Either way, the next render shows saved replies as annotations.

### Compact reply format

`S` composes one message from every answered field and comment:

```
re: <page title> (claude, turn 0)
q1: SQLite. We can move later.
q2: no
"Use the existing pool…" (turn 1, L42-48): user commented: "this leaks on error"
```

Fields use their `qN` id. A comment quote runs to 100 characters of the block,
cut on a word boundary, and names the turn and line range, so the agent can
find the text without the full document, then the full comment. A comment on a
question is an answer to that question. Unanswered fields are left out.

### Delivery

The viewer delivers a reply by running `reply.command`, with the message on stdin
and these environment variables:

| Variable | Meaning | Source |
|---|---|---|
| `LIMPID_AGENT` | `claude`, `codex`, `pi` or `file` | page meta |
| `LIMPID_SESSION` | Session id, when the page came from a transcript | page meta |
| `LIMPID_SOURCE` | Path of the page's source | page meta |
| `LIMPID_PANE` | Terminal pane that opened the page | `$WEZTERM_PANE`, captured by `limpid` and sent in the handoff |

The default command copies the message to the clipboard (`wl-copy`).
`limpid-core send` types the message into the agent's pane with
`wezterm cli send-text --pane-id $LIMPID_PANE` and does **not** press Enter, so
you can still review it in the agent's own prompt. It falls back to the
clipboard when no pane is known. Agent-specific delivery (for example a pi
extension that receives the message directly) plugs in through the same
setting.

## Feedback loop

Reading a page is half the loop. The other half is getting answers back to the
agent without polling and without spending turns nobody asked for.

### Stable page identity

Agent pages are named from their content, not from the process that rendered
them:

```
page_id  = blake3(agent + "\0" + session_id + "\0" + turn_spec + "\0" + content_hash)
filename = "<agent>-<first 16 hex of page_id>.html"
```

`content_hash` covers the exact prompts and responses being rendered. The same
session reading the same turns always lands on the same path, so its
`<page>.replies.json` is reused and answers survive every close and reopen. A
new turn changes `content_hash` and gets a fresh page, so old comments never
attach to new text. `session_id` keeps concurrent sessions in different
projects from colliding.

Because identity is content, the sidecar is the durable record.
`prune_agent_pages` skips any page whose sidecar holds non-empty feedback, so
pruning never drops feedback that was already saved with `:w`.

### Pull: `limpid-core feedback`

```
limpid-core feedback <page.html>                 exact
limpid-core feedback --session <id> [--spec S]  resolve through <page>.meta.json
limpid-core feedback --json <page.html>
```

It prints the compact form `compose_message` already produces (fields, then
quoted comments), or `{"replies":…,"comments":…}` with `--json`. An agent page
reads its sidecar; a markdown path reads its in-file annotations. Feedback is
keyed to the questions and comment ranges of the page, the same keys the
sidecar uses. Empty feedback prints one line and exits zero. The composed form
is self-contained: the agent answers from it and only pulls the page when the
feedback is unclear, so it does not drag the whole document into context.

### `/read` does not start the agent

After the page opens, the integration records a note beside the conversation
with no turn:

- pi: `sendMessage({customType:"limpid-open", …}, {triggerTurn:false})`.
- Claude Code: return `{context:[…]}` from `command.run`.
- Codex: the note is just the line `limpid` prints.

The note carries the page path and says the feedback is complete; the pull
command is a fallback for when it is unclear. It reaches the model on the next
turn the user starts; nothing wakes the agent on its own.

### Command mode

`q` is unbound. `:` opens a vim-style command line at the bottom of the page:

| Command | Action |
|---|---|
| `:q` | quit; fails with unsaved changes and points to `:wq` or `:q!` |
| `:w` | save progress to the sidecar |
| `:wq` | save, send the composed feedback, then quit |
| `:q!`, `:qa` | quit and discard unsaved changes (`:qa` closes every limpid window) |

Edits do not go to disk as they are made; the page holds them until `:w`.
`:q` refuses while that buffer is non-empty, leaving a one-line message
(`unsaved changes — :wq to save and send, :q! to discard`), and only `:wq`
writes *and* wakes the agent. This guard is what replaces a close menu: the
same prompt, driven by a command instead of a dialog.

`S` still previews the composed message in the review dialog before it is
sent, but it composes from the page's pending buffer rather than the saved
sidecar.

### Wake signal

The viewer is detached and the agent runs elsewhere, so the integration owns a
per-session unix socket:

```
limpid-view ── connect, one JSON line ──▶ $XDG_RUNTIME_DIR/limpid/<session>.sock
                                              │
                                              ▼
                              pi extension: sendUserMessage(message)
                              Claude Code plugin: $.prompt.submit(message)
```

- The integration binds the socket in `session_start` (mode 0600 under a 0700
directory) and removes it in `session_shutdown`. Claude Code mods have no
sockets, so its plugin spawns a small Python listener that prints each frame
and answers `{ok}` only after the print succeeds.
- It passes the path as `feedback_sock` in the JSON it already sends to
`limpid-core`, which records it in `<page>.meta.json`, so the viewer needs no
global state to find it.
- On `:wq` the viewer composes the feedback, connects, writes `{page, message}`
as one newline-terminated frame, and waits for `{ok}`. The listener replies,
then calls `sendUserMessage` (or `{deliverAs:"followUp"}` while streaming).
The message is the composed feedback followed by a note that it is complete,
with the pull command as a fallback when something is unclear.
- No socket, a refused connection, or no `{ok}` — the agent is gone, or it is
Codex or a plain file — falls back to `reply.command`, the same delivery path
used today.

`:wq` writes the sidecar before it attempts delivery, so feedback is never
lost when the wake fails. The socket exists only to auto-send and start a turn;
without it (Codex, or an agent that has exited) the user
says the feedback is in limpid and the agent pulls it with
`limpid-core feedback <page>`. Delivery is a convenience, not the record.

### Waiting without burning turns

Feedback is optional. The agent never blocks, never polls, and limpid never
pushes saved-but-unsent feedback on its own.

- **Pull on request.** Saved feedback stays in the sidecar. The `/read` note
names the pull command, and the agent may run `limpid-core feedback <page>` on
any later turn, but only when the feedback it was given is unclear; the
composed message stands on its own. If it does not pull, the feedback is
skipped. The agent decides, so no turn is spent unless it chooses one.
- **Explicit send.** `:wq` pushes one message and one turn, so the agent reads
the feedback and responds. This is the only thing that starts a turn, and the
only token spend the user did not type. `:w` and `:q` never send.

There is no `feedback --wait`: an empty wait costs a turn and a timeout.

## Windows

Every handoff opens one window. One viewer process per color scheme serves all
of them and shares its fonts and caches, so an extra window costs little more
than its pixels. The viewer quits when its last window closes. Tests set
`LIMPID_WINDOW_CLASS=limpid-test` so a compositor rule can keep their windows
off your desktop.

limpid does not try to control keyboard focus. A window that opens takes focus
like any other; what has focus is the compositor's decision, not the app's.
There is no automatic opening: `/read` (or running `limpid`) is the only thing
that opens a page.

## Watch

The viewer polls the file it shows every `view.watch_interval_ms` (300 by
default; `0` turns watching off). When the file changes it parses and lays the
page out again and keeps the viewport anchored to the block that was at the
top, found by its text and position rather than by a pixel offset, so
annotations appear after an external edit and after limpid saves a comment. A
file that is briefly missing (an editor saving by rename) is ignored until it
returns. `limpid --watch` is accepted and does nothing; watching is always on.

## Helpers

Typesetting and diagram rendering never run inside the viewer. After a page
opens or reloads, the viewer starts `limpid-core math FILE` when the page has
formulas that are not cached, and `limpid-host --render-mermaid FILE` when it
has uncached diagrams, `mermaid.enabled` is true and `limpid-host` is
installed. Each runs as a child process in its own process group, is killed
after its time limit (and with the page), and is not started again for the same
hashes if it left them uncached. When a child exits, the viewer redraws the
blocks whose output reached the cache. Until then, and when no helper is
available, a formula or diagram shows its source.

## Dependencies

- **Runtime:** a Wayland or X11 session, `nvim`, `wl-copy`, and `wezterm` for
  `send`. WebKitGTK 6.0 and GTK4 only for `limpid-host` (`--features webkit`).
- **Installing grammars:** `git` and a C compiler.
- **Rust crates:** `pulldown-cmark`, `pulldown-latex`, `ammonia`, `tree-sitter`,
  `tree-sitter-highlight`, `libloading`, `toml`, `serde`, `serde_json`, `gtk4`,
  `webkit6`. Every new crate needs a reason written in its PR.
- **Vendored JavaScript:** `mermaid.min.js`, and a CodeMirror 6 bundle with
  `@replit/codemirror-vim`. Each is built once outside the normal build and
  committed, with its version, licenses and rebuild steps in
  `vendor/README.md`. Building limpid never needs npm.

## Testing

- **Golden pages:** fixed `.md` inputs and the expected HTML for headings, TOC,
  tables, code (including Rust macro bodies), math, question fields, saved
  replies, escaping and sanitizing. Changes are reviewed as diffs.
- **Parity:** the existing TypeScript tests (spec, codex, page, cli, the Claude
  Code `read` tests) are ported first and must pass before the TypeScript code
  is deleted. Agent pages rendered from the existing fixtures must match the
  TypeScript output, ignoring whitespace and highlighting.
- **Adversarial input:**
  - **Headings:** duplicate, empty, emoji and very long headings, and a
    document with no headings.
  - **Malformed input:** unclosed fences, a 10 MB file, invalid UTF-8, CRLF line
    endings, raw `<script>`.
  - **Questions:** a `## Questions` heading with no list, a bulleted list
    instead of a numbered one, and two Questions sections.
  - **Grammars:** a fence naming a language with no grammar, and a grammar `.so`
    that fails to load.
  - **Settings:** an unknown key in the settings file.
  - **Replies:** a reply containing `-->`, and a file that changed on disk
    between render and save.
- **Viewer:** layout and navigation are tested without a display on the
  layout and through PNG snapshots (`limpid-view --snapshot WxH --out FILE.png`).
  Window tests are display-gated, set `LIMPID_WINDOW_CLASS=limpid-test`, and
  cover the handoff socket, the close request and routing from `limpid`.
- **Host:** the handoff and window tests from htmlview are ported, plus the
  ported key dispatcher, and build only with `--features webkit`.
- **Benchmarks:** `bench/` holds the measurement scripts from
  `~/.cache/htmlview-bench/`, adapted for limpid. Each phase that touches the
  hot path reports cold start, warm start and private memory for 1, 2 and 3
  windows.

## Open questions

- How long do the larger Helix queries take to compile on first use? If that is
  too slow, first paint shows plain code and the highlighted version swaps in.
- Do Helix's Rust injections parse macro bodies as Rust, like nvim's? If not,
  limpid ships its own `injections.scm` override for Rust.
- Is MathML output from `pulldown-latex` good enough, or does KaTeX need to load
  lazily as a fallback?
- How fast is Mermaid with the JIT off? If it is too slow, pages that contain
  diagrams might need the JIT on.
- Can pi receive a reply directly through its extension API instead of through
  the terminal?
