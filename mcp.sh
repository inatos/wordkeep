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
  ADAPTER_REL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("semif",{}).get("adapter") or "")' "$CFG" 2>/dev/null || true)"
  if [[ -n "${ADAPTER_REL:-}" ]]; then
    if [[ "$ADAPTER_REL" = /* ]]; then
      ADAPTER_ABS="$ADAPTER_REL"
    else
      ADAPTER_ABS="$ROOT/$ADAPTER_REL"
    fi
    if [[ ! -e "$ADAPTER_ABS" ]]; then
      echo "[wordkeep:mcp] warning: semif.adapter pin missing: $ADAPTER_ABS — draft loads without LoRA until the adapter GGUF is present." >&2
    fi
  fi
fi

# Reap rebuilt-but-still-running MCP binaries (Cursor often leaves (deleted) exe
# maps holding multi-GiB Vulkan KV after cargo rebuild).
wordkeep_reap_stale_bins() {
  local root="$1" pid exe cmd
  for pid in $(pgrep -x wordkeep 2>/dev/null || true); do
    [[ "$pid" == "$$" ]] && continue
    exe=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
    [[ "$exe" == *"(deleted)"* ]] || continue
    cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)
    [[ "$cmd" == *"--root ${root}"* || "$cmd" == *"--root ${root} "* || "$cmd" == *" ${root}"* ]] || continue
    echo "[wordkeep:mcp] reaping stale deleted-binary wordkeep pid=$pid" >&2
    kill "$pid" 2>/dev/null || true
  done
}

# One Vulkan SemIf draft per workspace: a second Cursor window/MCP used to each
# prefetch n_gpu_layers=99 + n_seq_max=32 (~2–4 GiB VRAM) and starve the 1080 Ti
# (Spectacle EGL_BAD_CONTEXT, game OOMs). Loser stays off GGUF (heuristic) by default.
wordkeep_acquire_semif_gpu_lease() {
  local root="$1"
  local lockdir="$root/.wordkeep"
  local lockfile="$lockdir/semif-gpu.lock"
  mkdir -p "$lockdir"
  # FD 9 must survive exec so the lease stays held for the MCP lifetime.
  exec 9>"$lockfile" || {
    echo "[wordkeep:mcp] warning: could not open $lockfile — SemIf GPU lease skipped" >&2
    return 0
  }
  if flock -n 9; then
    echo $$ >"$lockdir/semif-gpu.pid" 2>/dev/null || true
    echo "[wordkeep:mcp] SemIf GPU lease acquired (pid $$)" >&2
  else
    local holder
    holder=$(cat "$lockdir/semif-gpu.pid" 2>/dev/null || echo unknown)
    # Vulkan-linked llama.cpp still reserves ~1–2 GiB at n_gpu_layers=0. Keep the
    # loser off GGUF entirely (heuristic SemIf) unless the user opts into CPU GGUF.
    if [[ "${WORDKEEP_SEMIF_ALLOW_CPU_GGUF:-0}" == "1" ]]; then
      echo "[wordkeep:mcp] SemIf GPU lease held by pid ${holder} — WORDKEEP_SEMIF_ALLOW_CPU_GGUF=1; forcing ERESHKIGAL_N_GPU_LAYERS=0" >&2
      export ERESHKIGAL_N_GPU_LAYERS=0
    else
      echo "[wordkeep:mcp] SemIf GPU lease held by pid ${holder} — forcing WORDKEEP_SEMIF_BACKEND=heuristic (no GGUF/VRAM). Set WORDKEEP_SEMIF_ALLOW_CPU_GGUF=1 for CPU GGUF." >&2
      export WORDKEEP_SEMIF_BACKEND=heuristic
    fi
    exec 9>&-
  fi
}

wordkeep_reap_stale_bins "$ROOT"
# Brief settle so a reaped holder's flock can release before we contend.
sleep 0.2
wordkeep_acquire_semif_gpu_lease "$ROOT"

exec "$BIN" "$@"
