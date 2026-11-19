<#
.SYNOPSIS
  Idempotently provisions runtime secrets required by the Istio Ambient Zero-Trust Gateway,
  cert-manager Cloudflare DNS-01 (`*.lab.martensite.dev`), and Tailscale Split DNS Router
  on `cluster-pits-lab` (never committed to Git):
    1. `iam-system/oauth2-proxy-cookie-secret` (32-byte AES cookie secret)
    2. `cert-manager/cloudflare-api-token` (Cloudflare DNS-01 API token)
    3. `istio-system/lab-tailscale-auth` (optional Tailscale Auth Key when provided)
#>
[CmdletBinding()]
param(
    [string]$Kubeconfig = "",
    [string]$CloudflareApiToken = $env:CLOUDFLARE_API_TOKEN,
    [string]$ZeroSslEabHmacKey = $env:ZEROSSL_EAB_HMAC_KEY,
    [string]$TailscaleAuthKey = $env:LAB_TAILSCALE_AUTHKEY
)

$ErrorActionPreference = "Stop"
if ([string]::IsNullOrWhiteSpace($Kubeconfig)) {
    $Kubeconfig = Join-Path $env:LOCALAPPDATA "PITS\Client\clusters\pits-lab-talos-k8s.yaml"
}
if (-not (Test-Path $Kubeconfig)) {
    throw "Lab kubeconfig not found at $Kubeconfig"
}
$env:KUBECONFIG = $Kubeconfig

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

$nsYaml = @"
apiVersion: v1
kind: Namespace
metadata:
  name: iam-system
  labels:
    app.kubernetes.io/part-of: pits-iam
    istio.io/dataplane-mode: ambient
    pod-security.kubernetes.io/enforce: baseline
    pod-security.kubernetes.io/warn: baseline
---
apiVersion: v1
kind: Namespace
metadata:
  name: istio-system
  labels:
    app.kubernetes.io/part-of: pits-istio-ambient
    pod-security.kubernetes.io/enforce: privileged
    pod-security.kubernetes.io/warn: privileged
---
apiVersion: v1
kind: Namespace
metadata:
  name: cert-manager
  labels:
    app.kubernetes.io/part-of: pits-cert-manager
    pod-security.kubernetes.io/enforce: baseline
    pod-security.kubernetes.io/warn: baseline
"@
$nsYaml | & kubectl apply -f - | Out-Null

# 1. oauth2-proxy cookie secret (24 random bytes -> 32 URL-safe base64 chars)
$existingCookie = Invoke-KubectlOptional @("-n", "iam-system", "get", "secret", "oauth2-proxy-cookie-secret", "-o", "jsonpath={.data.cookie-secret}")
if ([string]::IsNullOrWhiteSpace($existingCookie)) {
    $buf24 = New-Object byte[] 24
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($buf24)
    $cookieSecret = [Convert]::ToBase64String($buf24).Replace("+", "-").Replace("/", "_")
    & kubectl -n iam-system create secret generic oauth2-proxy-cookie-secret `
        --from-literal=cookie-secret=$cookieSecret `
        --dry-run=client -o yaml | & kubectl apply -f - | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed to create iam-system/oauth2-proxy-cookie-secret" }
    Write-Host "  Created iam-system/oauth2-proxy-cookie-secret (32-char URL-safe key)." -ForegroundColor Green
} else {
    Write-Host "  Reusing existing iam-system/oauth2-proxy-cookie-secret." -ForegroundColor DarkGray
}

# 2. Cloudflare DNS-01 API Token for cert-manager (*.lab.martensite.dev)
$existingCf = Invoke-KubectlOptional @("-n", "cert-manager", "get", "secret", "cloudflare-api-token", "-o", "jsonpath={.data.api-token}")
if (-not [string]::IsNullOrWhiteSpace($CloudflareApiToken)) {
    $cfTrim = $CloudflareApiToken.Trim()
    & kubectl -n cert-manager create secret generic cloudflare-api-token `
        --from-literal=api-token=$cfTrim `
        --dry-run=client -o yaml | & kubectl apply -f - | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed to apply cert-manager/cloudflare-api-token" }
    Write-Host "  Configured cert-manager/cloudflare-api-token." -ForegroundColor Green
} elseif (-not [string]::IsNullOrWhiteSpace($existingCf)) {
    Write-Host "  Reusing existing cert-manager/cloudflare-api-token." -ForegroundColor DarkGray
} else {
    Write-Warning "cert-manager/cloudflare-api-token not present and -CloudflareApiToken / CLOUDFLARE_API_TOKEN not provided."
}

# 3. ZeroSSL ACME EAB HMAC secret for cert-manager/zerossl-eab-secret
$existingEab = Invoke-KubectlOptional @("-n", "cert-manager", "get", "secret", "zerossl-eab-secret", "-o", "jsonpath={.data.hmac-key}")
if (-not [string]::IsNullOrWhiteSpace($ZeroSslEabHmacKey)) {
    $eabTrim = $ZeroSslEabHmacKey.Trim()
    & kubectl -n cert-manager create secret generic zerossl-eab-secret `
        --from-literal=hmac-key=$eabTrim `
        --dry-run=client -o yaml | & kubectl apply -f - | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed to apply cert-manager/zerossl-eab-secret" }
    Write-Host "  Configured cert-manager/zerossl-eab-secret." -ForegroundColor Green
} elseif (-not [string]::IsNullOrWhiteSpace($existingEab)) {
    Write-Host "  Reusing existing cert-manager/zerossl-eab-secret." -ForegroundColor DarkGray
} else {
    Write-Warning "cert-manager/zerossl-eab-secret not present and -ZeroSslEabHmacKey / ZEROSSL_EAB_HMAC_KEY not provided."
}

# 4. Optional Tailscale Auth Key for istio-system/lab-split-dns
if (-not [string]::IsNullOrWhiteSpace($TailscaleAuthKey)) {
    $tsTrim = $TailscaleAuthKey.Trim()
    & kubectl -n istio-system create secret generic lab-tailscale-auth `
        --from-literal=authkey=$tsTrim `
        --dry-run=client -o yaml | & kubectl apply -f - | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed to apply istio-system/lab-tailscale-auth" }
    Write-Host "  Configured istio-system/lab-tailscale-auth." -ForegroundColor Green
} else {
    $existingTs = Invoke-KubectlOptional @("-n", "istio-system", "get", "secret", "lab-tailscale-auth", "-o", "jsonpath={.data.authkey}")
    if (-not [string]::IsNullOrWhiteSpace($existingTs)) {
        Write-Host "  Reusing existing istio-system/lab-tailscale-auth." -ForegroundColor DarkGray
    } else {
        Write-Host "  [info] istio-system/lab-tailscale-auth not set yet (set -TailscaleAuthKey or authenticate via lab-split-dns logs)." -ForegroundColor Yellow
    }
}
