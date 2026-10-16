# limpid focus-mode tasks

Phase 10 from [DESIGN.md](DESIGN.md). Replaces the `Tab` question-field model
with a vim-style block cursor and restyles the comment path. Each task ends in
something usable and merges on its own. Task ids are stable once work starts.

## Phase 10: Focus mode and comment styling

- [x] **F10.1 Block cursor.** In `page.js`, build the ordered list of focus
  stops from the rendered DOM: `p[data-src]`, `blockquote[data-src]`,
  `figure.code[data-src]`, `figure.diagram[data-src]`, `div.table-wrap[data-src]`,
  every `li[data-src]` except `.question-option`, and saved `.limpid-annotation`
  callouts. Skip headings, `hr`, and any non-`li` stop nested inside a stopped
  `li` or `blockquote` (the outer block covers it). `focusStops` queries the DOM
  on each move, so callouts placed by `limpidRestoreComments` are included.
  Keep one `focused` element and a `.limpid-focused` class. Minimal style: a
  slight background tint and an inset teal rule, no layout shift, both schemes.
- [x] **F10.2 Movement.** `j` / `k` and `Down` / `Up` move focus to the next /
  previous stop. Scroll only when the new stop is off screen. `gg` / `G` go to
  the first / last stop. Reuse the existing `pendingG` timer.
- [x] **F10.3 Viewport alignment.** Add a `pendingZ` prefix: `zt`, `zz`, `zb`
  put the focused stop at the top, center, or bottom of the viewport.
- [x] **F10.4 Sections and scrolling.** `PageDown` / `PageUp` and `Ctrl-d` /
  `Ctrl-u` scroll half a page and leave focus alone. `{` / `}` and
  `Ctrl-Up` / `Ctrl-Down` move focus to the first stop of the previous / next
  section and scroll it to the top.
- [x] **F10.5 Initial focus.** On load, and after `--watch` reload, focus the
  first stop inside the restored viewport; fall back to the first stop. Some
  block is always focused.
- [x] **F10.6 Remove `Tab`.** Drop the `Tab` / `Shift-Tab` question-field
  branch. Route `1`–`9` and `Enter` / `i` through the focused `.question-field`
  instead of `document.activeElement`.
- [x] **F10.7 Comment on the focused block.** `c` opens the comment editor on
  the focused block. Drop `nearestBlock`. The editor mounts under the focused
  block and the saved callout goes to the section end, as today.
- [x] **F10.8 Edit a comment.** An `.limpid-annotation` stop carries its
  comment key (`<turn>:<src>` or `file:<start>-<end>`). `c` on it reopens the
  editor prefilled and replaces the callout on save. Agent pages upsert by key
  in `save_comment`. File pages get `FileEdit::CommentReplace`, and render.rs
  stamps each saved callout with its own `data-src` so the span is known.
- [x] **F10.9 Delete a comment.** `x` on a focused comment removes it and marks
  the key deleted. `dirtyPayload` carries `deletions`, so `save` and `deliver`
  remove the key from the agent sidecar or drop the annotation span from the
  markdown source in the same batch as other edits. Keeping the deletion in the
  buffer means unsaved replies survive. A stale file is refused as usual, and
  the host reloads file pages once.
- [x] **F10.10 CodeMirror restyle.** In `vendor/editor-entry.js`, raise the font
  to 15px with comfortable line height and padding, quiet gutters, teal cursor,
  and the page's surface / code colours. Rebuilt `vendor/editor.min.js` with
  the command in `vendor/README.md` (versions unchanged). Matched the height and
  borders in `page.css` `.limpid-editor`.
- [x] **F10.11 Callout restyle.** In `page.css`, make `.limpid-annotation`
  read as body text (15.5px, not 13px) with more padding, a clear label, and
  consistent spacing between consecutive callouts.
- [x] **F10.12 Host keys.** Removed the plain `j` / `k` bindings and the
  `ScrollDown` / `ScrollUp` actions from `limpid-host/src/keys.rs`, and updated
  the tests. `Page_Down` / `Page_Up` and `Ctrl-Up` / `Ctrl-Down` stay.
- [x] **F10.13 Tests and verification.** `cargo test --workspace` and `cargo
  clippy --workspace --all-targets -- -D warnings` pass, with new tests for
  file-comment replace and delete. The golden page was regenerated for the new
  CSS and JS. A live manual pass (focus movement, `zt` / `zb` / `zz`, comment
  edit and delete, editor and callout legibility in both schemes) is the one
  remaining check.

**Done when:** a reader moves a visible block cursor with `j` / `k`, keeps the
page still until the cursor leaves it, can comment, edit and delete a comment
from the keyboard, and the editor and callouts are legible in the page.

## Notes

- The stop list uses DOM selectors, not a render-time marker: fewer moving
  parts. If the nesting rules get fragile, add a `data-block` attribute in
  `render.rs` instead.
- Comments stay keyed as today: `<turn>:<start>-<end>` for agent pages. Saved
  file callouts are keyed `file:<start>-<end>` from the annotation's own
  `data-src`. The callout element carries that key so edit and delete know what
  to replace.
- Deletion is buffered, not a separate host message: `x` marks the key and the
  next `:w` / `:wq` sends it with the rest of the dirty buffer.

## Follow-ups

- Comments and editors sit directly under their block. A comment on a list item
  goes into the item, so it never renders as a new list element. Callouts and
  the focus band stretch to the article's reading edges, whatever the indent.
- Comments carry no `COMMENT` label; comment text is tinted blue so user text
  reads apart from the agent's.
- The focus highlight is a translucent full-width band with no left bar.
- Editor `:q` refuses to close a changed buffer and points to `:wq` or `:q!`.
- `PageDown` / `PageUp` and `Ctrl-d` / `Ctrl-u` move the cursor to the element
  at the new viewport center and center it. A tall element, the element already
  focused, or a center more than a viewport away falls back to a plain
  half-page scroll.
- `{` / `}` and `Ctrl-Up` / `Ctrl-Down` step one section at a time: focus the
  section's first stop and scroll so the section title sits at the top.
