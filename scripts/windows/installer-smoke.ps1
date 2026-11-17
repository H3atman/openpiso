# Smoke-test the OpenPiso Windows installers on a single machine.
#
# This installs OpenPiso Admin and Client on THIS machine and starts them, so
# run it only on a disposable machine (a CI runner or a throwaway VM). It must
# run in an ELEVATED PowerShell (the Client installs per machine with a
# service), and ports 3210 and 8080 must be free:
#   powershell -ExecutionPolicy Bypass -File .\installer-smoke.ps1 -InstallerDir dist\installers
#
# It checks that the Admin installs, adds its firewall rules, and serves an
# unconfigured Web Admin; that the Client installs with a running Guard; and
# that the Client's heartbeat shows up as a Pending PC with the right Approval
# Code. It never approves the PC. A JSON report lands in -OutDir either way.

param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerDir,
    [string]$OutDir = "smoke-out"
)

$ErrorActionPreference = "Stop"

$webAdmin = "http://127.0.0.1:8080/web-admin"
$adminExe = "$env:LOCALAPPDATA\OpenPiso Central Terminal\openpiso-admin.exe"
$clientExe = "$env:ProgramFiles\OpenPiso Client\openpiso-client.exe"
$guardService = "OpenPisoClientGuard"

$checks = New-Object System.Collections.ArrayList
$failed = $false
$clientLaunchedBy = $null
$pending = $null
$installers = @{}

# Records a check and prints it; a failure stops the remaining checks.
function Add-Check([string]$Name, [bool]$Ok, [string]$Detail) {
    [void]$script:checks.Add([ordered]@{ name = $Name; ok = $Ok; detail = $Detail })
    if ($Ok) { Write-Host "PASS ${Name}: $Detail" } else { Write-Host "FAIL ${Name}: $Detail" }
    if (-not $Ok) {
        $script:failed = $true
        throw "Stopping after failed check '$Name'"
    }
}

# Polls $Condition until it returns something truthy (which is returned) or
# the timeout passes (returns $null). Errors count as "not yet".
function Wait-Until([scriptblock]$Condition, [int]$TimeoutSeconds, [int]$IntervalSeconds) {
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ($true) {
        try {
            $result = & $Condition
            if ($result) { return $result }
        } catch {}
        if ((Get-Date) -ge $deadline) { return $null }
        Start-Sleep -Seconds $IntervalSeconds
    }
}

function Find-Installer([string]$SubDir) {
    $found = @(Get-ChildItem -Path (Join-Path $InstallerDir $SubDir) -Filter "*-setup.exe" -File -ErrorAction SilentlyContinue)
    if ($found.Count -ne 1) {
        throw "Expected exactly one *-setup.exe in $(Join-Path $InstallerDir $SubDir), found $($found.Count)"
    }
    return $found[0]
}

