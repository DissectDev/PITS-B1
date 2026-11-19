#!/usr/bin/env bash
set -euo pipefail

mkdir -p /tmp /ramdisk/home /ramdisk/work /ramdisk/cargo-home /ramdisk/cargo-target \
         /ramdisk/go-cache /ramdisk/act-runner/work /ramdisk/actions-cache /ramdisk/reports /ramdisk/npm-cache
chmod 1777 /tmp 2>/dev/null || true

export HOME="/ramdisk/home"
export CARGO_HOME="/ramdisk/cargo-home"
export CARGO_TARGET_DIR="/ramdisk/cargo-target"
export GOCACHE="/ramdisk/go-cache"
export npm_config_cache="/ramdisk/npm-cache"

cp -f /usr/local/bin/devspacehelper /tmp/devspacehelper
chmod 0755 /tmp/devspacehelper
cp -f /opt/pits-cache/cargo-config.toml "$CARGO_HOME/config.toml"

if [ ! -d /ramdisk/portal-node-modules/.bin ] && [ -e /opt/pits-cache/portal-node-modules/vite/package.json ]; then
  (
    STORE_NM="$(dirname "$(dirname "$(readlink -f /opt/pits-cache/portal-node-modules/vite/package.json)")")"
    cp -a "$STORE_NM" /ramdisk/portal-node-modules.tmp &&
    chmod -R u+w /ramdisk/portal-node-modules.tmp &&
    mv /ramdisk/portal-node-modules.tmp /ramdisk/portal-node-modules
  ) >/dev/null 2>&1 &
fi

INSTANCE_URL="${GITEA_INSTANCE_URL:?GITEA_INSTANCE_URL is required}"
REG_TOKEN="${GITEA_RUNNER_REGISTRATION_TOKEN:?GITEA_RUNNER_REGISTRATION_TOKEN is required}"
RUNNER_NAME="${GITEA_RUNNER_NAME:-$(hostname)}"
RUNNER_LABELS="${GITEA_RUNNER_LABELS:-ubuntu-latest:host}"
CFG_FILE="/ramdisk/act-runner/config.yaml"
RUNNER_FILE="/ramdisk/act-runner/.runner"

cat > "$CFG_FILE" <<YAML
log:
  level: info
runner:
  file: ${RUNNER_FILE}
  capacity: 2
  timeout: 1h
  insecure: true
  fetch_timeout: 10s
  fetch_interval: 2s
  labels:
YAML
IFS=',' read -ra LBLS <<< "$RUNNER_LABELS"
for lbl in "${LBLS[@]}"; do
  echo "    - \"${lbl}\"" >> "$CFG_FILE"
done
cat >> "$CFG_FILE" <<YAML
cache:
  enabled: true
  dir: /ramdisk/actions-cache
host:
  workdir_parent: /ramdisk/act-runner/work
YAML

echo "[cicd-runner] Waiting for Gitea at ${INSTANCE_URL}..."
for _ in $(seq 1 120); do
  if curl -sf "${INSTANCE_URL}/api/v1/version" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
curl -sf "${INSTANCE_URL}/api/v1/version" >/dev/null

echo "[cicd-runner] Registering runner ${RUNNER_NAME} with labels: ${RUNNER_LABELS}"
rm -f "$RUNNER_FILE"
/usr/local/bin/act_runner register --no-interactive \
  --instance "$INSTANCE_URL" \
  --token "$REG_TOKEN" \
  --name "$RUNNER_NAME" \
  --labels "$RUNNER_LABELS" \
  --config "$CFG_FILE"

echo "[cicd-runner] Starting act_runner daemon..."
exec /usr/local/bin/act_runner daemon --config "$CFG_FILE"
