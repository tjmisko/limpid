# limpid-view parity with the WebKit page

The behaviour the WebKit page (`limpid-core/src/page.js`, `page.css`,
`client.css`, `limpid-host`) had, recovered from the git log, the pi and Claude
sessions in this project (2026-10-15 to 2026-10-27), and the old code. Each item
is the spec for porting it to `limpid-view`.

Status, where checked against `crates/limpid-view` on 2026-10-27:

- **missing**: not in the native viewer.
- **differs**: present but behaves or looks unlike the spec.
- **ok**: present and matches.
- *(blank)*: not checked yet.

Source tags: `pi 10-16` means a pi session that day; a short hash is a commit.

## Status after the 2026-10-27 workflows

Two workflows (`wf_e3216006-835`, nine units; `wf_0715d064-00d`, four units)
and the fixes after them landed every row below on main, except these:

- **K10, skipped by choice.** `yy` on a code block copies only the code; there
  is no mouse copy button.
- **Light theme bold and italic** keep the ink colour. page.css sets `strong`
  to `#fff` with no light value, which would vanish on the light page.
- `G` reveals the last stop, as `page.js` does (`setFocus(last, true)`); it
  does not scroll to the end of the document.
- `editor::rpc` has a test that occasionally times out under load.

## 1. Window

| # | Behaviour | Source | Status |
|---|---|---|---|
| W1 | Rounded corners on the in-page boxes: editor box, callouts, questions, code blocks, tables, the command and search bars (radii as in `page.css`/`client.css`: 10 for blocks and callouts, 8 for questions, 7 for bars, 6 for bands). Hyprland handles the window itself. | user 10-27 | |
| W2 | First frame is the page background (`#0c1015` dark), never a magenta, pink or white flash. | pi 10-16, b114fdc | |
| W3 | Wayland native, no XWayland fallback. | 559fdbc | |
| W4 | Window title is the page title. | host main.rs | |
| W5 | Windows open only on `/read` or an explicit `limpid` call; no auto-open, no "don't focus me" machinery. | pi 10-16, 03529a2 | |
| W6 | One viewer process per colour scheme serves every window. | DESIGN Windows | ok |
| W7 | No scroll animation anywhere: arrows, paging, section jumps and search snap. | pi 10-16 | |

## 2. Focus cursor

| # | Behaviour | Source | Status |
|---|---|---|---|
| F1 | Something is always focused. On open and after reload, focus goes to the first stop inside the restored viewport, else the first stop. | pi 10-16 | |
| F2 | Stops: headings, paragraphs, blockquotes, code blocks, diagrams, tables, every list item, questions, saved comment callouts. Not stops: `hr`, question options, a block nested inside a stopped `li` or blockquote, a callout's inner paragraph. | pi 10-16, 10-19, 10-22 | |
| F3 | Agent and source-backed pages: the metadata line (model name) and the compact contents block are never stops, so the cursor starts on the first real line. | pi 10-19, 03a5522 | |
| F4 | A saved comment is one stop: the outer box and the paragraph inside it are never focused separately. | pi 10-22 | |
| F5 | Text blocks: a translucent background band, `rgba(124,222,199,.06)` dark / `rgba(18,109,93,.05)` light, radius 6. **No left bar.** | pi 10-16 ×2, f34f10d | differs: teal bar painted, tint is .12 |
| F6 | The band spans the reading column, not the block: every band has the same width whatever the list indent. It stops 10 px (`BLEED_INSET`) inside the column edges on both sides. | pi 10-16 | |
| F7 | Code blocks, diagrams and tables get no band, only a 2 px outline (`rgba(124,222,199,.5)`) offset 2 px, so they stay legible. | pi 10-16, 10-19, 001f58d | missing |
| F8 | Headings get no band and no outline: the heading text turns teal. | pi 10-16, 6a7da68 | missing |
| F9 | Questions get a 2 px teal outline that follows their rounded box, not a band. | pi 10-19, f21f71d | |
| F10 | Input mode: the latest event decides. Mouse move, click or wheel switches to mouse mode, any key to keyboard mode. In mouse mode the band drops to `.02` and outlines to `.15` alpha (barely visible). | pi 10-16 | missing |
| F11 | Moving the cursor scrolls only when the new stop leaves the viewport, with a 12 px margin. | page.js `revealStop` | |

## 3. Motions