try {
    $adminInstaller = Find-Installer "admin-web-admin"
    $clientInstaller = Find-Installer "client"
    $installers = [ordered]@{ admin = $adminInstaller.Name; client = $clientInstaller.Name }

    # 1. Admin installer (per user; its hook adds the firewall rules).
    $proc = Start-Process -FilePath $adminInstaller.FullName -ArgumentList "/S" -Wait -PassThru
    Add-Check "admin_installer_exit" ($proc.ExitCode -eq 0) "exit code $($proc.ExitCode)"
    Add-Check "admin_exe_installed" (Test-Path $adminExe) $adminExe

    # 2. Firewall rules. The hook only warns when netsh fails, so assert them here.
    foreach ($rule in @(
            @{ check = "firewall_device_api"; name = "OpenPiso device API"; protocol = "TCP" },
            @{ check = "firewall_discovery"; name = "OpenPiso discovery"; protocol = "UDP" })) {
        $filter = Get-NetFirewallRule -DisplayName $rule.name -ErrorAction SilentlyContinue |
            Where-Object { "$($_.Enabled)" -eq "True" -and "$($_.Direction)" -eq "Inbound" -and "$($_.Action)" -eq "Allow" } |
            Get-NetFirewallPortFilter
        $ports = @($filter | ForEach-Object { "$($_.Protocol) $($_.LocalPort)" })
        Add-Check $rule.check ($ports -contains "$($rule.protocol) 3210") "'$($rule.name)' enabled inbound allow filters: $($ports -join ', ')"
    }

    # 3. Admin serves an unconfigured Web Admin.
    Start-Process -FilePath $adminExe | Out-Null
    $session = Wait-Until { Invoke-RestMethod -Uri "$webAdmin/session" -UseBasicParsing -TimeoutSec 5 } 120 2
    Add-Check "web_admin_session" ($null -ne $session) "GET /web-admin/session within 120 s"
    Add-Check "web_admin_unconfigured" ($session.configured -eq $false) "configured = $($session.configured)"

    # 4. Client installer. Its hook can abort without a documented exit code,
    # so the files and the Guard service are checked too.
    $proc = Start-Process -FilePath $clientInstaller.FullName -ArgumentList "/S" -Wait -PassThru
    Add-Check "client_installer_exit" ($proc.ExitCode -eq 0) "exit code $($proc.ExitCode)"
    Add-Check "client_exe_installed" (Test-Path $clientExe) $clientExe
    $guard = Wait-Until {
        $svc = Get-Service -Name $guardService -ErrorAction SilentlyContinue
        if ($svc -and $svc.Status -eq "Running") { $svc }
    } 30 1
    Add-Check "guard_running" ($null -ne $guard) "$guardService Running within 30 s"

    # 5. The Guard launches the Client into the active console session; a CI
    # runner may have none, so fall back to starting it here.
    $clientProc = Wait-Until { Get-Process -Name "openpiso-client" -ErrorAction SilentlyContinue } 45 1
    if ($clientProc) {
        $clientLaunchedBy = "guard"
    } else {
        $clientLaunchedBy = "script"
        Start-Process -FilePath $clientExe | Out-Null
        if ($env:GITHUB_ACTIONS) {
            Write-Host "::warning::Guard did not launch the Client (no console session?); the smoke script started it directly"
        }
    }
    Write-Host "Client launched by: $clientLaunchedBy"

    # 6. Owner Session via first-run setup. The PIN stays in memory only.
    $pin = "{0:D8}" -f (Get-Random -Minimum 0 -Maximum 100000000)
    $body = @{ pin = $pin } | ConvertTo-Json -Compress
    try {
        $setup = Invoke-RestMethod -Method Post -Uri "$webAdmin/setup" -Body $body -ContentType "application/json" `
            -SessionVariable web -UseBasicParsing -TimeoutSec 10
        Add-Check "owner_setup" ($null -ne $setup.csrf_token) "POST /web-admin/setup"
    } catch {
        if ($failed) { throw }
        Add-Check "owner_setup" $false "POST /web-admin/setup: $($_.Exception.Message)"
    }

    # 7. The unknown Client's heartbeat (every 5 s) lists it as a Pending PC.
    # It reaches the Admin over loopback by default, or over this machine's
    # LAN address when Server Discovery answers first. Never approve it: a
    # registered PC with no time is told to power off.
    $pending = Wait-Until {
        $list = Invoke-RestMethod -Uri "$webAdmin/pcs/pending" -WebSession $web -UseBasicParsing -TimeoutSec 5
        @($list | Where-Object { $_ }) | Select-Object -First 1
    } 120 3
    Add-Check "pending_pc_listed" ($null -ne $pending) "GET /web-admin/pcs/pending non-empty within 120 s"
    $expectedCode = ($pending.mac -replace ":", "").ToUpper()
    $expectedCode = $expectedCode.Substring([Math]::Max(0, $expectedCode.Length - 4))
    Add-Check "approval_code" ($pending.approval_code -ceq $expectedCode) "mac $($pending.mac) -> $($pending.approval_code), expected $expectedCode"
    $localIps = @("127.0.0.1") + @(Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object { $_.IPAddress })
    Add-Check "pending_source_ip" ($localIps -contains $pending.source_ip) "source_ip $($pending.source_ip) is this machine"
} catch {
    # A failed check already recorded itself; anything else is a script error.
    if (-not $failed) {
        $failed = $true
        [void]$checks.Add([ordered]@{ name = "script_error"; ok = $false; detail = $_.Exception.Message })
        Write-Host "FAIL script_error: $($_.Exception.Message)"
    }
} finally {
    # Stop the Guard first, or it relaunches the Client every 15 s.
    try { sc.exe stop $guardService | Out-Null } catch {}
    try { sc.exe config $guardService start= disabled | Out-Null } catch {}
    Get-Process -Name "openpiso-client", "openpiso-admin" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue

    $report = [ordered]@{
        ok = -not $failed
        checks = $checks
        client_launched_by = $clientLaunchedBy
        pending_pc = $pending
        installers = $installers
    }
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
    $report | ConvertTo-Json -Depth 5 | Set-Content -Path (Join-Path $OutDir "installer-smoke.json") -Encoding UTF8
}

if ($failed) { exit 1 }
exit 0
