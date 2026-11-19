<#
.SYNOPSIS
  End-to-end 5-step bootstrap for the PITS Lab CI/CD & Crossplane stack on `cluster-pits-lab`.
  Follows `docs/lab-cicd-crossplane-design.md` with strict `netboot-004` exclusion and
  zero impact on `pits-core` hardware Flux CD (`Base/`).
#>
[CmdletBinding()]
param(
    [string]$Kubeconfig = "",
    [string]$SshTarget = "root@100.95.95.88",
    [string]$GiteaAdminUser = "pitsadmin",
    [string]$GithubMirrorPat = $env:GITHUB_MIRROR_PAT,
    [switch]$SkipImageImport,
    [switch]$SkipWorkstationPush
)

$ErrorActionPreference = "Stop"
$CicdRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}
if (-not (Test-Path $Kubeconfig)) {
    throw "Lab kubeconfig not found at $Kubeconfig"
}
$env:KUBECONFIG = $Kubeconfig

function New-RandomHex([int]$ByteCount = 24) {
    $buf = New-Object byte[] $ByteCount
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($buf)
    return ($buf | ForEach-Object { $_.ToString("x2") }) -join ""
}

function New-RandomBase64([int]$ByteCount = 64) {
    $buf = New-Object byte[] $ByteCount
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($buf)
    return [Convert]::ToBase64String($buf)
}

Write-Host "=== Step 1/5: Mirroring OCI images & building cicd-runner ===" -ForegroundColor Cyan
if (-not $SkipImageImport) {
    & (Join-Path $PSScriptRoot "Import-CicdImages.ps1") -SshTarget $SshTarget
    if ($LASTEXITCODE -ne 0) { throw "Import-CicdImages.ps1 failed" }
}

Write-Host "=== Step 2/5: Installing Lab Flux CD & Crossplane v2.4.3 ===" -ForegroundColor Cyan
& kubectl apply -f (Join-Path $CicdRoot "bootstrap\gotk-components.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply Flux gotk-components.yaml" }

foreach ($dep in @("source-controller", "kustomize-controller", "helm-controller", "notification-controller")) {
    & kubectl -n flux-system rollout status "deployment/$dep" --timeout=180s
    if ($LASTEXITCODE -ne 0) { throw "Flux controller $dep failed to become Ready" }
}

# Install Crossplane via Helm (using local proxy if reachable for chart repo index)
if (Test-NetConnection -ComputerName 127.0.0.1 -Port 7897 -InformationLevel Quiet -WarningAction SilentlyContinue) {
    $env:HTTP_PROXY = "http://127.0.0.1:7897"
    $env:HTTPS_PROXY = "http://127.0.0.1:7897"
}
& helm repo add crossplane-stable https://charts.crossplane.io/stable --force-update | Out-Null
& helm repo update crossplane-stable | Out-Null
& helm upgrade --install crossplane crossplane-stable/crossplane `
    --version 2.4.3 `
    --namespace crossplane-system `
    --create-namespace `
    --values (Join-Path $CicdRoot "crossplane\core\values.yaml") `
    --wait --timeout 5m
if ($LASTEXITCODE -ne 0) { throw "Helm install/upgrade of Crossplane failed" }

& kubectl apply -f (Join-Path $CicdRoot "crossplane\core\providers.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply Crossplane providers.yaml" }

Write-Host "Waiting for Crossplane Providers & Function to become Healthy..." -ForegroundColor Cyan
& kubectl wait --for=condition=Healthy provider.pkg.crossplane.io/provider-helm provider.pkg.crossplane.io/provider-kubernetes function.pkg.crossplane.io/function-go-templating --timeout=300s
if ($LASTEXITCODE -ne 0) { throw "Crossplane providers/function did not become Healthy within 300s" }

& kubectl apply -f (Join-Path $CicdRoot "crossplane\core\providerconfigs.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply Crossplane providerconfigs.yaml" }

Write-Host "=== Step 3/5: Ensuring runtime Secrets (not committed to Git) ===" -ForegroundColor Cyan
$nsYaml = @"
apiVersion: v1
kind: Namespace
metadata:
  name: pits-cicd
  labels:
    app.kubernetes.io/part-of: pits-cicd
    pod-security.kubernetes.io/enforce: baseline
    pod-security.kubernetes.io/warn: baseline
"@
$nsYaml | & kubectl apply -f - | Out-Null

function Invoke-KubectlOptional([string[]]$KubectlArgs) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $out = & kubectl @KubectlArgs 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }
        return ($out -join "`n")
    } finally {
        $ErrorActionPreference = $prev
    }
}

