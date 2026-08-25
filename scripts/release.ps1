#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Version,

    [switch]$Signed,
    [switch]$Publish,
    [switch]$Draft,
    [switch]$Prerelease,

    [string]$NotesFile,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-Application {
    param([Parameter(Mandatory = $true)][string]$Name)

    $command = Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        throw "找不到必需命令：$Name"
    }

    return $command.Source
}

function Format-NativeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @()
    )

    $displayArguments = @($ArgumentList | ForEach-Object {
        if ($_ -match '[\s'']') {
            "'{0}'" -f $_.Replace("'", "''")
        } else {
            $_
        }
    })

    return ('> {0} {1}' -f ([System.IO.Path]::GetFileName($FilePath)), ($displayArguments -join ' ')).TrimEnd()
}

function Invoke-NativeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    Write-Host (Format-NativeCommand -FilePath $FilePath -ArgumentList $ArgumentList) -ForegroundColor DarkGray
    Push-Location -LiteralPath $WorkingDirectory
    try {
        & $FilePath @ArgumentList
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }

    if ($exitCode -ne 0) {
        throw "命令执行失败（exit code: $exitCode）：$([System.IO.Path]::GetFileName($FilePath))"
    }
}

function Invoke-NativeCapture {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    Write-Host (Format-NativeCommand -FilePath $FilePath -ArgumentList $ArgumentList) -ForegroundColor DarkGray
    Push-Location -LiteralPath $WorkingDirectory
    try {
        $output = & $FilePath @ArgumentList
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }

    if ($exitCode -ne 0) {
        throw "命令执行失败（exit code: $exitCode）：$([System.IO.Path]::GetFileName($FilePath))"
    }

    return ($output -join [Environment]::NewLine)
}

function Test-NativeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    Push-Location -LiteralPath $WorkingDirectory
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $FilePath @ArgumentList *> $null
        return $LASTEXITCODE -eq 0
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
        Pop-Location
    }
}

function Get-SingleArtifact {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$ArtifactVersion,
        [Parameter(Mandatory = $true)][string]$Extension,
        [Parameter(Mandatory = $true)][string]$Description
    )

    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        throw "未生成 $Description 目录：$Directory"
    }

    $pattern = '*_{0}_*{1}' -f $ArtifactVersion, $Extension
    $matches = @(Get-ChildItem -LiteralPath $Directory -File |
        Where-Object { $_.Name -like $pattern })

    if ($matches.Count -ne 1) {
        $found = if ($matches.Count -eq 0) {
            '无'
        } else {
            ($matches.Name -join ', ')
        }
        throw "$Description 产物数量应为 1，实际为 $($matches.Count)（$found）。"
    }

    return $matches[0]
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Codex Launcher 的 bundle/release 脚本仅支持 Windows。'
}

