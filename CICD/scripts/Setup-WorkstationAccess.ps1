<#
.SYNOPSIS
  Configures workstation (win11-001) DNS/hosts entries, Git `lab` remote, and verifies
  direct VLAN 20 access to Gitea, Zot OCI Registry, and Attic Nix Binary Cache.
#>
[CmdletBinding()]
param(
    [string]$LabIp = "10.89.20.21",
    [string]$Domain = "lab.pits.home.arpa",
    [string]$Kubeconfig = "",
    [switch]$PushMain
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}

$hostsPath = "$env:SystemRoot\System32\drivers\etc\hosts"
$entries = @(
    "git.$Domain",
    "oci.$Domain",
    "nix-cache.$Domain"
)

Write-Host "[1/4] Updating Windows hosts mappings for *.$Domain -> $LabIp..." -ForegroundColor Cyan
$currentHosts = Get-Content $hostsPath -Raw
$markerStart = "# PITS Lab CICD Start"
$markerEnd = "# PITS Lab CICD End"
$blockLines = @($markerStart) + ($entries | ForEach-Object { "$LabIp $_" }) + @($markerEnd)
$block = ($blockLines -join "`r`n")

$pattern = "(?s)# PITS Lab CICD Start.*?# PITS Lab CICD End"
if ($currentHosts -match $pattern) {
    $newHosts = [regex]::Replace($currentHosts, $pattern, $block)
} else {
    $newHosts = $currentHosts.TrimEnd() + "`r`n`r`n" + $block + "`r`n"
}

try {
    [System.IO.File]::WriteAllText($hostsPath, $newHosts, [System.Text.UTF8Encoding]::new($false))
    Write-Host "  Updated $hostsPath" -ForegroundColor Green
} catch {
    $entryList = ($entries -join " ")
    Write-Warning "Could not write $hostsPath directly ($($_.Exception.Message)). You can run elevated or add manually: $LabIp $entryList"
}

Write-Host "[2/4] Ensuring *.lab.martensite.dev bypasses Windows/Clash system proxy (for Tailscale Split DNS)..." -ForegroundColor Cyan
$inetPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
if (Test-Path $inetPath) {
    $curOverride = (Get-ItemProperty -Path $inetPath -Name ProxyOverride -ErrorAction SilentlyContinue).ProxyOverride
    $overrideItems = @()
    if (-not [string]::IsNullOrWhiteSpace($curOverride)) {
        $overrideItems = @($curOverride -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    }
    $changedOverride = $false
    foreach ($bypassHost in @("*.lab.martensite.dev", "lab.martensite.dev")) {
        if ($overrideItems -notcontains $bypassHost) {
            $overrideItems += $bypassHost
            $changedOverride = $true
        }
    }
    if ($changedOverride) {
        Set-ItemProperty -Path $inetPath -Name ProxyOverride -Value ($overrideItems -join ';')
        Write-Host "  Updated Windows ProxyOverride with *.lab.martensite.dev;lab.martensite.dev" -ForegroundColor Green
    } else {
        Write-Host "  Windows ProxyOverride already includes *.lab.martensite.dev" -ForegroundColor DarkGray
    }
}

Write-Host "[3/4] Configuring Git remote 'lab' -> http://git.${Domain}:3000/DissectDev/PITS-B1.git..." -ForegroundColor Cyan
Push-Location $RepoRoot
try {
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $existingLab = (& git remote get-url lab 2>$null)
    if ($LASTEXITCODE -ne 0) { $existingLab = "" }
    $ErrorActionPreference = $prevEap
    $targetUrl = "http://git.${Domain}:3000/DissectDev/PITS-B1.git"
    if ([string]::IsNullOrWhiteSpace($existingLab)) {
        & git remote add lab $targetUrl
    } elseif ($existingLab.Trim() -ne $targetUrl) {
        & git remote set-url lab $targetUrl
    }
    Write-Host "  git remote lab = $targetUrl" -ForegroundColor Green

    if ($PushMain) {
        if (-not (Test-Path $Kubeconfig)) { throw "Kubeconfig not found: $Kubeconfig" }
        $b64User = (& kubectl --kubeconfig $Kubeconfig -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_USER}').Trim()
        $b64Pass = (& kubectl --kubeconfig $Kubeconfig -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_PASS}').Trim()
        $giteaUser = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64User))
        $giteaPass = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64Pass))
        $escUser = [Uri]::EscapeDataString($giteaUser)
        $escPass = [Uri]::EscapeDataString($giteaPass)
        $pushUrl = "http://${escUser}:${escPass}@${LabIp}:30300/DissectDev/PITS-B1.git"
        Write-Host "  Pushing HEAD:main to Lab Gitea..." -ForegroundColor Cyan
        & git push $pushUrl HEAD:refs/heads/main
        if ($LASTEXITCODE -ne 0) { throw "git push to Lab Gitea failed" }
    }
} finally {
    Pop-Location
}

Write-Host "[4/4] Checking Lab CICD endpoints..." -ForegroundColor Cyan
foreach ($ep in @(
    @{ Name = "Gitea"; Url = "http://${LabIp}:30300/api/v1/version" },
    @{ Name = "Zot OCI"; Url = "http://${LabIp}:30500/v2/" },
    @{ Name = "Attic"; Url = "http://${LabIp}:30808/" }
)) {
    try {
        $resp = Invoke-WebRequest -Uri $ep.Url -TimeoutSec 5 -UseBasicParsing
        Write-Host ("  [OK] {0} ({1}) -> HTTP {2}" -f $ep.Name, $ep.Url, $resp.StatusCode) -ForegroundColor Green
    } catch {
        Write-Warning ("  [WARN] {0} ({1}) -> {2}" -f $ep.Name, $ep.Url, $_.Exception.Message)
    }
}
