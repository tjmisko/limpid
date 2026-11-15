Execution plan for opening remote agent responses in local Limpid windows and
returning feedback to the originating remote session.

The first version supports direct OpenSSH sessions and Claude `/read`, using
the native viewer. tmux, screen, client switching for a surviving agent,
arbitrary HTML pages, and remote image assets are deferred. Independent SSH
logins from several clients remain in scope.

The proposed entry point is `limpid ssh nlessfun`, followed by the usual
`claude` and `/read`. A later shell integration can preserve the spelling
`ssh nlessfun`. The launcher owns a local bridge and a fresh reverse Unix
socket route for that login. Page traffic and feedback use that SSH
connection. No desktop address discovery or reverse login is needed.

The remote integration keeps the canonical page and feedback files. The
local viewer receives an exact Markdown snapshot and keeps a durable local
copy of saved feedback. A relay holds each page's connection after the
opening command returns. The local viewer, including Neovim, runs locally.

Implement the stages in order, with a separate commit for each working
increment. Each stage has a test gate before the next begins. Existing local
opening and feedback behavior must continue to pass its tests.

1. **Define and exercise the bridge protocol.**

   Add a versioned protocol and exercise it over local Unix sockets before
   involving SSH or a GUI. Messages cover handshake, open, save, submit,
   close, acknowledgments, and errors. Each open binds a login route, agent
   session, immutable page identity, and an open-instance ID. Remote paths
   remain metadata; the receiving side chooses its own cache paths.

   Test fragmented frames, Unicode, size limits, incompatible versions,
   disconnects, and interleaved independent pages. Bind operations to the
   page accepted at open time. A remote delivery target must never fall back
   to local clipboard or terminal delivery, including during partial rollout.

   Done when a headless harness transfers a page and routes responses back
   to the correct connection without shared filesystem paths.

2. **Open a transferred snapshot in the local native viewer.**

   Import the Markdown, display metadata, and existing annotations into a
   local cache. Construct an agent-mode handoff with explicit remote origin
   information. Preserve Markdown bytes so annotation ranges stay valid.
   Remote asset references must not resolve against unrelated local files.
   Start the viewer through its existing launch path when necessary.

   Test with separate sender and receiver cache directories. Compare the
   imported document and annotations with an ordinary local agent page.
   Exercise two open instances and missing or malformed metadata. Confirm
   opening acknowledgment reports actual acceptance or failure. Until the
   return path is implemented, expose this as a preview with submission
   unavailable.

   Done when a local process can send a fixture and get the expected native
   Limpid window, with no access to sender-side files.

3. **Establish the route through a direct SSH login.**

   Add the connection launcher, unique reverse socket forwarding, and remote
   shell bootstrap. Supply the route through that shell's environment rather
   than requiring server-wide environment acceptance. Remote `limpid`
   recognizes the route and launches the relay; `/read` returns after the
   local opening acknowledgment. Forwarding setup and application handshake
   failures both produce useful errors.

   Own the SSH transport lifetime explicitly, including any connection
   sharing behavior. Tie relay cleanup to the login lifecycle. Keep bridge
   sockets private and short enough for Unix socket path limits; remove only
   endpoints owned by this launcher.

   Test launcher arguments and quoting, forwarding failure, absent remote
   helpers, absent local viewers, early relay failure, and clean logout with
   a page still open. Verify the actual forwarding path using an isolated
   SSH test setup, then a manual connection to `nlessfun`.

   Done when remote `/read` opens locally through SSH, returns promptly, and
   logout neither hangs nor leaves relays behind. This is the first live demo.

4. **Save annotations back to the remote page.**

   Add the remote save backend. Persist locally before network work and send
   structured annotations, including deletions. Save the remote sidecar
   atomically and acknowledge its revision. Serialize saves per page; use
   revisions to reject stale updates and make a repeated save safe. Network
   work runs outside the GUI event loop.

   Test replies, comments, edits, deletions, repeated saves, stale revisions,
   and disconnects before and after remote persistence. Verify remote
   `limpid-core feedback` reads the saved result. Show whether feedback is
   saved locally or also saved remotely. Saving alone never submits a prompt.

   Done when `:w` preserves feedback on both machines while Claude stays idle,
   and interrupted delivery preserves the local saved copy.

5. **Submit feedback to the originating Claude session.**

   After remote persistence, compose the message remotely and deliver it to
   the listener captured for that agent session. Keep pull-command paths
   remote. Strengthen listener acknowledgments so submission success means
   the integration accepted the prompt, rather than merely printed a frame.
   Preserve `:wq` submission, `:w` save-only behavior, and cancellation.

   Test two simultaneous sessions, `/clear`, an exited agent, empty feedback,
   failed submission, and a lost acknowledgment. Never select a newer agent
   as a substitute. If submission may have happened but confirmation was
   lost, report an uncertain result and preserve feedback; do not blindly
   retry and risk a duplicate turn. Keep the page open on failed submission.

   Done when local `:wq` starts a turn in the correct remote Claude session,
   and the other session remains unaffected. This completes the round trip.

6. **Validate failures, concurrency, and installation.**

   Exercise two clients connected to the same remote account, several pages
   on one login, viewer and bridge restarts, shell exit, network loss, and
   explicit recovery of locally saved but unsent feedback. Define recovery
   behavior without promising migration of an agent between clients. Check
   bounded timeouts, resource cleanup, and compatibility diagnostics.

   Run the complete relevant test suites and measure warm opening, cold
   viewer startup, and feedback acknowledgment latency. Document the local
   installation, headless remote helper requirements, forwarding requirement,
   and direct-SSH scope. Leave performance claims tied to measurements.

   Done when the installed tools complete the live `nlessfun` round trip,
   preserve saved feedback under the tested failures, and pass local workflow
   regression checks.

The starting build step is stage 1. The first user-visible checkpoint is the
read-only local window in stage 2; the first real remote checkpoint is stage 3.
