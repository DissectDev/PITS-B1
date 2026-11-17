<#
.SYNOPSIS
  Builds the reproducible offline `docker.io/pits/test-runner:v1` OCI image natively via Nix
  on `pits-core` (`100.74.25.42`), mirrors `registry.k8s.io/agent-sandbox/agent-sandbox-controller:v1.0.11`,
  and publishes both into `talos-image-cache` (`http://10.89.20.14:3172`) for `cluster-pits-lab`.
#>
[CmdletBinding()]
param(
    [string]$SshTarget = "root@100.74.25.42",
    [string]$FlakeDir = "/home/libraskywalker/PITS-B1/Base",
    [string]$FlakeAttr = "core",
    [string]$ImageTag = "v1.2",
    [string]$DevSpaceVersion = "v6.3.21",
    [string]$AgentSandboxVersion = "v1.0.11",
    [string]$ClientRepoRoot = "c:\Project\PITS\Infra"
)

$ErrorActionPreference = "Stop"
$PoolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Invoke-SshScript([string]$Script) {
    $clean = $Script.Replace("`r`n", "`n") + "`n"
    $bytes = $Utf8NoBom.GetBytes($clean)
    $psi = [System.Diagnostics.ProcessStartInfo]::new("ssh", "-T -o BatchMode=yes -o ConnectTimeout=15 $SshTarget `"head -c $($bytes.Length) | bash -eu`"")
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $proc.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $proc.StandardInput.Close()
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $proc.WaitForExit()
    $stdout = $outTask.GetAwaiter().GetResult()
    $stderr = $errTask.GetAwaiter().GetResult()
    if ($proc.ExitCode -ne 0) {
        throw "SSH script failed (exit $($proc.ExitCode)):`nSTDERR: $stderr`nSTDOUT: $stdout"
    }
    return $stdout
}

function Send-TarGzOverSsh([byte[]]$Bytes, [string]$RemoteDir) {
    $len = $Bytes.Length
    $cmd = "umask 022 && rm -rf '$RemoteDir.new' && mkdir -p '$RemoteDir.new' && head -c $len | tar -xzf - -C '$RemoteDir.new' && rm -rf '$RemoteDir' && mv '$RemoteDir.new' '$RemoteDir'"
    $psi = [System.Diagnostics.ProcessStartInfo]::new("ssh", "-T -o BatchMode=yes -o ConnectTimeout=15 $SshTarget `"$cmd`"")
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $proc.StandardInput.BaseStream.Write($Bytes, 0, $Bytes.Length)
    $proc.StandardInput.Close()
    $null = $proc.StandardOutput.ReadToEndAsync().GetAwaiter().GetResult()
    $stderr = $proc.StandardError.ReadToEndAsync().GetAwaiter().GetResult()
    $proc.WaitForExit()
    if ($proc.ExitCode -ne 0) {
        throw "Tar upload to ${SshTarget}:${RemoteDir} failed: $stderr"
    }
}

Write-Host "[1/4] Staging TestPool builder assets, Go vendor & Portal lockfile..." -ForegroundColor Cyan
$StageDir = Join-Path $env:TEMP "pits-testpool-stage"
$TarPath  = Join-Path $env:TEMP "pits-testpool-stage.tar.gz"
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
if (Test-Path $TarPath)  { Remove-Item -Force $TarPath }

New-Item -ItemType Directory -Force -Path $StageDir, (Join-Path $StageDir "portal-lock") | Out-Null

# Ensure LF line endings on shell / JS / Python / Nix files sent to Linux
foreach ($item in @(
    @{ Src = Join-Path $PoolRoot "image\test-runner-image.nix"; Dst = Join-Path $StageDir "test-runner-image.nix" },
    @{ Src = Join-Path $PoolRoot "image\import-to-talos-cache.py"; Dst = Join-Path $StageDir "import-to-talos-cache.py" },
    @{ Src = Join-Path $PoolRoot "scripts\runner-verify-shard.sh"; Dst = Join-Path $StageDir "runner-verify-shard.sh" },
    @{ Src = Join-Path $PoolRoot "scripts\portal-headless-e2e.mjs"; Dst = Join-Path $StageDir "portal-headless-e2e.mjs" }
)) {
    $txt = [System.IO.File]::ReadAllText($item.Src).Replace("`r`n", "`n")
    [System.IO.File]::WriteAllBytes($item.Dst, $Utf8NoBom.GetBytes($txt))
}

Copy-Item -Recurse -Force (Join-Path $ClientRepoRoot "server\vendor") (Join-Path $StageDir "go-vendor")
Copy-Item -Force (Join-Path $ClientRepoRoot "portal\package.json") (Join-Path $StageDir "portal-lock\package.json")
Copy-Item -Force (Join-Path $ClientRepoRoot "portal\package-lock.json") (Join-Path $StageDir "portal-lock\package-lock.json")
Copy-Item -Force (Join-Path $ClientRepoRoot "client\Cargo.toml") (Join-Path $StageDir "Cargo.toml")
Copy-Item -Force (Join-Path $ClientRepoRoot "client\Cargo.lock") (Join-Path $StageDir "Cargo.lock")

& tar.exe -czf $TarPath -C $StageDir .
if ($LASTEXITCODE -ne 0) { throw "tar.exe pack failed" }

try {
    Write-Host "[2/4] Uploading builder context (~1.5 MB) to $SshTarget..." -ForegroundColor Cyan
    $tarBytes = [System.IO.File]::ReadAllBytes($TarPath)
    Send-TarGzOverSsh -Bytes $tarBytes -RemoteDir "/var/lib/pits/test-pool-build/context"
} finally {
    Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
    Remove-Item -Force $TarPath -ErrorAction SilentlyContinue
}

Write-Host "[3/4] Building Nix OCI image (pits/test-runner:$ImageTag) & mirroring agent-sandbox ($AgentSandboxVersion) on pits-core..." -ForegroundColor Cyan
$remoteBuild = @"
set -euo pipefail
WORK_ROOT="/var/lib/pits/test-pool-build"
CTX="`$WORK_ROOT/context"
CACHE_ROOT="/var/lib/pits/tinkerbell-20261108/image-cache-v1.14.5"
mkdir -p "`$WORK_ROOT/bin" "`$WORK_ROOT/portal-deps" "`$WORK_ROOT/oci"

# 1. Cache matching devspacehelper ($DevSpaceVersion) for zero-latency air-gapped DevSpace sync
HELPER_BIN="`$WORK_ROOT/bin/devspacehelper-$DevSpaceVersion"
if [ ! -x "`$HELPER_BIN" ]; then
  echo "Downloading devspacehelper $DevSpaceVersion on pits-core..."
  curl -fSL --retry 3 "https://github.com/devspace-sh/devspace/releases/download/$DevSpaceVersion/devspacehelper" -o "`$HELPER_BIN.tmp"
  chmod 0755 "`$HELPER_BIN.tmp"
  mv -f "`$HELPER_BIN.tmp" "`$HELPER_BIN"
fi
cp -f "`$HELPER_BIN" "`$CTX/devspacehelper"
chmod 0755 "`$CTX/devspacehelper" "`$CTX/runner-verify-shard.sh" "`$CTX/portal-headless-e2e.mjs"

# 2. Ensure /var/lib/pits/portal-vendor matches Cargo.lock
LOCK_SHA=`$(sha256sum "`$CTX/Cargo.lock" | awk '{print `$1}')
PREV_CARGO_SHA=""
if [ -f /var/lib/pits/portal-vendor/.cargo-lock.sha256 ]; then
  PREV_CARGO_SHA=`$(cat /var/lib/pits/portal-vendor/.cargo-lock.sha256)
fi
if [ "`$LOCK_SHA" != "`$PREV_CARGO_SHA" ] || [ ! -d /var/lib/pits/portal-vendor/anyhow ]; then
  echo "Refreshing /var/lib/pits/portal-vendor for Cargo.lock=`$LOCK_SHA..."
  mkdir -p "`$CTX/dummy-src/src"
  echo "fn main() {}" > "`$CTX/dummy-src/src/main.rs"
  cp -f "`$CTX/Cargo.toml" "`$CTX/Cargo.lock" "`$CTX/dummy-src/"
  nix --extra-experimental-features 'nix-command flakes' shell --impure --no-warn-dirty \
    --expr '(let p = (builtins.getFlake "path:$FlakeDir").nixosConfigurations.$FlakeAttr.pkgs; in p.symlinkJoin { name = "cargo-env"; paths = [ p.cargo p.rustc ]; })' \
    -c cargo vendor --locked --manifest-path "`$CTX/dummy-src/Cargo.toml" /var/lib/pits/portal-vendor.new >/dev/null
  printf '%s\n' "`$LOCK_SHA" > /var/lib/pits/portal-vendor.new/.cargo-lock.sha256
  chmod -R a+rX /var/lib/pits/portal-vendor.new
  rm -rf /var/lib/pits/portal-vendor
  mv /var/lib/pits/portal-vendor.new /var/lib/pits/portal-vendor
fi

# 3. Build & cache Linux x86_64 portal/node_modules keyed by package.json + package-lock.json SHA-256
NPM_SHA=`$(cat "`$CTX/portal-lock/package.json" "`$CTX/portal-lock/package-lock.json" | sha256sum | awk '{print `$1}')
PREV_NPM_SHA=""
if [ -f "`$WORK_ROOT/portal-deps/.package-lock.sha256" ]; then
  PREV_NPM_SHA=`$(cat "`$WORK_ROOT/portal-deps/.package-lock.sha256")
