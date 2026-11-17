<#
.SYNOPSIS
  Single-entry parallel verification trigger for `cluster-pits-lab` (`pits-test-pool`),
  powered by CNCF DevSpace (`v6.3.21`) + Kubernetes SIG Apps `agent-sandbox` (`v1.0.11`).

.DESCRIPTION
  - Default mode: Runs incremental binary-diff sync + parallel execution across the 3 dedicated
    DDR5 `tmpfs` Warm Pool pods (`netboot-001` Rust, `netboot-002` Go-Linux, `netboot-003` Portal-E2E;
    strictly avoiding `netboot-004` where `win11-001` runs).
  - `-IncludeHardware`: Adds Shard 4 (`shard-npu-dra` Intel NPU DRA v1 hardware claim verification).
  - `-ClaimSandbox`: Allocates a dedicated isolated `tmpfs` Pod from `SandboxWarmPool` (`pits-agent-warmpool`)
    in <100ms via `SandboxClaim` — ideal when multiple `/teamwork` or Verifier Subagents run concurrently.
  - Emits a structured JSON summary for Antigravity / Verifier Subagents.
#>
[CmdletBinding()]
param(
    [ValidateSet("all", "rust", "go-linux", "portal-e2e", "npu-dra")]
    [string]$Shard = "all",
    [string]$ClientDir = "c:\Project\PITS\Infra",
    [string]$Kubeconfig = "",
    [switch]$IncludeHardware,
    [switch]$ClaimSandbox,
    [switch]$DeployPool
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$PoolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Namespace = "pits-test-pool"

if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}
if (-not (Test-Path $Kubeconfig)) {
    throw "Lab kubeconfig not found at $Kubeconfig"
}

