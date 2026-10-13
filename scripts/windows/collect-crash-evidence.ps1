# Collect Central Terminal crash evidence (openpiso-admin.exe).
#
# Run on the shop's server PC in an ELEVATED PowerShell:
#   powershell -ExecutionPolicy Bypass -File .\collect-crash-evidence.ps1
#
# It does two things:
#   1. Arms WER LocalDumps so the NEXT "has stopped working" leaves a minidump.
#   2. Zips everything already on disk that describes past crashes.
#
# Nothing here is a secret: diagnostics logs are the app's own INFO/WARN lines
# and the WER records are Windows' own. Review the zip before sending it on.

param(
    [string]$ProcessName = "openpiso-admin",
    [string]$OutDir      = "$env:USERPROFILE\Desktop"
)

$ErrorActionPreference = "Continue"
$exe   = "$ProcessName.exe"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$work  = Join-Path $env:TEMP "openpiso-crash-$stamp"
New-Item -ItemType Directory -Path $work -Force | Out-Null
$summary = Join-Path $work "00-summary.txt"

# --- 1. Arm WER LocalDumps (needs elevation) -------------------------------
$dumpDir = "$env:LOCALAPPDATA\CrashDumps"
$werKey  = "HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\$exe"
$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()
           ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    "NOT ARMED: re-run this script as Administrator to capture a dump of the next crash." |
      Tee-Object -FilePath $summary -Append
} else {
    try {
        New-Item -Path $werKey -Force -ErrorAction Stop | Out-Null
        New-ItemProperty -Path $werKey -Name DumpFolder -Value $dumpDir -PropertyType ExpandString -Force -ErrorAction Stop | Out-Null
        New-ItemProperty -Path $werKey -Name DumpCount  -Value 10 -PropertyType DWord -Force -ErrorAction Stop | Out-Null
        # 2 = full dump. Bigger, but a minidump often loses the Rust panic message.
        New-ItemProperty -Path $werKey -Name DumpType   -Value 2  -PropertyType DWord -Force -ErrorAction Stop | Out-Null
        $readback = Get-ItemProperty -Path $werKey -ErrorAction Stop
        "ARMED: WER LocalDumps -> $($readback.DumpFolder) (DumpType=$($readback.DumpType))" |
          Tee-Object -FilePath $summary -Append
    } catch {
        "NOT ARMED: $($_.Exception.Message)" | Tee-Object -FilePath $summary -Append
    }
}

# --- 2. Windows crash records ----------------------------------------------
Get-WinEvent -FilterHashtable @{
    LogName      = 'Application'
    ProviderName = @('Application Error','Application Hang','Windows Error Reporting')
    StartTime    = (Get-Date).AddDays(-30)
} -ErrorAction SilentlyContinue |
  Where-Object { $_.Message -match [regex]::Escape($ProcessName) -or $_.Message -match 'OpenPiso' } |
  Select-Object TimeCreated, Id, ProviderName, Message |
  Format-List | Out-File "$work\01-wer-events.txt" -Encoding utf8

# Unexpected shutdowns / bugchecks, in case the whole box is going down.
Get-WinEvent -FilterHashtable @{
    LogName   = 'System'
    Id        = @(41,1001,6008,7031,7034)
    StartTime = (Get-Date).AddDays(-30)
} -ErrorAction SilentlyContinue |
  Select-Object TimeCreated, Id, ProviderName, Message |
  Format-List | Out-File "$work\02-system-events.txt" -Encoding utf8

# --- 3. App's own diagnostics logs -----------------------------------------
$logSrc = "$env:APPDATA\org.openpiso.admin\logs"
if (Test-Path $logSrc) {
    Copy-Item $logSrc "$work\03-diagnostics" -Recurse -Force
} else {
    "no diagnostics logs at $logSrc" | Out-File "$work\03-diagnostics-MISSING.txt"
}

# --- 4. Any dumps already present ------------------------------------------
foreach ($d in @($dumpDir, "$env:LOCALAPPDATA\Microsoft\Windows\WER\ReportArchive")) {
    if (Test-Path $d) {
        Get-ChildItem $d -Recurse -File -ErrorAction SilentlyContinue |
          Where-Object { $_.Name -match $ProcessName -or $_.FullName -match 'OpenPiso' } |
          Select-Object FullName, Length, LastWriteTime |
          Format-List | Out-File "$work\04-existing-dumps.txt" -Append -Encoding utf8
    }
}

# --- 5. Environment ---------------------------------------------------------
@(
  "collected : $(Get-Date -Format o)"
  "host      : $env:COMPUTERNAME"
  "os        : $((Get-CimInstance Win32_OperatingSystem).Caption) $([Environment]::OSVersion.Version)"
  "ram MB    : $([math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory/1MB))"
  "webview2  : $((Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue).pv)"
  "running   : $((Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object Id,StartTime,WorkingSet64,HandleCount | Out-String))"
) | Out-File "$work\05-environment.txt" -Encoding utf8

# --- 6. Zip -----------------------------------------------------------------
$zip = Join-Path $OutDir "openpiso-crash-evidence-$stamp.zip"
Compress-Archive -Path "$work\*" -DestinationPath $zip -Force
Write-Output ""
Write-Output "Evidence written to: $zip"
Get-Content $summary | ForEach-Object { Write-Output $_ }
Write-Output "Re-run this script after the next crash to pick up the dump."
