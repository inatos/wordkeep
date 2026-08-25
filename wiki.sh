#!/usr/bin/env bash
# Idempotent wiki/dashboard launcher for Cursor folder-open tasks.
# Starts Meilisearch (compose) + wordkeep-wiki serve --watch --runtime-attach on :8787.
# Opt out of attach: WIKI_RUNTIME_ATTACH=0 ./wiki.sh
set -euo pipefail

WK="$(cd "$(dirname "$0")" && pwd)"
# Prefer explicit root (other repos / multi-workspace). Default: monorepo parent of tools/wordkeep.
if [[ -n "${WORDKEEP_ROOT:-}" ]]; then
  ROOT="$(cd "$WORDKEEP_ROOT" && pwd)"
else
  ROOT="$(cd "$WK/../.." && pwd)"
fi
export CARGO_TARGET_DIR="$WK/target"

BIND="${WIKI_BIND:-127.0.0.1:8787}"
PORT="${BIND##*:}"
STATE_DIR="${WORDKEEP_WIKI_STATE:-$ROOT/.wordkeep/wiki-runtime}"
LOG="$STATE_DIR/wiki.log"
PIDFILE="$STATE_DIR/wiki.pid"
# Cursor local wiki defaults attach on so Health → Runtime can peek / ECS-apply.
RUNTIME_ATTACH=1
case "${WIKI_RUNTIME_ATTACH:-1}" in
  0|false|FALSE|no|NO|off|OFF) RUNTIME_ATTACH=0 ;;
esac
FORCE=0
if [[ "${1:-}" == "--restart" || "${1:-}" == "-f" ]]; then
  FORCE=1
fi

mkdir -p "$STATE_DIR"

pick_bin() {
  local release="$WK/target/release/wordkeep-wiki"
  local debug="$WK/target/debug/wordkeep-wiki"
  if [[ -x "$release" ]]; then
    printf '%s\n' "$release"
  elif [[ -x "$debug" ]]; then
    printf '%s\n' "$debug"
  else
    return 1
  fi
}

http_code() {
  curl -s -o /dev/null -w '%{http_code}' --max-time 1 "http://${BIND}$1" 2>/dev/null || printf '000'
}

ui_up() {
  local code
  code="$(http_code /)"
  [[ "$code" == "200" || "$code" == "304" ]]
}

dashboard_ok() {
  # /api/dashboard is required for the GUI telemetry tab; older binaries 404 it.
  [[ "$(http_code /api/dashboard)" == "200" ]]
}

runtime_ok() {
  # Health → Runtime needs the census API. Older binaries 404 it.
  [[ "$(http_code /api/runtime)" == "200" ]]
}

runtime_attach_ok() {
  [[ "$RUNTIME_ATTACH" -eq 0 ]] && return 0
  local body
  body="$(curl -s --max-time 1 "http://${BIND}/api/runtime" 2>/dev/null || true)"
  [[ "$body" == *'"attach_enabled":true'* || "$body" == *'"attach_enabled": true'* ]]
}

