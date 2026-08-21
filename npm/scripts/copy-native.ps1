# Copies the release binary into the win32-x64-msvc npm package.
# Run from the repository root:
#   powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$src = Join-Path $root 'target\release\winkit.exe'
$destDir = Join-Path $PSScriptRoot '..\win32-x64-msvc\bin'
$dest = Join-Path $destDir 'winkit.exe'

if (-not (Test-Path $src)) {
    Write-Error "Release binary not found at $src. Run 'cargo build --release' first."
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null
Copy-Item -Force $src $dest
Write-Host "Copied $src -> $dest"

# Also stage the skill alongside each npm package so `npx --yes @winkit/mcp` can find it
$skillSrc = Join-Path $root 'skills\winkit-developer-debugging'
foreach ($pkg in @('npm\mcp\skills', 'npm\win32-x64-msvc\skills')) {
    $skillDest = Join-Path $root $pkg | Join-Path -ChildPath 'winkit-developer-debugging'
    if (Test-Path $skillSrc) {
        New-Item -ItemType Directory -Force -Path $skillDest | Out-Null
        Copy-Item -Force -Recurse "$skillSrc\*" $skillDest
        Write-Host "Staged skill $skillSrc -> $skillDest"
    } else {
        Write-Warning "Skill source not found at $skillSrc, skipping"
    }
}
