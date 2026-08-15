# WinKit benchmark: end-to-end tool latency over MCP stdio.
# Runs against the release binary; Chrome rows need a debug Chrome on 9222.
param(
    [string]$Bin = ".\target\release\winkit.exe",
    [int]$Runs = 3,
    [int]$ChromeRuns = 2
)

# Native stderr (winkit's startup log) must never abort the run: under
# $ErrorActionPreference = "Stop" on Windows PowerShell 5.1, stderr redirected
# with 2>$null is still promoted to a terminating error. "Continue" keeps the
# script resilient; Resolve-Path below opts into Stop explicitly.
$ErrorActionPreference = "Continue"
$bin = (Resolve-Path $Bin -ErrorAction Stop).Path
$init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}}'
$notify = '{"jsonrpc":"2.0","method":"notifications/initialized"}'
$results = [System.Collections.Generic.List[object]]::new()

function Invoke-Frame([string]$name, [string]$argsJson) {
    $call = '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"' + $name + '","arguments":' + $argsJson + '}}'
    $frames = $init + "`n" + $notify + "`n" + $call + "`n"
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $out = @($frames | & $bin 2>$null)
    $sw.Stop()
    $errFrame = $out | Where-Object { $_ -match '"id":2' -and $_ -match '"error"' } | Select-Object -First 1
    return [pscustomobject]@{ Ms = $sw.Elapsed.TotalMilliseconds; Error = $null -ne $errFrame; Out = $out }
}

function Run-Tool([string]$name, [string]$argsJson, [int]$runs) {
    $times = [System.Collections.Generic.List[double]]::new()
    $errors = 0
    for ($i = 0; $i -lt $runs; $i++) {
        $r = Invoke-Frame $name $argsJson
        if ($r.Error) { $errors++ }
        $times.Add($r.Ms)
    }
    $sorted = $times | Sort-Object
    $med = $sorted[[math]::Floor($sorted.Count / 2)]
    $script:results.Add([pscustomobject]@{
        Tool = $name; Runs = $runs
        MinMs = [math]::Round($sorted[0], 0)
        MedianMs = [math]::Round($med, 0)
        MaxMs = [math]::Round($sorted[-1], 0)
        Errors = $errors
    })
    Write-Host ("{0,-28} runs={1} min={2}ms median={3}ms max={4}ms errors={5}" -f `
        $name, $runs, $sorted[0].ToString("0"), $med.ToString("0"), $sorted[-1].ToString("0"), $errors)
}

# Discover a tab id for the Chrome rows (skip them if Chrome is not reachable).
$tabId = $null
try {
    $r = Invoke-Frame "chrome_list_tabs" "{}"
    $m = [regex]::Match(($r.Out -join "`n"), '\\?"id\\?":\\?"([0-9A-F]{32})')
    if ($m.Success) { $tabId = $m.Groups[1].Value }
} catch { }

Run-Tool "system_info" "{}" $Runs
Run-Tool "snapshot" "{}" $Runs
Run-Tool "list_processes" "{}" $Runs
Run-Tool "get_process" '{"pid":4}' $Runs
Run-Tool "get_process_tree" '{"pid":4}' $Runs
Run-Tool "find_process" '{"name":"chrome"}' $Runs
Run-Tool "list_listening_ports" "{}" $Runs
Run-Tool "find_process_on_port" '{"port":9222}' $Runs
Run-Tool "list_network_interfaces" "{}" $Runs
Run-Tool "list_connections" "{}" $Runs
Run-Tool "list_drives" "{}" $Runs
Run-Tool "disk_usage" '{"path":"."}' $Runs
Run-Tool "list_services" "{}" $Runs
Run-Tool "get_service" '{"name":"Spooler"}' $Runs
Run-Tool "get_recent_events" "{}" $Runs
Run-Tool "list_windows" "{}" $Runs
Run-Tool "dev_environment" "{}" $Runs
Run-Tool "list_applications" "{}" $Runs
Run-Tool "get_application" '{"id":"chrome"}' $Runs
Run-Tool "chrome_info" "{}" $Runs
Run-Tool "chrome_list_tabs" "{}" $Runs
Run-Tool "system_health" "{}" $Runs
Run-Tool "system_diagnose" "{}" $Runs

if ($tabId) {
    $tabArgs = '{"tab_id":"' + $tabId + '"}'
    Run-Tool "chrome_get_tab" $tabArgs $ChromeRuns
    Run-Tool "chrome_get_tab_performance" $tabArgs $ChromeRuns
    Run-Tool "chrome_get_tab_memory" $tabArgs $ChromeRuns
    Run-Tool "chrome_get_tab_network" $tabArgs $ChromeRuns
    Run-Tool "chrome_get_tab_runtime" $tabArgs $ChromeRuns
    Run-Tool "chrome_diagnose_tab" $tabArgs $ChromeRuns
    Run-Tool "chrome_tab_trend" $tabArgs $ChromeRuns
} else {
    Write-Host "No Chrome tab discovered - skipping Chrome rows"
}

$outDir = Join-Path $env:TEMP "opencode"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$sorted = $results | Sort-Object MedianMs
$sorted | Format-Table -AutoSize | Out-File (Join-Path $outDir "bench_results.txt") -Encoding utf8
$sorted | ConvertTo-Json | Out-File (Join-Path $outDir "bench_results.json") -Encoding utf8
Write-Host "Saved to $outDir\bench_results.{txt,json}"
