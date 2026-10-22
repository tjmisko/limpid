Put questions that need the user's answer under a `## Questions` heading as a numbered list; give suggested answers as nested `- ` items.

## Redeploy

`./install.sh` rebuilds and installs `limpid`, `limpid-core`, and
`limpid-host` into `${PREFIX:-$HOME/.local}/bin`. Needs Cargo, `pkg-config`,
and the GTK4/WebKitGTK 6.0 development packages.

A running host keeps the old binary. Save open pages (`:wq`), then
`pkill -x limpid-host`; the next `limpid` open starts a fresh host.
