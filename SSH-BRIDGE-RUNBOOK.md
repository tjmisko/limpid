# SSH bridge: live-host runbook

Manual check of the bridge over plain `ssh` against a real host (`nlessfun`).
The automated tests in `crates/limpid/tests/ssh_bridge.rs` cover the same path
against an isolated loopback sshd; this runbook is the one step they cannot
do. Run it by hand. Nothing in the test suite connects to `nlessfun`.

## Prerequisites

Local machine (the desktop):

- `limpid`, `limpid-view` and `limpid-core` installed (`./install.sh`), with
  `limpid` and `limpid-view` side by side or on `PATH`.
- `limpid-bridge.socket` enabled (see SSH-BRIDGE.md) and
  `systemctl --user status limpid-bridge.socket` listening.
- `ssh -G nlessfun | grep -i remoteforward` prints
  `/tmp/limpid-<you>.sock .../limpid/hub.sock`.
- A graphical session (the viewer opens a window).

Remote machine (`nlessfun`):

- `limpid` and `limpid-core` on the login shell's `PATH` (headless build is
  fine; no viewer is needed there), and the Claude Code `/read` integration
  installed.
- `sshd` allows stream-local forwarding (the default) and has
  `StreamLocalBindUnlink yes`.

## Steps

1. Locally: `ssh nlessfun`.
2. On the remote: `ls -l /tmp/limpid-$USER.sock` shows `srw-------`.
3. Run `claude`, have it write a response (a `## Questions` section is a good
   one), then `/read`.
4. Expect: a Limpid window opens on the desktop with the response; `/read`
   returns within a second or two (a cold viewer start can take a few
   seconds).
5. Leave the window open, quit Claude, and `exit` the login. Expect the ssh
   session to end within about a second.
6. Afterwards, on the remote: `pgrep -af 'limpid --bridge-relay'` prints
   nothing. The socket file stays until the next login replaces it.
7. Log in again and `/read`: it works without anything left over getting in
   the way. In a tmux pane started under the first login, `/read` also works.

## Failure checks

- Forwarding refused (`AllowStreamLocalForwarding no`): ssh warns that the
  remote forward failed and logs in; `/read` says the forwarding is not active.
- Stale socket (sshd without `StreamLocalBindUnlink`): the second login's
  forward fails with a warning, and `/read` says the socket is stale and names
  the setting.
- Hub not running locally (`systemctl --user stop limpid-bridge.socket`):
  the remote socket still binds, and `/read` reports that the bridge
  connection closed during the handshake.
- Remote `limpid` missing: the shell says `command not found`; logout still
  works.
- Local viewer not installed: `/read` reports `limpid-view is not installed`.

## Stage 4: saving feedback to the remote page

Manual check after the steps above. Submitting (`:wq` to the agent) is still
unavailable; saving never wakes Claude.

1. Open a page with `/read` over `ssh nlessfun`. Answer a question and
   add a comment in the local window, then `:w`. The status line first says
   `Saved locally, saving to the remote`, then
   `Saved locally and on the remote (revision N)`.
2. On the remote: `limpid-core feedback --json <page.html>` shows the reply
   and comment. Claude stays idle: no new turn starts.
3. Delete the comment locally, `:w`, and confirm the remote no longer lists it.
4. Cut the connection (kill the `ssh` process of the login), edit, `:w`. The
   status line says `Saved locally only, not on the remote`; the local copy is
   at `~/.cache/limpid/remote/<stem>.replies.json`.
5. Two logins that opened the same page: save from one, then from the other.
   The second says `stale revision` and keeps its local copy.

## Stage 5: submitting feedback to the originating Claude session

Manual check, run by hand; nothing in the test suite connects to `nlessfun`.
The remote Claude plugin must be the one in `integrations/claude-code`, which
acknowledges each prompt to its listener. Restart Claude after updating it.

1. Open a page with `/read` over `ssh nlessfun`, answer a question, and
   `:wq`. The status line says `Saved locally, sending to the remote session`
   and then `Sent to the agent session`; the window closes and Claude starts a
   turn that answers the feedback.
2. Run two `claude` sessions (two logins or two panes) and `/read` in each.
   `:wq` in the first session's window: only that session starts a turn.
3. `/clear` in the session, then `:wq` in its older window: the status line
   says `Not sent` and the window stays open. The newer session does nothing.
4. Quit Claude and `:wq` again: `Not sent`, naming a missing listener; the
   feedback is still readable with `limpid-core feedback <page.html>`.
5. `:wq` with no feedback just closes the window and contacts nobody.
6. `Possibly sent, not confirmed` means the agent may have started a turn
   without confirming. Look at the session before sending again.

## Stage 6: failures, concurrency and installation

Manual check against `nlessfun`, run by hand; nothing in the test suite or
`scripts/measure-ssh-bridge` connects to it. The automated equivalents (two
clients, several pages, shell exit, killed ssh) run against a loopback sshd in
`crates/limpid/tests/ssh_bridge.rs` and `crates/limpid-core/tests/bridge_resilience.rs`.

1. Install: `./install.sh` locally. On `nlessfun`, install only `limpid` and
   `limpid-core` (`cargo install --path crates/limpid-core --locked`, same for
   `crates/limpid`) and the Claude plugin; confirm `limpid --help` and
   `limpid-core --help` run in a login shell. A build of a different
   protocol version there makes `/read` fail with a message about incompatible
   versions.
2. Two clients: from two desktops (or two local accounts) run `ssh
   nlessfun`, start `claude` in each, `/read` in each. Two independent windows
   open, one per desktop. `:wq` in one starts a turn only in that client's
   session. Open the same remote page from both with `limpid-core`'s page path;
   save from one, then the other: the second says `stale revision`.
3. Several pages: run `/read` three times in one login without closing the
   windows. Each window saves and submits to its own page.
4. Shell exit: with a window open, `exit` the remote shell. The local `ssh` ends within about a second; `pgrep -af 'limpid --bridge-relay'`
   on the remote prints nothing.
5. Network loss: with a window open, edit and `:w` (remote saved), edit again,
   then drop the connection (kill the local `ssh` process, or disable the
   network briefly). `:w` now says the login is gone. On the desktop,
   `limpid recover` lists the page; `limpid recover --text NAME` prints the
   feedback. Paste it into a new Claude session if wanted. Then
   `rm ~/.cache/limpid/remote/NAME.*`.
6. Restarts: close the Limpid window and reopen the same page from the cache
   (`limpid --viewer view ~/.cache/limpid/remote/NAME.html`), then `:w`: it still
   pushes while the login lives. Log out and in again: the old import cannot
   push and is listed by `limpid recover`.
7. Measurements: on the desktop run `LIMPID_MEASURE_COLD=1
   scripts/measure-ssh-bridge` for the cold and warm viewer start (real
   windows), and note the time `/read` takes against `nlessfun` over the real
   link. Add all three, with date and link, to `SSH-BRIDGE.md`.
