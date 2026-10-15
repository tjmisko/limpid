# limpid design

limpid is for reading what agents write, quickly, and answering from the
keyboard. It opens agent responses and markdown files as clean pages in a
lightweight window. Agents can mark the places where they need an answer, and
limpid lets you move between those places, write replies with vim keys, and
send the replies back to the agent in compact form.

This document describes the target system. [TASKS.md](TASKS.md) orders the work.

## Goals

- **Fast to first readable text.** With a host already running, under 150 ms
  from command to visible window for a 2,000-line file. Cold start under
  0.3 s (htmlview today: about 0.4 s).
- **Small.** One Rust workspace and one web engine instance shared by all
  windows. No Python, Node or npm at runtime. JavaScript runs only in the page,
  and the heavy parts load lazily.
- **Driven from the keyboard.** Every action has a key. The mouse is optional.
- **Read-only except comments.** limpid never edits content. It writes only
  replies and comments.

## Non-goals

- Leaving WebKit. A native toolkit would mean rebuilding CodeMirror, Mermaid
  and lazy page loading. htmlview's `PERFORMANCE.md` measured the shared-engine
  cost at about 10 MiB per extra window, which the toolkit research rated
  already competitive.
- macOS, X11-only setups, remote URLs for markdown, and general browsing.
- Editing documents beyond comments.

## Components

```
you / agent hook
   │
limpid-core <agent> [spec]   ─▶ ~/.cache/limpid/<page>.html
   │
limpid <file.md | file.html | ->      small binary, does not link GTK
   │   renders .md in-process (limpid-core library)
   │   Unix socket: {path, slot, theme, origin}
   ▼
limpid-host                           GTK4 + WebKit 6.0, one process,
   │                                  many windows, one shared web process
   │   script message handlers (reply, comment, mermaid-svg)
   ▼
page: static HTML/CSS + small JS (rail, scroll-spy, search, fields)
      lazy: mermaid.js, CodeMirror 6 + vim
```

One Cargo workspace, three crates:

| Crate | Kind | Responsibility |
|---|---|---|
| `limpid-core` | lib + bin | Read sources (markdown files, stdin, Claude Code, Codex and pi transcripts with turn specs), render pages, find question fields, format replies, print agent instructions. No GTK. |
| `limpid` | bin | The command you type. Renders `.md` in-process, writes the page to the cache, hands it to the host over the socket, and starts `limpid-host` when none is running. |
| `limpid-host` | bin | Windows, keys, the shared WebKit context, script message handlers, writing comments to disk, and delivering replies. |

`limpid` must not link GTK or WebKit. The dynamic linker maps every linked
library at startup, so keeping GTK and WebKit out of `limpid` keeps the handoff
path to a few milliseconds. htmlview had the same split: its front end handed
the file to the host and exited before importing GTK.

### Command surface

```
limpid notes.md                 render and open
limpid page.html                open as is
limpid -                        read markdown from stdin
limpid --slot ID notes.md       reuse the window holding slot ID
limpid --dark | --light         theme (separate host per scheme)
limpid --separate               own process, no host
limpid-core claude [spec]       newest Claude Code turn(s) -> page path on stdout
limpid-core codex  [spec]
limpid-core pi     [spec]
limpid-core instructions        the one-line agent instruction (see Ask fields)
```

Turn specs keep today's grammar: `0`, `3`, `0..2`, `1..=3`, `2..`, `..4`, `..`,
capped at 10 turns.

Integrations are thin wrappers: `/read` in Claude Code, pi's `Ctrl-R` and
`!limpid codex` all run `limpid "$(limpid-core <agent> <spec>)"`.

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

Every block element carries `data-src="start-end"`, its byte range in the
markdown source. Comments use these ranges to point at the source.

### Highlighting

Code highlighting uses the `tree-sitter` and `tree-sitter-highlight` crates
directly, with no wrapper crate and no regex grammars. The reason is
injections: macro bodies in Rust, SQL in strings and code in markdown get
re-parsed as their own language. nvim's Rust macro highlighting works the same
way.

- **Grammars are shared libraries loaded at runtime** from
  `~/.local/share/limpid/grammars/<lang>.so`, the way nvim and Helix load them.
  Only languages that appear on a page get loaded.
- **Queries come from Helix** (`highlights.scm`, `injections.scm`), not
  nvim-treesitter, whose queries use nvim-only predicates such as
  `#lua-match?`. Helix's `languages.toml` also pins a source repository and
  commit for about 200 grammars, which `limpid-core grammars` uses as its
  catalog.
- **Which grammars are installed is a setting:**

  ```toml
  # ~/.config/limpid/config.toml
  [highlight]
  grammars = ["rust", "python", "typescript", "tsx", "bash", "json", "toml",
              "yaml", "go", "c", "cpp", "lua", "sql", "html", "css", "markdown"]
  theme = "default"
  ```

  `limpid-core grammars sync` fetches and compiles exactly that list (it needs
  a C compiler at install time). `grammars list` shows the catalog and what is
  installed. Fence names map to grammars through aliases (`rs`, `py`, `sh`,
  `ts`), taken from the same catalog.
- **Themes are CSS.** Highlight names become classes (`hl-keyword`,
  `hl-function-macro`), and a theme is a stylesheet with light and dark
  variants, so switching scheme needs no re-render. The default theme matches
  the page palette.

Vendored JavaScript (rail, search, fields, mermaid, CodeMirror) is compiled in
with `include_str!` and written once to `~/.cache/limpid/assets/<hash>.js`, so
pages refer to shared files instead of inlining megabytes. Pages saved with
`--out` inline everything and remain self-contained.

