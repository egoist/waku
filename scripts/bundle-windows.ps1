[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-ChildPath {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Parent,
        [Parameter(Mandatory = $true)]
        [string] $Child
    )

    $parentPath = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $childPath = [System.IO.Path]::GetFullPath($Child)
    $prefix = $parentPath + [System.IO.Path]::DirectorySeparatorChar
    if (-not $childPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to operate outside ${parentPath}: ${childPath}"
    }
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$configuredTarget = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR")
$targetDirectory = if ([string]::IsNullOrWhiteSpace($configuredTarget)) {
    Join-Path $repositoryRoot "target"
} elseif ([System.IO.Path]::IsPathRooted($configuredTarget)) {
    $configuredTarget
} else {
    Join-Path $repositoryRoot $configuredTarget
}
$targetDirectory = [System.IO.Path]::GetFullPath($targetDirectory)
$stagingRoot = $null

Push-Location $repositoryRoot
try {
    $metadataJson = cargo metadata --locked --no-deps --format-version 1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE"
    }
    $metadata = $metadataJson | ConvertFrom-Json
    $wakuPackage = @($metadata.packages | Where-Object { $_.name -eq "waku" })[0]
    if ($null -eq $wakuPackage) {
        throw "Could not resolve the waku package version"
    }

    $rustcVersion = rustc -vV
    if ($LASTEXITCODE -ne 0) {
        throw "rustc -vV failed with exit code $LASTEXITCODE"
    }
    $hostLine = $rustcVersion | Where-Object { $_ -like "host: *" } | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($hostLine)) {
        throw "Could not resolve the Rust host target"
    }
    $targetTriple = $hostLine.Substring("host: ".Length).Trim()
    if ($targetTriple -notlike "*-windows-*") {
        throw "bundle-windows.ps1 must run on a Windows Rust host, got ${targetTriple}"
    }

    cargo build --locked --release --bin waku
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }

    $releaseDirectory = Join-Path $targetDirectory "release"
    $sourceExecutable = Join-Path $releaseDirectory "waku.exe"
    if (-not (Test-Path -LiteralPath $sourceExecutable -PathType Leaf)) {
        throw "Release executable was not created at ${sourceExecutable}"
    }

    $packageName = "waku-$($wakuPackage.version)-${targetTriple}"
    $versionedExecutable = Join-Path $releaseDirectory "${packageName}.exe"
    $archive = Join-Path $releaseDirectory "${packageName}.zip"
    Copy-Item -LiteralPath $sourceExecutable -Destination $versionedExecutable -Force

    $stagingRoot = Join-Path $targetDirectory ".bundle-windows-$PID-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
    Assert-ChildPath -Parent $targetDirectory -Child $stagingRoot
    $packageDirectory = Join-Path $stagingRoot $packageName
    New-Item -ItemType Directory -Path $packageDirectory -Force | Out-Null
    Copy-Item -LiteralPath $sourceExecutable -Destination (Join-Path $packageDirectory "waku.exe")
    Copy-Item -LiteralPath (Join-Path $repositoryRoot "LICENSE") -Destination $packageDirectory

    if (Test-Path -LiteralPath $archive) {
        Remove-Item -LiteralPath $archive -Force
    }
    Compress-Archive -LiteralPath $packageDirectory -DestinationPath $archive -CompressionLevel Optimal

    Write-Output "Created ${versionedExecutable}"
    Write-Output "Created ${archive}"
} finally {
    if ($null -ne $stagingRoot -and (Test-Path -LiteralPath $stagingRoot)) {
        Assert-ChildPath -Parent $targetDirectory -Child $stagingRoot
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
    Pop-Location
}
