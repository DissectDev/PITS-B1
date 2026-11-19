<#
.SYNOPSIS
  Mirrors all OCI images required by the PITS Lab CI/CD & Crossplane stack into
  `talos-image-cache` (`http://10.89.20.14:3172`) on `pits-core` (`100.95.95.88`).
#>
[CmdletBinding()]
param(
    [string]$SshTarget = "root@100.95.95.88",
    [switch]$SkipRunnerBuild
)

$ErrorActionPreference = "Stop"
$CicdRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$RepoRoot = (Resolve-Path (Join-Path $CicdRoot "..")).Path
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

if (-not $SkipRunnerBuild) {
    Write-Host "[1/2] Building & publishing docker.io/pits/cicd-runner:v1.1..." -ForegroundColor Cyan
    & (Join-Path $CicdRoot "image\build-cicd-runner.ps1") -SshTarget $SshTarget -RepoRoot $RepoRoot
    if ($LASTEXITCODE -ne 0) { throw "build-cicd-runner.ps1 failed" }
}

Write-Host "[2/2] Mirroring upstream Flux / Crossplane / CNPG / Zot / Gitea / Attic images into talos-image-cache..." -ForegroundColor Cyan

$importPy = [System.IO.File]::ReadAllText((Join-Path $RepoRoot "test-pool\image\import-to-talos-cache.py")).Replace("`r`n", "`n")
$importPyB64 = [Convert]::ToBase64String($Utf8NoBom.GetBytes($importPy))

$remoteScript = @"
set -euo pipefail
CACHE_ROOT="/var/lib/pits/tinkerbell-20261108/image-cache-v1.14.5"
WORK_DIR="/var/lib/pits/cicd-image-mirror"
mkdir -p "`$WORK_DIR"
echo '$importPyB64' | base64 -d > "`$WORK_DIR/import-to-talos-cache.py"

OCI_TOOLS=`$(nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty --no-link --print-out-paths \
  --option substituters https://mirrors.ustc.edu.cn/nix-channels/store \
  --expr 'let p = (builtins.getFlake "path:/home/libraskywalker/PITS-B1/Base").nixosConfigurations.core.pkgs; in p.symlinkJoin { name = "oci-tools"; paths = [ p.skopeo p.python3 ]; }')
SKOPEO="`$OCI_TOOLS/bin/skopeo"
PYTHON3="`$OCI_TOOLS/bin/python3"