$existingSecretRaw = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "secret", "lab-devstack-secrets", "-o", "json")
$existingSecret = if ($existingSecretRaw) { $existingSecretRaw | ConvertFrom-Json } else { $null }
if ($null -ne $existingSecret) {
    $giteaUser = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.GITEA_ADMIN_USER))
    $giteaPass = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.GITEA_ADMIN_PASS))
    $webhookSecret = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.FLUX_WEBHOOK_SECRET))
    $atticToken = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.ATTIC_TOKEN))
    $atticSigningKey = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.ATTIC_SIGNING_KEY_BASE64))
    if ([string]::IsNullOrWhiteSpace($GithubMirrorPat) -and $existingSecret.data.GITHUB_MIRROR_PAT) {
        $GithubMirrorPat = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($existingSecret.data.GITHUB_MIRROR_PAT))
    }
    Write-Host "  Reusing existing pits-cicd/lab-devstack-secrets." -ForegroundColor DarkGray
} else {
    $giteaUser = $GiteaAdminUser
    $giteaPass = New-RandomHex 16
    $webhookSecret = New-RandomHex 24
    $atticToken = New-RandomHex 24
    $atticSigningKey = New-RandomBase64 64
    Write-Host "  Generated fresh credentials for pits-cicd/lab-devstack-secrets." -ForegroundColor Green
}
if ($null -eq $GithubMirrorPat) { $GithubMirrorPat = "" }

& kubectl -n pits-cicd create secret generic lab-devstack-secrets `
    --from-literal=GITEA_ADMIN_USER=$giteaUser `
    --from-literal=GITEA_ADMIN_PASS=$giteaPass `
    --from-literal=GITHUB_MIRROR_PAT=$GithubMirrorPat `
    --from-literal=FLUX_WEBHOOK_SECRET=$webhookSecret `
    --from-literal=ATTIC_TOKEN=$atticToken `
    --from-literal=ATTIC_SIGNING_KEY_BASE64=$atticSigningKey `
    --dry-run=client -o yaml | & kubectl apply -f -
if ($LASTEXITCODE -ne 0) { throw "Failed to create lab-devstack-secrets" }

& kubectl -n pits-cicd create secret generic gitea-admin-secret `
    --from-literal=username=$giteaUser `
    --from-literal=password=$giteaPass `
    --dry-run=client -o yaml | & kubectl apply -f -
if ($LASTEXITCODE -ne 0) { throw "Failed to create gitea-admin-secret" }

& kubectl -n flux-system create secret generic gitea-webhook-token `
    --from-literal=token=$webhookSecret `
    --dry-run=client -o yaml | & kubectl apply -f -
if ($LASTEXITCODE -ne 0) { throw "Failed to create flux-system/gitea-webhook-token" }

& (Join-Path $PSScriptRoot "Ensure-LabGatewaySecrets.ps1") -Kubeconfig $Kubeconfig -SshTarget $SshTarget
if ($LASTEXITCODE -ne 0) { throw "Ensure-LabGatewaySecrets.ps1 failed" }

Write-Host "=== Step 4/5: Applying XRD, Composition, LabDevStack Claim & Flux Sync ===" -ForegroundColor Cyan
& kubectl apply -f (Join-Path $CicdRoot "crossplane\xrds\xlabdevstack-xrd.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply XRD" }
& kubectl wait --for=condition=Established crd/xlabdevstacks.lab.pits.io crd/labdevstacks.lab.pits.io --timeout=60s

