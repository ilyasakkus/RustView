param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$TargetRoot = if ($env:CARGO_TARGET_DIR) {
    if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
        $env:CARGO_TARGET_DIR
    } else {
        Join-Path $RepoRoot $env:CARGO_TARGET_DIR
    }
} else {
    Join-Path $RepoRoot "target"
}
$ReleaseDir = Join-Path $TargetRoot "release"
$DistDir = Join-Path $RepoRoot "dist"
$PackageName = "RustView-Windows-x86_64"
$Archive = Join-Path $DistDir "$PackageName.zip"
$StageRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("rustview-windows-" + [guid]::NewGuid())
$PackageRoot = Join-Path $StageRoot $PackageName

try {
    $DesktopBinary = Join-Path $ReleaseDir "rustview-desktop.exe"
    $RelayBinary = Join-Path $ReleaseDir "rustview-relay.exe"
    foreach ($Path in @($DesktopBinary, $RelayBinary)) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Required build output is missing: $Path"
        }
    }

    New-Item -ItemType Directory -Force -Path $PackageRoot, $DistDir | Out-Null
    Copy-Item -LiteralPath $DesktopBinary -Destination (Join-Path $PackageRoot "RustView.exe")
    Copy-Item -LiteralPath $RelayBinary -Destination (Join-Path $PackageRoot "rustview-relay.exe")
    Copy-Item -LiteralPath (Join-Path $RepoRoot "README.md") -Destination $PackageRoot
    Copy-Item -LiteralPath (Join-Path $RepoRoot "LICENSE-MIT") -Destination $PackageRoot

    if (Test-Path -LiteralPath $Archive) {
        Remove-Item -LiteralPath $Archive -Force
    }
    Compress-Archive -LiteralPath $PackageRoot -DestinationPath $Archive -CompressionLevel Optimal
    Write-Host "Created $Archive"
} finally {
    if (Test-Path -LiteralPath $StageRoot) {
        Remove-Item -LiteralPath $StageRoot -Recurse -Force
    }
}
