#!/usr/bin/env bash
# MCP launcher: prefer the newest wordkeep binary under this repo's target/.
set -euo pipefail
WK="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="$WK/target"

# shellcheck source=ereshkigal_gpu.sh
source "$WK/ereshkigal_gpu.sh"
wordkeep_nvidia_icd

wordkeep_features() {
  # Vulkan llama.cpp by default so n_gpu_layers offloads. CPU-only:
  # WORDKEEP_FEATURES=embeddings,daslang,dashboard,ereshkigal
  wordkeep_default_features
}

pick_bin() {
  # Prefer release: cargo test / default debug builds omit --features ereshkigal
  # and win a newest-mtime race against an older GGUF-linked release binary.
  local release="$WK/target/release/wordkeep"
  local debug="$WK/target/debug/wordkeep"
  if [[ -x "$release" ]]; then
    printf '%s\n' "$release"
    return 0
  fi
  if [[ -x "$debug" ]]; then
    printf '%s\n' "$debug"
    return 0
  fi
  return 1
}

BIN="$(pick_bin)" || {
  echo "[wordkeep:mcp] missing wordkeep binary." >&2
  echo "  run: (cd \"$WK\" && CARGO_TARGET_DIR=\"$WK/target\" cargo build --release --features embeddings,daslang,dashboard,ereshkigal-vulkan)" >&2
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
  echo "  (cd \"$WK\" && CARGO_TARGET_DIR=\"$WK/target\" cargo build --release --features embeddings,daslang,dashboard,ereshkigal-vulkan)  # then reload MCP" >&2
  dev_rebuild="$WK/../dev/rebuild_wordkeep.sh"
  if [[ -x "$dev_rebuild" ]]; then
    echo "  or: $dev_rebuild" >&2
  fi
  if [[ "${WORDKEEP_AUTO_REBUILD:-}" == "1" ]]; then
    echo "[wordkeep:mcp] WORDKEEP_AUTO_REBUILD=1 — rebuilding into $WK/target ..." >&2
    wordkeep_vulkan_cmake_env || true
    wordkeep_wipe_cpu_llama_sys
    if ! (cd "$WK" && CARGO_TARGET_DIR="$WK/target" cargo build --release --features "$(wordkeep_features)"); then
      echo "[wordkeep:mcp] auto-rebuild failed; continuing with stale binary" >&2
    else
      BIN="$(pick_bin)" || {
        echo "[wordkeep:mcp] rebuild succeeded but binary still missing" >&2
        exit 1
      }
    fi
  fi
fi

if [[ "${WORDKEEP_AUTO_REBUILD:-}" == "1" ]]; then
  case ",$(wordkeep_features)," in
    *,ereshkigal-vulkan,*)
      if wordkeep_bin_missing_vulkan "$BIN"; then
        echo "[wordkeep:mcp] release binary has no ggml-vulkan — rebuilding GPU llama.cpp …" >&2
        wordkeep_vulkan_cmake_env || true
        wordkeep_wipe_cpu_llama_sys
        if (cd "$WK" && CARGO_TARGET_DIR="$WK/target" cargo build --release --features "$(wordkeep_features)"); then
          BIN="$(pick_bin)" || true
        fi
      fi
      ;;
  esac
fi

echo "[wordkeep:mcp] using $BIN" >&2
echo "[wordkeep:mcp] surface: 51 tools, 2 resources (wordkeep://readme, wordkeep://capabilities)" >&2
ROOT="$(cd "$WK/../.." && pwd)"
GGUF_PIN="$ROOT/.wordkeep/models/Qwen3-0.6B-Q8_0.gguf"
if [[ ! -e "$GGUF_PIN" ]]; then
  echo "[wordkeep:mcp] warning: $GGUF_PIN is missing — ereshkigal decide/rerank will error until the 0.6B Q8 pin is present (gitignored)." >&2
fi
CFG="$ROOT/.wordkeep/config.json"
if [[ -f "$CFG" ]] && command -v python3 >/dev/null 2>&1; then
  VERIFY_REL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("semif",{}).get("gguf_verify") or "")' "$CFG" 2>/dev/null || true)"
  if [[ -n "${VERIFY_REL:-}" ]]; then
    if [[ "$VERIFY_REL" = /* ]]; then
      VERIFY_ABS="$VERIFY_REL"
    else
      VERIFY_ABS="$ROOT/$VERIFY_REL"
    fi
    if [[ ! -e "$VERIFY_ABS" ]]; then
      echo "[wordkeep:mcp] warning: gguf_verify pin missing: $VERIFY_ABS — cascade-verify will skip until the 4B file is present." >&2
    fi
  fi
fi
exec "$BIN" "$@"