& kubectl apply -f (Join-Path $CicdRoot "crossplane\compositions\lab-devstack-composition.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply Composition" }

& kubectl apply -f (Join-Path $CicdRoot "bootstrap\gotk-sync.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply gotk-sync.yaml" }

& kubectl apply -f (Join-Path $CicdRoot "platform\instances\pits-lab-devstack.yaml")
if ($LASTEXITCODE -ne 0) { throw "Failed to apply LabDevStack instance" }

Write-Host "Waiting for CNPG Cluster, Zot, Gitea, Attic, Bootstrap Job & act_runner pool..." -ForegroundColor Cyan
$deadline = (Get-Date).AddMinutes(10)
while ((Get-Date) -lt $deadline) {
    $pgReady = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "clusters.postgresql.cnpg.io", "gitea-pg", "-o", "jsonpath={.status.readyInstances}")
    $zotReady = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "sts", "zot", "-o", "jsonpath={.status.readyReplicas}")
    $giteaReady = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "deploy", "gitea", "-o", "jsonpath={.status.readyReplicas}")
    $atticReady = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "deploy", "attic", "-o", "jsonpath={.status.readyReplicas}")
    $jobDone = Invoke-KubectlOptional @("-n", "pits-cicd", "get", "job", "lab-devstack-bootstrap-v1", "-o", "jsonpath={.status.succeeded}")
    $cpuReady = Invoke-KubectlOptional @("-n", "pits-test-pool", "get", "deploy", "runner-cicd-cpu", "-o", "jsonpath={.status.readyReplicas}")
    $npuReady = Invoke-KubectlOptional @("-n", "pits-test-pool", "get", "deploy", "runner-cicd-npu", "-o", "jsonpath={.status.readyReplicas}")
    $gpuReady = Invoke-KubectlOptional @("-n", "pits-test-pool", "get", "deploy", "runner-cicd-gpu", "-o", "jsonpath={.status.readyReplicas}")

    $pgVal = if ($pgReady) { $pgReady } else { "0" }
    $zotVal = if ($zotReady) { $zotReady } else { "0" }
    $giteaVal = if ($giteaReady) { $giteaReady } else { "0" }
    $atticVal = if ($atticReady) { $atticReady } else { "0" }
    $jobVal = if ($jobDone) { $jobDone } else { "0" }
    $cpuVal = if ($cpuReady) { $cpuReady } else { "0" }
    $npuVal = if ($npuReady) { $npuReady } else { "0" }
    $gpuVal = if ($gpuReady) { $gpuReady } else { "0" }

    Write-Host ("  status: pg={0}/2 zot={1}/1 gitea={2}/1 attic={3}/1 bootstrap={4}/1 runners(cpu={5}/3,npu={6}/1,gpu={7}/1)" -f `
        $pgVal, $zotVal, $giteaVal, $atticVal, $jobVal, $cpuVal, $npuVal, $gpuVal)

    if ($pgVal -eq "2" -and $zotVal -eq "1" -and $giteaVal -eq "1" -and $atticVal -eq "1" -and $jobVal -eq "1" -and $cpuVal -eq "3" -and $npuVal -eq "1" -and $gpuVal -eq "1") {
        break
    }
    Start-Sleep -Seconds 10
}

Write-Host "=== Step 5/5: Configuring workstation access & verifying invariants ===" -ForegroundColor Cyan
if (-not $SkipWorkstationPush) {
    & (Join-Path $PSScriptRoot "Setup-WorkstationAccess.ps1") -Kubeconfig $Kubeconfig -PushMain
} else {
    & (Join-Path $PSScriptRoot "Setup-WorkstationAccess.ps1") -Kubeconfig $Kubeconfig
}

& (Join-Path $PSScriptRoot "Verify-CicdStack.ps1") -Kubeconfig $Kubeconfig
if ($LASTEXITCODE -ne 0) { throw "Verify-CicdStack.ps1 reported failures" }