if (($Draft -or $Prerelease -or -not [string]::IsNullOrWhiteSpace($NotesFile)) -and -not $Publish) {
    throw '-Draft、-Prerelease 和 -NotesFile 仅能与 -Publish 一起使用。'
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$uiRoot = Join-Path $repoRoot 'ui'
$tauriRoot = Join-Path $repoRoot 'src-tauri'
$cargoManifest = Join-Path $tauriRoot 'Cargo.toml'
$tauriConfigPath = Join-Path $tauriRoot 'tauri.conf.json'

$cargo = Resolve-Application -Name 'cargo.exe'
$npm = Resolve-Application -Name 'npm.cmd'
$git = $null
$gh = $null
if ($Publish) {
    $git = Resolve-Application -Name 'git.exe'
    $gh = Resolve-Application -Name 'gh.exe'
}

Write-Host '读取并校验 release 版本...' -ForegroundColor Cyan
$metadataJson = Invoke-NativeCapture -FilePath $cargo -WorkingDirectory $repoRoot -ArgumentList @(
    'metadata',
    '--locked',
    '--no-deps',
    '--format-version', '1',
    '--manifest-path', $cargoManifest
)
$metadata = $metadataJson | ConvertFrom-Json
$packages = @($metadata.packages)
if ($packages.Count -ne 1) {
    throw "预期 cargo metadata 返回 1 个 package，实际为 $($packages.Count)。"
}

$cargoVersion = [string]$packages[0].version
$tauriConfig = Get-Content -Raw -LiteralPath $tauriConfigPath | ConvertFrom-Json
$tauriVersion = [string]$tauriConfig.version
$productName = [string]$tauriConfig.productName

if ($cargoVersion -ne $tauriVersion) {
    throw "版本不一致：src-tauri/Cargo.toml=$cargoVersion，src-tauri/tauri.conf.json=$tauriVersion。"
}

$releaseVersion = $cargoVersion
if (-not [string]::IsNullOrWhiteSpace($Version)) {
    $requestedVersion = $Version.Trim()
    if ($requestedVersion.StartsWith('v', [StringComparison]::OrdinalIgnoreCase)) {
        $requestedVersion = $requestedVersion.Substring(1)
    }
    if ($requestedVersion -ne $releaseVersion) {
        throw "请求发布 $requestedVersion，但项目当前版本为 $releaseVersion；请先同步两个版本文件。"
    }
}

$semVerPattern = '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
if ($releaseVersion -notmatch $semVerPattern) {
    throw "项目版本不是合法 SemVer：$releaseVersion"
}

$tag = "v$releaseVersion"
$releaseTitle = '{0} {1}' -f $productName.Replace('-', ' '), $tag
$notesPath = $null
if (-not [string]::IsNullOrWhiteSpace($NotesFile)) {
    $notesCandidate = $NotesFile
    if (-not [System.IO.Path]::IsPathRooted($notesCandidate)) {
        $notesCandidate = Join-Path $repoRoot $notesCandidate
    }
    if (-not (Test-Path -LiteralPath $notesCandidate -PathType Leaf)) {
        throw "Release notes 文件不存在：$notesCandidate"
    }
    $notesPath = (Resolve-Path -LiteralPath $notesCandidate).Path
}

Write-Host "版本：$releaseVersion" -ForegroundColor Green
Write-Host "模式：$(if ($Signed) { 'signed' } else { 'unsigned' }) / $(if ($Publish) { 'build + GitHub release' } else { 'build only' })"

$headCommit = $null
$localTagExists = $false
if ($Publish -and -not $DryRun) {
    Write-Host '检查 Git 与 GitHub 发布前置条件...' -ForegroundColor Cyan
    $gitStatus = Invoke-NativeCapture -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @(
        'status', '--porcelain=v1', '--untracked-files=all'
    )
    if (-not [string]::IsNullOrWhiteSpace($gitStatus)) {
        throw "发布前工作区必须干净。当前变更：`n$gitStatus"
    }

    Invoke-NativeCommand -FilePath $gh -WorkingDirectory $repoRoot -ArgumentList @('auth', 'status')

    $releaseExists = Test-NativeCommand -FilePath $gh -WorkingDirectory $repoRoot -ArgumentList @(
        'release', 'view', $tag, '--json', 'url'
    )
    if ($releaseExists) {
        throw "GitHub Release $tag 已存在。"
    }

    $headCommit = Invoke-NativeCapture -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @('rev-parse', 'HEAD')
    $localTagExists = Test-NativeCommand -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @(
        'show-ref', '--verify', '--quiet', "refs/tags/$tag"
    )
    if ($localTagExists) {
        $tagCommit = Invoke-NativeCapture -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @(
            'rev-list', '-n', '1', $tag
        )
        $localTagExists = $true
        if ($tagCommit.Trim() -ne $headCommit.Trim()) {
            throw "本地 tag $tag 未指向当前 HEAD。"
        }
    }
}

if ($DryRun) {
    Write-Host ''
    Write-Host 'Dry-run：未执行安装、测试、构建、打 tag 或发布。计划如下：' -ForegroundColor Yellow
    Write-Host '  1. npm ci + frontend lint/test'
    Write-Host '  2. cargo fmt/clippy/test（--locked）'
    Write-Host "  3. Tauri NSIS + MSI $($(if ($Signed) { 'signed' } else { 'unsigned' })) bundle"
    Write-Host "  4. 整理到 artifacts\$tag 并生成 SHA256SUMS.txt"
    if ($Publish) {
        Write-Host "  5. 创建并推送 $tag，发布 GitHub Release"
    }
    return
}

Write-Host '运行 frontend quality gates...' -ForegroundColor Cyan
Invoke-NativeCommand -FilePath $npm -WorkingDirectory $uiRoot -ArgumentList @('ci')
Invoke-NativeCommand -FilePath $npm -WorkingDirectory $uiRoot -ArgumentList @('run', 'lint')
Invoke-NativeCommand -FilePath $npm -WorkingDirectory $uiRoot -ArgumentList @('test')

Write-Host '运行 Rust quality gates...' -ForegroundColor Cyan
Invoke-NativeCommand -FilePath $cargo -WorkingDirectory $tauriRoot -ArgumentList @(
    'fmt', '--all', '--', '--check'
)
Invoke-NativeCommand -FilePath $cargo -WorkingDirectory $tauriRoot -ArgumentList @(
    'clippy', '--locked', '--all-targets', '--all-features', '--', '-D', 'warnings'
)
Invoke-NativeCommand -FilePath $cargo -WorkingDirectory $tauriRoot -ArgumentList @(
    'test', '--locked', '--all-targets', '--all-features'
)

Write-Host '构建 Windows installers...' -ForegroundColor Cyan
$bundleScript = if ($Signed) { 'bundle:signed' } else { 'bundle:unsigned' }
Invoke-NativeCommand -FilePath $npm -WorkingDirectory $uiRoot -ArgumentList @('run', $bundleScript)

$bundleRoot = Join-Path $tauriRoot 'target\release\bundle'
$sourceArtifacts = @(
    Get-SingleArtifact -Directory (Join-Path $bundleRoot 'nsis') -ArtifactVersion $releaseVersion -Extension '.exe' -Description 'NSIS'
    Get-SingleArtifact -Directory (Join-Path $bundleRoot 'msi') -ArtifactVersion $releaseVersion -Extension '.msi' -Description 'MSI'
)

$artifactDirectory = Join-Path $repoRoot (Join-Path 'artifacts' $tag)
$null = New-Item -ItemType Directory -Path $artifactDirectory -Force
$stagedArtifacts = @($sourceArtifacts | ForEach-Object {
    $destination = Join-Path $artifactDirectory $_.Name
    Copy-Item -LiteralPath $_.FullName -Destination $destination -Force
    Get-Item -LiteralPath $destination
})

foreach ($artifact in $stagedArtifacts) {
    $signature = Get-AuthenticodeSignature -LiteralPath $artifact.FullName
    Write-Host ('Authenticode {0}: {1}' -f $artifact.Name, $signature.Status)
    if ($Signed -and $signature.Status -ne 'Valid') {
        throw "Signed release 的产物签名无效：$($artifact.Name) ($($signature.Status))"
    }
}

$checksumLines = @($stagedArtifacts |
    Sort-Object -Property Name |
    ForEach-Object {
        $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        '{0}  {1}' -f $hash, $_.Name
    })
$checksumPath = Join-Path $artifactDirectory 'SHA256SUMS.txt'
[System.IO.File]::WriteAllLines(
    $checksumPath,
    $checksumLines,
    (New-Object System.Text.UTF8Encoding($false))
)

Write-Host "Release artifacts 已生成：$artifactDirectory" -ForegroundColor Green
$checksumLines | ForEach-Object { Write-Host "  $_" }

if (-not $Publish) {
    Write-Host '未指定 -Publish；没有创建 tag 或 GitHub Release。' -ForegroundColor Yellow
    return
}

if (-not $Signed) {
    Write-Warning '正在发布 unsigned installers；Windows SmartScreen 可能显示安全警告。'
}

if (-not $localTagExists) {
    Invoke-NativeCommand -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @(
        'tag', '--annotate', $tag, '--message', $releaseTitle
    )
}
Invoke-NativeCommand -FilePath $git -WorkingDirectory $repoRoot -ArgumentList @(
    'push', 'origin', "refs/tags/$tag"
)

$releaseAssets = @($stagedArtifacts.FullName) + @($checksumPath)
$releaseArguments = @('release', 'create', $tag) + $releaseAssets + @(
    '--verify-tag',
    '--title', $releaseTitle
)
if ($null -ne $notesPath) {
    $releaseArguments += @('--notes-file', $notesPath)
} else {
    $releaseArguments += '--generate-notes'
}
if ($Draft) {
    $releaseArguments += '--draft'
}
if ($Prerelease) {
    $releaseArguments += '--prerelease'
}

Invoke-NativeCommand -FilePath $gh -WorkingDirectory $repoRoot -ArgumentList $releaseArguments
$releaseUrl = Invoke-NativeCapture -FilePath $gh -WorkingDirectory $repoRoot -ArgumentList @(
    'release', 'view', $tag, '--json', 'url', '--jq', '.url'
)
Write-Host "GitHub Release 已发布：$releaseUrl" -ForegroundColor Green
