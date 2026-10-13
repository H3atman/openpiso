# Register the OpenPiso client watchdog as a per-logon Scheduled Task.
#
# Run this ONCE while preparing the diskless master image, in an elevated
# PowerShell. After the image is sealed, every diskless boot starts the
# watchdog automatically and a customer killing the client only causes a
# relaunch; killing the watchdog too is undone by the next reboot (ADR-0005).

param(
    [string]$TaskName = "OpenPiso Client Watchdog",
    [string]$WatchdogScript = "C:\Program Files\OpenPiso Client\openpiso-watchdog.ps1",
    [string]$ServerUrl = "http://127.0.0.1:3210",
    [string]$PcId = "pc-01"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $WatchdogScript)) {
    throw "Watchdog script not found at $WatchdogScript. Copy openpiso-watchdog.ps1 into the image first."
}

$action = New-ScheduledTaskAction -Execute "powershell.exe" `
    -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$WatchdogScript`" -ServerUrl `"$ServerUrl`" -PcId `"$PcId`""

# Start at logon of the kiosk user and keep running for the whole session.
$trigger = New-ScheduledTaskTrigger -AtLogOn

$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit (New-TimeSpan -Days 0)

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Settings $settings -RunLevel Highest -Force | Out-Null

Write-Host "Registered scheduled task '$TaskName'."
Write-Host "Seal the master image now; the watchdog will start at every diskless boot."
