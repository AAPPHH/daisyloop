$ErrorActionPreference = 'Stop'
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run as administrator' }
$repo = Split-Path $PSScriptRoot -Parent
$userName = 'user'
$base = 'C:\Users\user\AppData\Local\daisyloop'
$bin = Join-Path $base 'bin'
$exe = Join-Path $bin 'daisyloop.exe'
$prev = Join-Path $bin 'daisyloop.exe.prev'
$lhmExe = 'C:\Users\user\AppData\Local\LibreHardwareMonitor\LibreHardwareMonitor.exe'
$newExe = Join-Path $repo 'target\release\daisyloop.exe'
$newSidecar = Join-Path $repo 'sidecar\publish'
$stop = Join-Path $base 'stop'
$ok = $false
$hadRunning = $false
$swappedExe = $false
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" } }
New-Item -ItemType Directory -Force $base | Out-Null
Start-Transcript -Path (Join-Path $base 'install.log') -Append | Out-Null
try {
    foreach ($p in $newExe, (Join-Path $newSidecar 'daisyloop-lhm.exe'), (Join-Path $PSScriptRoot 'watchdog.ps1')) { if (-not (Test-Path $p)) { throw "missing $p (run scripts\install.ps1 build first)" } }
    New-Item -ItemType Directory -Force $bin | Out-Null
    $old = Get-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue
    if ($old) {
        Disable-ScheduledTask -TaskName 'daisyloop-watchdog' | Out-Null
        Stop-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue
    }
    $running = @(Get-Process daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe })
    $hadRunning = $running.Count -gt 0
    if ($hadRunning) {
        Set-Content $stop ''
        $running | Wait-Process -Timeout 240 -ErrorAction SilentlyContinue
        $running | Where-Object { -not $_.HasExited } | Stop-Process -Force
        Remove-Item $stop -ErrorAction SilentlyContinue
    }
    Get-Process daisyloop-lhm -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $bin 'sidecar\daisyloop-lhm.exe') } | Stop-Process -Force
    if (Test-Path $exe) { Copy-Item $exe $prev -Force }
    Copy-Item $newExe $exe -Force
    $swappedExe = $true
    Copy-Item (Join-Path $PSScriptRoot 'watchdog.ps1') (Join-Path $bin 'watchdog.ps1') -Force
    New-Item -ItemType Directory -Force (Join-Path $bin 'sidecar') | Out-Null
    Copy-Item (Join-Path $newSidecar '*') (Join-Path $bin 'sidecar') -Recurse -Force
    & icacls $bin /setowner '*S-1-5-32-544' /T /C | Out-Null
    Check 'icacls setowner'
    & icacls $bin /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${userName}:(OI)(CI)RX" | Out-Null
    Check 'icacls grant'
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$(Join-Path $bin 'watchdog.ps1')`""
    $repeat = (New-ScheduledTaskTrigger -Once -At '2000-01-01' -RepetitionInterval (New-TimeSpan -Minutes 1)).Repetition
    $boot = New-ScheduledTaskTrigger -AtStartup
    $boot.Delay = 'PT1M'
    $boot.Repetition = $repeat
    $armed = New-ScheduledTaskTrigger -Once -At (Get-Date)
    $armed.Repetition = $repeat
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName 'daisyloop-watchdog' -Action $action -Trigger @($boot, $armed) -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName 'daisyloop-watchdog'
    $t0 = (Get-ScheduledTaskInfo -TaskName 'daisyloop-watchdog').LastRunTime
    Start-Sleep -Seconds 25
    $p = Get-CimInstance Win32_Process -Filter "Name='daisyloop.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
    if (-not $p) { throw 'new collector is not running 25 s after the watchdog start' }
    $beat = Join-Path $base 'heartbeat'
    $age = if (Test-Path $beat) { [int]((Get-Date) - (Get-Item $beat).LastWriteTime).TotalSeconds } else { -1 }
    if ($age -lt 0 -or $age -gt 30) { throw "heartbeat age ${age}s" }
    Start-Sleep -Seconds 70
    $t1 = (Get-ScheduledTaskInfo -TaskName 'daisyloop-watchdog').LastRunTime
    if ($t1 -le $t0) { throw "watchdog LastRunTime did not advance ($t0 -> $t1), repetition is not armed" }
    Unregister-ScheduledTask -TaskName 'daisyloop' -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName 'LibreHardwareMonitor' -Confirm:$false -ErrorAction SilentlyContinue
    $sid = (New-Object Security.Principal.NTAccount($userName)).Translate([Security.Principal.SecurityIdentifier]).Value
    $run = "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Run"
    if (Test-Path $run) { foreach ($n in 'LibreHardwareMonitor', 'daisyloop') { Remove-ItemProperty -Path $run -Name $n -ErrorAction SilentlyContinue } }
    Get-Process LibreHardwareMonitor -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $lhmExe } | Stop-Process -Force
    $p = Get-CimInstance Win32_Process -Filter "Name='daisyloop.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
    $owner = if ($p) { (Invoke-CimMethod -InputObject $p -MethodName GetOwner).User } else { 'none' }
    "task: $((Get-ScheduledTask -TaskName 'daisyloop-watchdog').State), LastRunTime $t0 -> $t1"
    "collector: pid $($p.ProcessId) owner $owner, heartbeat age ${age}s"
    "sidecar: $(@(Get-Process daisyloop-lhm -ErrorAction SilentlyContinue).Count) process"
    "user task daisyloop: $(if (Get-ScheduledTask -TaskName 'daisyloop' -ErrorAction SilentlyContinue) { 'STILL PRESENT' } else { 'removed' })"
    Get-Content (Join-Path $base 'collector.log') -Tail 4
    $ok = $true
} catch {
    Write-Host "INSTALL FAILED: $($_.Exception.Message)" -ForegroundColor Red
    try {
        if ($swappedExe -and (Test-Path $prev)) {
            Get-Process daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
            Get-Process daisyloop-lhm -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $bin 'sidecar\daisyloop-lhm.exe') } | Stop-Process -Force
            Copy-Item $prev $exe -Force
        }
        if ($hadRunning -and (Test-Path $exe)) {
            Start-Process $exe -WorkingDirectory $bin -WindowStyle Hidden
            Write-Host 'previous collector restarted'
        }
        if (Get-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue) { Enable-ScheduledTask -TaskName 'daisyloop-watchdog' | Out-Null }
    } catch {
        Write-Host "ROLLBACK FAILED: $($_.Exception.Message)" -ForegroundColor Red
    }
} finally {
    Stop-Transcript | Out-Null
}
if (-not $ok) {
    Read-Host 'Install failed, see install.log. Press Enter to close'
    exit 1
}
