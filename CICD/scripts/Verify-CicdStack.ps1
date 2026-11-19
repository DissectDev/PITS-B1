<#
.SYNOPSIS
  Verifies all health, storage, network, runner, Flux, Crossplane, and netboot-004 exclusion
  invariants of the PITS Lab CI/CD stack, emitting a structured JSON report.
#>
[CmdletBinding()]
param(
    [string]$Kubeconfig = "",
    [string]$LabIp = "10.89.20.21"
)

$ErrorActionPreference = "Stop"
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}
$env:KUBECONFIG = $Kubeconfig

$checks = @()
$allOk = $true

function Add-Check([string]$Name, [bool]$Passed, [string]$Detail) {
    if (-not $Passed) { $script:allOk = $false }
    $script:checks += [ordered]@{
        name   = $Name
        passed = $Passed
        detail = $Detail
    }
}

function Invoke-KubectlJson([string[]]$KubectlArgs) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $out = & kubectl @KubectlArgs 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $out) { return $null }
        return (($out -join "`n") | ConvertFrom-Json)
    } finally {
        $ErrorActionPreference = $prev
    }
}

# 1. Verify zero CI/CD & Ambient Gateway pods scheduled on netboot-004 (win11-001 host)
$cicdNamespaces = @("flux-system", "crossplane-system", "cnpg-system", "pits-cicd", "pits-test-pool", "istio-system", "ztunnel", "iam-system", "cert-manager", "ai-storage-pits")
$podsOn004 = @()
foreach ($ns in $cicdNamespaces) {
    $podJson = Invoke-KubectlJson @("-n", $ns, "get", "pods", "-o", "json")
    if ($podJson -and $podJson.items) {
        foreach ($p in $podJson.items) {
            if ($p.spec.nodeName -eq "netboot-004") {
                $podsOn004 += "$ns/$($p.metadata.name)"
            }
        }
    }
}
Add-Check "netboot-004-exclusion" ($podsOn004.Count -eq 0) $(if ($podsOn004.Count -eq 0) { "0 pods on netboot-004" } else { "Found pods on netboot-004: $($podsOn004 -join ', ')" })

# 2. Crossplane Providers, Function, and LabDevStack Claim
$xpClaim = Invoke-KubectlJson @("-n", "pits-cicd", "get", "labdevstack", "pits-lab-acceleration", "-o", "json")
$xpSynced = $false; $xpReady = $false
if ($xpClaim -and $xpClaim.status.conditions) {
    foreach ($c in $xpClaim.status.conditions) {
        if ($c.type -eq "Synced" -and $c.status -eq "True") { $xpSynced = $true }
        if ($c.type -eq "Ready" -and $c.status -eq "True") { $xpReady = $true }
    }
}
Add-Check "crossplane-labdevstack" ($xpSynced -and $xpReady) "Synced=$xpSynced Ready=$xpReady"

# 3. CNPG Postgres Cluster (2 instances on mayastor-2)
$pg = Invoke-KubectlJson @("-n", "pits-cicd", "get", "clusters.postgresql.cnpg.io", "gitea-pg", "-o", "json")
$pgReady = if ($pg) { [int]$pg.status.readyInstances } else { 0 }
Add-Check "cnpg-postgres" ($pgReady -eq 2) "readyInstances=$pgReady/2"

# 4. Zot OCI Registry endpoint
$zotOk = $false; $zotDetail = ""
try {
    $r = Invoke-WebRequest -Uri "http://${LabIp}:30500/v2/" -TimeoutSec 5 -UseBasicParsing
    $zotOk = ($r.StatusCode -eq 200)
    $zotDetail = "HTTP $($r.StatusCode) at :30500/v2/"
} catch { $zotDetail = $_.Exception.Message }
Add-Check "zot-registry" $zotOk $zotDetail

# 5. Attic Nix Binary Cache endpoint
$atticOk = $false; $atticDetail = ""
try {
    $r = Invoke-WebRequest -Uri "http://${LabIp}:30808/" -TimeoutSec 5 -UseBasicParsing
    $atticOk = ($r.StatusCode -eq 200)
    $atticDetail = "HTTP $($r.StatusCode) at :30808/"
} catch { $atticDetail = $_.Exception.Message }
Add-Check "attic-nix-cache" $atticOk $atticDetail

# 6. Gitea API + Repo + Online act_runner count
$giteaOk = $false; $giteaDetail = ""
$runnersOnline = 0
try {
    $b64User = (& kubectl -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_USER}').Trim()
    $b64Pass = (& kubectl -n pits-cicd get secret lab-devstack-secrets -o jsonpath='{.data.GITEA_ADMIN_PASS}').Trim()
    $u = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64User))
    $p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64Pass))
    $hdr = @{ Authorization = "Basic " + [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("${u}:${p}")) }
    $ver = Invoke-RestMethod -Uri "http://${LabIp}:30300/api/v1/version" -TimeoutSec 5
    $repo = Invoke-RestMethod -Uri "http://${LabIp}:30300/api/v1/repos/DissectDev/PITS-B1" -Headers $hdr -TimeoutSec 5
    $runnersResp = Invoke-RestMethod -Uri "http://${LabIp}:30300/api/v1/admin/actions/runners" -Headers $hdr -TimeoutSec 5
    $runnersList = if ($runnersResp.runners) { $runnersResp.runners } else { $runnersResp }
    $online = @($runnersList | Where-Object { $_.status -eq "online" })
    $runnersOnline = $online.Count
    $giteaOk = ($null -ne $ver.version -and $repo.full_name -eq "DissectDev/PITS-B1")
    $giteaDetail = "version=$($ver.version) repo=$($repo.full_name) onlineRunners=$runnersOnline"
} catch { $giteaDetail = $_.Exception.Message }
Add-Check "gitea-api-and-repo" $giteaOk $giteaDetail
Add-Check "gitea-act-runners" ($runnersOnline -ge 5) "online=$runnersOnline (expected >=5: 3 cpu + 1 npu + 1 gpu)"

