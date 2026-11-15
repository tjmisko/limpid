# SSH bridge: stage 3 live-demo runbook

Manual check of `limpid ssh` against a real host (`nlessfun`). The automated
tests in `crates/limpid/tests/ssh_bridge.rs` cover the same path against an
isolated loopback sshd; this runbook is the one step they cannot do. Run it by
hand. Nothing in the test suite connects to `nlessfun`.

Scope: direct SSH only, Claude `/read`, preview windows (saving and submitting
arrive in later stages).

## Prerequisites

Local machine (the desktop):

- `limpid`, `limpid-view` and `limpid-core` installed (`./install.sh`), with
  `limpid` and `limpid-view` side by side or on `PATH`.
- OpenSSH client; key-based login to `nlessfun` already working.
- A graphical session (the viewer opens a window).

Remote machine (`nlessfun`):

- `limpid` and `limpid-core` on the login shell's `PATH` (headless build is
  fine; no viewer is needed there), and the Claude Code `/read` integration
  installed.
- `sshd` allows stream-local forwarding: `AllowStreamLocalForwarding` is not
  `no` or `remote` (the default `yes` works).
- `/tmp` is writable; the login's socket is `/tmp/limpid-<route>.sock`.

## Steps

1. Locally: `limpid ssh nlessfun` (add ssh options before the host, for
   example `limpid ssh -p 2222 nlessfun`). You land in a normal login shell.
2. On the remote shell: `echo $LIMPID_BRIDGE_SOCKET $LIMPID_BRIDGE_ROUTE`.
   Both must be set.
3. Run `claude`, have it write a response (a `## Questions` section is a good
   one), then `/read`.
4. Expect: a Limpid window opens on the desktop with the response; `/read`
   returns within a second or two (a cold viewer start can take a few
   seconds). The page is a preview; saving and submitting do not work yet.
5. Leave the window open, quit Claude, and `exit` the login. Expect the ssh
   session to end promptly.
6. Afterwards:
   - Remote: `pgrep -af 'limpid --bridge-relay'` prints nothing, and
     `ls /tmp/limpid-*.sock` shows none for this login.
   - Local: `ls "${XDG_RUNTIME_DIR:-/tmp/limpid-$UID}"/limpid` is empty.
7. Optional: open a second terminal, run `limpid ssh nlessfun` again, `/read`
   there, and confirm both windows are independent and both logins end cleanly.

## Failure checks

- Forwarding refused: with the server's `AllowStreamLocalForwarding no`,
  `limpid ssh` exits 255 and prints a hint naming that setting.
- Remote `limpid` missing: the shell says `command not found`; logout still
  works.
- Plain `ssh nlessfun` (no launcher): `/read` fails with a message saying
  the bridge socket is absent; nothing opens remotely.
- Local viewer not installed: `/read` reports `limpid-view is not installed`.

## Cleaning up after a failed run

Only touch files named for your own route. Local: remove stale
`$XDG_RUNTIME_DIR/limpid/b-*.sock` left by a killed launcher. Remote: remove
`/tmp/limpid-<route>.sock` for the route in that shell's
`$LIMPID_BRIDGE_ROUTE`.
