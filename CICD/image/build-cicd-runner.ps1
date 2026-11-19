<#
.SYNOPSIS
  Builds the `docker.io/pits/cicd-runner:v1.1` Nix OCI image on `pits-core` (`100.95.95.88`)
  and publishes it to `talos-image-cache` (`http://10.89.20.14:3172`) for `cluster-pits-lab`.
#>
[CmdletBinding()]
param(
    [string]$SshTarget = "root@100.95.95.88",
    [string]$FlakeDir = "/home/libraskywalker/PITS-B1/Base",
    [string]$FlakeAttr = "core",
    [string]$ImageTag = "v1.1",
    [string]$DevSpaceVersion = "v6.3.21",
    [string]$ActRunnerVersion = "v6.0.0",
    [string]$TeaVersion = "v0.16.0",
    [string]$RepoRoot = "c:\Project\PITS\Infra"
)

$ErrorActionPreference = "Stop"
$CicdRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$PoolRoot = Join-Path $RepoRoot "test-pool"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Invoke-SshScript([string]$Script) {
    $clean = $Script.Replace("`r`n", "`n") + "`n"
    $tmpLocal = Join-Path $env:TEMP ("pits-ssh-" + [Guid]::NewGuid().ToString("N").Substring(0, 8) + ".sh")
    $remoteSh = "/tmp/pits-cicd-build-$([Guid]::NewGuid().ToString('N').Substring(0,8)).sh"
    [System.IO.File]::WriteAllBytes($tmpLocal, $Utf8NoBom.GetBytes($clean))
    try {
        & scp -B -o ConnectTimeout=15 $tmpLocal "${SshTarget}:${remoteSh}"
        if ($LASTEXITCODE -ne 0) { throw "SCP upload of build script failed" }
        & ssh -T -o BatchMode=yes -o ConnectTimeout=15 $SshTarget "bash -eu '$remoteSh'; rc=`$?; rm -f '$remoteSh'; exit `$rc"
        if ($LASTEXITCODE -ne 0) { throw "Remote build script failed (exit $LASTEXITCODE)" }
    } finally {
        Remove-Item -Force $tmpLocal -ErrorAction SilentlyContinue
    }
}

function Send-TarGzFileOverScp([string]$LocalTarPath, [string]$RemoteDir) {
    $remoteTmp = "/tmp/pits-cicd-ctx-$([Guid]::NewGuid().ToString('N').Substring(0,8)).tar.gz"
    & scp -B -o ConnectTimeout=15 $LocalTarPath "${SshTarget}:${remoteTmp}"
    if ($LASTEXITCODE -ne 0) {
        throw "SCP upload to ${SshTarget}:${remoteTmp} failed"
    }
    $cmd = "umask 022 && rm -rf '$RemoteDir.new' && mkdir -p '$RemoteDir.new' && tar -xzf '$remoteTmp' -C '$RemoteDir.new' && rm -f '$remoteTmp' && rm -rf '$RemoteDir' && mv '$RemoteDir.new' '$RemoteDir'"
    & ssh -T -o BatchMode=yes -o ConnectTimeout=15 $SshTarget $cmd
    if ($LASTEXITCODE -ne 0) {
        throw "Remote unpack to ${SshTarget}:${RemoteDir} failed"
    }
}

Write-Host "[1/3] Ensuring cached Linux amd64 runner binaries & staging CICD Nix context..." -ForegroundColor Cyan
$BinCacheDir = Join-Path $env:LOCALAPPDATA "PITS\cicd-bin-cache"
New-Item -ItemType Directory -Force -Path $BinCacheDir | Out-Null

$UseProxy = $null
if (Test-NetConnection -ComputerName 127.0.0.1 -Port 7897 -InformationLevel Quiet -WarningAction SilentlyContinue) {
    $UseProxy = "http://127.0.0.1:7897"
}

