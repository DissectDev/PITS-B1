#!/usr/bin/env bash
# Unified inside-container shard executor for PITS Warm Test Pool.
# Runs completely inside `/ramdisk` (`emptyDir: medium: Memory`), preserving incremental
# `CARGO_TARGET_DIR` and `GOCACHE` across runs while touching zero SAN / NVMe disk blocks.

set -euo pipefail

SHARD="${1:-}"
if [ -z "$SHARD" ]; then
  echo "Usage: runner-verify-shard <rust|go-linux|portal-e2e>" >&2
  exit 2
fi

export HOME="/ramdisk/home"
export TMPDIR="/tmp"
export CARGO_HOME="/ramdisk/cargo-home"
export CARGO_TARGET_DIR="/ramdisk/cargo-target"
export GOCACHE="/ramdisk/go-cache"
mkdir -p "$HOME" "$CARGO_HOME" "$CARGO_TARGET_DIR" "$GOCACHE" /ramdisk/work /ramdisk/reports

# Ensure DevSpace sync helper is always ready in /tmp even after tmpfs reset
if [ ! -x /tmp/devspacehelper ] && [ -x /usr/local/bin/devspacehelper ]; then
  cp -f /usr/local/bin/devspacehelper /tmp/devspacehelper
  chmod 0755 /tmp/devspacehelper
fi

# Point Cargo offline source replacement to baked-in `/opt/pits-cache/cargo-vendor`
cp -f /opt/pits-cache/cargo-config.toml "$CARGO_HOME/config.toml"

START_MS=$(date +%s%3N)
NODE_HOST="$(uname -n)"
KERNEL_REL="$(uname -r)"
LOG_FILE="/ramdisk/reports/${SHARD}.log"
JSON_FILE="/ramdisk/reports/${SHARD}.json"
: > "$LOG_FILE"

finish_json() {
  local exit_code="$1"
  local summary="$2"
  local end_ms
  end_ms=$(date +%s%3N)
  local dur=$((end_ms - START_MS))
  local status="passed"
  if [ "$exit_code" -ne 0 ]; then
    status="failed"
  fi
  jq -n \
    --arg shard "$SHARD" \
    --arg status "$status" \
    --arg node "$NODE_HOST" \
    --arg kernel "$KERNEL_REL" \
    --arg summary "$summary" \
    --arg logTail "$(tail -n 50 "$LOG_FILE" 2>/dev/null || true)" \
    --argjson exitCode "$exit_code" \
    --argjson durationMs "$dur" \
    '{
      shard: $shard,
      status: $status,
      exitCode: $exitCode,
      node: $node,
      kernel: $kernel,
      durationMs: $durationMs,
      summary: $summary,
      logTail: $logTail
    }' > "$JSON_FILE"
  cat "$JSON_FILE"
  exit "$exit_code"
}

run_rust() {
  echo "[rust] Running cargo nextest --offline & cargo check --all-targets on $NODE_HOST ($KERNEL_REL)..." &&
  cd /ramdisk/work/client &&
  cargo nextest run --offline --frozen --no-fail-fast &&
  cargo check --offline --frozen --all-targets
}

run_go_linux() {
  echo "[go-linux] Running CGO_ENABLED=1 go test -mod=vendor -race ./... on $NODE_HOST ($KERNEL_REL)..." &&
  cd /ramdisk/work/server &&
  ( [ -d vendor ] || ln -sfn /opt/pits-cache/go-vendor vendor ) &&
  export CGO_ENABLED=1 GOPROXY=off GOSUMDB=off &&
  go vet -mod=vendor ./... &&
  go test -mod=vendor -race -count=1 -v ./...
}

run_portal_e2e() {
  echo "[portal-e2e] Building Backstage v1.56 Portal (tsc + vite) + pitsctl serve + Headless Chromium E2E..." &&
  cd /ramdisk/work/portal &&
  if [ -L node_modules ] || [ ! -d node_modules/.bin ]; then
    rm -rf node_modules
    if [ -d /ramdisk/portal-node-modules/.bin ]; then
      mv /ramdisk/portal-node-modules node_modules
    else
      STORE_NM="$(dirname "$(dirname "$(readlink -f /opt/pits-cache/portal-node-modules/vite/package.json)")")"
      cp -a "$STORE_NM" node_modules
      chmod -R u+w node_modules
    fi
  fi &&
  ./node_modules/.bin/tsc --noEmit &&
  ./node_modules/.bin/vite build &&
  cd /ramdisk/work/client &&
  cargo nextest run --offline --frozen server::tests &&
  cargo build --offline --frozen &&
  {
    local portal_port=18090
    local ready=0
    "$CARGO_TARGET_DIR/debug/pitsctl" serve --listen "127.0.0.1:${portal_port}" >/tmp/pitsctl-serve.log 2>&1 &
    local serve_pid=$!
    local e2e_rc=0
    for _ in $(seq 1 80); do
      if curl -sf "http://127.0.0.1:${portal_port}/api/pits/v1/resource-types" >/dev/null 2>&1; then
        ready=1
        break
      fi
      sleep 0.1
    done
    if [ "$ready" -ne 1 ]; then
      echo "pitsctl serve failed to become ready on :${portal_port}:" >&2
      cat /tmp/pitsctl-serve.log >&2 || true
      kill -9 "$serve_pid" >/dev/null 2>&1 || true
      return 1
    fi
    PITS_PORTAL_URL="http://127.0.0.1:${portal_port}" node /usr/local/bin/portal-headless-e2e.mjs | tee /tmp/e2e-result.json || e2e_rc=$?
    kill -9 "$serve_pid" >/dev/null 2>&1 || true
    return "$e2e_rc"
  }
}

rc=0
case "$SHARD" in
  rust)
    run_rust >"$LOG_FILE" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ]; then
      finish_json "$rc" "Rust cargo-nextest or cargo check failed"
    fi
    PASSED_LINE="$( (grep -E 'Summary \[' "$LOG_FILE" || true) | tail -n 1 | sed 's/^[[:space:]]*//' )"
    [ -n "$PASSED_LINE" ] || PASSED_LINE="cargo nextest & cargo check passed"
    finish_json 0 "$PASSED_LINE"
    ;;

  go-linux)
    run_go_linux >"$LOG_FILE" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ]; then
      finish_json "$rc" "Linux Go vet or go test -race failed"
    fi
    PKG_COUNT="$(grep -c '^ok  ' "$LOG_FILE" || true)"
    TEST_COUNT="$(grep -c '^--- PASS:' "$LOG_FILE" || true)"
    finish_json 0 "go test -race passed (${PKG_COUNT} packages, ${TEST_COUNT} tests on Linux ${KERNEL_REL})"
    ;;

  portal-e2e)
    run_portal_e2e >"$LOG_FILE" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ]; then
      finish_json "$rc" "Portal tsc/vite build, pitsctl serve, or Headless Chromium E2E failed"
    fi
    E2E_SUMMARY="$(tail -n 1 /tmp/e2e-result.json 2>/dev/null || echo 'tsc + vite + pitsctl + chromium passed')"
    finish_json 0 "Portal build + Headless Chromium E2E passed: ${E2E_SUMMARY}"
    ;;

  *)
    echo "Unknown shard: $SHARD" >&2
    exit 2
    ;;
esac
