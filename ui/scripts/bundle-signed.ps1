$ErrorActionPreference = 'Stop'

$thumbprint = $env:CODEX_LAUNCHER_CERT_THUMBPRINT
$timestampUrl = $env:CODEX_LAUNCHER_TIMESTAMP_URL
$digestAlgorithm = $env:CODEX_LAUNCHER_DIGEST_ALGORITHM

if ([string]::IsNullOrWhiteSpace($thumbprint)) {
    throw 'CODEX_LAUNCHER_CERT_THUMBPRINT is required for a signed bundle.'
}
if ([string]::IsNullOrWhiteSpace($timestampUrl)) {
    throw 'CODEX_LAUNCHER_TIMESTAMP_URL is required for a signed bundle.'
}
if ([string]::IsNullOrWhiteSpace($digestAlgorithm)) {
    $digestAlgorithm = 'sha256'
}

$normalizedThumbprint = $thumbprint.Replace(' ', '').ToUpperInvariant()
$certificate = Get-ChildItem -Path 'Cert:\CurrentUser\My', 'Cert:\LocalMachine\My' |
    Where-Object { $_.Thumbprint -eq $normalizedThumbprint } |
    Select-Object -First 1
if ($null -eq $certificate) {
    throw "No code-signing certificate with thumbprint $normalizedThumbprint was found."
}

$configOverride = @{
    bundle = @{
        windows = @{
            certificateThumbprint = $normalizedThumbprint
            digestAlgorithm = $digestAlgorithm
            timestampUrl = $timestampUrl
        }
    }
} | ConvertTo-Json -Compress -Depth 4

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$tauri = Join-Path $repoRoot 'ui\node_modules\.bin\tauri.cmd'
Push-Location $repoRoot
try {
    & $tauri build --bundles 'nsis,msi' --ci --config $configOverride
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}