| # | Behaviour | Source | Status |
|---|---|---|---|
| M1 | `j`/`k` and `Down`/`Up` move to the next or previous stop. | pi 10-16 | ok |
| M2 | Counts on every motion: `3j`, `5k`, `2Down`, `2}`, `3{`, `3Ctrl-d`. `0` is not a count start. | pi 10-16, d5a5756 | |
| M3 | `gg` / `G`: first or last stop. | F10.2 | ok |
| M4 | `zt` / `zz` / `zb` put the focused stop at the top (8 px margin), centre, or bottom (8 px margin) of the viewport. | pi 10-16 | differs: bound, but user reports them not working; see M5 |
| M5 | Scroll-off: one viewport of empty space below the last block, so `zt` works on the last lines. | pi 10-19, 42d0c15 | missing: `max_scroll` stops at `height - viewport` |
| M6 | `PageDown`/`PageUp` and `Ctrl-d`/`Ctrl-u`: scroll half a page, then move the cursor to the stop at the new viewport centre and centre it. Fall back to a plain half-page scroll when that stop is taller than the viewport, is already focused, or centring it would jump more than a viewport. | pi 10-16, 23d1cdb | differs: the cursor stays put unless it leaves the screen |
| M7 | `{`/`}` and `Ctrl-Up`/`Ctrl-Down`: focus the first stop of the previous or next section and scroll so the section title is at the top (24 px heading scroll margin). Before the first section, go to the top and the first stop; past the last, go to the bottom and the last stop. | pi 10-16, 3aad2b9 | |
| M8 | `Q` jumps to the first unanswered question from anywhere, or flashes "No unanswered questions". | pi 10-19 | |

## 4. Contents rail and scrollspy

| # | Behaviour | Source | Status |
|---|---|---|---|
| R1 | Keyboard mode: the active rail entry is the section of the focused block. | pi 10-16, 9c0cb02 | differs: the rail follows scroll only |
| R2 | Mouse mode: the active entry follows scroll. At the very top it is the first section, at the bottom the last; otherwise it is the last heading above 90 px. | pi 10-16 | |
| R3 | `t` and `F1` toggle the rail. | pi 10-16 | ok |
| R4 | Rail look: 220 px, uppercase "Contents" label, active entry teal with a 2 px teal left rule on the surface colour, depth-3 entries indented. | page.css | |
| R5 | Below 1100 px wide, the rail hides and a collapsible compact contents block shows under the metadata line. | page.css | |

## 5. Visual mode and yank

| # | Behaviour | Source | Status |
|---|---|---|---|
| V1 | `v` starts visual mode at the cursor; `v` or `Esc` ends it. `j`/`k` with counts extend it. | pi 10-16 | |
| V2 | The selection is one continuous rectangle, `rgba(124,222,199,.12)`, radius 6, column-wide minus the 10 px inset, from the first block's top to the last block's bottom. | pi 10-16 | differs: tint is .24 |
| V3 | Code blocks, tables and diagrams inside the selection paint above it on their own background with a 2 px teal outline, so they stay legible while the band reads as continuous. | pi 10-16 | |
| V4 | Visual on a lone heading is text colour only; once the selection reaches another block the heading joins the band. | pi 10-16 | |
| V5 | `y` in visual mode yanks the selection and exits. | pi 10-16 | |
| Y1 | `yy` yanks the focused block. On a code block it yanks only the code: no fences, no language tag, one trailing newline. | pi 10-16, 10-19, e22b5c2 | |
| Y2 | `yis` / `yas`: the section without or with its heading. `y3j`, `y2k`, `yG`. | pi 10-16 | |
| Y3 | `Y` yanks the whole page. | pi 10-16 | ok |
| Y4 | Yank copies the Markdown source of the blocks, sliced by source range, not the rendered text. | pi 10-16, 8ba830a | |
| Y5 | A short status flash: "Yanked code", "Yanked 3 blocks", "Yanked section", "Yanked page" (1.6 s). | page.js | |

## 6. Comments and replies

| # | Behaviour | Source | Status |
|---|---|---|---|
| C1 | `c` and `o` comment on the focused block. On a saved comment they reopen it for editing. On a question they edit its answer. | pi 10-16 ×2 | differs: `o` is unbound |
| C2 | `x` on a saved comment deletes it, buffered until `:w`/`:wq`. | pi 10-16 | ok |
| C3 | `i` / `I` / `A` open the editor with the cursor at the start, start, or end. `Enter` or `i` on a question edits its reply; `Enter` on a comment edits it. | c0aac8a | missing: `I`/`A` |
| C4 | A comment sits directly under its block. Inside a list it goes inside the item, so it never renders as a new list element. | pi 10-16 | |
| C5 | Callout look: no label (no "COMMENT"/"Reply" heading), comment text blue (`--blue`), monospace 15 px with line-height 1.7, 1 px rule border, surface background, radius 10, padding 10×14, margin 12 above and 20 below. Same column-wide bleed as the focus band. | pi 10-16 ×2, f34f10d | differs: labelled "Reply"/"Comment" |
| C6 | Comment and reply bodies render a Markdown subset: inline code, links, bold, italic, strikethrough, bulleted and numbered lists. Other syntax stays literal. | pi 10-22, 63e2933 | |
| C7 | Question replies stand out: weight 500, a bordered surface box under the options. The chosen option turns teal and bold. Options are numbered in teal monospace, and `1`–`9` picks one. | pi 10-19 | |
| C8 | Edit in place: the callout itself becomes the editor (teal border, glow), with no second copy of the text above or below. On close it becomes the callout again and looks almost the same. Applies to questions and comments. | pi 10-22, 63e2933 | |

## 7. Editor box (was CodeMirror, now nvim)

