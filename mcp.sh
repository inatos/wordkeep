#!/usr/bin/env bash
# MCP launcher: use the release binary built in this repo's target/ directory.
set -euo pipefail
WK="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="$WK/target"
BIN="$WK/target/release/wordkeep"
if [ ! -x "$BIN" ]; then
  echo "[wordkeep:mcp] missing $BIN - run: cargo build --release" >&2
  exit 1
fi
exec "$BIN" "$@"
