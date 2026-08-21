# Copies a release binary into the matching win32-*-msvc npm package and
# stages the companion skill into every npm package.
# Run from the repository root:
#   powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1
#   powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1 -Arch arm64 -Target aarch64-pc-windows-msvc
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Arch = 'x64',
    # Cargo target triple; defaults to the host triple for x64.
    [string]$Target = ''
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

if ($Target -eq '') {
    if ($Arch -eq 'arm64') { $Target = 'aarch64-pc-windows-msvc' }
    else { $Target = 'x86_64-pc-windows-msvc' }
}

$profileDir = 'release'
if ($Target -ne 'x86_64-pc-windows-msvc') {
    $profileDir = "$Target\release"
}
$src = Join-Path $root "target\$profileDir\winkit.exe"
$destDir = Join-Path $PSScriptRoot "..\win32-$Arch-msvc\bin"
$dest = Join-Path $destDir 'winkit.exe'

if (-not (Test-Path $src)) {
    Write-Error "Release binary not found at $src. Build it first, e.g.: cargo build --release --target $Target"
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null
Copy-Item -Force $src $dest
Write-Host "Copied $src -> $dest"

# Also stage the skill alongside each npm package so `npx --yes @winkit/mcp` can find it
$skillSrc = Join-Path $root 'skills\winkit-developer-debugging'
foreach ($pkg in @('npm\mcp\skills', "npm\win32-x64-msvc\skills", "npm\win32-arm64-msvc\skills")) {
    $skillDest = Join-Path $root $pkg | Join-Path -ChildPath 'winkit-developer-debugging'
    if (Test-Path $skillSrc) {
        New-Item -ItemType Directory -Force -Path $skillDest | Out-Null
        Copy-Item -Force -Recurse "$skillSrc\*" $skillDest
        Write-Host "Staged skill $skillSrc -> $skillDest"
    } else {
        Write-Warning "Skill source not found at $skillSrc, skipping"
    }
}
