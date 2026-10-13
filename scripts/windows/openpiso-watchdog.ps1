# OpenPiso client watchdog (ADR-0005)
#
# Baked into the diskless master image and run as a per-logon Scheduled Task.
# Relaunches the OpenPiso client if a customer kills it. Because the admin
# server is authoritative for time (ADR-0001), this is about friction and UX,
# not theft prevention: any tampering is wiped by the diskless reboot.
#
# This watchdog is NOT a security boundary. The arms race ends at the power
# button, which the owner controls.

param(
    # Full path to the installed OpenPiso client executable in the master image.
    [string]$ClientPath = "C:\Program Files\OpenPiso Client\openpiso-client.exe",
    # Process name to watch (without .exe).
    [string]$ProcessName = "openpiso-client",
    # Seconds between liveness checks.
    [int]$IntervalSeconds = 5,
    # Admin server base URL for forwarding watchdog diagnostics.
    [string]$ServerUrl = "http://127.0.0.1:3210"
)

function Send-Diagnostic {
    param([string]$Level, [string]$Message)
    # Best-effort: a diskless client's local logs evaporate on reboot, so
    # meaningful watchdog events are forwarded to the server (ADR-0016,
    # ADR-0078). The batched Forwarded Diagnostics endpoint authenticates the
    # device with the `X-OpenPiso-Mac` header (identity hint, same as the
    # heartbeat) and accepts `warn` and `error` lines only; the INFO
    # allowlist follows in a later ticket. A refused or unreachable server
    # must never stop the watchdog.
    if ($Level -notin @("warn", "error")) { return }
    try {
        $mac = (Get-NetAdapter -ErrorAction SilentlyContinue |
            Where-Object { $_.Status -eq "Up" } |
            Sort-Object -Property ifIndex |
            Select-Object -First 1).MacAddress
        $headers = @{}
        if ($mac) {
            # Admin stores MACs lowercase and colon-separated.
            $headers["X-OpenPiso-Mac"] = $mac.Replace("-", ":").ToLowerInvariant()
        }
        $payload = @{
            source = "client"
            lines  = @(@{
                level             = $Level
                target            = "openpiso_watchdog"
                message           = $Message
                timestamp_unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            })
        } | ConvertTo-Json -Compress
        Invoke-RestMethod -Uri "$ServerUrl/api/forwarded-diagnostics" -Method Post `
            -ContentType "application/json" -Headers $headers -Body $payload -TimeoutSec 3 | Out-Null
    } catch {
        # The server may be briefly unreachable; the watchdog must keep running.
    }
}

while ($true) {
    $running = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue
    if (-not $running) {
        if (Test-Path $ClientPath) {
            try {
                Start-Process -FilePath $ClientPath | Out-Null
                Send-Diagnostic -Level "warn" -Message "Client was not running; watchdog relaunched it"
            } catch {
                Send-Diagnostic -Level "error" -Message "Watchdog could not relaunch the client: $($_.Exception.Message)"
            }
        } else {
            Send-Diagnostic -Level "error" -Message "Watchdog could not find the client at $ClientPath"
        }
    }
    Start-Sleep -Seconds $IntervalSeconds
}