$env:KUBECONFIG = $Kubeconfig
$resolvedClient = (Resolve-Path $ClientDir).Path
# Convert Windows drive-letter path (e.g. C:\Project\PITS\Client) to relative POSIX path from $PoolRoot
# so `devspace sync --path <local>:<remote>` never confuses `C:` with the `:` path separator!
$poolUri = New-Object System.Uri(($PoolRoot.TrimEnd('\') + '\'))
$clientUri = New-Object System.Uri(($resolvedClient.TrimEnd('\') + '\'))
$relClientPosix = [System.Uri]::UnescapeDataString($poolUri.MakeRelativeUri($clientUri).ToString()).TrimEnd('/')
if ([string]::IsNullOrWhiteSpace($relClientPosix)) { $relClientPosix = "." }
$overallSw = [System.Diagnostics.Stopwatch]::StartNew()

Push-Location $PoolRoot
try {
    if ($DeployPool) {
        & devspace run-pipeline deploy -n $Namespace --no-warn
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        return
    }

    if ($ClaimSandbox) {
        $claimName = "verify-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)
        $claimYaml = @"
apiVersion: extensions.agents.x-k8s.io/v1beta1
kind: SandboxClaim
metadata:
  name: $claimName
  namespace: $Namespace
spec:
  lifecycle:
    shutdownPolicy: Delete
  warmPoolRef:
    name: pits-agent-warmpool
"@
        $prevEap = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            $claimYaml | & kubectl apply -f - | Out-Null
            & kubectl -n $Namespace wait --for=condition=Ready "sandboxclaim/$claimName" --timeout=30s | Out-Null
            $sandboxName = (& kubectl -n $Namespace get "sandboxclaim/$claimName" -o jsonpath='{.status.sandbox.name}').Trim()
            if ([string]::IsNullOrWhiteSpace($sandboxName)) {
                throw "SandboxClaim $claimName became Ready without status.sandbox.name"
            }
            $podSelector = (& kubectl -n $Namespace get "sandbox/$sandboxName" -o jsonpath='{.status.selector}' 2>$null).Trim()
            $podName = ""
            if (-not [string]::IsNullOrWhiteSpace($podSelector)) {
                $podName = (& kubectl -n $Namespace get pods -l $podSelector -o jsonpath='{.items[0].metadata.name}' 2>$null).Trim()
            }
            if ([string]::IsNullOrWhiteSpace($podName)) {
                $podName = $sandboxName
            }
            $targets = if ($Shard -eq "all") { @("rust", "go-linux", "portal-e2e") } else { @($Shard) }
            if ($targets -contains "rust" -or $targets -contains "portal-e2e") {
                & devspace sync -n $Namespace --no-warn --silent --pick=false --no-watch --upload-only --initial-sync=mirrorLocal `
                    --pod $podName --path "$relClientPosix/deploy:/ramdisk/work/deploy" 2>&1 | Out-Null
                & devspace sync -n $Namespace --no-warn --silent --pick=false --no-watch --upload-only --initial-sync=mirrorLocal `
                    --pod $podName -e "target/**" -e "bin/**" -e "*.exe" -e "*.pdb" `
                    --path "$relClientPosix/client:/ramdisk/work/client" 2>&1 | Out-Null
            }
            if ($targets -contains "go-linux") {
                & devspace sync -n $Namespace --no-warn --silent --pick=false --no-watch --upload-only --initial-sync=mirrorLocal `
                    --pod $podName -e "vendor/**" -e "bin/**" -e "*.exe" `
                    --path "$relClientPosix/server:/ramdisk/work/server" 2>&1 | Out-Null
            }
            if ($targets -contains "portal-e2e") {
                & devspace sync -n $Namespace --no-warn --silent --pick=false --no-watch --upload-only --initial-sync=mirrorLocal `
                    --pod $podName -e "node_modules/**" -e "dist/**" `
                    --path "$relClientPosix/portal:/ramdisk/work/portal" 2>&1 | Out-Null
            }

            $reports = @()
            $anyFailed = $false
            foreach ($t in $targets) {
                if ($t -eq "npu-dra") { continue }
                $rawJson = & kubectl -n $Namespace exec $podName -- /usr/local/bin/runner-verify-shard $t 2>$null
                if ($LASTEXITCODE -ne 0) { $anyFailed = $true }
                if ($rawJson) {
                    $reports += ($rawJson | Out-String | ConvertFrom-Json)
                }
            }
            $ErrorActionPreference = $prevEap
            $overallSw.Stop()
            $final = [ordered]@{
                mode            = "agent-sandbox-claim"
                claim           = $claimName
                pod             = $podName
                overallStatus   = if ($anyFailed) { "failed" } else { "passed" }
                totalDurationMs = $overallSw.ElapsedMilliseconds
                shards          = $reports
            }
            $final | ConvertTo-Json -Depth 6 | Write-Output
            if ($anyFailed) { exit 1 }
            exit 0
        } finally {
            $ErrorActionPreference = "Continue"
            & kubectl -n $Namespace delete "sandboxclaim/$claimName" --ignore-not-found --wait=false 2>$null | Out-Null
            $ErrorActionPreference = $prevEap
        }
    }

    # Standard 3-node parallel Warm Pool execution via CNCF DevSpace pipeline
    $pipelineName = switch ($Shard) {
        "all"        { if ($IncludeHardware) { "verify-full" } else { "verify" } }
        "rust"       { "shard-rust" }
        "go-linux"   { "shard-go" }
        "portal-e2e" { "shard-portal" }
        "npu-dra"    { "shard-npu-dra" }
    }

    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $pipeOutput = & devspace run-pipeline $pipelineName -n $Namespace --no-warn --no-colors --var "CLIENT_DIR=$relClientPosix" 2>&1
    $pipeExit = $LASTEXITCODE

    # Fetch structured per-shard JSON reports from `/ramdisk/reports/<shard>.json`
    $expectedShards = switch ($Shard) {
        "all"        { @("rust", "go-linux", "portal-e2e") }
        "npu-dra"    { @() }
        default      { @($Shard) }
    }

    $shardResults = @()
    foreach ($s in $expectedShards) {
        $jsonRaw = & kubectl -n $Namespace exec "deploy/runner-$s" -- cat "/ramdisk/reports/$s.json" 2>$null
        if ($LASTEXITCODE -eq 0 -and $jsonRaw) {
            try {
                $shardResults += ($jsonRaw | Out-String | ConvertFrom-Json)
            } catch { }
        }
    }
    $ErrorActionPreference = $prevEap
    $overallSw.Stop()

    if ($Shard -eq "npu-dra" -or $IncludeHardware) {
        $npuLine = ($pipeOutput | ForEach-Object { "$_" } | Where-Object { $_ -match '"shard"\s*:\s*"npu-dra"' } | Select-Object -Last 1)
        if ($npuLine) {
            try { $shardResults += ($npuLine | ConvertFrom-Json) } catch { }
        } else {
            # Reconstruct multi-line jq JSON if printed across lines
            $joined = ($pipeOutput -join "`n")
            if ($joined -match '(\{\s*"shard":\s*"npu-dra"[\s\S]*?\})') {
                try { $shardResults += ($Matches[1] | ConvertFrom-Json) } catch { }
            }
        }
    }

    $anyShardFailed = ($shardResults | Where-Object { $_.status -ne "passed" }).Count -gt 0
    $effectiveExit = if ($pipeExit -ne 0) { $pipeExit } elseif ($anyShardFailed) { 1 } else { 0 }

    $resultObj = [ordered]@{
        mode            = "devspace-warm-pool"
        pipeline        = $pipelineName
        overallStatus   = if ($effectiveExit -eq 0) { "passed" } else { "failed" }
        exitCode        = $effectiveExit
        totalDurationMs = $overallSw.ElapsedMilliseconds
        shards          = $shardResults
    }
    if ($effectiveExit -ne 0 -and $shardResults.Count -eq 0) {
        $resultObj["rawOutputTail"] = ($pipeOutput | Select-Object -Last 40) -join "`n"
    }

    $resultObj | ConvertTo-Json -Depth 6 | Write-Output
    exit $effectiveExit
} finally {
    Pop-Location
}
