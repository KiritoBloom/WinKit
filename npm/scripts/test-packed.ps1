# End-to-end packed-package smoke test.
#
# Packs the real tarballs for @winkit/mcp and @winkit/win32-x64-msvc,
# installs them into a temporary isolated project with an isolated npm
# cache (no global packages, no developer cache, no registry dependency),
# and runs the exact commands a user would run through the installed
# launcher: --version, --help, doctor, init, configure --dry-run, the MCP
# initialize handshake over stdio, exit-code propagation, and
# missing-native-runtime behavior.
#
# Nothing is published and nothing outside the repository-created
# temporary smoke-test directory is touched.
#
# Usage (from the repository root, after `cargo build --release`):
#   powershell -ExecutionPolicy Bypass -File npm/scripts/test-packed.ps1
#
# Exit codes: 0 = all checks passed, 1 = a check failed, 2 = skipped
# because the release binary is absent (a documented limitation, not a pass).

# The launcher intentionally writes human notes to stderr (e.g. the init
# disclaimer), which PowerShell 5.1 turns into error records under
# ErrorActionPreference=Stop. Every step here checks its exit code
# explicitly, so Continue is the correct, robust setting.
$ErrorActionPreference = 'Continue'

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$mcpDir = Join-Path $root 'npm\mcp'
$nativeDir = Join-Path $root 'npm\win32-x64-msvc'
$copyScript = Join-Path $PSScriptRoot 'copy-native.ps1'

$src = Join-Path $root 'target\release\winkit.exe'
if (-not (Test-Path $src)) {
    Write-Host "SKIP: release binary not found at $src; run 'cargo build --release' first." -ForegroundColor Yellow
    exit 2
}

# 1. Make sure the native package actually contains the release binary.
& powershell -ExecutionPolicy Bypass -File $copyScript
$nativeExe = Join-Path $nativeDir 'bin\winkit.exe'
if (-not (Test-Path $nativeExe)) {
    Write-Error "copy-native.ps1 did not produce $nativeExe"
}

# 2. Pack both packages. The isolated project lives on the repository's
# drive: the system temp drive is often nearly full on dev machines, and a
# failing `npm install` must never be mistaken for a package problem.
$temp = Join-Path $root ('.pack-smoke-' + [guid]::NewGuid().ToString('N'))
$cache = Join-Path $temp 'npm-cache'
$project = Join-Path $temp 'project'
New-Item -ItemType Directory -Force -Path $cache, $project | Out-Null

# The npm cache is isolated inside the smoke-test directory from the very
# first npm invocation (pack included), so the developer's global cache is
# never consulted and never written.
$env:npm_config_cache = $cache
$env:NPM_CONFIG_CACHE = $cache

$failures = 0
function Assert-Check($name, $condition, $detail) {
    if ($condition) {
        Write-Host "  [PASS] $name"
    } else {
        Write-Host "  [FAIL] $name - $detail" -ForegroundColor Red
        $script:failures++
    }
}

# Run `npm.cmd pack --json` and normalize the result to an array.
# Windows PowerShell 5.1 unwraps a single-element JSON array into a scalar
# object (and a scalar has no .Count), so `@(...)` normalization is
# mandatory; never assume `.Count` exists on the raw parse result.
function Invoke-NpmPackJson($dir) {
    Push-Location $dir
    try {
        $json = npm.cmd pack --json 2>$null | Out-String
        if ($LASTEXITCODE -ne 0) {
            throw "npm.cmd pack exited $LASTEXITCODE in $dir`n$json"
        }
        if ([string]::IsNullOrWhiteSpace($json)) {
            throw "npm.cmd pack returned no output in $dir"
        }
        $parsed = $null
        try { $parsed = $json | ConvertFrom-Json } catch {
            throw "npm.cmd pack output is not JSON in ${dir}: $json"
        }
        if ($null -eq $parsed) { throw "npm.cmd pack returned empty JSON in $dir" }
        # Normalize: a one-element array becomes an array; a single object
        # (e.g. an npm error object) becomes a one-element array too and is
        # validated below by the filename/name/version checks.
        return @($parsed)
    } finally {
        Pop-Location
    }
}

