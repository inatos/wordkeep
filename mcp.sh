#!/usr/bin/env bash
# MCP launcher: prefer the newest wordkeep binary under this repo's target/.
set -euo pipefail
WK="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="$WK/target"

pick_bin() {
  local release="$WK/target/release/wordkeep"
  local debug="$WK/target/debug/wordkeep"
  local best=""
  local best_mtime=0
  local cand mtime
  for cand in "$release" "$debug"; do
    if [[ -x "$cand" ]]; then
      mtime=$(stat -c '%Y' "$cand" 2>/dev/null || echo 0)
      if [[ -z "$best" || "$mtime" -gt "$best_mtime" ]]; then
        best="$cand"
        best_mtime="$mtime"
      fi
    fi
  done
  if [[ -z "$best" ]]; then
    return 1
  fi
  printf '%s\n' "$best"
}

BIN="$(pick_bin)" || {
  echo "[wordkeep:mcp] missing wordkeep binary." >&2
  echo "  run: (cd \"$WK\" && cargo build --release)" >&2
  exit 1
}

# Warn when sources look newer than the chosen binary (common after a submodule bump).
NEWEST_SRC=0
if [[ -d "$WK/src" ]]; then
  NEWEST_SRC=$(find "$WK/src" -type f -name '*.rs' -printf '%T@\n' 2>/dev/null \
    | sort -n | tail -1 | cut -d. -f1)
fi
BIN_MTIME=$(stat -c '%Y' "$BIN" 2>/dev/null || echo 0)
if [[ -n "${NEWEST_SRC:-}" && "$NEWEST_SRC" -gt "$BIN_MTIME" ]]; then
  echo "[wordkeep:mcp] warning: $BIN is older than src/; rebuild recommended:" >&2
  echo "  (cd \"$WK\" && cargo build --release)  # then reload MCP in the editor" >&2
fi

echo "[wordkeep:mcp] using $BIN" >&2
exec "$BIN" "$@"
