# limpid focus-mode tasks

Phase 10 from [DESIGN.md](DESIGN.md). Replaces the `Tab` question-field model
with a vim-style block cursor and restyles the comment path. Each task ends in
something usable and merges on its own. Task ids are stable once work starts.

## Phase 10: Focus mode and comment styling

- [ ] **F10.1 Block cursor.** In `page.js`, build the ordered list of focus
  stops from the rendered DOM: `p[data-src]`, `blockquote[data-src]`,
  `figure.code[data-src]`, `figure.diagram[data-src]`, `div.table-wrap[data-src]`,
  every `li[data-src]` except `.question-option`, and saved `.limpid-annotation`
  callouts. Skip headings, `hr`, and any non-`li` stop nested inside a stopped
  `li` or `blockquote` (the outer block covers it). Query after
  `limpidRestoreComments` places the callouts, so edits rebuild the list.
  Keep one `focused` element and a `.limpid-focused` class. Minimal style: a
  slight background tint, no layout shift, works in both schemes.
- [ ] **F10.2 Movement.** `j` / `k` and `Down` / `Up` move focus to the next /
  previous stop. Scroll only when the new stop is off screen. `gg` / `G` go to
  the first / last stop. Reuse the existing `pendingG` timer.
- [ ] **F10.3 Viewport alignment.** Add a `pendingZ` prefix: `zt`, `zz`, `zb`
  put the focused stop at the top, center, or bottom of the viewport.
- [ ] **F10.4 Sections and scrolling.** `PageDown` / `PageUp` and `Ctrl-d` /
  `Ctrl-u` scroll half a page and leave focus alone. `{` / `}` and
  `Ctrl-Up` / `Ctrl-Down` move focus to the first stop of the previous / next
  section and scroll it to the top.
- [ ] **F10.5 Initial focus.** On load, and after `--watch` reload, focus the
  first stop inside the restored viewport; fall back to the first stop. Some
  block is always focused.
- [ ] **F10.6 Remove `Tab`.** Drop the `Tab` / `Shift-Tab` question-field
  branch. Route `1`–`9` and `Enter` / `i` through the focused `.question-field`
  instead of `document.activeElement`.
- [ ] **F10.7 Comment on the focused block.** `c` opens the comment editor on
  the focused block. Drop `nearestBlock`. The editor mounts under the focused
  block and the saved callout goes to the section end, as today.
- [ ] **F10.8 Edit a comment.** An `.limpid-annotation` stop carries its
  comment key (`<turn>:<src>` or the file range). `c` on it reopens the editor
  prefilled and replaces the callout on save. Agent pages already upsert by key
  in `save_comment`. File pages must replace the existing `<!-- limpid comment
  -->` block instead of inserting a second one: add a replace path to
  `FileRevision` keyed by the annotation span.
- [ ] **F10.9 Delete a comment.** `x` on a focused comment removes it. New host
  message `delete-comment` carrying the key: remove the key from the agent
  sidecar, or remove the annotation block from the markdown source. Refuse if
  the file changed on disk. Re-render file pages once and ack the page.
- [ ] **F10.10 CodeMirror restyle.** In `vendor/editor-entry.js`, raise the font
  to ~15px with comfortable line height and padding, quiet gutters, teal cursor,
  and the page's surface / code colours. Rebuild `vendor/editor.min.js` with
  the command in `vendor/README.md` (versions unchanged, so that file stays).
  Match the height and borders in `page.css` `.limpid-editor`.
- [ ] **F10.11 Callout restyle.** In `page.css`, make `.limpid-annotation`
  read as body text (roughly 15–16px, not 13px) with more padding, a clear
  label, and consistent spacing between consecutive callouts.
- [ ] **F10.12 Host keys.** Remove the plain `j` / `k` bindings from
  `limpid-host/src/keys.rs` and `plain_bindings`, and update their tests, so the
  page owns focus movement. Keep `Page_Down` / `Page_Up` and
  `Ctrl-Up` / `Ctrl-Down` as they are.
- [ ] **F10.13 Tests and verification.** No render change, so the golden pages
  stay. `cargo test --workspace` and `cargo clippy --workspace --all-targets --
  -D warnings` pass. Manual live check: `j` / `k` and the arrows move focus,
  `Ctrl-d` / `Ctrl-u` and the page keys scroll, `zt` / `zb` / `zz` align, `c`
  comments on and reopens the focused block, `x` deletes, callouts are legible
  in dark and light, and the editor is readable. Confirm `Tab` is gone and
  number keys answer the focused question.

**Done when:** a reader moves a visible block cursor with `j` / `k`, keeps the
page still until the cursor leaves it, can comment, edit and delete a comment
from the keyboard, and the editor and callouts are legible in the page.

## Notes

- The stop list uses DOM selectors, not a render-time marker: fewer moving
  parts and no golden churn. If the nesting rules get fragile, add a
  `data-block` attribute in `render.rs` instead.
- Comments stay keyed as today: `<turn>:<start>-<end>` for agent pages, and the
  in-file annotation for opened markdown. The callout element carries that key
  so edit and delete know what to replace.