function Pack-Package($dir, $expectedName, $expectedVersion) {
    # PowerShell unrolls the function's returned array into a scalar at the
    # call site, and a scalar PSCustomObject has no .Count; @() re-wraps so
    # .Count and [0] are always reliable.
    $items = @(Invoke-NpmPackJson $dir)
    if ($items.Count -ne 1) {
        throw "expected exactly one tarball from ${dir}, got $($items.Count)"
    }
    $info = $items[0]
    if ($null -eq $info -or $null -eq $info.filename) {
        throw "npm pack reported no tarball filename from $dir"
    }
    $tarball = Join-Path $dir $info.filename
    if (-not (Test-Path $tarball)) {
        throw "npm pack produced no tarball file at $tarball"
    }
    if ($info.name -ne $expectedName) {
        throw "unexpected package name '$($info.name)' from $dir (expected '$expectedName')"
    }
    if ($info.version -ne $expectedVersion) {
        throw "unexpected package version '$($info.version)' from $dir (expected '$expectedVersion')"
    }
    Write-Host "Packed $($info.filename) ($($info.size) bytes)"
    return $tarball
}

# The whole pack -> install -> assert -> cleanup flow is inside one outer
# try/finally so a failure at ANY point (packing, installation, a command,
# or an assertion) still removes the tarballs and the temp project. A
# failed run must never leave `.pack-smoke-*` directories behind.
$mcpTarball = $null
$nativeTarball = $null
try {
    $mcpVersion = (Get-Content -Raw (Join-Path $mcpDir 'package.json') | ConvertFrom-Json).version
    $nativeVersion = (Get-Content -Raw (Join-Path $nativeDir 'package.json') | ConvertFrom-Json).version
    if ($mcpVersion -ne $nativeVersion) {
        throw "package versions diverge: @winkit/mcp is $mcpVersion but @winkit/win32-x64-msvc is $nativeVersion"
    }

    Write-Host "Packing packages..."
    $mcpTarball = Pack-Package $mcpDir '@winkit/mcp' $mcpVersion
    $nativeTarball = Pack-Package $nativeDir '@winkit/win32-x64-msvc' $nativeVersion

    # 3. Install both tarballs into the isolated project with the isolated
    # cache. --ignore-scripts proves the packages need no install scripts.
    Push-Location $project
    try {
        # Anchor npm to this project: without a package.json here, npm walks
        # up the directory tree and can resolve a developer's user-level
        # node_modules, silently "installing" nothing into this project.
        Set-Content -Path (Join-Path $project 'package.json') -Value '{"name":"winkit-packed-smoke","private":true,"version":"0.0.0"}' -Encoding Ascii
        # Run npm through cmd /c with the explicit .cmd shim so npm.ps1 is
        # never resolved and its stderr shim cannot turn npm notices into
        # terminating errors. cmd ignores .ps1 files, so this reliably picks
        # npm.cmd/npm.exe. Paths are quoted because the temp path can
        # contain spaces.
        $install = "npm.cmd install --no-audit --no-fund --ignore-scripts --loglevel=error `"$mcpTarball`" `"$nativeTarball`""
        cmd /c $install
        if ($LASTEXITCODE -ne 0) { throw "npm install exited $LASTEXITCODE" }
    } finally {
        Pop-Location
    }

    $installedLauncher = Join-Path $project 'node_modules\@winkit\mcp\bin\winkit.js'
    $installedNative = Join-Path $project 'node_modules\@winkit\win32-x64-msvc\bin\winkit.exe'
    Assert-Check 'packed install places the launcher' (Test-Path $installedLauncher) 'launcher bin missing after install'
    Assert-Check 'packed install places the native runtime' (Test-Path $installedNative) 'native bin missing after install'

    if ((Test-Path $installedLauncher) -and (Test-Path $installedNative)) {
        Remove-Item Env:\WINKIT_NATIVE_PATH -ErrorAction SilentlyContinue

        Push-Location $project
        try {
            # --version
            $out = & node $installedLauncher --version 2>$null
            Assert-Check '--version prints winkit <semver>' ($LASTEXITCODE -eq 0 -and ($out -match '^winkit \d+\.\d+\.\d+')) "got: $out"

            # --help
            $help = & node $installedLauncher --help 2>$null | Out-String
            Assert-Check '--help exits 0 and mentions WinKit' ($LASTEXITCODE -eq 0 -and $help -match 'WinKit') "got: $help"

            # doctor (the packed install must be healthy end to end)
            $doctor = & node $installedLauncher doctor --json 2>$null | Out-String
            $doctorOk = $LASTEXITCODE -eq 0 -and ($doctor -match '"ok":\s*true')
            Assert-Check 'doctor passes on a packed install' $doctorOk "got: $doctor"

            # doctor exit-code propagation (missing config must fail)
            $missingConfig = Join-Path $temp 'does-not-exist.toml'
            & node $installedLauncher doctor --json --config $missingConfig *> $null
            Assert-Check 'doctor propagates a failing exit code' ($LASTEXITCODE -ne 0) "exit was $LASTEXITCODE"

            # init --client codex
            $codex = & node $installedLauncher init --client codex 2>$null | Out-String
            Assert-Check 'init --client codex prints mcp_servers.winkit' ($LASTEXITCODE -eq 0 -and $codex -match 'mcp_servers\.winkit') "got: $codex"

            # init --client opencode
            $opencode = & node $installedLauncher init --client opencode 2>$null | Out-String
            Assert-Check 'init --client opencode prints mcpServers' ($LASTEXITCODE -eq 0 -and $opencode -match 'mcpServers') "got: $opencode"

            # configure --dry-run (no changes: prints the effective summary)
            $configure = & node $installedLauncher configure --dry-run 2>$null | Out-String
            Assert-Check 'configure --dry-run prints the config summary' ($LASTEXITCODE -eq 0 -and $configure -match 'WinKit configuration' -and $configure -match 'Permission mode') "got: $configure"

            # configure --set --dry-run (a staged mutation shows the change
            # list and stays a dry run: nothing is written)
            $configureSet = & node $installedLauncher configure --dry-run --set limits.operation_timeout_ms=40000 2>$null | Out-String
            Assert-Check 'configure --set --dry-run shows changes without writing' ($LASTEXITCODE -eq 0 -and $configureSet -match 'limits.operation_timeout_ms' -and $configureSet -match 'Dry run') "got: $configureSet"

            # MCP initialize handshake over stdio (protocol-clean stdout:
            # every line must parse as a JSON-RPC frame)
            $initialize = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"pack-test","version":"0.0.0"}}}'
            $exit = '{"jsonrpc":"2.0","method":"exit"}'
            $frames = ($initialize + "`n" + $exit + "`n")
            $handshake = $frames | & node $installedLauncher 2>$null
            $lines = @($handshake | Where-Object { $_.Trim() -ne '' })
            $allJson = $true
            foreach ($line in $lines) {
                try { $null = $line | ConvertFrom-Json } catch { $allJson = $false }
            }
            Assert-Check 'MCP stdout carries only JSON-RPC frames' $allJson "got: $handshake"
            $reply = $lines | Select-Object -First 1
            $parsed = $null
            try { $parsed = $reply | ConvertFrom-Json } catch {}
            $handshakeOk = $null -ne $parsed -and $parsed.result.serverInfo.name -eq 'winkit' -and $parsed.id -eq 1
            Assert-Check 'MCP initialize over stdio succeeds through the launcher' $handshakeOk "got: $reply"

            # Installed-bin invocation: the node_modules/.bin shim is exactly
            # what npx and MCP clients resolve when they run `winkit`. (A bare
            # `npx --no-install winkit` is deliberately not used: npx walks
            # up to user-level node_modules on developer machines and can
            # resolve an unrelated installation.)
            $binShim = Join-Path $project 'node_modules\.bin\winkit.cmd'
            $binOut = & $binShim --version 2>$null
            Assert-Check 'installed bin shim runs --version' ($LASTEXITCODE -eq 0 -and ($binOut -match '^winkit \d+\.\d+\.\d+')) "got: $binOut"

            # Missing-native-runtime behavior: hide the native package and
            # confirm the launcher fails with an actionable message.
            $nativePkgDir = Join-Path $project 'node_modules\@winkit\win32-x64-msvc'
            $hidden = Join-Path $project 'node_modules\@winkit\win32-x64-msvc.hidden'
            if (Test-Path $nativePkgDir) {
                Rename-Item $nativePkgDir $hidden
                $err = & node $installedLauncher --version 2>&1 | Out-String
                $missingOk = ($LASTEXITCODE -ne 0) -and ($err -match 'Windows x64')
                Assert-Check 'missing native runtime fails with an actionable message' $missingOk "got: $err"
                Rename-Item $hidden $nativePkgDir
            }
        } finally {
            Pop-Location
        }
    }
} finally {
    # 4. Unconditional cleanup: the tarballs and the temp project this
    # script created. The temp leaf is verified to be a `.pack-smoke-*`
    # directory before removal, so a corrupted variable can never make this
    # delete anything outside the repository-created smoke-test directory.
    if ($null -ne $mcpTarball -and (Test-Path $mcpTarball)) {
        Remove-Item -Force $mcpTarball -ErrorAction SilentlyContinue
    }
    if ($null -ne $nativeTarball -and (Test-Path $nativeTarball)) {
        Remove-Item -Force $nativeTarball -ErrorAction SilentlyContinue
    }
    $tempLeaf = Split-Path -Leaf $temp
    if ($tempLeaf -like '.pack-smoke-*' -and (Test-Path $temp)) {
        Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
    }
}

if ($failures -gt 0) {
    Write-Host "Packed-package smoke test: $failures check(s) FAILED." -ForegroundColor Red
    exit 1
}
Write-Host "Packed-package smoke test: all checks passed." -ForegroundColor Green
exit 0
