#!/usr/bin/env bash
# Idempotent wiki/dashboard launcher for Cursor folder-open tasks.
# Starts Meilisearch (compose) + wordkeep-wiki serve --watch on :8787 if needed.
set -euo pipefail

WK="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$WK/../.." && pwd)"
export CARGO_TARGET_DIR="$WK/target"

BIND="${WIKI_BIND:-127.0.0.1:8787}"
PORT="${BIND##*:}"
STATE_DIR="${WORDKEEP_WIKI_STATE:-$ROOT/.wordkeep/wiki-runtime}"
LOG="$STATE_DIR/wiki.log"
PIDFILE="$STATE_DIR/wiki.pid"

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

already_up() {
  local code
  code="$(http_code /)"
  [[ "$code" == "200" || "$code" == "304" ]]
}

alive_pid() {
  local pid="$1"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

if already_up; then
  echo "[wordkeep:wiki] already serving http://${BIND}"
  exit 0
fi

if [[ -f "$PIDFILE" ]]; then
  oldpid="$(cat "$PIDFILE" 2>/dev/null || true)"
  if alive_pid "$oldpid"; then
    echo "[wordkeep:wiki] waiting for pid $oldpid on ${BIND}…"
    for _ in $(seq 1 20); do
      if already_up; then
        echo "[wordkeep:wiki] already serving http://${BIND}"
        exit 0
      fi
      sleep 0.25
    done
  fi
  rm -f "$PIDFILE"
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

if already_up; then
  echo "[wordkeep:wiki] already serving http://${BIND}"
  exit 0
fi

echo "[wordkeep:wiki] starting $BIN serve --watch --root $ROOT"
# Detach from the launcher/task process group so Cursor reload doesn't kill serve.
setsid "$BIN" --root "$ROOT" serve --bind "$BIND" --watch \
  >>"$LOG" 2>&1 </dev/null &
echo $! >"$PIDFILE"

for _ in $(seq 1 40); do
  if already_up; then
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