function Ensure-CachedBinary([string]$FileName, [string]$Url) {
    $dst = Join-Path $BinCacheDir $FileName
    $tmp = "$dst.tmp"
    if (Test-Path $tmp) { Remove-Item -Force $tmp -ErrorAction SilentlyContinue }
    if (-not (Test-Path $dst) -or (Get-Item $dst).Length -lt 1024) {
        Write-Host "  Downloading $FileName..." -ForegroundColor DarkGray
        $curlArgs = @("-fSL", "--retry", "3", "--connect-timeout", "15", "--max-time", "180")
        if ($UseProxy) { $curlArgs += @("--proxy", $UseProxy) }
        $curlArgs += @("-o", $tmp, $Url)
        & curl.exe @curlArgs
        if ($LASTEXITCODE -ne 0) { throw "Failed to download $Url (exit $LASTEXITCODE)" }
        Move-Item -Force $tmp $dst
    }
    return $dst
}

$actVerClean = $ActRunnerVersion.TrimStart("v")
$teaVerClean = $TeaVersion.TrimStart("v")
$helperPath = Ensure-CachedBinary "devspacehelper-$DevSpaceVersion" "https://github.com/devspace-sh/devspace/releases/download/$DevSpaceVersion/devspacehelper"
$dsPath     = Ensure-CachedBinary "devspace-$DevSpaceVersion" "https://github.com/devspace-sh/devspace/releases/download/$DevSpaceVersion/devspace-linux-amd64"
$teaPath    = Ensure-CachedBinary "tea-$TeaVersion" "https://gitea.com/gitea/tea/releases/download/$TeaVersion/tea-$teaVerClean-linux-amd64"
$actPath    = Ensure-CachedBinary "act_runner-$ActRunnerVersion" "https://gitea.com/gitea/runner/releases/download/$ActRunnerVersion/gitea-runner-$actVerClean-linux-amd64"

$nextestPath = Join-Path $BinCacheDir "cargo-nextest-0.9.152"
if (-not (Test-Path $nextestPath) -or (Get-Item $nextestPath).Length -lt 1024) {
    $nextestTgz = Ensure-CachedBinary "cargo-nextest-0.9.152.tar.gz" "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.152/cargo-nextest-0.9.152-x86_64-unknown-linux-musl.tar.gz"
    & tar.exe -xzf $nextestTgz -C $BinCacheDir cargo-nextest
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract cargo-nextest" }
    Move-Item -Force (Join-Path $BinCacheDir "cargo-nextest") $nextestPath
}

$StageDir = Join-Path $env:TEMP "pits-cicd-runner-stage"
$TarPath  = Join-Path $env:TEMP "pits-cicd-runner-stage.tar.gz"
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
if (Test-Path $TarPath)  { Remove-Item -Force $TarPath }

New-Item -ItemType Directory -Force -Path $StageDir, (Join-Path $StageDir "portal-lock") | Out-Null

foreach ($item in @(
    @{ Src = Join-Path $CicdRoot "image\cicd-runner-image.nix"; Dst = Join-Path $StageDir "cicd-runner-image.nix" },
    @{ Src = Join-Path $CicdRoot "image\cicd-runner-entrypoint.sh"; Dst = Join-Path $StageDir "cicd-runner-entrypoint.sh" },
    @{ Src = Join-Path $PoolRoot "image\import-to-talos-cache.py"; Dst = Join-Path $StageDir "import-to-talos-cache.py" },
    @{ Src = Join-Path $PoolRoot "scripts\runner-verify-shard.sh"; Dst = Join-Path $StageDir "runner-verify-shard.sh" },
    @{ Src = Join-Path $PoolRoot "scripts\portal-headless-e2e.mjs"; Dst = Join-Path $StageDir "portal-headless-e2e.mjs" }
)) {
    $txt = [System.IO.File]::ReadAllText($item.Src).Replace("`r`n", "`n")
    [System.IO.File]::WriteAllBytes($item.Dst, $Utf8NoBom.GetBytes($txt))
}

Copy-Item -Force $helperPath  (Join-Path $StageDir "devspacehelper")
Copy-Item -Force $dsPath      (Join-Path $StageDir "devspace")
Copy-Item -Force $teaPath     (Join-Path $StageDir "tea")
Copy-Item -Force $actPath     (Join-Path $StageDir "act_runner")
Copy-Item -Force $nextestPath (Join-Path $StageDir "cargo-nextest")
Copy-Item -Recurse -Force (Join-Path $RepoRoot "server\vendor") (Join-Path $StageDir "go-vendor")
Copy-Item -Force (Join-Path $RepoRoot "portal\package.json") (Join-Path $StageDir "portal-lock\package.json")
Copy-Item -Force (Join-Path $RepoRoot "portal\package-lock.json") (Join-Path $StageDir "portal-lock\package-lock.json")
Copy-Item -Force (Join-Path $RepoRoot "client\Cargo.toml") (Join-Path $StageDir "Cargo.toml")
Copy-Item -Force (Join-Path $RepoRoot "client\Cargo.lock") (Join-Path $StageDir "Cargo.lock")

