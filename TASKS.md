# limpid tasks

Phased plan for [DESIGN.md](DESIGN.md). Each phase ends in something usable and
merges on its own. Task ids are stable once work starts; refer to them in branch
names and commits.

Performance measurements in Phases 0, 3 and 4 are deferred at the user's
request. Build and behavior work continues independently of those benchmarks.

## Phase 0: Measure and check the risky parts

Small experiments, no product code. The answers decide details in later phases.

- [ ] **P0.1** Move the benchmark scripts from `~/.cache/htmlview-bench/` into
  `bench/`, made independent of specific cache pages. Record the htmlview
  baseline: cold start, warm start, private memory for 1, 2 and 3 windows.
- [ ] **P0.2** Build an empty window with `gtk4` + `webkit6` in Rust on this
  machine (aarch64, 16 KiB pages). Measure cold start and memory against the
  Python host.
- [ ] **P0.3** Highlighting spike: load a Rust grammar `.so` with Helix's
  queries through `tree-sitter-highlight`. Check that macro bodies are
  highlighted as Rust, and time query compilation for Rust, TypeScript and
  markdown (the largest queries). Decide whether first paint waits for it.
- [ ] **P0.4** Render a math sample with `pulldown-latex` in WebKit. Decide
  whether MathML is good enough or KaTeX loads lazily.
- [ ] **P0.5** Time Mermaid with the JIT off and on for small and large
  diagrams.

**Done when:** each item has a short write-up in `bench/NOTES.md`, and
DESIGN.md's open questions are answered or updated.

## Phase 1: limpid-core renders markdown

- [x] **P1.1** Create the Cargo workspace (`limpid-core`, `limpid`,
  `limpid-host`) next to the TypeScript code. CI runs `cargo test` and
  `cargo clippy`.
- [x] **P1.2** Settings: `~/.config/limpid/config.toml` with every key
  optional, defaults when the file is absent, and an error naming any unknown
  key.
- [x] **P1.3** Port the page template (`page.ts`): rail, compact TOC, styles,
  and the rail, scroll-spy and search scripts as compiled-in assets.
- [x] **P1.4** Render markdown with `pulldown-cmark` (GFM, footnotes, front
  matter title), heading ids, TOC (depth 3), table wrapping and `data-src`
  ranges.
- [x] **P1.5** Grammar catalog: generate `catalog.toml` from Helix's
  `languages.toml` (repository, commit, aliases, query paths). Implement
  `limpid-core grammars sync | list`.
- [x] **P1.6** Highlighting: load grammars at runtime, compile each language's
  queries on first use, output highlight classes, and ship a default theme as
  light and dark CSS. Plain text when a grammar is missing or fails to load.
- [x] **P1.7** Escape raw HTML (agent mode) or sanitize it with `ammonia` (file
  mode).
- [x] **P1.8** `limpid-core render FILE.md`, golden tests, and the adversarial
  input set.

**Done when:** `limpid-core render notes.md` writes a page that looks like
today's agent pages, with Rust macro bodies highlighted, and the golden and
adversarial tests pass.

## Phase 2: limpid-core reads agents

- [x] **P2.1** Port the turn specs (`spec.ts`) and their tests.
- [ ] **P2.2** Port the Claude Code reader (`turns.ts`) and the `read` tests.
- [x] **P2.3** Port the Codex reader (`codex.ts`) and its tests and fixtures.
- [x] **P2.4** Add the pi transcript reader; today pi passes turns in-process.
- [x] **P2.5** Port `cli.ts`: `limpid-core <agent> [spec]` writes the page and
  `<page>.meta.json` (agent, session id, spec, source), prints the page path,
  and keeps the newest 50 pages.
- [ ] **P2.6** Parity check: pages from the existing fixtures match the
  TypeScript output.

**Done when:** every TypeScript test has a passing Rust counterpart.

## Phase 3: `limpid` front end on the existing host

- [x] **P3.1** `limpid` binary: open `.md`, `.html` and `-`; write to the
  cache; `--out`; hand off over the socket protocol. When no host is running,
  start it first so GTK starts up while the page renders. The Rust host
  replaced the planned interim Python host.
- [ ] **P3.2** Extend the handoff message with `slot`, `origin` and `pane`
  (captured from `$WEZTERM_PANE`). The Python host supports slot reuse and
  no-focus for `--hook`.
- [ ] **P3.3** Fold htmlview into this repo. Rename the window class, socket
  name and `HTMLVIEW_JIT` to `limpid` / `LIMPID_JIT`. Update the Hyprland rules
  in the same change. Archive the htmlview repo.