The JIT stays off by default (`JSC_useJIT=0`, about 10 MiB per web process).
`LIMPID_JIT=1` turns it back on.

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
  document order. Nested `- ` items are suggested answers. Free text is always
  allowed.
- The heading may be any level. limpid also accepts "Open questions" and
  "Questions for you", matched without regard to case. The list is in
  `config.toml` (`questions.headings`).
- In limpid, each field shows its options numbered and has a reply area.
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
`G`, PageUp/PageDown as half pages, `/ n N` with smartcase, and `Esc`.

| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | Next / previous question field. Wraps around and scrolls the field into view |
| `1`–`9` | On a focused field: pick that option |
| `Enter` or `i` | Open the reply editor on the focused field (CodeMirror, vim mode, starts in insert) |
| `c` | Comment on the block nearest the top of the view (any block, not only fields) |
| `Esc` then `:w`, or `Ctrl-Enter` | Save the reply and close the editor |
| `:q` | Close the editor without saving |
| `S` | Show the composed reply message for review |
| `Ctrl-Enter` (in review) | Send the reply |
| `t` | Toggle the rail |
| `{` / `}` | Previous / next heading |

The page owns keys while an editor or the search field has focus. The host
owns them otherwise. This rule exists today in `keys.py` and carries over.

## Replies

### Storage

Replies are stored in a sidecar file, never in agent transcripts:

- Agent pages: `~/.cache/limpid/<page>.replies.json`
- Markdown files you opened: written into the file itself as an HTML comment
  right after the block, so they stay with the document and are invisible in
  other renderers:

  ```markdown
  <!-- limpid reply q1
  SQLite. We can move later.
  -->
  ```

  When limpid renders the file again, it shows these comments as annotations on
  the block above them. It writes the file atomically (temporary file and
  rename) and refuses to save if the file changed on disk since it was rendered.

### Compact reply format

`S` composes one message from every answered field and comment:

```
re: <page title> (claude, turn 0)
q1: SQLite. We can move later.
q2: no
c "Use the existing pool…" (L42-48): this leaks on error
```

Fields use their `qN` id, which matches the agent's own numbering. Comments quote the first
few words of the block and its line range. Unanswered fields are left out.

### Delivery

The host delivers a reply by running a configurable reply command, with the
message on stdin and these environment variables:

| Variable | Meaning |
|---|---|
| `LIMPID_AGENT` | `claude`, `codex`, `pi` or `file` |
| `LIMPID_SESSION` | Session id, when the page came from a transcript |
| `LIMPID_PANE` | Terminal pane that opened the page (`$WEZTERM_PANE` captured by the hook or wrapper) |
| `LIMPID_SOURCE` | Path of the page's source |

The default command copies the message to the clipboard (`wl-copy`). The
shipped alternative, `limpid-core send`, types the message into the agent's
pane with `wezterm cli send-text --pane-id $LIMPID_PANE` and does **not** press
Enter, so you can still review it in the agent's own prompt. Agent-specific
delivery (for example a pi extension that receives the message directly) plugs
in through the same command.

## Windows and slots

The handoff message carries an optional `slot`. If a window already holds that
slot, the host loads the new page there instead of opening a window and keeps
the scroll position when the page is the same source. Hooks use the agent's
session id as the slot, so each agent gets one window that updates.

Opening a window never takes focus when the request comes from a hook
(`origin: hook`). Hyprland rules match the window class `limpid`.

## Watch

`--watch` re-renders on save and keeps the scroll position, anchored to the
`data-src` of the block at the top of the view rather than to a pixel offset.
Markdown files you open with `limpid` are watched by default, so annotations
appear after an external edit.

## Dependencies

Runtime: WebKitGTK 6.0 and GTK4 (system packages), `wl-copy`, and `wezterm`
for `send`. Compiled grammars live in `~/.local/share/limpid/grammars/`.

Install time: a C compiler and `git`, for `limpid-core grammars sync`.

Rust crates: `pulldown-cmark`, `pulldown-latex`, `ammonia`, `tree-sitter`,
`tree-sitter-highlight`, `libloading`, `toml`, `serde`, `serde_json`, `gtk4`,
`webkit6`. Every new crate needs a reason written in its PR.

Vendored JavaScript: `mermaid.min.js` and a CodeMirror 6 bundle with
`@replit/codemirror-vim`, built once and committed with the version and
licenses recorded in `vendor/README.md`.

## Testing

- **Golden pages:** fixed `.md` inputs and the expected HTML for headings, TOC,
  tables, code (including Rust macro bodies), math, question fields, escaping
  and sanitizing. Changes are reviewed
  as diffs.
- **Parity:** the existing TypeScript tests (spec, codex, page, cli, the
  Claude Code `read` tests) are ported first and must pass before the
  TypeScript code is deleted. Agent pages rendered from the existing fixtures
  must match the TypeScript output, ignoring whitespace.
- **Adversarial input:** duplicate, empty, emoji and very long headings;
  unclosed fences; a 10 MB file; invalid UTF-8; CRLF; no headings; raw
  `<script>`; a `## Questions` heading with no list, a bulleted instead of
  numbered list, or two Questions sections; a fence naming a language with no
  grammar; a grammar `.so` that fails to load; a reply comment containing `-->`;
  a file changed on disk between render and save.
- **Host:** the 19 handoff tests and 10 window tests from htmlview are ported.
  New tests cover reusing a slot, reply round trips through a fake reply
  command, and Tab order across fields.
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
