<#
.SYNOPSIS
  Single-entry CI/CD trigger for PITS Infra: pushes either the committed HEAD or a
  working-tree snapshot (`-Snapshot`) to Lab Gitea, waits for Gitea Actions `act_runner`
  workflows to pass, and optionally runs CD promotion (`-DeployCore`).
#>
[CmdletBinding()]
param(
    [string]$Branch = "main",
    [string]$Kubeconfig = "",
    [string]$LabIp = "10.89.20.21",
    [switch]$Snapshot,
    [switch]$Force,
    [switch]$AllowMissingPlatform,
    [switch]$DeployCore,
    [ValidateSet("all", "portal", "agent", "test-runner", "cicd-runner", "windows", "base")]
    [string]$DeployTarget = "all",
    [int]$TimeoutSeconds = 600
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}
$env:KUBECONFIG = $Kubeconfig

$b64User = (& kubectl -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_USER}').Trim()
$b64Pass = (& kubectl -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_PASS}').Trim()
$u = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64User))
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64Pass))
$hdr = @{ Authorization = "Basic " + [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("${u}:${p}")) }

Push-Location $RepoRoot
try {
    $escU = [Uri]::EscapeDataString($u)
    $escP = [Uri]::EscapeDataString($p)
    $pushUrl = "http://${escU}:${escP}@${LabIp}:30300/DissectDev/PITS-B1.git"
    $readUrl = "http://${LabIp}:30300/DissectDev/PITS-B1.git"

    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $remoteTip = ""
    $lsRemote = (& git ls-remote $pushUrl "refs/heads/$Branch" 2>$null)
    if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($lsRemote)) {
        $remoteTip = ($lsRemote -split '\s+')[0].Trim()
    }
    $ErrorActionPreference = $prevEap

    $sha = (& git rev-parse HEAD).Trim()
    if ($Snapshot) {
        Write-Host "[1/3] Creating clean working-tree snapshot commit (without altering local HEAD or index)..." -ForegroundColor Cyan
        $tmpIdx = Join-Path $env:TEMP ("pits-cicd-idx-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
        $prevIdx = $env:GIT_INDEX_FILE
        try {
            $parentSha = $sha
            $ErrorActionPreference = "Continue"
            if (-not [string]::IsNullOrWhiteSpace($remoteTip)) {
                & git fetch $pushUrl "refs/heads/$Branch" --quiet 2>$null
                if ($LASTEXITCODE -eq 0) {
                    $parentSha = (& git rev-parse FETCH_HEAD).Trim()
                }
            }
            $env:GIT_INDEX_FILE = $tmpIdx
            & git read-tree $parentSha
            if ($LASTEXITCODE -ne 0) { throw "git read-tree failed" }

            & git -c core.autocrlf=false add -A -- `
                CICD .github .agents deploy docs catalog Base README.md AGENTS.md .gitignore .gitattributes `
                ai-storage/Cargo.toml ai-storage/Cargo.lock ai-storage/README.md ai-storage/.ai-store.example.yaml ai-storage/src ai-storage/tests ai-storage/consumers `
                client/Cargo.toml client/Cargo.lock client/build.rs client/src `
                server/go.mod server/go.sum server/cmd server/internal `
                portal/package.json portal/package-lock.json portal/tsconfig.json portal/vite.config.ts portal/index.html portal/src `
                test-pool/devspace.yaml test-pool/README.md test-pool/manifests test-pool/scripts test-pool/image 2>&1 | Out-Null
            $treeSha = (& git write-tree).Trim()
            if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($treeSha)) { throw "git write-tree failed" }

            $parentTree = (& git rev-parse "${parentSha}^{tree}").Trim()
            if ($treeSha -eq $parentTree) {
                $sha = $parentSha
                Write-Host "  Working tree already matches Lab Gitea $Branch ($sha)." -ForegroundColor DarkGray
            } else {
                $snapSha = (& git commit-tree $treeSha -p $parentSha -m "chore(cicd): workstation snapshot from $sha").Trim()
                if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($snapSha)) { throw "git commit-tree failed" }
                $sha = $snapSha
                Write-Host "  Pushing snapshot $sha -> Lab Gitea ($Branch)..." -ForegroundColor Cyan
                & git push $pushUrl "${sha}:refs/heads/$Branch"
                if ($LASTEXITCODE -ne 0) { throw "git push snapshot to Lab Gitea failed" }
            }
            $ErrorActionPreference = $prevEap
        } finally {
            $ErrorActionPreference = $prevEap
            if ($null -eq $prevIdx) { Remove-Item Env:GIT_INDEX_FILE -ErrorAction SilentlyContinue }
            else { $env:GIT_INDEX_FILE = $prevIdx }
            Remove-Item -Force $tmpIdx -ErrorAction SilentlyContinue
        }
    } else {
        if ($Branch -eq "main" -and -not $AllowMissingPlatform) {
            $ErrorActionPreference = "Continue"
            & git cat-file -e "${sha}:CICD/platform/kustomization.yaml" 2>$null
            $hasPlat = ($LASTEXITCODE -eq 0)
            $ErrorActionPreference = $prevEap
            if (-not $hasPlat) {
                throw "Local HEAD ($sha) does not contain CICD/platform/kustomization.yaml yet (pushing it to 'main' would cause Flux CD prune to delete LabDevStack). Either commit your CICD/ changes locally first, or run with -Snapshot to test your uncommitted working tree safely."
            }
        }

        if (-not [string]::IsNullOrWhiteSpace($remoteTip) -and $remoteTip -ne $sha -and -not $Force) {
            $ErrorActionPreference = "Continue"
            & git fetch $pushUrl "refs/heads/$Branch" --quiet 2>$null
            & git merge-base --is-ancestor $remoteTip $sha 2>$null
            $isAnc = ($LASTEXITCODE -eq 0)
            $ErrorActionPreference = $prevEap
            if (-not $isAnc) {
                throw "Local HEAD ($sha) is behind or diverged from Lab Gitea $Branch ($remoteTip). Run with -Snapshot to push your current workspace onto Lab Gitea $Branch, or pass -Force after committing locally to reset Lab Gitea $Branch to your local HEAD."
            }
        }

        Write-Host "[1/3] Pushing $sha to Lab Gitea ($Branch)..." -ForegroundColor Cyan
        $pushArgs = @("push")
        if ($Force) { $pushArgs += "--force" }
        $pushArgs += @($pushUrl, "HEAD:refs/heads/$Branch")
        $ErrorActionPreference = "Continue"
        & git @pushArgs
        $pushRc = $LASTEXITCODE
        $ErrorActionPreference = $prevEap
        if ($pushRc -ne 0) { throw "git push to Lab Gitea failed" }
    }

    Write-Host "[2/3] Waiting for Gitea Actions runs for commit $sha..." -ForegroundColor Cyan
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $seenRuns = $false
    while ((Get-Date) -lt $deadline) {
        $runsResp = Invoke-RestMethod -Uri "http://${LabIp}:30300/api/v1/repos/DissectDev/PITS-B1/actions/runs?limit=20" -Headers $hdr -TimeoutSec 10
        $runs = @($runsResp.workflow_runs | Where-Object { $_.head_sha -eq $sha })
        if ($runs.Count -gt 0) {
            $seenRuns = $true
            $pending = @($runs | Where-Object { $_.status -notin @("completed", "success", "failure", "cancelled", "skipped") })
            $failed  = @($runs | Where-Object { $_.conclusion -in @("failure", "cancelled", "timed_out") -or $_.status -eq "failure" })
            Write-Host ("  runs={0} pending={1} failed={2}" -f $runs.Count, $pending.Count, $failed.Count)
            if ($failed.Count -gt 0) {
                $failed | ForEach-Object { Write-Host ("  [FAIL] {0}: status={1} conclusion={2}" -f $_.name, $_.status, $_.conclusion) -ForegroundColor Red }
                throw "One or more Gitea Actions workflow runs failed for $sha"
            }
            if ($pending.Count -eq 0) {
                Write-Host "  All $($runs.Count) workflow runs passed for $sha!" -ForegroundColor Green
                break
            }
        }
        Start-Sleep -Seconds 5
    }
    if (-not $seenRuns) {
        Write-Warning "No Gitea Actions runs matched SHA $sha within timeout (maybe paths filter did not trigger any workflow)."
    }

    if ($DeployCore) {
        Write-Host "[3/3] Handing off to Deploy-CoreArtifacts.ps1 -Target $DeployTarget..." -ForegroundColor Cyan
        & (Join-Path $PSScriptRoot "Deploy-CoreArtifacts.ps1") -Target $DeployTarget -SkipVerify
        if ($LASTEXITCODE -ne 0) { throw "Deploy-CoreArtifacts.ps1 failed" }
    }
} finally {
    Pop-Location
}