# 7. Flux CD GitRepository & Kustomization Ready
$fluxRepo = Invoke-KubectlJson @("-n", "flux-system", "get", "gitrepository", "lab-platform-repo", "-o", "json")
$fluxKust = Invoke-KubectlJson @("-n", "flux-system", "get", "kustomization", "lab-cicd-platform", "-o", "json")
$fluxRepoReady = @($fluxRepo.status.conditions | Where-Object { $_.type -eq "Ready" -and $_.status -eq "True" }).Count -gt 0
$fluxKustReady = @($fluxKust.status.conditions | Where-Object { $_.type -eq "Ready" -and $_.status -eq "True" }).Count -gt 0
Add-Check "flux-gitops" ($fluxRepoReady -and $fluxKustReady) "GitRepositoryReady=$fluxRepoReady KustomizationReady=$fluxKustReady"

# 8. Istio Ambient + Keycloak ext_authz Gateway readiness
$hrStates = @()
$meshReady = $true
foreach ($hrRef in @(
    @{ Ns = "cert-manager"; Name = "cert-manager" },
    @{ Ns = "istio-system"; Name = "istio-base" },
    @{ Ns = "istio-system"; Name = "istiod" },
    @{ Ns = "istio-system"; Name = "istio-cni" },
    @{ Ns = "ztunnel"; Name = "ztunnel" }
)) {
    $hr = Invoke-KubectlJson @("-n", $hrRef.Ns, "get", "helmrelease.helm.toolkit.fluxcd.io", $hrRef.Name, "-o", "json")
    $ok = $false
    if ($hr -and $hr.status.conditions) {
        $ok = @($hr.status.conditions | Where-Object { $_.type -eq "Ready" -and $_.status -eq "True" }).Count -gt 0
    }
    if (-not $ok) { $meshReady = $false }
    $hrStates += "$($hrRef.Name)=$ok"
}
Add-Check "istio-ambient-helm" $meshReady ($hrStates -join " ")

$gw = Invoke-KubectlJson @("-n", "istio-system", "get", "gateway.gateway.networking.k8s.io", "lab-zero-trust-gateway", "-o", "json")
$gwProg = $false
if ($gw -and $gw.status.conditions) {
    $gwProg = @($gw.status.conditions | Where-Object { $_.type -eq "Programmed" -and $_.status -eq "True" }).Count -gt 0
}
$cert = Invoke-KubectlJson @("-n", "istio-system", "get", "certificate.cert-manager.io", "lab-gateway-tls", "-o", "json")
$certReady = $false
if ($cert -and $cert.status.conditions) {
    $certReady = @($cert.status.conditions | Where-Object { $_.type -eq "Ready" -and $_.status -eq "True" }).Count -gt 0
}
$oauthDep = Invoke-KubectlJson @("-n", "iam-system", "get", "deploy", "oauth2-proxy", "-o", "json")
$echoDep = Invoke-KubectlJson @("-n", "iam-system", "get", "deploy", "lab-auth-echo", "-o", "json")
$oauthReady = if ($oauthDep -and $oauthDep.status.readyReplicas) { "$($oauthDep.status.readyReplicas)" } else { "0" }
$echoReady = if ($echoDep -and $echoDep.status.readyReplicas) { "$($echoDep.status.readyReplicas)" } else { "0" }
$gwReady = ($gwProg -and $certReady -and $oauthReady -eq "1" -and $echoReady -eq "1")
Add-Check "istio-zero-trust-gateway" $gwReady "GatewayProgrammed=$gwProg CertReady=$certReady oauth2-proxy=$oauthReady/1 lab-auth-echo=$echoReady/1"

# 9. Crossplane AIStorageWorkspace (OpenDAL 5-layer MCP Producer)
$aiClaim = Invoke-KubectlJson @("-n", "pits-cicd", "get", "aistorageworkspace", "pits-agent-space", "-o", "json")
$aiSynced = $false; $aiReady = $false; $aiEndpoint = ""
if ($aiClaim -and $aiClaim.status.conditions) {
    foreach ($c in $aiClaim.status.conditions) {
        if ($c.type -eq "Synced" -and $c.status -eq "True") { $aiSynced = $true }
        if ($c.type -eq "Ready" -and $c.status -eq "True") { $aiReady = $true }
    }
    $aiEndpoint = "$($aiClaim.status.mcpEndpoint)"
}
Add-Check "crossplane-ai-storage" ($aiSynced -and $aiReady) "Synced=$aiSynced Ready=$aiReady endpoint=$aiEndpoint"

$report = [ordered]@{
    overallStatus = if ($allOk) { "passed" } else { "failed" }
    checks        = $checks
}
$report | ConvertTo-Json -Depth 5 | Write-Output
if (-not $allOk) { exit 1 }
