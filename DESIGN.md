# limpid design

limpid is for reading what agents write, quickly, and answering from the
keyboard. It opens agent responses and markdown files as clean pages in a
lightweight window. Agents list what they need from you under a `## Questions`
heading. limpid lets you move between those questions, write replies with vim
keys, comment on any block, and send everything back to the agent in compact
form.

This document describes the target system. [TASKS.md](TASKS.md) orders the work.

## Goals

- **Fast to first readable text.** With a host already running, under 150 ms
  from command to visible window for a 2,000-line file. Cold start under
  0.3 s (htmlview today: about 0.4 s).
- **Small.** One Rust workspace and one web engine instance shared by all
  windows. No Python, Node or npm at runtime or build time. JavaScript runs only
  in the page, and the heavy parts load lazily.
- **Driven from the keyboard.** Every action has a key. The mouse is optional.
- **Transparent to agents.** Agents write ordinary markdown. Nothing limpid
  asks of them looks odd in a terminal.
- **Read-only except comments.** limpid never edits content. It writes only
  replies and comments.

## Non-goals

- Leaving WebKit. A native toolkit would mean rebuilding CodeMirror, Mermaid
  and lazy page loading. htmlview's `PERFORMANCE.md` measured the shared-engine
  cost at about 10 MiB per extra window, which the toolkit research rated
  already competitive.
- macOS, X11-only setups, remote URLs for markdown, and general browsing.
- Editing documents beyond comments.

## Architecture

```
you / agent hook / integration
   │
limpid-core <agent> [spec]  ─▶  ~/.cache/limpid/<page>.html + <page>.meta.json
   │  (page path on stdout)
   ▼
limpid <file.md | page.html | ->      small binary, does not link GTK
   │   renders .md in-process (limpid-core library)
   │   captures $WEZTERM_PANE
   │   Unix socket: {path, theme, pane, source}
   ▼
limpid-host                           GTK4 + WebKit 6.0, links limpid-core
   │                                  one process, many windows,
   │                                  one shared web process
   │   script message handlers: reply, comment, mermaid-svg
   ▼
page: static HTML/CSS + small JS (rail, scroll-spy, search, fields)
      lazy: mermaid.js, CodeMirror 6 + vim
```

One Cargo workspace, three crates:

| Crate | Kind | Responsibility |
|---|---|---|
| `limpid-core` | lib + bin | Settings; reading sources (markdown files, stdin, Claude Code, Codex and pi transcripts with turn specs); rendering pages; highlighting and the grammar catalog; finding question fields; reply storage and formatting; agent instructions; `send`. No GTK. |
| `limpid` | bin | The command you type. Renders `.md` in-process, writes the page to the cache, and hands it to the host over the socket. When no host is running, it starts `limpid-host` first, so GTK starts up while the page renders. |
| `limpid-host` | bin | Windows, keys, the shared WebKit context, and the script message handlers. It stores replies and runs the reply command through `limpid-core`. |

`limpid` must not link GTK or WebKit. The dynamic linker maps every linked
library at startup, so keeping GTK and WebKit out of `limpid` keeps the handoff
path to a few milliseconds. htmlview had the same split: its front end handed
the file to the host and exited before importing GTK.

### Commands

```
limpid notes.md                    render and open
limpid page.html                   open as is
limpid -                           read markdown from stdin
limpid --dark | --light <target>   theme (separate host per scheme)
limpid --separate <target>         own process, no host
limpid --out FILE <target>         write a self-contained page, don't open

limpid-core render FILE.md         render to the cache, print the page path
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
| `~/.cache/limpid/<page>.html` | Rendered pages, newest 50 kept | `limpid`, `limpid-core` |
| `~/.cache/limpid/<page>.meta.json` | Agent, session id, turn spec, source path | `limpid-core` |
| `~/.cache/limpid/<page>.replies.json` | Replies and comments on agent pages | host |
| `$XDG_RUNTIME_DIR/limpid/<session>.sock` | Feedback wake socket, one per agent session | pi extension |
| `~/.cache/limpid/assets/<hash>.js` | Shared page scripts and styles | `limpid` |
| `~/.cache/limpid/mermaid/<hash>.svg` | Rendered diagrams | host |
| `~/.local/share/limpid/grammars/` | Compiled grammars (`<lang>.so`) and their queries | `limpid-core grammars sync` |
| `$XDG_RUNTIME_DIR/limpid-<scheme>.sock` | Handoff socket, one per color scheme | host |

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
```

Unknown keys are an error with the key's name, so typos don't fail silently.

## Rendering

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
  block. Fields only decide where `Tab` stops.

`limpid-core instructions` prints the line that goes into each agent's
instructions (CLAUDE.md, AGENTS.md, the pi system prompt):

> Put questions that need the user's answer under a `## Questions` heading as a
> numbered list; give suggested answers as nested `- ` items.

If agents drift from the convention in practice, the fallback is a custom kind
of GitHub alert, `> [!ASK]`, which reads as a quote with a small tag in the
terminal.

