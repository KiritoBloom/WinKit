# Detects and repairs mojibake in markdown files: UTF-8 byte sequences that
# were decoded as Latin-1 and re-encoded, leaving artifacts like the three
# characters U+00E2 U+20AC U+2014 where an em dash belonged.
#
# Run from the repository root:
#   powershell -ExecutionPolicy Bypass -File scripts/fix-mojibake.ps1          # report only
#   powershell -ExecutionPolicy Bypass -File scripts/fix-mojibake.ps1 -Fix     # repair in place
param(
    [switch]$Fix
)

$ErrorActionPreference = 'Stop'

# U+00E2 followed by U+20AC ("â‚¬" prefix) then one of the common punctuation
# code points whose UTF-8 bytes start with E2 80 / E2 82.
$trailCodes = @(0x2013, 0x2014, 0x2018, 0x2019, 0x201A, 0x201C, 0x201D, 0x201E, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203A, 0x2122)
$trailClass = ($trailCodes | ForEach-Object { [string][char]$_ }) -join ''
$pattern = ([string][char]0xE2) + ([string][char]0x20AC) + '[' + $trailClass + ']'

$replacementMap = @{
    ([string][char]0x2013) = '-'      # en dash
    ([string][char]0x2014) = '-'      # em dash (house style: plain hyphen)
    ([string][char]0x2018) = "'"      # left single quote
    ([string][char]0x2019) = "'"      # right single quote
    ([string][char]0x201A) = "'"
    ([string][char]0x201C) = '"'      # left double quote
    ([string][char]0x201D) = '"'      # right double quote
    ([string][char]0x201E) = '"'
    ([string][char]0x2020) = '+'
    ([string][char]0x2021) = '++'
    ([string][char]0x2022) = '*'
    ([string][char]0x2026) = '...'    # ellipsis
    ([string][char]0x2030) = '%'
    ([string][char]0x2039) = '<'
    ([string][char]0x203A) = '>'
    ([string][char]0x2122) = '(TM)'
}

$targets = @('README.md', 'CHANGELOG.md', 'SECURITY.md', 'CONTRIBUTING.md', 'CODE_OF_CONDUCT.md')
$targets += Get-ChildItem -Recurse -File -Include *.md -Path docs, npm, skills, examples | ForEach-Object { $_.FullName }

$utf8 = New-Object System.Text.UTF8Encoding($false)
$totalFiles = 0
$totalHits = 0
foreach ($f in $targets) {
    if (-not (Test-Path $f)) { continue }
    $t = [System.IO.File]::ReadAllText($f, $utf8)
    $matchesFound = [regex]::Matches($t, $pattern)
    if ($matchesFound.Count -eq 0) { continue }
    $totalFiles++
    $totalHits += $matchesFound.Count
    if ($Fix) {
        $t = [regex]::Replace($t, $pattern, {
            param($m)
            $key = $m.Value.Substring($m.Value.Length - 1)
            if ($replacementMap.ContainsKey($key)) { $replacementMap[$key] } else { '' }
        })
        [System.IO.File]::WriteAllText($f, $t, $utf8)
        Write-Host "fixed $($matchesFound.Count) in $f"
    } else {
        Write-Host "$f : $($matchesFound.Count) mojibake sequences"
    }
}
if ($totalHits -eq 0) { Write-Host 'No mojibake found.' } else { Write-Host "total: $totalHits across $totalFiles files" }
