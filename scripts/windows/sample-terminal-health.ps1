# Sample Central Terminal resource use, so a daily crash leaves a trend behind.
#
# Run on the shop's server PC (no elevation needed) and leave it running:
#   powershell -ExecutionPolicy Bypass -File .\sample-terminal-health.ps1
#
# Writes one CSV row per interval for openpiso-admin.exe and every WebView2
# child it owns. If the crash is memory exhaustion or a handle leak, the ramp
# is visible in the rows before the gap; if the numbers are flat right up to
# the crash, memory is ruled out and the dump is the next lead.

param(
    [string]$ProcessName  = "openpiso-admin",
    [int]$IntervalSeconds = 60,
    [string]$OutFile      = "$env:APPDATA\org.openpiso.admin\logs\health-samples.csv"
)

$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Path (Split-Path $OutFile) -Force | Out-Null
if (-not (Test-Path $OutFile)) {
    "timestamp,process,pid,working_set_mb,private_mb,handles,threads,started" |
      Out-File $OutFile -Encoding utf8
}

Write-Output "Sampling $ProcessName every ${IntervalSeconds}s -> $OutFile  (Ctrl+C to stop)"

function Get-DescendantIds {
    param([int[]]$RootIds)
    # WebView2 spawns one process per renderer/GPU/utility role. Only the ones
    # descended from the Central Terminal are ours; the shop PC's browser uses
    # the same executable name.
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($id in $RootIds) { [void]$ids.Add($id) }
    for ($pass = 0; $pass -lt 5; $pass++) {
        $before = $ids.Count
        foreach ($proc in $all) {
            if ($ids.Contains([int]$proc.ParentProcessId)) { [void]$ids.Add([int]$proc.ProcessId) }
        }
        if ($ids.Count -eq $before) { break }
    }
    $ids
}

while ($true) {
    $now  = (Get-Date).ToString("o")
    $root = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)

    if (-not $root) {
        "$now,<not-running>,,,,,," | Out-File $OutFile -Append -Encoding utf8
    } else {
        $ids   = Get-DescendantIds -RootIds ($root | ForEach-Object { $_.Id })
        $procs = Get-Process -Id ($ids) -ErrorAction SilentlyContinue

        foreach ($p in $procs) {
            $started = try { $p.StartTime.ToString("o") } catch { "" }
            $row = "{0},{1},{2},{3},{4},{5},{6},{7}" -f `
                $now, $p.ProcessName, $p.Id,
                [math]::Round($p.WorkingSet64/1MB,1),
                [math]::Round($p.PrivateMemorySize64/1MB,1),
                $p.HandleCount, $p.Threads.Count, $started
            $row | Out-File $OutFile -Append -Encoding utf8
        }
    }
    Start-Sleep -Seconds $IntervalSeconds
}