## Keys

The existing keys stay: `q`, `r`, `+ - 0`, `[ ]` back and forward, `j k`, `gg`,
`G`, PageUp/PageDown as half pages, `Ctrl-Up`/`Ctrl-Down` section jumps,
`/ n N` with smartcase, and `Esc`. `n` and `N` center the current match like
Vim's `nzz`. Code blocks carry a hover copy button.

| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | Next / previous question field. Wraps around and scrolls the field into view |
| `1`–`9` | On a focused field: pick that option |
| `Enter` or `i` | Open the reply editor on the focused field (CodeMirror, vim mode, starts in insert) |
| `c` | Comment on the block nearest the top of the view (any block, not only fields). Editors and comments appear at the end of the heading's section |
| `Esc` then `:w`, or `Ctrl-Enter` | Save the reply and close the editor |
| `:q` | Close the editor without saving |
| `S` | Show the composed reply message for review |
| `Ctrl-Enter` (in review) | Send the reply |
| `t` or `F1` | Toggle the rail |
| `{` / `}` | Previous / next heading |

The page owns keys while an editor or the search field has focus. The host
owns them otherwise. This rule exists today in `keys.py` and carries over.

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
c "Use the existing pool…" (L42-48): this leaks on error
```

Fields use their `qN` id. Comments quote the first few words of the block and
its line range. Unanswered fields are left out.

### Delivery

The host delivers a reply by running `reply.command`, with the message on stdin
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
sidecar uses. Empty feedback prints one line and exits zero.

### `/read` does not start the agent

After the page opens, the integration records a note beside the conversation
with no turn:

- pi: `sendMessage({customType:"limpid-open", …}, {triggerTurn:false})`.
- Claude Code: return `{context:[…]}` from `command.run`.
- Codex: the note is just the line `limpid` prints.

The note carries the page path and the pull command. It reaches the model on
the next turn the user starts; nothing wakes the agent on its own.

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

The host is detached and the agent runs elsewhere, so the integration owns a
per-session unix socket:

```
limpid-host ── connect, one JSON line ──▶ $XDG_RUNTIME_DIR/limpid/<session>.sock
                                              │
                                              ▼
                              pi extension: sendUserMessage(message)
```

- The integration binds the socket in `session_start` (mode 0600 under a 0700
directory) and removes it in `session_shutdown`.
- It passes the path as `feedback_sock` in the JSON it already sends to
`limpid-core`, which records it in `<page>.meta.json`, so the host needs no
global state to find it.
- On `:wq` the host composes the feedback, connects, writes `{page, message}`
as one newline-terminated frame, and waits for `{ok}`. The listener replies,
then calls `sendUserMessage` (or `{deliverAs:"followUp"}` while streaming). The message is the composed feedback followed by the pull command,
so the agent has the answers in context and a way to re-read them.
- No socket, a refused connection, or no `{ok}` — the agent is gone, or it is
Codex or a plain file — falls back to `reply.command`, the same delivery path
used today.

`:wq` writes the sidecar before it attempts delivery, so feedback is never
lost when the wake fails. The socket exists only to auto-send and start a turn;
without it (Claude Code for now, Codex, or an agent that has exited) the user
says the feedback is in limpid and the agent pulls it with
`limpid-core feedback <page>`. Delivery is a convenience, not the record.

### Waiting without burning turns

Feedback is optional. The agent never blocks, never polls, and limpid never
pushes saved-but-unsent feedback on its own.

- **Pull on request.** Saved feedback stays in the sidecar. The `/read` note
names the pull command, and the agent may run `limpid-core feedback <page>` on
any later turn. If it does not, the feedback is skipped. The agent decides, so
no turn is spent unless it chooses one.
- **Explicit send.** `:wq` pushes one message and one turn, so the agent reads
the feedback and responds. This is the only thing that starts a turn, and the
only token spend the user did not type. `:w` and `:q` never send.

There is no `feedback --wait`: an empty wait costs a turn and a timeout.

## Windows

Every handoff opens one window. One host process serves all of them and shares
a single WebKit context, so an extra window costs about 10 MiB rather than a
whole new engine. The host quits when its last window closes.

limpid does not try to control keyboard focus. A window that opens takes focus
like any other; what has focus is the compositor's decision, not the app's.
There is no automatic opening: `/read` (or running `limpid`) is the only thing
that opens a page.

## Watch

`--watch` re-renders on save and keeps the scroll position, anchored to the
`data-src` of the block at the top of the view rather than to a pixel offset.
Markdown files you open with `limpid` are watched by default, so annotations
appear after an external edit and after limpid saves a comment.

## Dependencies

- **Runtime:** WebKitGTK 6.0 and GTK4 (system packages), `wl-copy`, and
  `wezterm` for `send`.
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
- **Host:** the handoff and window tests from htmlview are ported, plus the
  ported key dispatcher. Window tests are display-gated and cover file://
  fetch and ES modules, the forced color scheme, one shared web process, and
  watched-file reload.
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