fi
if [ "`$NPM_SHA" != "`$PREV_NPM_SHA" ] || [ ! -d "`$WORK_ROOT/portal-deps/node_modules/@backstage" ]; then
  echo "Installing Linux portal node_modules (hash=`$NPM_SHA)..."
  rm -rf "`$WORK_ROOT/portal-deps.new"
  mkdir -p "`$WORK_ROOT/portal-deps.new"
  cp -f "`$CTX/portal-lock/package.json" "`$CTX/portal-lock/package-lock.json" "`$WORK_ROOT/portal-deps.new/"
  nix --extra-experimental-features 'nix-command flakes' shell --impure --no-warn-dirty \
    --expr '(builtins.getFlake "path:$FlakeDir").nixosConfigurations.$FlakeAttr.pkgs.nodejs' \
    -c bash -c "cd '`$WORK_ROOT/portal-deps.new' && HOME=/tmp npm install --legacy-peer-deps --no-audit --no-fund --prefer-offline"
  printf '%s\n' "`$NPM_SHA" > "`$WORK_ROOT/portal-deps.new/.package-lock.sha256"
  chmod -R a+rX "`$WORK_ROOT/portal-deps.new"
  rm -rf "`$WORK_ROOT/portal-deps"
  mv "`$WORK_ROOT/portal-deps.new" "`$WORK_ROOT/portal-deps"
