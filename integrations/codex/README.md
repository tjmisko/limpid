# Codex reader

The CLI and local app use the same `UserPromptSubmit` hook. It handles `$read`
and plain-text `limpid-read` locally, renders the exact session transcript, opens Limpid,
and returns a blocking hook decision to prevent model generation. Codex may
display this as a blocked prompt; that is how the local command is consumed.
Ordinary prompts produce no hook output. Read errors also block the prompt.

Install the current `limpid-core`, then run `python3 integrations/codex/install.py`
from the repository. The installer merges `hooks.json` into the user's
`~/.codex/hooks.json`, preserving other handlers, and installs the `read` skill.
It refuses to overwrite a different existing `read` skill. Use an absolute path to
`limpid-core` if it is not on the Codex process's PATH. Review and trust the
handler in Codex's `/hooks` interface; do not disable hook trust globally.
Restart the app or CLI so it loads the hook.

Use `$read` or `$read 0..2` in the CLI. In the app, select the read skill
from the skill/slash menu or type `$read`. Plain-text `limpid-read [spec]`
avoids skill resolution and also works with the trusted hook.

CLI 0.166.0 rejects `/read` and `/prompts:read` before hooks run. The hook
accepts `/read` for clients that submit literal slash text; it does not
register a new native slash command. The skill provides a model-run fallback
when hooks are unavailable: it opens the existing response without regenerating
it. That fallback costs a model turn. The trusted local hook is the fast path.

`:w` saves answers without sending. `:wq` saves first and uses the page's
session ID to run `codex queue --thread ID --message TEXT --resume` through
the default `reply.command = "limpid-core send"`. Custom reply commands still
take precedence. Feedback contains the question/comment context and the page
pull command. Delivery errors preserve the saved feedback. A resume warning
is reported even if queue exits successfully; check the queue before retrying
to avoid duplicating an already queued message. Clipboard and terminal-pane
fallbacks remain available for non-Codex pages.

Requires Codex with `UserPromptSubmit` hooks and `queue --resume` (developed
against 0.166.0). Local daemon sessions are the initial delivery target;
remote servers require an explicit delivery configuration. No new model
process is launched with `codex exec resume`.

The hook currently requires a JSONL transcript with a matching `session_meta`
identity. It fails clearly if the path is absent, inaccessible, or belongs to
another thread. Paginated-only and ephemeral threads are not yet supported.
Tests cover rendering, ranges, isolation, command consumption, argument handling,
delivery failures, and preserving existing hooks. Live checks confirmed CLI
slash rejection, skill discovery, App Server hook discovery, and rendering the
user's current app conversation. Untrusted hooks are skipped by Codex: opening
and live idle/busy feedback still need verification after trusting the handler.
Cloud orchestration may not run local hooks; use the skill fallback where local
tools are available.
