#!/usr/bin/env bash
# Cross round trip with the TypeScript engine of XkyHold:
#   1. the Rust engine writes a format 2 repository;
#   2. the TypeScript engine reads its bytes and writes a format 1 repository;
#   3. the Rust engine restores that one.
# Usage: scripts/cross-ts.sh <path to an XkyHold checkout with node_modules>
# The XkyHold checkout is not touched: a throwaway worktree is used.
set -euo pipefail
xkyhold=$(cd "${1:?path to an XkyHold checkout}" && pwd)
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'git -C "$xkyhold" worktree remove --force "$work/xkyhold" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
export CROSS_DIR="$work"

(cd "$here" && cargo test -q -p xkyhold-core --test cross_ts -- --ignored rust_writes --exact)
git -C "$xkyhold" worktree add --detach -q "$work/xkyhold" HEAD
ln -s "$xkyhold/node_modules" "$work/xkyhold/node_modules"
cp "$here/scripts/cross-ts/cross-lx.test.ts" "$work/xkyhold/src/engine/"
(cd "$work/xkyhold" && npx vitest run src/engine/cross-lx.test.ts)
(cd "$here" && cargo test -q -p xkyhold-core --test cross_ts -- --ignored rust_reads --exact)
echo "Cross round trip with the TypeScript engine: OK"