fi

# 4. Build Nix layered OCI image (`docker.io/pits/test-runner:$ImageTag`)
NIX_SUBS="--option substituters https://mirrors.ustc.edu.cn/nix-channels/store"
echo "Building Nix layered container image docker.io/pits/test-runner:$ImageTag..."
PREV_IMG_REAL=`$(readlink -f "`$WORK_ROOT/result-image" 2>/dev/null || true)
nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty `$NIX_SUBS \
  --out-link "`$WORK_ROOT/result-image" \
  --expr "let p = (builtins.getFlake \"path:$FlakeDir\").nixosConfigurations.$FlakeAttr.pkgs; in import `$CTX/test-runner-image.nix { pkgs = p; assetsDir = `$CTX; cargoVendorDir = /var/lib/pits/portal-vendor; portalNodeModulesDir = `$WORK_ROOT/portal-deps/node_modules; imageTag = \"$ImageTag\"; }"
NEW_IMG_REAL=`$(readlink -f "`$WORK_ROOT/result-image")

# 5. Convert docker-archive -> OCI layout via skopeo & import into talos-image-cache (10.89.20.14:3172)
rm -rf "`$WORK_ROOT/oci/test-runner" "`$WORK_ROOT/oci/agent-sandbox"
mkdir -p "`$WORK_ROOT/oci"

nix --extra-experimental-features 'nix-command flakes' shell --impure --no-warn-dirty `$NIX_SUBS \
  --expr '(let p = (builtins.getFlake "path:$FlakeDir").nixosConfigurations.$FlakeAttr.pkgs; in p.symlinkJoin { name = "oci-tools"; paths = [ p.skopeo p.python3 ]; })' \
  -c bash -euo pipefail -c "
    if [ '`$PREV_IMG_REAL' != '`$NEW_IMG_REAL' ] || [ ! -f '`$CACHE_ROOT/manifests/docker.io/pits/test-runner/reference/$ImageTag' ]; then
      skopeo copy --insecure-policy docker-archive:`$WORK_ROOT/result-image oci:`$WORK_ROOT/oci/test-runner:$ImageTag >/dev/null
      python3 '`$CTX/import-to-talos-cache.py' '`$WORK_ROOT/oci/test-runner' '`$CACHE_ROOT' 'docker.io/pits/test-runner:$ImageTag'
    else
      echo 'docker.io/pits/test-runner:$ImageTag already up to date in talos-image-cache.'
    fi

    if [ ! -f '`$CACHE_ROOT/manifests/registry.k8s.io/agent-sandbox/agent-sandbox-controller/reference/$AgentSandboxVersion' ]; then
      echo 'Mirroring registry.k8s.io/agent-sandbox/agent-sandbox-controller:$AgentSandboxVersion into talos-image-cache...'
      if ! skopeo copy --insecure-policy --override-os linux --override-arch amd64 \
        docker://k8s.m.daocloud.io/agent-sandbox/agent-sandbox-controller:$AgentSandboxVersion \
        oci:`$WORK_ROOT/oci/agent-sandbox:$AgentSandboxVersion >/dev/null 2>&1; then
        if ss -ltn 2>/dev/null | grep -q ':7890 '; then
          export https_proxy=http://127.0.0.1:7890 http_proxy=http://127.0.0.1:7890
        fi
        skopeo copy --insecure-policy --override-os linux --override-arch amd64 \
          docker://registry.k8s.io/agent-sandbox/agent-sandbox-controller:$AgentSandboxVersion \
          oci:`$WORK_ROOT/oci/agent-sandbox:$AgentSandboxVersion >/dev/null
      fi
      python3 '`$CTX/import-to-talos-cache.py' '`$WORK_ROOT/oci/agent-sandbox' '`$CACHE_ROOT' 'registry.k8s.io/agent-sandbox/agent-sandbox-controller:$AgentSandboxVersion'
    else
      echo 'agent-sandbox-controller:$AgentSandboxVersion already cached in talos-image-cache.'
    fi
  "
rm -rf "`$WORK_ROOT/oci"
ls -lh "`$CACHE_ROOT/manifests/docker.io/pits/test-runner/reference/$ImageTag"
"@

$out = Invoke-SshScript $remoteBuild
Write-Host $out
Write-Host "[4/4] Image build & VLAN 20 Talos cache publication complete!" -ForegroundColor Green