- [ ] **P3.4** Turn the Claude Code, pi and Codex integrations into wrappers
  around the binaries. Delete the TypeScript renderer and `node_modules`.
  Switch `install.sh` to `cargo install --path` plus `grammars sync`, and
  rewrite the README.

**Done when:** `limpid notes.md` and `/read` work end to end with no Node
installed, and the benchmarks are no worse than the P0.1 baseline.

## Phase 4: Rust host

- [ ] **P4.1** `limpid-host`: windows, shared WebKit context, JIT setting,
  per-scheme hosts, `--separate`, and exit when the last window closes.
- [ ] **P4.2** Port the key dispatcher (`keys.py`) and its tests.
- [ ] **P4.3** Port the handoff server and its 19 tests, including slot, origin
  and pane. Accept only the same user, as today.
- [ ] **P4.4** Port the 10 window tests. Add tests for slot reuse with scroll
  kept and `--hook` not taking focus.
- [ ] **P4.5** Script message handler plumbing (`reply`, `comment`,
  `mermaid-svg`) with a test harness. Each handler is filled in by its phase.
- [ ] **P4.6** Delete the Python host.

**Done when:** all host tests pass, cold start is under 0.3 s, and a warm open
of a 2,000-line `.md` is under 150 ms.

## Phase 5: Richer markdown

- [x] **P5.1** Shared assets written once to `~/.cache/limpid/assets/`. `--out`
  inlines everything except lazy bundles.
- [ ] **P5.2** Math as MathML, or lazy KaTeX, following P0.4.
- [ ] **P5.3** Mermaid: vendor `mermaid.min.js` (`vendor/README.md` records
  version, license and rebuild steps), load it after first paint, send the SVG
  to the host, cache it in `~/.cache/limpid/mermaid/`, and inline cached
  diagrams on later renders.
- [ ] **P5.4** `--watch` with scroll kept by `data-src` anchor. Watch opened
  `.md` files by default.
- [x] **P5.5** `t` toggles the rail, `{` / `}` jump between headings.

**Done when:** a document with math, a diagram and footnotes shows readable
text first and the diagram after, and reopening it loads no Mermaid script.

## Phase 6: Question fields

- [x] **P6.1** Find the Questions section (`questions.headings`, any level,
  case-insensitive): top-level numbered items become `q1`, `q2`, …, and nested
  `- ` items become options. Golden tests, including a heading with no list,
  bulleted lists, nested numbering and two Questions sections.
- [x] **P6.2** Render fields: the question, numbered options, an empty reply
  area.
- [x] **P6.3** `Tab` / `Shift-Tab` focus moves between fields; `1`–`9` picks an
  option.
- [x] **P6.4** `limpid-core instructions`, and the line added to the Claude Code
  plugin, AGENTS.md for Codex, and the pi extension.

**Done when:** an agent following the instruction produces fields you can step
through and pick options in without the mouse.

## Phase 7: Replies and comments

- [x] **P7.1** Vendor CodeMirror 6 with vim mode as one bundle (recorded in
  `vendor/README.md`); load it lazily on the first `Enter`, `i` or `c`.
- [ ] **P7.2** Reply editor on fields, and `c` comments on any block. Save with
  `:w` or `Ctrl-Enter`, cancel with `:q`.
- [ ] **P7.3** Storage in `limpid-core`: `<page>.replies.json` for agent pages;
  in-file `<!-- limpid reply qN -->` and `<!-- limpid comment -->` blocks for
  opened files, written atomically, with `-->` escaped, refused if the file
  changed on disk. Render saved replies as annotations.
- [ ] **P7.4** `S` composes the compact reply for review; `Ctrl-Enter` runs
  `reply.command` with the `LIMPID_*` variables from the page meta and handoff.
  Clipboard by default.
- [x] **P7.5** `limpid-core send`: types the reply into `$LIMPID_PANE` with
  `wezterm cli send-text`, without pressing Enter; clipboard when no pane is
  known.

**Done when:** you can read an agent turn, answer every field, comment on a
block and get the reply into the agent's prompt without touching the mouse.

## Phase 8: Auto-open

- [x] **P8.1** Claude Code Stop hook: when `autoopen.claude` is on and the
  newest turn has a Questions section, render it and open it with
  `--hook --slot <session id>`.
- [ ] **P8.2** The same for pi (an extension event) and Codex (a notify hook),
  where those agents support it.
- [ ] **P8.3** End-to-end test: a fixture turn with questions opens in its
  slot, a reply is sent through a fake reply command, and the variables match
  the session and pane.

**Done when:** an agent asking a question makes a limpid window appear with
the fields ready, and your answers reach the agent's prompt.
