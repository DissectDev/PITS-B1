<#
.SYNOPSIS
  Build and declaratively deploy/promote the PITS Backstage v1.56 Control-Plane Portal (`pits-portal`)
  to NixOS `pits-core` (`100.95.95.88`).

.DESCRIPTION
  Unified build, verification, and deployment workflows for `c:\Project\PITS\Infra` have been
  handed over to the `CICD/` subproject (`CICD/scripts/Invoke-CicdPipeline.ps1` and
  `CICD/scripts/Deploy-CoreArtifacts.ps1 -Target portal`). This script remains the declarative
  NixOS promotion backend invoked by `Deploy-CoreArtifacts.ps1` and stays directly callable for
  backward compatibility (pass `-ViaCicd` to delegate through `Deploy-CoreArtifacts.ps1` with
  pre-promotion `cluster-pits-lab` verification).

  1. Optionally rebuilds `portal/dist` (`npm.cmd run build`) with your latest UI edits.
  2. Vendors Cargo crates offline outside the Git tree and streams clean source (`client/`,
     `portal/dist/`, `vendor/`) over Tailscale SSH to `/var/lib/pits/portal-src`.
  3. Installs declarative NixOS modules (`pits-portal.nix`, `pits-portal-package.nix`) into
     `/home/libraskywalker/PITS-B1/modules/`, registers `../../modules/pits-portal.nix` in
     `hosts/core/default.nix`, and stages non-secret `edge-router` hardware contracts +
     root-only `0600` loopback operator profile (`/var/lib/pits/portal/profile.yaml`).
  4. Compiles `pitsctl` natively inside Nix on `pits-core` (pinning a Nix GC root at
     `/var/lib/pits/portal/current`), links `/var/lib/pits/portal/bin/pitsctl`, runs
     `nixos-rebuild switch --flake path:/home/libraskywalker/PITS-B1#core` (with rollback
     on error), and verifies live portal readiness on `http://100.95.95.88:18090`.