ui_has_runtime_view() {
  # Built SPA must include the Runtime/Knowledge Health subview.
  grep -q 'Health — Runtime' "$WK/wiki/dist"/assets/*.js 2>/dev/null \
    || grep -q 'attach_enabled' "$WK/wiki/dist"/assets/*.js 2>/dev/null
}

ui_has_local_base() {
  # Local host serve must use Vite base `/` (lab Docker sets /lab/wordkeep/ui/).
  local index="$WK/wiki/dist/index.html"
  [[ -f "$index" ]] || return 1
  ! grep -q '/lab/wordkeep/ui' "$index"
}

rebuild_ui_dist() {
  if ! command -v bun >/dev/null 2>&1; then
    echo "[wordkeep:wiki] wiki/dist needs rebuild and bun is missing" >&2
    echo "  run: (cd \"$WK/wiki\" && VITE_WIKI_BASE=/ bun run build)" >&2
    return 1
  fi
  echo "[wordkeep:wiki] rebuilding wiki/dist with VITE_WIKI_BASE=/ …"
  (cd "$WK/wiki" && VITE_WIKI_BASE=/ bun run build)
}

ensure_ui_dist() {
  if ui_has_runtime_view && ui_has_local_base; then
    return 0
  fi
  if ! ui_has_local_base && [[ -f "$WK/wiki/dist/index.html" ]]; then
    echo "[wordkeep:wiki] wiki/dist has lab base (/lab/wordkeep/ui) — forcing local /"
  elif ! ui_has_runtime_view; then
    echo "[wordkeep:wiki] wiki/dist is stale (no Runtime Health UI)"
  fi
  rebuild_ui_dist || return 1
  ui_has_runtime_view && ui_has_local_base
}

healthy() {
  ui_up && dashboard_ok && runtime_ok && runtime_attach_ok && ui_has_runtime_view && ui_has_local_base
}

alive_pid() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

stop_existing() {
  local pid=""
  if [[ -f "$PIDFILE" ]]; then
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
  fi
  if alive_pid "$pid"; then
    echo "[wordkeep:wiki] stopping pid $pid"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 20); do
      alive_pid "$pid" || break
      sleep 0.1
    done
    if alive_pid "$pid"; then
      kill -9 "$pid" 2>/dev/null || true
    fi
  fi
  # Fall back: anything still bound to the wiki port.
  if command -v ss >/dev/null 2>&1 && ss -ltnp 2>/dev/null | grep -q ":${PORT}"; then
    pkill -f "wordkeep-wiki.*serve.*${PORT}|wordkeep-wiki.*--bind ${BIND}" 2>/dev/null || true
    sleep 0.3
  fi
  rm -f "$PIDFILE"
}

if [[ "$FORCE" -eq 1 ]]; then
  stop_existing
elif healthy; then
  echo "[wordkeep:wiki] already serving http://${BIND}"
  exit 0
elif ui_up; then
  echo "[wordkeep:wiki] UI up but dashboard/runtime API stale — restarting"
  stop_existing
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "[wordkeep:wiki] docker not found; Meilisearch won't start" >&2
  exit 1
fi

echo "[wordkeep:wiki] ensuring Meilisearch (compose)…"
docker compose -f "$WK/docker-compose.wiki.yml" up -d

BIN="$(pick_bin)" || {
  echo "[wordkeep:wiki] missing wordkeep-wiki binary." >&2
  echo "  run: (cd \"$WK\" && cargo build -p wordkeep-wiki --release)" >&2
  exit 1
}

if ! ensure_ui_dist; then
  exit 1
fi

if [[ "$RUNTIME_ATTACH" -eq 1 ]] && ! "$BIN" serve --help 2>&1 | grep -q -- '--runtime-attach'; then
  echo "[wordkeep:wiki] $BIN has no --runtime-attach (rebuild required)." >&2
  echo "  run: (cd \"$WK\" && cargo build -p wordkeep-wiki --release) && \"$WK/wiki.sh\" --restart" >&2
  exit 1
fi

if healthy; then
  echo "[wordkeep:wiki] already serving http://${BIND}"
  exit 0
fi

if ui_up; then
  echo "[wordkeep:wiki] ${BIND} still occupied after stop; see $LOG" >&2
  exit 1
fi

ATTACH_ARGS=()
if [[ "$RUNTIME_ATTACH" -eq 1 ]]; then
  ATTACH_ARGS+=(--runtime-attach)
fi
echo "[wordkeep:wiki] starting $BIN serve --watch ${ATTACH_ARGS[*]:-} --root $ROOT"
# Detach from the launcher/task process group so Cursor reload doesn't kill serve.
setsid "$BIN" --root "$ROOT" serve --bind "$BIND" --watch "${ATTACH_ARGS[@]}" \
  >>"$LOG" 2>&1 </dev/null &
echo $! >"$PIDFILE"

for _ in $(seq 1 40); do
  if healthy; then
    echo "[wordkeep:wiki] UI: http://${BIND}"
    exit 0
  fi
  if ! alive_pid "$(cat "$PIDFILE")"; then
    echo "[wordkeep:wiki] process exited early; see $LOG" >&2
    exit 1
  fi
  sleep 0.25
done

echo "[wordkeep:wiki] started (pid $(cat "$PIDFILE")) but health check timed out; see $LOG" >&2
exit 1
