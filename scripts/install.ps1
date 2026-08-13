#Requires -Version 5.1

[CmdletBinding()]
param(
    [ValidateSet("stable", "prerelease")]
    [string] $Channel = "stable",

    [string] $BinDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Repository = "iebb/termgram"
$ReleasesApi = "https://api.github.com/repos/$Repository/releases"
$Headers = @{
    Accept                 = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    "User-Agent"           = "termgram-installer"
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "The Windows installer can only install Windows release binaries."
}
$architecture = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITEW6432")
if ([string]::IsNullOrWhiteSpace($architecture)) {
    $architecture = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")
}
if ([string]::IsNullOrWhiteSpace($architecture)) {
    throw "Unable to determine the native Windows architecture."
}
$platform = switch ($architecture.ToUpperInvariant()) {
    "AMD64" { "windows" }
    "ARM64" { "windows-aarch64" }
    default {
        throw "Windows release binaries require x64 (AMD64) or ARM64; detected $architecture."
    }
}

if ($null -ne [Net.ServicePointManager]::SecurityProtocol) {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

if ([string]::IsNullOrWhiteSpace($BinDir)) {
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $BinDir = Join-Path $env:LOCALAPPDATA "Programs\Termgram\bin"
    }
    elseif (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        $BinDir = Join-Path $env:USERPROFILE ".local\bin"
    }
    else {
        throw "Unable to choose an install directory; pass -BinDir explicitly."
    }
}

$bestRelease = $null
$bestHeight = [long]-1
$page = 1
do {
    $pageUri = "${ReleasesApi}?per_page=100&page=$page"
    $pageReleases = @(Invoke-RestMethod -Method Get -Uri $pageUri -Headers $Headers)
    foreach ($release in $pageReleases) {
        if ([bool]$release.draft) {
            continue
        }
        if ($Channel -eq "stable" -and [bool]$release.prerelease) {
            continue
        }

        $tagMatch = [regex]::Match([string]$release.tag_name, '^v0\.1\.(0|[1-9][0-9]*)$')
        if (-not $tagMatch.Success) {
            continue
        }

        $height = [long]::Parse(
            $tagMatch.Groups[1].Value,
            [Globalization.CultureInfo]::InvariantCulture
        )
        if ($height -gt $bestHeight) {
            $bestHeight = $height
            $bestRelease = $release
        }
    }
    $page++
} while ($pageReleases.Count -eq 100)

if ($null -eq $bestRelease) {
    throw "No $Channel Termgram release is available."
}

$tag = [string]$bestRelease.tag_name
$version = $tag.Substring(1)
$assetName = "termgram-$version-$platform.zip"
$releaseUrl = "https://github.com/$Repository/releases/download/$tag"

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("termgram-install-" + [Guid]::NewGuid().ToString("N"))
$null = [IO.Directory]::CreateDirectory($tempRoot)
$archivePath = Join-Path $tempRoot $assetName
$checksumsPath = Join-Path $tempRoot "SHA256SUMS"
$extractedPath = Join-Path $tempRoot "tg.exe"
$stagePath = $null
$backupPath = $null

try {
    Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/SHA256SUMS" `
        -Headers $Headers -OutFile $checksumsPath
    Invoke-WebRequest -UseBasicParsing -Uri "$releaseUrl/$assetName" `
        -Headers $Headers -OutFile $archivePath

    if ((Get-Item -LiteralPath $checksumsPath).Length -gt 65536) {
        throw "SHA256SUMS exceeded the 64 KiB size limit."
    }
    if ((Get-Item -LiteralPath $archivePath).Length -gt 134217728) {
        throw "Release archive exceeded the 128 MiB size limit."
    }

    $expectedHashes = @(
        foreach ($line in [IO.File]::ReadAllLines($checksumsPath)) {
            $checksumMatch = [regex]::Match($line, '^([0-9A-Fa-f]{64})[ \t]+\*?(.+)$')
            if ($checksumMatch.Success -and $checksumMatch.Groups[2].Value -ceq $assetName) {
                $checksumMatch.Groups[1].Value.ToLowerInvariant()
            }
        }
    )
    if ($expectedHashes.Count -ne 1) {
        throw "SHA256SUMS must contain exactly one entry for $assetName."
    }

    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -cne $expectedHashes[0]) {
        throw "Checksum verification failed for $assetName."
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $entries = @($zip.Entries)
        if ($entries.Count -ne 1 -or
            $entries[0].FullName -cne "tg.exe" -or
            $entries[0].Name -cne "tg.exe") {
            throw "Release archive must contain only a root-level tg.exe binary."
        }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $extractedPath, $false)
    }
    finally {
        $zip.Dispose()
    }

    $null = [IO.Directory]::CreateDirectory($BinDir)
    $resolvedBinDir = (Get-Item -LiteralPath $BinDir).FullName
    $targetPath = Join-Path $resolvedBinDir "tg.exe"
    if ([IO.Directory]::Exists($targetPath)) {
        throw "Install target is a directory: $targetPath"
    }
    if ([IO.File]::Exists($targetPath)) {
        $targetAttributes = [IO.File]::GetAttributes($targetPath)
        if (($targetAttributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Install target is a symbolic link or reparse point: $targetPath"
        }
    }

    $stagePath = Join-Path $resolvedBinDir (".tg.install." + [Guid]::NewGuid().ToString("N") + ".exe")
    [IO.File]::Copy($extractedPath, $stagePath, $false)

    if ([IO.File]::Exists($targetPath)) {
        $backupPath = Join-Path $resolvedBinDir (".tg.backup." + [Guid]::NewGuid().ToString("N") + ".exe")
        [IO.File]::Replace($stagePath, $targetPath, $backupPath, $true)
    }
    else {
        [IO.File]::Move($stagePath, $targetPath)
    }
    $stagePath = $null

    Write-Host "Installed tg $version to $targetPath"

    $normalizedBinDir = $resolvedBinDir.TrimEnd('\')
    $pathContainsBinDir = @(
        $env:Path -split [IO.Path]::PathSeparator |
            Where-Object { $_.TrimEnd('\') -ieq $normalizedBinDir }
    ).Count -gt 0
    if (-not $pathContainsBinDir) {
        Write-Warning "Add $resolvedBinDir to PATH, reopen the terminal, then run: tg"
    }
}
finally {
    if ($null -ne $stagePath -and [IO.File]::Exists($stagePath)) {
        [IO.File]::Delete($stagePath)
    }
    if ($null -ne $backupPath -and [IO.File]::Exists($backupPath)) {
        [IO.File]::Delete($backupPath)
    }
    if ([IO.Directory]::Exists($tempRoot)) {
        [IO.Directory]::Delete($tempRoot, $true)
    }
}