#>
[CmdletBinding()]
param(
    [string]$SshTarget = "root@100.95.95.88",
    [string]$FlakeDir = "/home/libraskywalker/PITS-B1/Base",
    [string]$FlakeAttr = "core",
    [string]$PortalHost = "100.95.95.88",
    [int]$PortalPort = 18090,
    [string]$WorkspaceSource = "c:\Users\pitsadmin\edge-router",
    [switch]$SkipUiBuild,
    [switch]$SyncProfile,
    [switch]$NoSwitch,
    [switch]$ViaCicd
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$CicdDeployScript = Join-Path $RepoRoot "CICD\scripts\Deploy-CoreArtifacts.ps1"
if ($ViaCicd) {
    if (-not (Test-Path $CicdDeployScript)) {
        throw "CICD handover orchestrator not found at $CicdDeployScript"
    }
    Write-Host "[Handover] Delegating portal promotion to CICD/scripts/Deploy-CoreArtifacts.ps1 -Target portal..." -ForegroundColor Cyan
    & $CicdDeployScript -Target portal -SshTarget $SshTarget
    exit $LASTEXITCODE
}

$PortalDir = Join-Path $RepoRoot "portal"
$ClientDir = Join-Path $RepoRoot "client"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Invoke-SshScript {
    param(
        [Parameter(Mandatory = $true)][string]$Script
    )
    $cleanScript = $Script.Replace("`r`n", "`n") + "`n"
    $bytes = $Utf8NoBom.GetBytes($cleanScript)
    $b64 = [Convert]::ToBase64String($bytes)
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = "ssh"
    $psi.Arguments = "-T -o BatchMode=yes -o ConnectTimeout=10 $SshTarget `"printf '%s' '$b64' | base64 -d | sh -eu`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $proc.WaitForExit()
    $stdout = $outTask.GetAwaiter().GetResult()
    $stderr = $errTask.GetAwaiter().GetResult()
    if ($proc.ExitCode -ne 0) {
        throw "SSH command on $SshTarget failed (exit $($proc.ExitCode)):`nSTDERR: $stderr`nSTDOUT: $stdout"
    }
    return $stdout
}

function Send-BytesOverSsh {
    param(
        [Parameter(Mandatory = $true)][byte[]]$Bytes,
        [Parameter(Mandatory = $true)][string]$RemotePath,
        [string]$ModeOctal = "0644"
    )
    $id = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $localTmp = Join-Path $env:TEMP "pits-file-$id.tmp"
    $remoteTmp = "/tmp/pits-file-$id.tmp"
    $parent = ($RemotePath -replace '/[^/]+$', '')
    if ([string]::IsNullOrWhiteSpace($parent)) { $parent = "/var/lib/pits/portal" }
    [System.IO.File]::WriteAllBytes($localTmp, $Bytes)
    try {
        & scp.exe -q -o BatchMode=yes -o ConnectTimeout=10 $localTmp "${SshTarget}:${remoteTmp}"
        if ($LASTEXITCODE -ne 0) {
            throw "scp upload to ${SshTarget}:${RemotePath} failed (exit $LASTEXITCODE)"
        }
        $null = Invoke-SshScript "umask 077 && mkdir -p '$parent' && chmod $ModeOctal '$remoteTmp' && mv -f '$remoteTmp' '$RemotePath'"
    } finally {
        Remove-Item -Force $localTmp -ErrorAction SilentlyContinue
    }
}

function Send-TarGzOverSsh {
    param(
        [Parameter(Mandatory = $true)][byte[]]$Bytes,
        [Parameter(Mandatory = $true)][string]$RemoteCommandTemplate,
        [Parameter(Mandatory = $true)][string]$Label
    )
    $id = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $localTmp = Join-Path $env:TEMP "pits-scp-$id.tar.gz"
    $remoteTmp = "/tmp/pits-scp-$id.tar.gz"
    [System.IO.File]::WriteAllBytes($localTmp, $Bytes)
    try {
        & scp.exe -q -o BatchMode=yes -o ConnectTimeout=10 $localTmp "${SshTarget}:${remoteTmp}"
        if ($LASTEXITCODE -ne 0) {
            throw "scp upload $Label to $SshTarget failed (exit $LASTEXITCODE)"
        }
        $remoteCmd = $RemoteCommandTemplate.Replace("head -c __LEN__ | tar -xzf -", "tar --warning=no-unknown-keyword -xzf '$remoteTmp'") + " && rm -f '$remoteTmp'"
        $null = Invoke-SshScript "trap 'rm -f $remoteTmp' EXIT`n$remoteCmd"
    } finally {
        Remove-Item -Force $localTmp -ErrorAction SilentlyContinue
    }
}

