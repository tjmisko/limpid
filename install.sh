#!/usr/bin/env bash
# Install limpid into $PREFIX/bin (default ~/.local/bin).
#
#   ./install.sh                  native viewer, command and helpers
#   LIMPID_WEBKIT=1 ./install.sh  also the WebKit host: HTML pages and Mermaid
#   ./install.sh --dry-run        print what would run, change nothing
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"
webkit="${LIMPID_WEBKIT:-0}"
dry_run=0

for argument in "$@"; do
  case "$argument" in
    --dry-run) dry_run=1 ;;
    -h | --help)
      sed -n '2,6p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "install: unknown option: $argument" >&2
      exit 2
      ;;
  esac
done

case "$webkit" in
  0 | 1) ;;
  *)
    echo "install: LIMPID_WEBKIT must be 0 or 1, not '$webkit'" >&2
    exit 2
    ;;
esac

run() {
  if [ "$dry_run" = 1 ]; then
    printf '+'
    printf ' %q' "$@"
    printf '\n'
    return 0
  fi
  "$@"
}

if [ "$dry_run" = 0 ]; then
  command -v cargo >/dev/null || { echo "install: Rust/Cargo is required" >&2; exit 1; }
  if [ "$webkit" = 1 ]; then
    command -v pkg-config >/dev/null || { echo "install: pkg-config is required for LIMPID_WEBKIT=1" >&2; exit 1; }
    pkg-config --exists gtk4 webkitgtk-6.0 || {
      echo "install: GTK4 and WebKitGTK 6.0 development packages are required for LIMPID_WEBKIT=1" >&2
      exit 1
    }
  fi
fi

for crate in limpid-core limpid-view limpid; do
  run cargo install --path "$root/crates/$crate" --root "$prefix" --locked --force
done

if [ "$webkit" = 1 ]; then
  run cargo install --path "$root/crates/limpid-host" --root "$prefix" --locked --force --features webkit
fi

run "$prefix/bin/limpid-core" grammars sync
if [ "$dry_run" = 1 ]; then
  echo "Dry run: nothing was installed"
else
  echo "Installed limpid in $prefix/bin"
fi
