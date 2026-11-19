<#
.SYNOPSIS
  Unified build & deployment handoff orchestrator for PITS Infra (`CICD` subproject).
  Replaces ad-hoc local builds/promotions by gating on `cluster-pits-lab` CI/CD (`test-pool`
  / Gitea Actions) and declaratively promoting `pits-portal`, `pits-agent`, `windows` (`pitsctl.exe`),
  and `Base/` NixOS onto `pits-core` (`100.95.95.88`) plus publishing container images to Zot & Talos cache.
#>
[CmdletBinding()]
param(
    [ValidateSet("all", "portal", "agent", "test-runner", "cicd-runner", "windows", "base")]
    [string]$Target = "all",
    [string]$SshTarget = "root@100.95.95.88",
    [string]$Kubeconfig = "",
    [switch]$SkipUiBuild,
    [switch]$SkipVerify,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$CicdRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$RepoRoot = (Resolve-Path (Join-Path $CicdRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}

Write-Host "[CICD Deploy] Target=$Target DryRun=$DryRun SkipVerify=$SkipVerify" -ForegroundColor Cyan

if (-not $SkipVerify) {
    Write-Host "[1/3] Running pre-promotion verification on cluster-pits-lab Warm Pool..." -ForegroundColor Cyan
    $shard = switch ($Target) {
        "portal"  { "portal-e2e" }
        "agent"   { "go-linux" }
        "windows" { "rust" }
        default   { "all" }
    }
    if ($shard -ne "all" -or $Target -eq "all") {
        if (-not $DryRun) {
            & (Join-Path $RepoRoot "test-pool\scripts\Invoke-PitsTestPool.ps1") -Shard $shard
            if ($LASTEXITCODE -ne 0) { throw "Pre-promotion test-pool verification failed for shard=$shard" }
        } else {
            Write-Host "  [DryRun] Would run Invoke-PitsTestPool.ps1 -Shard $shard"
        }
    }
}

Write-Host "[2/3] Executing declarative builds & image publications..." -ForegroundColor Cyan
if ($Target -in @("all", "test-runner")) {
    if (-not $DryRun) {
        & (Join-Path $RepoRoot "test-pool\image\build-and-publish-image.ps1") -SshTarget $SshTarget -ClientRepoRoot $RepoRoot
        if ($LASTEXITCODE -ne 0) { throw "build-and-publish-image.ps1 failed" }
    } else {
        Write-Host "  [DryRun] Would build and publish docker.io/pits/test-runner"
    }
}

if ($Target -in @("all", "cicd-runner")) {
    if (-not $DryRun) {
        & (Join-Path $CicdRoot "image\build-cicd-runner.ps1") -SshTarget $SshTarget -RepoRoot $RepoRoot
        if ($LASTEXITCODE -ne 0) { throw "build-cicd-runner.ps1 failed" }
    } else {
        Write-Host "  [DryRun] Would build and publish docker.io/pits/cicd-runner"
    }
}

Write-Host "[3/3] Promoting verified control-plane services to pits-core..." -ForegroundColor Cyan
if ($Target -in @("all", "portal")) {
    if (-not $DryRun) {
        & (Join-Path $RepoRoot "deploy\nixos\promote-portal.ps1") -SshTarget $SshTarget
        if ($LASTEXITCODE -ne 0) { throw "promote-portal.ps1 failed" }
    } else {
        Write-Host "  [DryRun] Would promote pits-portal to $SshTarget"
    }
}

if ($Target -in @("all", "agent")) {
    Write-Host "  Promoting Go pits-agent static Linux ELF via Nix flake on $SshTarget..." -ForegroundColor Cyan
    if (-not $DryRun) {
        $Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
        $agentStage = Join-Path $env:TEMP "pits-agent-stage.tar.gz"
        if (Test-Path $agentStage) { Remove-Item -Force $agentStage }
        & tar.exe -czf $agentStage -C $RepoRoot server deploy/nixos/package.nix
        if ($LASTEXITCODE -ne 0) { throw "Failed to pack server source tarball" }
        try {
            $remoteTmp = "/tmp/pits-agent-stage.tar.gz"
            & scp.exe -q -o BatchMode=yes -o ConnectTimeout=10 $agentStage "${SshTarget}:${remoteTmp}"
            if ($LASTEXITCODE -ne 0) { throw "scp upload of agent-src failed (exit $LASTEXITCODE)" }
            $cmd = "umask 022 && trap 'rm -f $remoteTmp' EXIT && rm -rf /var/lib/pits/agent-src.new && mkdir -p /var/lib/pits/agent-src.new && tar --warning=no-unknown-keyword -xzf '$remoteTmp' -C /var/lib/pits/agent-src.new && rm -rf /var/lib/pits/agent-src && mv /var/lib/pits/agent-src.new /var/lib/pits/agent-src"
            ssh -T -o BatchMode=yes -o ConnectTimeout=10 $SshTarget "$cmd"
            if ($LASTEXITCODE -ne 0) { throw "Remote extract of agent-src failed (exit $LASTEXITCODE)" }
        } finally {
            Remove-Item -Force $agentStage -ErrorAction SilentlyContinue
        }

        $buildAgent = @'
set -euo pipefail
if ! nix --extra-experimental-features 'nix-command flakes' build --offline --impure --no-warn-dirty \
  --out-link /var/lib/pits/agent/current \
  --expr '((builtins.getFlake "path:/home/libraskywalker/PITS-B1/Base").nixosConfigurations.core.pkgs.callPackage /var/lib/pits/agent-src/deploy/nixos/package.nix { src = /var/lib/pits/agent-src/server; })'; then
  nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty \
    --out-link /var/lib/pits/agent/current \
    --expr '((builtins.getFlake "path:/home/libraskywalker/PITS-B1/Base").nixosConfigurations.core.pkgs.callPackage /var/lib/pits/agent-src/deploy/nixos/package.nix { src = /var/lib/pits/agent-src/server; })'
fi
mkdir -p /var/lib/pits/agent/bin
ln -sfn /var/lib/pits/agent/current/bin/pits-agent /var/lib/pits/agent/bin/pits-agent
systemctl restart pits-agent.service
systemctl is-active pits-agent.service
'@.Replace("`r`n","`n")
        $b64 = [Convert]::ToBase64String($Utf8NoBom.GetBytes($buildAgent))
        ssh -T -o BatchMode=yes -o ConnectTimeout=10 $SshTarget "echo $b64 | base64 -d | bash -s"
        if ($LASTEXITCODE -ne 0) { throw "Remote Nix build/restart of pits-agent failed" }
    } else {
        Write-Host "  [DryRun] Would build and restart pits-agent on $SshTarget"
    }
}

if ($Target -in @("all", "windows")) {
    Write-Host "  Building Windows client binary client/bin/pitsctl.exe (x86_64-pc-windows-gnu)..." -ForegroundColor Cyan
    $winBinDir = Join-Path $RepoRoot "client\bin"
    $winExePath = Join-Path $winBinDir "pitsctl.exe"
    if (-not $DryRun) {
        New-Item -ItemType Directory -Force -Path $winBinDir | Out-Null
        $isWinHost = ($env:OS -eq "Windows_NT")
        if (-not $isWinHost) {
            Push-Location (Join-Path $RepoRoot "client")
            try {
                if (Test-Path "/opt/pits-cache/cargo-config.toml") {
                    $ch = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:HOME ".cargo" }
                    New-Item -ItemType Directory -Force -Path $ch | Out-Null
                    Copy-Item -Force "/opt/pits-cache/cargo-config.toml" (Join-Path $ch "config.toml")
                }
                & cargo build --release --offline --frozen --target x86_64-pc-windows-gnu
                if ($LASTEXITCODE -ne 0) { throw "cargo build x86_64-pc-windows-gnu failed" }
                $td = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $RepoRoot "client\target" }
                $builtExe = Join-Path $td "x86_64-pc-windows-gnu\release\pitsctl.exe"
                if (-not (Test-Path $builtExe)) { throw "Built pitsctl.exe not found at $builtExe" }
                Copy-Item -Force $builtExe $winExePath
            } finally {
                Pop-Location
            }
        } else {
            if (-not (Test-Path $Kubeconfig)) {
                throw "Lab kubeconfig not found at $Kubeconfig (required to build Windows target on cluster-pits-lab cicd-runner)"
            }
            $podJson = (& kubectl --kubeconfig $Kubeconfig -n pits-test-pool get pods -l "app.kubernetes.io/name=pits-cicd-runner,pits.io/runner-pool=cpu" -o json | Out-String | ConvertFrom-Json)
            $runnerPod = $null
            foreach ($pod in @($podJson.items)) {
                $readyCond = @($pod.status.conditions | Where-Object { $_.type -eq "Ready" -and $_.status -eq "True" })
                if ($pod.status.phase -eq "Running" -and $readyCond.Count -gt 0) {
                    $runnerPod = $pod.metadata.name
                    break
                }
            }
            if ([string]::IsNullOrWhiteSpace($runnerPod)) {
                throw "No Ready runner-cicd-cpu pod found in pits-test-pool namespace"
            }
            Write-Host "  Using cluster-pits-lab DDR5 tmpfs pod: $runnerPod" -ForegroundColor DarkGray

            $checkCmd = 'command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 && test -d "$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu" && echo OK'
            $b64Check = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($checkCmd))
            $prevEap = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            $hasMinGw = (& kubectl --kubeconfig $Kubeconfig -n pits-test-pool exec $runnerPod -- sh -c "echo $b64Check | base64 -d | sh" 2>$null)
            $ErrorActionPreference = $prevEap
            if (($hasMinGw | Out-String).Trim() -ne "OK") {
                throw "Runner pod $runnerPod does not yet have x86_64-pc-windows-gnu / MinGW-w64. Run Deploy-CoreArtifacts.ps1 -Target cicd-runner and roll out cicd-runner:v1.1 first."
            }

            $buildId = [Guid]::NewGuid().ToString("N").Substring(0, 8)
            $winStage = Join-Path $env:TEMP "pits-win-src-$buildId"
            $winTarName = "pits-win-src-$buildId.tar.gz"
            $winTar = Join-Path $env:TEMP $winTarName
            $remoteWork = "/ramdisk/win-build-$buildId"
            $winExeNew = Join-Path $winBinDir "pitsctl.new.exe"
            if (Test-Path $winStage) { Remove-Item -Recurse -Force $winStage }
            if (Test-Path $winTar) { Remove-Item -Force $winTar }
            if (Test-Path $winExeNew) { Remove-Item -Force $winExeNew }

            try {
                New-Item -ItemType Directory -Force -Path $winStage | Out-Null
                New-Item -ItemType Directory -Force -Path (Join-Path $winStage "client"), (Join-Path $winStage "deploy") | Out-Null
                Copy-Item -Force (Join-Path $RepoRoot "client\Cargo.toml") (Join-Path $winStage "client\Cargo.toml")
                Copy-Item -Force (Join-Path $RepoRoot "client\Cargo.lock") (Join-Path $winStage "client\Cargo.lock")
                Copy-Item -Force (Join-Path $RepoRoot "client\build.rs") (Join-Path $winStage "client\build.rs")
                Copy-Item -Recurse -Force (Join-Path $RepoRoot "client\src") (Join-Path $winStage "client\src")
                Copy-Item -Recurse -Force (Join-Path $RepoRoot "deploy\nixos") (Join-Path $winStage "deploy\nixos")

                $distIndex = Join-Path $RepoRoot "portal\dist\index.html"
                $distAssets = Join-Path $RepoRoot "portal\dist\assets"
                if ((Test-Path $distIndex) -and (Test-Path $distAssets)) {
                    New-Item -ItemType Directory -Force -Path (Join-Path $winStage "portal") | Out-Null
                    Copy-Item -Recurse -Force (Join-Path $RepoRoot "portal\dist") (Join-Path $winStage "portal\dist")
                } elseif (-not $SkipUiBuild -and (Test-Path (Join-Path $RepoRoot "portal\package.json"))) {
                    New-Item -ItemType Directory -Force -Path (Join-Path $winStage "portal") | Out-Null
                    foreach ($pf in @("package.json", "package-lock.json", "tsconfig.json", "vite.config.ts", "index.html")) {
                        $srcPf = Join-Path $RepoRoot "portal\$pf"
                        if (Test-Path $srcPf) { Copy-Item -Force $srcPf (Join-Path $winStage "portal\$pf") }
                    }
                    Copy-Item -Recurse -Force (Join-Path $RepoRoot "portal\src") (Join-Path $winStage "portal\src")
                }

                & tar.exe -czf $winTar -C $winStage .
                if ($LASTEXITCODE -ne 0) { throw "Failed to pack Windows build context tarball" }

                Push-Location $env:TEMP
                try {
                    & kubectl --kubeconfig $Kubeconfig cp $winTarName "pits-test-pool/${runnerPod}:/ramdisk/${winTarName}"
                    if ($LASTEXITCODE -ne 0) { throw "kubectl cp upload to $runnerPod failed" }
                } finally {
                    Pop-Location
                }

                $Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
                $buildScript = @"
set -euo pipefail
export HOME=/ramdisk/home
export CARGO_HOME=/ramdisk/cargo-home
export CARGO_TARGET_DIR=/ramdisk/cargo-target
mkdir -p "`$HOME" "`$CARGO_HOME" "`$CARGO_TARGET_DIR" "$remoteWork"
cp -f /opt/pits-cache/cargo-config.toml "`$CARGO_HOME/config.toml"
tar -xzf "/ramdisk/$winTarName" -C "$remoteWork"
rm -f "/ramdisk/$winTarName"
trap 'rm -rf "$remoteWork"' EXIT

if [ ! -f "$remoteWork/portal/dist/index.html" ] && [ -f "$remoteWork/portal/package.json" ]; then
  if [ -d /ramdisk/portal-node-modules/.bin ]; then
    cp -a /ramdisk/portal-node-modules "$remoteWork/portal/node_modules"
  elif [ -e /opt/pits-cache/portal-node-modules/vite/package.json ]; then
    STORE_NM="`$(dirname "`$(dirname "`$(readlink -f /opt/pits-cache/portal-node-modules/vite/package.json)")")"
    cp -a "`$STORE_NM" "$remoteWork/portal/node_modules"
  fi
  if [ -x "$remoteWork/portal/node_modules/.bin/vite" ]; then
    chmod -R u+w "$remoteWork/portal/node_modules"
    (cd "$remoteWork/portal" && ./node_modules/.bin/vite build)
  fi
fi

cd "$remoteWork/client"
cargo build --release --offline --frozen --target x86_64-pc-windows-gnu
EXE_PATH="`$CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release/pitsctl.exe"
test -s "`$EXE_PATH"
DLLS="`$(x86_64-w64-mingw32-objdump -p "`$EXE_PATH" | grep 'DLL Name:' || true)"
if echo "`$DLLS" | grep -qiE 'libwinpthread|libgcc_s|libstdc\+\+'; then
  echo "ERROR: non-system MinGW DLL dependency detected:" >&2
  echo "`$DLLS" >&2
  exit 1
fi
sha256sum "`$EXE_PATH"
ls -lh "`$EXE_PATH"
"@.Replace("`r`n", "`n")
                $b64Build = [Convert]::ToBase64String($Utf8NoBom.GetBytes($buildScript))
                & kubectl --kubeconfig $Kubeconfig -n pits-test-pool exec $runnerPod -- bash -c "echo $b64Build | base64 -d | bash -s"
                if ($LASTEXITCODE -ne 0) { throw "Remote x86_64-pc-windows-gnu build failed on $runnerPod" }

                Push-Location $winBinDir
                try {
                    & kubectl --kubeconfig $Kubeconfig cp "pits-test-pool/${runnerPod}:/ramdisk/cargo-target/x86_64-pc-windows-gnu/release/pitsctl.exe" "pitsctl.new.exe"
                    if ($LASTEXITCODE -ne 0) { throw "kubectl cp download of pitsctl.exe failed" }
                } finally {
                    Pop-Location
                }

                $hdrBytes = [System.IO.File]::ReadAllBytes($winExeNew)
                if ($hdrBytes.Length -lt 1024 -or $hdrBytes[0] -ne 0x4D -or $hdrBytes[1] -ne 0x5A) {
                    throw "Downloaded pitsctl.new.exe failed PE (MZ) header check"
                }
                & $winExeNew --help | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "pitsctl.new.exe --help smoke check failed (exit $LASTEXITCODE)" }

                try {
                    Move-Item -Force $winExeNew $winExePath
                    $hash = (Get-FileHash -Algorithm SHA256 $winExePath).Hash.ToLowerInvariant()
                    $lenMb = [Math]::Round((Get-Item $winExePath).Length / 1MB, 2)
                    Write-Host "  Updated $winExePath (${lenMb} MB, sha256=$hash)" -ForegroundColor Green
                } catch {
                    Write-Warning "Could not overwrite locked $winExePath ($($_.Exception.Message)). Staged verified binary at $winExeNew"
                }
            } finally {
                Remove-Item -Recurse -Force $winStage -ErrorAction SilentlyContinue
                Remove-Item -Force $winTar -ErrorAction SilentlyContinue
            }
        }
    } else {
        Write-Host "  [DryRun] Would cross-compile client/bin/pitsctl.exe (x86_64-pc-windows-gnu) via cluster-pits-lab cicd-runner"
    }
}

if ($Target -eq "base") {
    Write-Host "  Triggering Base/ NixOS evaluation check..." -ForegroundColor Cyan
    if (-not $DryRun) {
        $evalCmd = "nix --extra-experimental-features 'nix-command flakes' eval path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.config.system.build.toplevel.drvPath"
        ssh -T -o BatchMode=yes -o ConnectTimeout=10 $SshTarget $evalCmd
        if ($LASTEXITCODE -ne 0) { throw "Base flake eval failed on $SshTarget" }
    } else {
        Write-Host "  [DryRun] Would run nix eval on Base#core"
    }
}

Write-Host "[CICD Deploy] Handover deployment complete!" -ForegroundColor Green