Write-Host "[1/6] Checking local toolchain & Portal UI dist bundle..." -ForegroundColor Cyan
if (-not $SkipUiBuild) {
    Push-Location $PortalDir
    try {
        & npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw "Portal UI build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}
$DistIndex = Join-Path $PortalDir "dist\index.html"
if (-not (Test-Path $DistIndex)) {
    throw "Missing $DistIndex; run without -SkipUiBuild"
}

Write-Host "[2/6] Staging lean source tarball (~2 MB, crates vendored natively on pits-core)..." -ForegroundColor Cyan
$StageDir = Join-Path $env:TEMP "pits-portal-stage"
$TarPath = Join-Path $env:TEMP "pits-portal-src.tar.gz"
$WsTarPath = Join-Path $env:TEMP "pits-portal-ws.tar.gz"
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
if (Test-Path $TarPath) { Remove-Item -Force $TarPath }
if (Test-Path $WsTarPath) { Remove-Item -Force $WsTarPath }

New-Item -ItemType Directory -Force -Path (Join-Path $StageDir "client"), (Join-Path $StageDir "portal"), (Join-Path $StageDir "deploy\nixos") | Out-Null
Copy-Item -Force (Join-Path $ClientDir "Cargo.toml") (Join-Path $StageDir "client\Cargo.toml")
Copy-Item -Force (Join-Path $ClientDir "Cargo.lock") (Join-Path $StageDir "client\Cargo.lock")
Copy-Item -Force (Join-Path $ClientDir "build.rs") (Join-Path $StageDir "client\build.rs")
Copy-Item -Recurse -Force (Join-Path $ClientDir "src") (Join-Path $StageDir "client\src")
Copy-Item -Recurse -Force (Join-Path $PortalDir "dist") (Join-Path $StageDir "portal\dist")
foreach ($embedFile in @("package.nix", "pits-agent-module.nix", "rke2-daemonset.yaml")) {
    Copy-Item -Force (Join-Path $PSScriptRoot $embedFile) (Join-Path $StageDir "deploy\nixos\$embedFile")
}

& tar.exe -czf $TarPath -C $StageDir client portal deploy
if ($LASTEXITCODE -ne 0) { throw "tar.exe pack portal-src failed" }

$HasWorkspaceContracts = $false
if (-not [string]::IsNullOrWhiteSpace($WorkspaceSource) -and (Test-Path $WorkspaceSource)) {
    $WsStage = Join-Path $StageDir "workspace"
    $WsPlatform = Join-Path $WsStage "examples\platform"
    $WsDevices = Join-Path $WsStage "deploy\tinkerbell\devices"
    New-Item -ItemType Directory -Force -Path $WsPlatform, $WsDevices | Out-Null
    $coreYaml = Join-Path $WorkspaceSource "examples\platform\pits-core.yaml"
    if (Test-Path $coreYaml) {
        Copy-Item -Force $coreYaml (Join-Path $WsPlatform "pits-core.yaml")
        $HasWorkspaceContracts = $true
    }
    $devDir = Join-Path $WorkspaceSource "deploy\tinkerbell\devices"
    if (Test-Path $devDir) {
        Get-ChildItem -Path $devDir -File | Where-Object {
            $_.Name -like "*.contract.json" -or $_.Name -like "*-san.json"
        } | ForEach-Object {
            Copy-Item -Force $_.FullName (Join-Path $WsDevices $_.Name)
            $HasWorkspaceContracts = $true
        }
    }
    if ($HasWorkspaceContracts) {
        & tar.exe -czf $WsTarPath -C $WsStage .
        if ($LASTEXITCODE -ne 0) { throw "tar.exe pack workspace contracts failed" }
    }
}

Write-Host "[3/6] Streaming source tarball & declarative Nix modules to $SshTarget..." -ForegroundColor Cyan
try {
    $tarBytes = [System.IO.File]::ReadAllBytes($TarPath)
    $srcCmd = "umask 022 && mkdir -p /var/lib/pits && rm -rf /var/lib/pits/portal-src.new && mkdir -p /var/lib/pits/portal-src.new && head -c __LEN__ | tar -xzf - -C /var/lib/pits/portal-src.new && chmod -R a+rX /var/lib/pits/portal-src.new && rm -rf /var/lib/pits/portal-src && mv /var/lib/pits/portal-src.new /var/lib/pits/portal-src"
    Send-TarGzOverSsh -Bytes $tarBytes -RemoteCommandTemplate $srcCmd -Label "portal-src"

    if ($HasWorkspaceContracts) {
        $wsBytes = [System.IO.File]::ReadAllBytes($WsTarPath)
        $wsCmd = "umask 077 && mkdir -p /var/lib/pits/portal && rm -rf /var/lib/pits/portal/workspace.new && mkdir -p /var/lib/pits/portal/workspace.new && head -c __LEN__ | tar -xzf - -C /var/lib/pits/portal/workspace.new && chmod -R u=rwX,go= /var/lib/pits/portal/workspace.new && rm -rf /var/lib/pits/portal/workspace && mv /var/lib/pits/portal/workspace.new /var/lib/pits/portal/workspace"
        Send-TarGzOverSsh -Bytes $wsBytes -RemoteCommandTemplate $wsCmd -Label "workspace contracts"
    }
} finally {
    Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
    Remove-Item -Force $TarPath, $WsTarPath -ErrorAction SilentlyContinue
}

$ModuleNixText = ([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot "pits-portal-module.nix"))).Replace("`r`n", "`n")
$PkgNixText = ([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot "pits-portal-package.nix"))).Replace("`r`n", "`n")
$FlakeTrim = $FlakeDir.TrimEnd('/')
Send-BytesOverSsh -Bytes ($Utf8NoBom.GetBytes($ModuleNixText)) -RemotePath "$FlakeTrim/modules/pits-portal.nix" -ModeOctal "0644"
Send-BytesOverSsh -Bytes ($Utf8NoBom.GetBytes($PkgNixText)) -RemotePath "$FlakeTrim/modules/pits-portal-package.nix" -ModeOctal "0644"

