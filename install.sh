#!/usr/bin/env bash
# Install limpid's dependencies and link it into ${PREFIX:-$HOME/.local}/bin.
set -euo pipefail

root="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
bin_dir="${PREFIX:-$HOME/.local}/bin"

command -v node >/dev/null || { echo "install: node (>= 22.6) is required" >&2; exit 1; }
command -v npm >/dev/null || { echo "install: npm is required" >&2; exit 1; }

cd "$root"
if [ -f package-lock.json ]; then
	npm ci --omit=dev
else
	npm install --omit=dev
fi

mkdir -p "$bin_dir"
ln -sfn "$root/limpid" "$bin_dir/limpid"
echo "linked $bin_dir/limpid -> $root/limpid"

case ":$PATH:" in
	*":$bin_dir:"*) ;;
	*) echo "note: $bin_dir is not on your PATH" ;;
esac

cat <<NEXT

Next steps:
  Claude Code  /plugin install limpid --marketplace tjmisko/limpid
  pi           pi install git:github.com/tjmisko/limpid
  Codex        run  !limpid codex 0  from inside a Codex session
NEXT