mirror_img() {
  local src="`$1"
  local cache_ref="`$2"
  local repo_part="`${cache_ref%:*}"
  local tag_part="`${cache_ref##*:}"
  if [ -f "`$CACHE_ROOT/manifests/`${repo_part}/reference/`${tag_part}" ]; then
    echo "[skip] `$cache_ref already cached"
    return 0
  fi
  echo "[pull] `$src -> `$cache_ref"
  local tmp_oci="`$WORK_DIR/oci-tmp"
  rm -rf "`$tmp_oci"
  local pulled=0
  # Try fast CN mirror first for ghcr.io images
  if [[ "`$src" == ghcr.io/* ]]; then
    local mirror_src="ghcr.m.daocloud.io/`${src#ghcr.io/}"
    if "`$SKOPEO" copy --insecure-policy --override-os linux --override-arch amd64 "docker://`$mirror_src" "oci:`$tmp_oci:`$tag_part" >/dev/null 2>&1; then
      pulled=1
    fi
  fi
  if [ "`$pulled" -eq 0 ] && ss -ltn 2>/dev/null | grep -q ':17897 '; then
    rm -rf "`$tmp_oci"
    if HTTPS_PROXY=http://127.0.0.1:17897 HTTP_PROXY=http://127.0.0.1:17897 https_proxy=http://127.0.0.1:17897 http_proxy=http://127.0.0.1:17897 \
      "`$SKOPEO" copy --insecure-policy --override-os linux --override-arch amd64 "docker://`$src" "oci:`$tmp_oci:`$tag_part" >/dev/null 2>&1; then
      pulled=1
    fi
  fi
  if [ "`$pulled" -eq 0 ]; then
    rm -rf "`$tmp_oci"
    if ss -ltn 2>/dev/null | grep -q ':7890 ' && \
      HTTPS_PROXY=http://127.0.0.1:7890 HTTP_PROXY=http://127.0.0.1:7890 https_proxy=http://127.0.0.1:7890 http_proxy=http://127.0.0.1:7890 \
      "`$SKOPEO" copy --insecure-policy --override-os linux --override-arch amd64 "docker://`$src" "oci:`$tmp_oci:`$tag_part" >/dev/null 2>&1; then
      pulled=1
    fi
  fi
  if [ "`$pulled" -eq 0 ]; then
    rm -rf "`$tmp_oci"
    "`$SKOPEO" copy --insecure-policy --override-os linux --override-arch amd64 "docker://`$src" "oci:`$tmp_oci:`$tag_part" >/dev/null
  fi
  "`$PYTHON3" "`$WORK_DIR/import-to-talos-cache.py" "`$tmp_oci" "`$CACHE_ROOT" "`$cache_ref"
  rm -rf "`$tmp_oci"
}

# Flux v2.10.0 controllers
mirror_img "ghcr.io/fluxcd/source-controller:v1.10.1"       "ghcr.io/fluxcd/source-controller:v1.10.1"
mirror_img "ghcr.io/fluxcd/kustomize-controller:v1.10.1"    "ghcr.io/fluxcd/kustomize-controller:v1.10.1"
mirror_img "ghcr.io/fluxcd/helm-controller:v1.7.1"          "ghcr.io/fluxcd/helm-controller:v1.7.1"
mirror_img "ghcr.io/fluxcd/notification-controller:v1.10.0" "ghcr.io/fluxcd/notification-controller:v1.10.0"

# Crossplane v2.4.3
mirror_img "xpkg.crossplane.io/crossplane/crossplane:v2.4.3" "ghcr.io/crossplane/crossplane:v2.4.3"

# CloudNativePG 1.30.1 + PostgreSQL 17.6
mirror_img "ghcr.io/cloudnative-pg/cloudnative-pg:1.30.1"   "ghcr.io/cloudnative-pg/cloudnative-pg:1.30.1"
mirror_img "ghcr.io/cloudnative-pg/postgresql:17.6"         "ghcr.io/cloudnative-pg/postgresql:17.6"

# Zot v2.1.23 + alpine helper
mirror_img "ghcr.io/project-zot/zot:v2.1.23"                "ghcr.io/project-zot/zot:v2.1.23"
mirror_img "docker.m.daocloud.io/library/alpine:3.18"       "docker.io/library/alpine:3.18"

# Gitea 1.27.0-rootless + busybox init
mirror_img "docker.gitea.com/gitea:1.27.0-rootless"         "docker.io/gitea/gitea:1.27.0-rootless"
mirror_img "docker.m.daocloud.io/library/busybox:1.36.1"    "docker.io/library/busybox:1.36.1"
mirror_img "docker.m.daocloud.io/library/busybox:1.36.1"    "docker.io/library/busybox:latest"

# Attic Nix binary cache
mirror_img "ghcr.io/zhaofengli/attic:latest"                "ghcr.io/zhaofengli/attic:latest"

# Istio 1.30.8 Ambient (pilot, cni, ztunnel, proxyv2) + oauth2-proxy v7.15.6
mirror_img "registry.istio.io/release/pilot:1.30.8-distroless"       "docker.io/istio/pilot:1.30.8-distroless"
mirror_img "registry.istio.io/release/install-cni:1.30.8-distroless" "docker.io/istio/install-cni:1.30.8-distroless"
mirror_img "registry.istio.io/release/ztunnel:1.30.8-distroless"     "docker.io/istio/ztunnel:1.30.8-distroless"
mirror_img "registry.istio.io/release/proxyv2:1.30.8-distroless"     "docker.io/istio/proxyv2:1.30.8-distroless"
mirror_img "quay.io/oauth2-proxy/oauth2-proxy:v7.15.6"               "ghcr.io/oauth2-proxy/oauth2-proxy:v7.15.6"

# cert-manager v1.21.3 (Let's Encrypt DNS-01) + Tailscale v1.104.7 (Split DNS & Subnet Router)
mirror_img "quay.io/jetstack/cert-manager-controller:v1.21.3"        "ghcr.io/jetstack/cert-manager-controller:v1.21.3"
mirror_img "quay.io/jetstack/cert-manager-webhook:v1.21.3"           "ghcr.io/jetstack/cert-manager-webhook:v1.21.3"
mirror_img "quay.io/jetstack/cert-manager-cainjector:v1.21.3"        "ghcr.io/jetstack/cert-manager-cainjector:v1.21.3"
mirror_img "quay.io/jetstack/cert-manager-startupapicheck:v1.21.3"   "ghcr.io/jetstack/cert-manager-startupapicheck:v1.21.3"
mirror_img "docker.m.daocloud.io/tailscale/tailscale:v1.104.7"          "docker.io/tailscale/tailscale:v1.104.7"
mirror_img "docker.m.daocloud.io/library/nginx:1.29-alpine"             "docker.io/library/nginx:1.29-alpine"

# AI Storage Runtime dependencies (RustFS S3 + Valkey KV/CAS)
mirror_img "docker.m.daocloud.io/rustfs/rustfs:latest"                  "docker.io/rustfs/rustfs:latest"
mirror_img "docker.m.daocloud.io/valkey/valkey:8-alpine"                "docker.io/valkey/valkey:8-alpine"

echo "All CICD OCI images cached in `$CACHE_ROOT."
"@

$clean = $remoteScript.Replace("`r`n", "`n") + "`n"
$b64Script = [Convert]::ToBase64String($Utf8NoBom.GetBytes($clean))
$tmpScript = Join-Path $env:TEMP "pits-import-cicd-images.sh"
[System.IO.File]::WriteAllBytes($tmpScript, $Utf8NoBom.GetBytes($clean))
try {
    $remoteSh = "/tmp/pits-import-cicd-images.sh"
    & scp -B -o ConnectTimeout=15 $tmpScript "${SshTarget}:${remoteSh}"
    if ($LASTEXITCODE -ne 0) { throw "SCP upload of image mirror script failed" }
    $sshArgs = @("-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15")
    if (Test-NetConnection -ComputerName 127.0.0.1 -Port 7897 -InformationLevel Quiet -WarningAction SilentlyContinue) {
        $sshArgs += @("-R", "17897:127.0.0.1:7897")
    }
    $sshArgs += @($SshTarget, "bash '$remoteSh'; rc=`$?; rm -f '$remoteSh'; exit `$rc")
    & ssh @sshArgs
    if ($LASTEXITCODE -ne 0) { throw "Image mirror script failed (exit $LASTEXITCODE)" }
} finally {
    Remove-Item -Force $tmpScript -ErrorAction SilentlyContinue
}
Write-Host "Image mirror completed successfully!" -ForegroundColor Green