Write-Host "[4/6] Ensuring root-only /var/lib/pits/portal/profile.yaml (`0600`)..." -ForegroundColor Cyan
$hasRemoteProfile = (Invoke-SshScript "if [ -s /var/lib/pits/portal/profile.yaml ]; then echo EXISTS; else echo MISSING; fi").Trim()
if ($SyncProfile -or $hasRemoteProfile -ne "EXISTS") {
    $LocalProfilePath = Join-Path $env:LOCALAPPDATA "PITS\Client\profile.yaml"
    if (-not (Test-Path $LocalProfilePath)) {
        throw "Cannot sync mTLS credentials: $LocalProfilePath not found"
    }
    $nodeHelper = @'
const fs = require('fs');
const path = require('path');
const YAML = require(path.join(process.env.PITS_PORTAL_DIR, 'node_modules', 'yaml'));
const raw = fs.readFileSync(process.env.PITS_LOCAL_PROFILE, 'utf8');
const prof = YAML.parse(raw) || {};
if (!prof.agentCaPem || !prof.agentClientCertPem || !prof.agentClientKeyPem) {
  throw new Error('Local profile.yaml is missing agent mTLS PKI material');
}
const linuxProfile = {
  activeWorkspaceSource: process.env.PITS_HAS_WS === '1' ? '/var/lib/pits/portal/workspace' : null,
  agentEndpoint: 'https://127.0.0.1:18443',
  agentBearerToken: prof.agentBearerToken || null,
  agentCaPem: prof.agentCaPem,
  agentClientCertPem: prof.agentClientCertPem,
  agentClientKeyPem: prof.agentClientKeyPem,
  coreKubeconfigPath: '/etc/rancher/rke2/rke2.yaml',
  coreContext: null,
  labKubeconfigPath: '/var/lib/pits/tinkerbell-20261108/private/talos-netboot-001-resume/kubeconfig',
  labContext: null,
  labViaAgentBridge: true,
  talosconfigPath: '/var/lib/pits/tinkerbell-20261108/private/talos-netboot-001-resume/talosconfig',
  talosContext: null,
  talosViaAgentBridge: true,
};
process.stdout.write(YAML.stringify(linuxProfile));
'@
    $env:PITS_PORTAL_DIR = $PortalDir
    $env:PITS_LOCAL_PROFILE = $LocalProfilePath
    $env:PITS_HAS_WS = if ($HasWorkspaceContracts) { "1" } else { "0" }
    $yamlBytes = $null
    try {
        $profileYamlStr = ($nodeHelper | & node.exe -)
        if ($LASTEXITCODE -ne 0) { throw "Node profile builder failed" }
        $yamlBytes = $Utf8NoBom.GetBytes(($profileYamlStr -join "`n").Replace("`r`n", "`n") + "`n")
    } finally {
        Remove-Item Env:PITS_PORTAL_DIR -ErrorAction SilentlyContinue
        Remove-Item Env:PITS_LOCAL_PROFILE -ErrorAction SilentlyContinue
        Remove-Item Env:PITS_HAS_WS -ErrorAction SilentlyContinue
    }
    Send-BytesOverSsh -Bytes $yamlBytes -RemotePath "/var/lib/pits/portal/profile.yaml" -ModeOctal "0600"
}