& tar.exe -czf $TarPath -C $StageDir .
if ($LASTEXITCODE -ne 0) { throw "tar.exe pack failed" }

try {
    Send-TarGzFileOverScp -LocalTarPath $TarPath -RemoteDir "/var/lib/pits/cicd-runner-build/context"
} finally {
    Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
    Remove-Item -Force $TarPath -ErrorAction SilentlyContinue
}

Write-Host "[2/3] Building Nix OCI image docker.io/pits/cicd-runner:$ImageTag on pits-core..." -ForegroundColor Cyan
$remoteBuild = @"
set -euo pipefail
WORK_ROOT="/var/lib/pits/cicd-runner-build"
POOL_ROOT="/var/lib/pits/test-pool-build"
CTX="`$WORK_ROOT/context"
CACHE_ROOT="/var/lib/pits/tinkerbell-20261108/image-cache-v1.14.5"
mkdir -p "`$WORK_ROOT/oci" "`$POOL_ROOT/bin" "`$POOL_ROOT/portal-deps"

chmod 0755 "`$CTX/devspacehelper" "`$CTX/devspace" "`$CTX/tea" "`$CTX/act_runner" "`$CTX/cargo-nextest" "`$CTX/cicd-runner-entrypoint.sh" "`$CTX/runner-verify-shard.sh" "`$CTX/portal-headless-e2e.mjs"

NIX_SUBS="--option substituters https://mirrors.ustc.edu.cn/nix-channels/store --option extra-substituters https://cache.nixos.org"
PREV_IMG_REAL=`$(readlink -f "`$WORK_ROOT/result-image" 2>/dev/null || true)
nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty `$NIX_SUBS \
  --out-link "`$WORK_ROOT/result-image" \
  --expr "let p = (builtins.getFlake \"path:$FlakeDir\").nixosConfigurations.$FlakeAttr.pkgs; in import `$CTX/cicd-runner-image.nix { pkgs = p; assetsDir = `$CTX; cargoVendorDir = /var/lib/pits/portal-vendor; portalNodeModulesDir = `$POOL_ROOT/portal-deps/node_modules; imageTag = \"$ImageTag\"; }"
NEW_IMG_REAL=`$(readlink -f "`$WORK_ROOT/result-image")

rm -rf "`$WORK_ROOT/oci/cicd-runner"
mkdir -p "`$WORK_ROOT/oci"
OCI_TOOLS=`$(nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty --no-link --print-out-paths `$NIX_SUBS \
  --expr 'let p = (builtins.getFlake "path:$FlakeDir").nixosConfigurations.$FlakeAttr.pkgs; in p.symlinkJoin { name = "oci-tools"; paths = [ p.skopeo p.python3 ]; }')
if [ "`$PREV_IMG_REAL" != "`$NEW_IMG_REAL" ] || [ ! -f "`$CACHE_ROOT/manifests/docker.io/pits/cicd-runner/reference/$ImageTag" ]; then
  "`$OCI_TOOLS/bin/skopeo" copy --insecure-policy "docker-archive:`$WORK_ROOT/result-image" "oci:`$WORK_ROOT/oci/cicd-runner:$ImageTag" >/dev/null
  "`$OCI_TOOLS/bin/python3" "`$CTX/import-to-talos-cache.py" "`$WORK_ROOT/oci/cicd-runner" "`$CACHE_ROOT" "docker.io/pits/cicd-runner:$ImageTag"
else
  echo "docker.io/pits/cicd-runner:$ImageTag already up to date in talos-image-cache."
fi
rm -rf "`$WORK_ROOT/oci"
ls -lh "`$CACHE_ROOT/manifests/docker.io/pits/cicd-runner/reference/$ImageTag"
"@

Invoke-SshScript $remoteBuild
Write-Host "[3/3] Published docker.io/pits/cicd-runner:$ImageTag to talos-image-cache!" -ForegroundColor Green