| # | Behaviour | Source | Status |
|---|---|---|---|
| E1 | `:wq`, `:x`, `:w` and `Ctrl-Enter` save and close. `:q` closes an unchanged or empty box. On a changed box `:q` refuses and asks for `:wq` or `:q!`. | pi 10-16 ×2 | |
| E2 | After closing, focus returns to the block the box belongs to (the question or the comment). | pi 10-16 | |
| E3 | Look: 15 px monospace, line-height 1.7, page surface and code colours, teal cursor, quiet gutters, radius 10, max 70 % of viewport tall, then it scrolls inside. | pi 10-16, F10.10 | |
| E4 | The editor's own `:` and `/` lines are dark and match the page (they were white on white). | pi 10-19 | |
| E5 | Keys typed in the box go to nvim, using the user's nvim config; outside the box the same keys are limpid's. | Claude 10-26 | ok |

## 8. Command line, search, help

| # | Behaviour | Source | Status |
|---|---|---|---|
| K1 | `:` opens a bar along the bottom with a `:` already shown, so the user does not type it again. A typed colon is stripped. Placeholder `q  w  wq  q!  qa`. `Esc` cancels. | pi 10-16 | |
| K2 | `q` quits when nothing is unsaved; otherwise it shows "unsaved changes — :wq to save and send, :q! to discard". `:q` does the same. | pi 10-16 | differs: plain `q` unbound (81dd5a3) |
| K2b | `q` and `:q` on a page with no unsaved edits close **this window at once**, whatever other windows are open. Only the last window closing ends the process. Today `WindowCloser::close` (`limpid-view/src/main.rs`) refuses `CloseKind::Window` when more than one page is live and flashes "other windows are open"; the window loop (`window.rs`) has only `UserEvent::CloseAll`, so it needs a per-window close event. Same for `:q!` and `:wq`. | user 10-27 | differs |
| K3 | `:w` saves without sending. `:wq` saves and sends; with no feedback it just closes and never wakes the agent. `:q!` discards. `:qa` closes every window. Unknown commands show "Not a command: :x". | pi 10-16 | |
| K4 | `/` opens search in the bottom-right corner (amber text, teal border). `Enter` searches forward, `Shift-Enter` backward, `Esc` closes. | pi 10-16, d0b258b | |
| K5 | Smartcase: case-insensitive unless the query has a capital. | page.js | ok |
| K6 | `n`/`N` go to the next or previous match, wrap, highlight it, and centre it (`nzz`). | pi 10-16 | |
| K7 | `?` toggles a key reference overlay; `?` or `Esc` closes it. | pi 10-16 | missing |
| K8 | `S` previews the composed reply, with `Ctrl-Enter` to send. | pi 10-16 | |
| K9 | `r` reloads, `+`/`-`/`0` zoom (0.5–2, steps of 0.1), `Ctrl-q`/`Ctrl-w` quit. | keys.rs | |
| K10 | Code blocks have a copy control (a hover button in the WebKit page). | pi 10-16 | |

## 9. Agent loop

| # | Behaviour | Source | Status |
|---|---|---|---|
| A1 | `/read` never starts an agent turn. The note it leaves is one quiet line. | pi 10-16, 568ed92 | |
| A2 | `:wq` writes the sidecar first, then wakes the agent through the session socket, then falls back to `reply.command`. | DESIGN Feedback loop | differs: the Claude wake fails, see below |
| A3 | Composed feedback: each comment quotes about 100 characters of the agent's text, cut at a word boundary, with its turn id, then the user's full comment. Comments on questions count as answers. The message stands on its own, and the agent pulls the page only if confused. | pi 10-16 | |
| A4 | Saved-but-unsent feedback can be pulled with `limpid-core feedback`, but is never pushed. | pi 10-16 | |
| A5 | Pages are named by a content hash of the selected turns, and pages with feedback survive pruning. Keep up to 500 pages. | pi 10-16, 10-21 | |

**Claude wake bug (found 2026-10-27).** This session's page has
`"feedback_sock": null` in its `meta.json`, and no socket exists for session
`43e44b1b…`. Two orphaned listeners from 09:32 and 09:33 are still running,
under session ids that have no transcript. Likely cause, not yet verified:
`/clear` (and possibly `/model`) runs `session.start` for the new session and
then `session.end` for the old one. `stopListener()` in
`integrations/claude-code/hooks/register.ts` then clears `feedbackSock` for the
new session. `listener.return()` does not kill the python process, so the
listeners pile up.

## 10. Configuration

| # | Behaviour | Source | Status |
|---|---|---|---|
| G1 | Font size is configurable in `~/.config/limpid/config.toml` (default 17). The config lives in `~/linux-config` and is installed with stow. | pi 10-22 | |
| G2 | Basic style options in `config.toml` replacing `custom.css`: at least font size, line height, content width, the page colour tokens (`bg`, `surface`, `ink`, `muted`, `teal`, `blue`, ...), and focus and selection alpha. | pi 10-22, user 10-27 | missing: the native viewer reads no CSS |
| G3 | Syntax colours are Catppuccin Mocha for code tokens only; the page theme stays limpid's own. | pi 10-16 | |
| G4 | Grammar list in `[highlight] grammars`. | pi 10-22 | |
