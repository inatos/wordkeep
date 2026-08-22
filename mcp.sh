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
  echo "  run: (cd \"$WK\" && CARGO_TARGET_DIR=\"$WK/target\" cargo build --release)" >&2
  dev_rebuild="$WK/../dev/rebuild_wordkeep.sh"
  if [[ -x "$dev_rebuild" ]]; then
    echo "  or: $dev_rebuild  # when wordkeep lives under tools/wordkeep in this repo" >&2
  fi
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
  echo "  (cd \"$WK\" && CARGO_TARGET_DIR=\"$WK/target\" cargo build --release)  # then reload MCP" >&2
  dev_rebuild="$WK/../dev/rebuild_wordkeep.sh"
  if [[ -x "$dev_rebuild" ]]; then
    echo "  or: $dev_rebuild" >&2
  fi
  if [[ "${WORDKEEP_AUTO_REBUILD:-}" == "1" ]]; then
    echo "[wordkeep:mcp] WORDKEEP_AUTO_REBUILD=1 — rebuilding into $WK/target ..." >&2
    if ! (cd "$WK" && CARGO_TARGET_DIR="$WK/target" cargo build --release); then
      echo "[wordkeep:mcp] auto-rebuild failed; continuing with stale binary" >&2
    else
      BIN="$(pick_bin)" || {
        echo "[wordkeep:mcp] rebuild succeeded but binary still missing" >&2
        exit 1
      }
    fi
  fi
fi

echo "[wordkeep:mcp] using $BIN" >&2
exec "$BIN" "$@"
