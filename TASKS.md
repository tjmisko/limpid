# limpid tasks

Phased plan for [DESIGN.md](DESIGN.md). Each phase ends in something usable and
merges on its own. Task ids are stable; refer to them in branch names and
commits.

## Phase 0: Measure and check the risky parts

Small experiments, no product code. The answers decide details in later phases.

- [ ] **P0.1** Move the benchmark scripts from `~/.cache/htmlview-bench/` into
  `bench/`, made independent of specific cache pages. Record the htmlview
  baseline: cold start, warm start, private memory for 1, 2 and 3 windows.
- [ ] **P0.2** Build an empty window with `gtk4` + `webkit6` in Rust on this
  machine (aarch64, 16 KiB pages). Measure cold start and memory against the
  Python host.
- [ ] **P0.3** Time nvim highlighting for a page with 1, 10 and 50 code blocks.
  Decide whether a cache miss blocks first paint.
- [ ] **P0.4** Render a math sample with `pulldown-latex` in WebKit. Decide
  whether MathML is good enough or KaTeX loads lazily.
- [ ] **P0.5** Time Mermaid with the JIT off and on for small and large
  diagrams.

**Done when:** each item has a short write-up in `bench/NOTES.md` and any
changes to DESIGN.md are made.

## Phase 1: limpid-core renders markdown

- [ ] **P1.1** Create the Cargo workspace (`limpid-core`, `limpid`,
  `limpid-host`) next to the TypeScript code. CI runs `cargo test` and
  `cargo clippy`.
- [ ] **P1.2** Port the page template (`page.ts`): rail, compact TOC, styles,
  rail and search JS as compiled-in assets.
- [ ] **P1.3** Render markdown with `pulldown-cmark` (GFM, footnotes, front
  matter), heading ids, TOC (depth 3), table wrapping and `data-src` ranges.
- [ ] **P1.4** Highlight code with nvim plus a cache, following the P0.3 result.
- [ ] **P1.5** Escape raw HTML (agent mode) or sanitize it with `ammonia` (file
  mode).
- [ ] **P1.6** Golden tests and the adversarial input set.

**Done when:** `limpid-core render notes.md` writes a page that looks like
today's agent pages, and the golden and adversarial tests pass.

## Phase 2: limpid-core reads agents

- [ ] **P2.1** Port the turn specs (`spec.ts`) and their tests.
- [ ] **P2.2** Port the Claude Code reader (`turns.ts`) and the `read` tests.
- [ ] **P2.3** Port the Codex reader (`codex.ts`) and its tests and fixtures.
- [ ] **P2.4** Add the pi transcript reader; today pi passes turns in-process.
- [ ] **P2.5** Port `cli.ts`: `limpid-core <agent> [spec]` prints the page
  path, with cache pruning to 50 files.
- [ ] **P2.6** Parity check: pages from the existing fixtures match the
  TypeScript output.

**Done when:** every TypeScript test has a passing Rust counterpart.

## Phase 3: `limpid` front end on the existing host

- [ ] **P3.1** `limpid` binary: open `.md`, `.html` and `-`, write to the cache,
  hand off over the existing socket protocol, and start a host when none is
  running (the Python host for now).
- [ ] **P3.2** Add `slot` and `origin` to the handoff protocol, and support them
  in the Python host.
- [ ] **P3.3** Fold htmlview into this repo. Rename the window class, socket
  name and `HTMLVIEW_JIT` to `limpid` / `LIMPID_JIT`. Update the Hyprland rules
  in the same change. Archive the htmlview repo.
- [ ] **P3.4** Turn the Claude Code, pi and Codex integrations into wrappers
  around the binaries. Delete the TypeScript renderer and `node_modules`.
  Update `install.sh` to use `cargo install --path`.

**Done when:** `limpid notes.md` and `/read` work end to end with no Node
installed, and the benchmarks are no worse than the P0.1 baseline.

## Phase 4: Rust host

- [ ] **P4.1** `limpid-host`: windows, shared WebKit context, JIT setting,
  dark/light hosts, `--separate`, and exit when the last window closes.
- [ ] **P4.2** Port the key dispatcher (`keys.py`) and its tests.
- [ ] **P4.3** Port the handoff server and its 19 tests. Accept only the same
  user, as today.
- [ ] **P4.4** Port the 10 window tests, plus slot reuse and no-focus for hook
  requests.
- [ ] **P4.5** Delete the Python host.

**Done when:** all host tests pass, cold start is under 0.3 s, and a warm open
of a 2,000-line `.md` is under 150 ms.

## Phase 5: Richer markdown

- [ ] **P5.1** Math as MathML, or lazy KaTeX, following P0.4.
- [ ] **P5.2** Mermaid: lazy `mermaid.js`, SVG sent to the host, cache, and
  inlining on later renders.
- [ ] **P5.3** Vendored assets written to `~/.cache/limpid/assets/`, and `--out`
  inlining everything.
- [ ] **P5.4** `--watch` with scroll kept by `data-src` anchor. Watch opened
  `.md` files by default.
- [ ] **P5.5** `t` toggles the rail, `{` / `}` jump between headings.

**Done when:** a document with math, a diagram and footnotes shows readable
text first and the diagram after.

## Phase 6: Ask fields

- [ ] **P6.1** Parse ```` ```ask ```` blocks (question, options, optional id,
  automatic `qN` ids). Golden tests, including malformed blocks.
- [ ] **P6.2** Render fields: the question, numbered options, an empty reply
  area.
- [ ] **P6.3** `Tab` / `Shift-Tab` focus moves between fields; `1`–`9` picks an
  option.
- [ ] **P6.4** `limpid-core instructions`, and the line added to the Claude Code
  plugin, AGENTS.md for Codex, and the pi extension.

**Done when:** an agent following the instruction produces fields you can step
through and pick options in without the mouse.

## Phase 7: Replies and comments

- [ ] **P7.1** Vendor CodeMirror 6 with vim mode as one bundle; load it lazily
  on the first `Enter`, `i` or `c`.
- [ ] **P7.2** Reply editor on fields, and `c` comments on any block. Save with
  `:w` or `Ctrl-Enter`, cancel with `:q`.
- [ ] **P7.3** Storage: a sidecar JSON for agent pages, and in-file
  `<!-- limpid reply … -->` comments for opened files, written atomically and
  refused if the file changed on disk. Render existing replies as annotations.
- [ ] **P7.4** `S` composes the compact reply for review; `Ctrl-Enter` sends it
  through the reply command (clipboard by default).
- [ ] **P7.5** `limpid-core send`: types the reply into `$LIMPID_PANE` with
  `wezterm cli send-text`, without pressing Enter.

**Done when:** you can read an agent turn, answer every field, comment on a
block and get the reply into the agent's prompt without touching the mouse.

## Phase 8: Auto-open

- [ ] **P8.1** Claude Code Stop hook: render the newest turn and open it in the
  session's slot without taking focus. Opt-in, and only when the turn contains
  ask fields (configurable).
- [ ] **P8.2** The same for pi (an extension event) and Codex (a notify hook),
  where those agents support it.
- [ ] **P8.3** Capture `$WEZTERM_PANE` and the session id at open time and pass
  them to the reply command.

**Done when:** an agent asking a question makes a limpid window appear with
the fields ready, and your answers reach the agent's prompt.
