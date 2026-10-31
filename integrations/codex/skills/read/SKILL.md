---
name: read
description: Open the latest Codex response, or a numbered range of responses, in Limpid for reading, question answers, and comments. Use when the user invokes the Limpid read workflow.
---

Open the existing response in Limpid without regenerating or summarizing it.
An installed `limpid-core codex-hook` normally consumes `$read [spec]` before
model generation. If this skill reaches the model, use the fallback below.

Use the user's turn spec, defaulting to `0` (newest response); ranges such as
`0..2` are supported. Obtain the current conversation's `CODEX_THREAD_ID` or
`CODEX_SESSION_ID` from the tool environment, then run:

```sh
limpid-core codex SPEC --session THREAD_ID
```

Pass its returned page path to `limpid --source-backed PAGE`. Keep arguments
literal, and use `~/.local/bin/` binaries if they are absent from PATH. If no
session ID is available, stop and explain that the conversation cannot be
identified; do not select another conversation by directory.

Do not poll for answers. The user sends them with `:wq`; feedback arrives as
a complete follow-up in this conversation. Only pull `limpid-core feedback
PAGE` if that feedback is unclear. Report opening or delivery failures plainly.
