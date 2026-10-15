#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"

command -v cargo >/dev/null || { echo "install: Rust/Cargo is required" >&2; exit 1; }
command -v pkg-config >/dev/null || { echo "install: pkg-config is required" >&2; exit 1; }
pkg-config --exists gtk4 webkitgtk-6.0 || {
  echo "install: GTK4 and WebKitGTK 6.0 development packages are required" >&2
  exit 1
}

for crate in limpid-core limpid-host limpid; do
  cargo install --path "$root/crates/$crate" --root "$prefix" --locked --force
done

"$prefix/bin/limpid-core" grammars sync
echo "Installed limpid in $prefix/bin"
