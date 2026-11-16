# SSH bridge: open remote agent responses locally

Open the response of a Claude Code session running on another machine in a
Limpid window on your desktop, and send your feedback back to that session.
The design and the staged build are in [SSH-BRIDGE-PLAN.md](SSH-BRIDGE-PLAN.md);
the manual live-host checks are in [SSH-BRIDGE-RUNBOOK.md](SSH-BRIDGE-RUNBOOK.md).

## Scope

- Direct OpenSSH logins only: `limpid ssh HOST`, then `claude` and `/read` on
  the remote. tmux, screen, moving a surviving agent to another client,
  arbitrary HTML pages and remote image assets are not supported.
- Claude Code `/read` and the native viewer only. A page whose Markdown
  sidecar is missing is refused on a bridged login rather than opened on the
  remote host.
- Several clients may log in to the same remote account at once. Each
  `limpid ssh` is independent: its own route, sockets, cache entries and
  windows.

## Installation

Local machine (where the windows appear):

- `./install.sh` (puts `limpid`, `limpid-view` and `limpid-core` side by side
  in `${PREFIX:-$HOME/.local}/bin`).
- An OpenSSH client (`LIMPID_SSH` names another one). A graphical session.

Remote machine (headless; no viewer, no display):

- Only `limpid` and `limpid-core` are needed, on the `PATH` of the login
  shell. Neither links a GUI toolkit, so they build without GTK or WebKit:

      cargo install --path crates/limpid-core --locked
      cargo install --path crates/limpid --locked

  Use the same release on both machines; mismatched protocol versions are
  refused with a message naming both sides.
- The Claude Code plugin in `integrations/claude-code` (it provides `/read` and
  acknowledges each submitted prompt). Restart Claude after updating it.
- `sshd` must allow remote stream-local forwarding: `AllowStreamLocalForwarding`
  must be `yes` (the default), `all` or `remote`; `no` and `local` refuse it.
  The launcher uses `-R /tmp/limpid-<route>.sock:...` with
  `ExitOnForwardFailure=yes`, so a refusal fails the login with a hint.
- `/tmp` writable on the remote.

## Use

    limpid ssh [--dark|--light] [ssh options] HOST

You get a normal login shell with `LIMPID_BRIDGE_SOCKET` and
`LIMPID_BRIDGE_ROUTE` set. `/read` there opens a window here and returns once
the viewer accepted the page. In the window, `:w` saves your annotations here
and on the remote page (Claude is not woken), and `:wq` also submits them to
the Claude session the page came from.

The status line says what is true: `Saved locally only`, `Saved locally and on
the remote (revision N)`, `Sent to the agent session`, `Not sent`, or
`Possibly sent, not confirmed` (look at the session before sending again; it
is never retried automatically).

## Failures and recovery

The local copy is written before anything touches the network, and is never
removed by the bridge. What each failure leaves:

| Failure | Result |
|---|---|
| Remote shell exits, with or without a page open | the ssh client exits promptly; the remote relay and both sockets are removed |
| Network loss (ssh killed) | the launcher exits; the remote relay sees the forward die and exits; later `:w` says the login is gone and keeps the local copy |
| Viewer restarted | nothing is lost: feedback is on disk; `:w` in the reopened window pushes it if the login is alive |
| Launcher restarted (`limpid ssh` ended and run again) | the old page cannot be pushed; its feedback stays in the local cache |
| Another client saved the same page first | the second save is refused as `stale revision`; its local copy is kept |
| Sender never answers | `:w` answers after the request timeout (10 s) with `Saved locally only`; `:wq` reports not sent |
| Agent exited, or `/clear` started a new session | `Not sent`; the page stays open; nothing goes to a newer session |

### Recovering feedback that was saved here but not on the remote

    limpid recover                     list pages whose feedback the remote lacks
    limpid recover --text NAME         print that feedback as the message :wq would send

Give the printed text to the agent yourself (paste it into the session). The
command only reads the cache (`~/.cache/limpid/remote`, or `$XDG_CACHE_HOME/limpid/remote`);
it never contacts the remote, never submits, and never deletes. Delete
`NAME.*` in the cache directory once the feedback is dealt with.

Recovery does not move an agent between clients. A page reopened with `/read`
in a new login is a new import: it starts from the remote's copy, so edits
that only exist in the old import are not merged in; use `--text` for them.
Feedback that reached the remote but was never submitted is read by the agent
with `limpid-core feedback <page.html>` as usual.

## Timeouts

Each layer answers before the one above gives up (checked by a test):
viewer launch 10 s, wait for the viewer to accept 20 s, the opening command
25 s; a save or submit request 10 s each, the agent's confirmation 3 s, the
viewer's wait for the launcher 30 s.

## Measurements

From `scripts/measure-ssh-bridge` on 2026-11-15, release build, one machine
(aarch64 Linux, Asahi), loopback sshd, 12 rounds. The viewer and the agent
are stand-ins, so these measure the bridge and ssh, not window creation:

| What | p50 | max |
|---|---|---|
| Remote `limpid` open command, viewer warm | 12 ms | 14 ms |
| `:w` until the remote acknowledged | 0.4 ms | 1.1 ms |
| `:wq` until the agent accepted | 0.6 ms | 0.8 ms |

Loopback has no network latency; on a real link each acknowledgment adds one
round trip over the existing connection and the open adds the transfer of the
page. Cold viewer startup is not measured yet: run
`LIMPID_MEASURE_COLD=1 scripts/measure-ssh-bridge` on a desktop and add the
result here. No claim about it is made until then.