Write-Host "[5/6] Refreshing cached Cargo vendor (if Cargo.lock changed) & building Nix ELF on pits-core..." -ForegroundColor Cyan
$buildScript = @"
set -eu
install -d -m 0700 /var/lib/pits/portal
install -d -m 0755 /var/lib/pits/portal/bin
cd '$FlakeTrim'
if [ -f '$FlakeTrim/flake.nix' ]; then
  chown --reference='$FlakeTrim/flake.nix' \
    '$FlakeTrim/modules/pits-portal.nix' \
    '$FlakeTrim/modules/pits-portal-package.nix' || true
fi
git -c safe.directory='$FlakeTrim' -C '$FlakeTrim' add -N modules/pits-portal.nix modules/pits-portal-package.nix || true

lock_hash=`$(sha256sum /var/lib/pits/portal-src/client/Cargo.lock | awk '{print `$1}')
prev_hash=""
if [ -f /var/lib/pits/portal-vendor/.cargo-lock.sha256 ]; then
  prev_hash=`$(cat /var/lib/pits/portal-vendor/.cargo-lock.sha256)
fi

if [ "`$lock_hash" != "`$prev_hash" ] || [ ! -d /var/lib/pits/portal-vendor/anyhow ]; then
  echo "Updating /var/lib/pits/portal-vendor directly on pits-core (Cargo.lock=`$lock_hash)..."
  TMP_CARGO=`$(mktemp -d)
  trap 'rm -rf "`$TMP_CARGO"' EXIT
  export CARGO_HOME="`$TMP_CARGO"
  if [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
  fi
  unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY
  if curl -sf --max-time 5 https://mirrors.ustc.edu.cn/crates.io-index/config.json >/dev/null 2>&1; then
    cat > "`$CARGO_HOME/config.toml" <<'CARGOEOF'
[source.crates-io]
replace-with = "ustc"

[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
CARGOEOF
  elif ! curl -sf --max-time 5 https://index.crates.io/config.json >/dev/null 2>&1 && command -v ss >/dev/null 2>&1 && ss -ltn | grep -q ':7890 '; then
    export https_proxy="http://127.0.0.1:7890"
    export http_proxy="http://127.0.0.1:7890"
    export HTTPS_PROXY="http://127.0.0.1:7890"
    export HTTP_PROXY="http://127.0.0.1:7890"
  fi
  nix --extra-experimental-features 'nix-command flakes' shell --impure --no-warn-dirty \
    --expr "(let p = (builtins.getFlake \"path:$FlakeTrim\").nixosConfigurations.$FlakeAttr.pkgs; in p.symlinkJoin { name = \"pits-cargo-vendor-env\"; paths = [ p.cargo p.rustc ]; })" \
    -c cargo vendor --locked --manifest-path /var/lib/pits/portal-src/client/Cargo.toml /var/lib/pits/portal-vendor.new >/dev/null
  printf '%s\n' "`$lock_hash" > /var/lib/pits/portal-vendor.new/.cargo-lock.sha256
  chmod -R a+rX /var/lib/pits/portal-vendor.new
  rm -rf /var/lib/pits/portal-vendor
  mv /var/lib/pits/portal-vendor.new /var/lib/pits/portal-vendor
fi

export NIX_REMOTE=daemon
nix --extra-experimental-features 'nix-command flakes' build \
  --offline \
  --impure \
  --no-warn-dirty \
  --out-link /var/lib/pits/portal/current \
  --expr "((builtins.getFlake \"path:$FlakeTrim\").nixosConfigurations.$FlakeAttr.pkgs.callPackage $FlakeTrim/modules/pits-portal-package.nix { src = /var/lib/pits/portal-src; vendorSrc = /var/lib/pits/portal-vendor; })" || \
nix --extra-experimental-features 'nix-command flakes' build \
  --impure \
  --no-warn-dirty \
  --out-link /var/lib/pits/portal/current \
  --expr "((builtins.getFlake \"path:$FlakeTrim\").nixosConfigurations.$FlakeAttr.pkgs.callPackage $FlakeTrim/modules/pits-portal-package.nix { src = /var/lib/pits/portal-src; vendorSrc = /var/lib/pits/portal-vendor; })"

ln -sfn /var/lib/pits/portal/current/bin/pitsctl /var/lib/pits/portal/bin/pitsctl
/var/lib/pits/portal/bin/pitsctl serve --help >/dev/null
ls -lh /var/lib/pits/portal/current/bin/pitsctl
"@
$buildOut = Invoke-SshScript $buildScript
Write-Host ($buildOut.Trim())

Write-Host "[6/6] Activating declarative NixOS pits-portal.service & verifying live endpoints..." -ForegroundColor Cyan
$HostDefaultPath = "$FlakeTrim/hosts/core/default.nix"
$subCmd = if ($NoSwitch) { "test" } else { "switch" }

$currentDefault = (Invoke-SshScript "cat '$HostDefaultPath'").Replace("`r`n", "`n")
if (-not ($currentDefault -match 'pits-portal\.nix')) {
    Send-BytesOverSsh -Bytes ($Utf8NoBom.GetBytes($currentDefault)) -RemotePath "/var/lib/pits/portal/default.nix.bak" -ModeOctal "0600"
    if ($currentDefault -match '(?s)(.*?\bimports\s*=\s*\[)(.*)') {
        $patchedDefault = $Matches[1] + "`n    ../../modules/pits-portal.nix" + $Matches[2]
    } else {
        throw "Could not locate imports = [ ... ] block in $HostDefaultPath"
    }
    Send-BytesOverSsh -Bytes ($Utf8NoBom.GetBytes($patchedDefault)) -RemotePath $HostDefaultPath -ModeOctal "0644"
}

$activateScript = @"
set -eu
HOST_DEFAULT='$HostDefaultPath'
if [ -f '$FlakeTrim/flake.nix' ]; then
  chown --reference='$FlakeTrim/flake.nix' "`$HOST_DEFAULT" || true
fi

export NIX_REMOTE=daemon
if ! nixos-rebuild $subCmd --offline --flake 'path:$FlakeTrim#$FlakeAttr' && ! nixos-rebuild $subCmd --flake 'path:$FlakeTrim#$FlakeAttr'; then
  if [ -f /var/lib/pits/portal/default.nix.bak ]; then
    cp -f /var/lib/pits/portal/default.nix.bak "`$HOST_DEFAULT"
    chown --reference='$FlakeTrim/flake.nix' "`$HOST_DEFAULT" || true
  fi
  exit 1
fi

systemctl restart pits-portal.service
systemctl is-active pits-portal.service

i=0
while [ "`$i" -lt 40 ]; do
  if curl -sf http://127.0.0.1:$PortalPort/api/pits/v1/resource-types >/dev/null 2>&1; then
    break
  fi
  i=`$((i + 1))
  sleep 0.25
done
curl -sf http://127.0.0.1:$PortalPort/api/pits/v1/resource-types | head -c 400
echo ""
"@
$actOut = Invoke-SshScript $activateScript
Write-Host ($actOut.Trim())

Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
Remove-Item -Force $TarPath, $WsTarPath -ErrorAction SilentlyContinue

Write-Host "Waiting briefly for Wave-1 live catalog convergence & checking from workstation..." -ForegroundColor Cyan
Start-Sleep -Seconds 3
$rt = Invoke-RestMethod -Uri "http://${PortalHost}:${PortalPort}/api/pits/v1/resource-types" -TimeoutSec 10
Write-Host ("pits-core portal live! Resource types served: " + $rt.Count) -ForegroundColor Green
