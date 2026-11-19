# SSH bridge: open remote agent responses locally

Open the response of a Claude Code session running on another machine in a
Limpid window on your desktop, and send your feedback back to that session.
The design and the staged build are in [SSH-BRIDGE-PLAN.md](SSH-BRIDGE-PLAN.md);
the manual live-host checks are in [SSH-BRIDGE-RUNBOOK.md](SSH-BRIDGE-RUNBOOK.md).

## Scope

- Direct OpenSSH logins: a plain `ssh HOST` with one `RemoteForward` line in
  `~/.ssh/config`, then `claude` and `/read` on the remote. Shells that
  outlive their login (tmux, screen) work too: `limpid` finds the forwarded
  socket by path, so a pane reaches whichever login currently forwards it.
- Claude Code `/read` and the native viewer only. A page whose Markdown
  sidecar is missing is refused on a bridged login rather than opened on the
  remote host. Arbitrary HTML pages and remote image assets are not supported.
- Several logins, from one client or several, may share a remote account.
  Each connection has its own route; pages from different logins stay apart.

## Installation

Local machine (where the windows appear):

- `./install.sh` (puts `limpid`, `limpid-view` and `limpid-core` side by side
  in `${PREFIX:-$HOME/.local}/bin`). A graphical session.
- The hub, socket-activated so nothing runs until a page arrives:

      cp integrations/systemd/limpid-bridge.{socket,service} ~/.config/systemd/user/
      systemctl --user daemon-reload
      systemctl --user enable --now limpid-bridge.socket

  It listens on `$XDG_RUNTIME_DIR/limpid/hub.sock`. Without systemd, run
  `limpid bridge serve [--dark|--light]` yourself.
- In `~/.ssh/config`, for each remote host:

      Host nlessfun
        RemoteForward /tmp/limpid-%r.sock ${XDG_RUNTIME_DIR}/limpid/hub.sock

  ssh expands `%r` to the remote user and `${XDG_RUNTIME_DIR}` locally
  (OpenSSH 8.4 or later); check with `ssh -G HOST | grep -i remoteforward`.

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
  must be `yes` (the default), `all` or `remote`.
- `sshd` should replace a socket left by an ended login. sshd does not remove
  the socket file at logout, and without this the next login's forward fails
  (ssh prints a warning and logs in anyway):

      echo 'StreamLocalBindUnlink yes' | sudo tee /etc/ssh/sshd_config.d/50-limpid.conf
      sudo systemctl reload sshd

## Use

    ssh HOST

`limpid` on the remote finds `/tmp/limpid-$USER.sock` and uses it only when
it is a socket owned by you with mode 0600; anything else at that path is an
error, never used. `LIMPID_BRIDGE_SOCKET` names a different path. `/read`
opens a window here and returns once the viewer accepted the page. In the
window, `:w` saves your annotations here and on the remote page (Claude is not
woken), and `:wq` also submits them to the Claude session the page came from.

A remote page holds its forwarded connection open for `:w` and `:wq`. When the
login's last session exits, the page lets go so ssh can exit; after that the
page's feedback can no longer reach the remote (see recovery below).

Two logins at once share the path: the newer one takes it over, so pages
opened after that go through the newer login, and end with it. If the newer
login ends first, `/read` in the older one reports a stale socket until you
log in again.

`limpid ssh [--dark|--light] [ssh options] HOST` still works and needs no
configuration: it runs its own hub for one login and sets
`LIMPID_BRIDGE_SOCKET` and `LIMPID_BRIDGE_ROUTE`.

The status line says what is true: `Saved locally only`, `Saved locally and on
the remote (revision N)`, `Sent to the agent session`, `Not sent`, or
`Possibly sent, not confirmed` (look at the session before sending again; it
is never retried automatically).

## Failures and recovery

The local copy is written before anything touches the network, and is never
removed by the bridge. What each failure leaves:

| Failure | Result |
|---|---|
| Remote shell exits, with or without a page open | the ssh client exits within about a second; the remote relay exits |
| Stale socket (login ended, sshd without `StreamLocalBindUnlink`) | `/read` says the socket is stale and names the sshd setting |
| Network loss (ssh killed) | the launcher exits; the remote relay sees the forward die and exits; later `:w` says the login is gone and keeps the local copy |
| Viewer restarted | nothing is lost: feedback is on disk; `:w` in the reopened window pushes it if the login is alive |
| Hub restarted, or the login ended and you logged in again | the old page cannot be pushed; its feedback stays in the local cache |
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
