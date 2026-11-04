$ErrorActionPreference = 'Stop'
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run as administrator' }
$repo = Split-Path $PSScriptRoot -Parent
$userName = 'user'
$base = 'C:\Users\user\AppData\Local\daisyloop'
$install = Join-Path $env:ProgramFiles 'daisyloop'
$exe = Join-Path $install 'daisyloop.exe'
$prev = Join-Path $install 'daisyloop.exe.prev'
$sidecarExe = Join-Path $install 'sidecar\daisyloop-lhm.exe'
$configPath = Join-Path $install 'config.toml'
$oldBin = Join-Path $base 'bin'
$oldExe = Join-Path $oldBin 'daisyloop.exe'
$oldSidecar = Join-Path $oldBin 'sidecar\daisyloop-lhm.exe'
$lhmExe = 'C:\Users\user\AppData\Local\LibreHardwareMonitor\LibreHardwareMonitor.exe'
$newExe = Join-Path $repo 'target\release\daisyloop.exe'
$newSidecar = Join-Path $repo 'sidecar\publish'
$stop = Join-Path $base 'stop'
$ok = $false
$oldRunning = $false
$swappedExe = $false
$cutover = $false
$userTaskDisabled = $false
$taskXml = $null
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" } }
function Procs($path) { @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $path }) }
New-Item -ItemType Directory -Force $base | Out-Null
Start-Transcript -Path (Join-Path $base 'install.log') -Append | Out-Null
try {
    foreach ($p in $newExe, (Join-Path $newSidecar 'daisyloop-lhm.exe'), (Join-Path $PSScriptRoot 'watchdog.ps1')) { if (-not (Test-Path $p)) { throw "missing $p (run scripts\install.ps1 build first)" } }
    $old = Get-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue
    if ($old) {
        $taskXml = Export-ScheduledTask -TaskName 'daisyloop-watchdog'
        Disable-ScheduledTask -TaskName 'daisyloop-watchdog' | Out-Null
        Stop-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue
    }
    if (Get-ScheduledTask -TaskName 'daisyloop' -ErrorAction SilentlyContinue) {
        Disable-ScheduledTask -TaskName 'daisyloop' | Out-Null
        $userTaskDisabled = $true
    }
    $running = @(Procs $oldExe) + @(Procs $exe)
    $oldRunning = $running.Count -gt 0
    if ($oldRunning) {
        Set-Content $stop ''
        $running | Wait-Process -Timeout 240 -ErrorAction SilentlyContinue
        $running | Where-Object { -not $_.HasExited } | Stop-Process -Force
        $running | Wait-Process -Timeout 20 -ErrorAction SilentlyContinue
        Remove-Item $stop -ErrorAction SilentlyContinue
    }
    @(Procs $oldSidecar) + @(Procs $sidecarExe) | Stop-Process -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $install | Out-Null
    New-Item -ItemType Directory -Force (Join-Path $install 'state') | Out-Null
    if (Test-Path $exe) { Copy-Item $exe $prev -Force }
    Copy-Item $newExe $exe -Force
    $swappedExe = $true
    Get-ChildItem $base -Filter 'committed-*' -File -ErrorAction SilentlyContinue | Remove-Item -Force
    Copy-Item (Join-Path $PSScriptRoot 'watchdog.ps1') (Join-Path $install 'watchdog.ps1') -Force
    New-Item -ItemType Directory -Force (Join-Path $install 'sidecar') | Out-Null
    Copy-Item (Join-Path $newSidecar '*') (Join-Path $install 'sidecar') -Recurse -Force
    New-Item -ItemType Directory -Force (Join-Path $install 'models') | Out-Null
    foreach ($m in 'feature_spec_deploy.json', 'model_deploy.json') { Copy-Item (Join-Path $PSScriptRoot "..\models\$m") (Join-Path $install 'models') -Force }
    if (-not (Test-Path $configPath)) {
        $text = "data_dir = '$base'`r`nwrites_enabled = false`r`n"
        [IO.File]::WriteAllText($configPath, $text, (New-Object Text.UTF8Encoding($false)))
    }
    if (-not (Select-String -Path $configPath -Pattern '^\s*data_dir\s*=' -Quiet)) { throw "$configPath has no data_dir line" }
    & icacls $install /setowner '*S-1-5-32-544' /T | Out-Null
    Check 'icacls setowner'
    & icacls $install /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "${userName}:(OI)(CI)RX" | Out-Null
    Check 'icacls grant'
    & icacls (Join-Path $install 'state') /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    Check 'icacls state'
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$(Join-Path $install 'watchdog.ps1')`" -DataDir `"$base`""
    $repeat = (New-ScheduledTaskTrigger -Once -At '2000-01-01' -RepetitionInterval (New-TimeSpan -Minutes 1)).Repetition
    $boot = New-ScheduledTaskTrigger -AtStartup
    $boot.Delay = 'PT1M'
    $boot.Repetition = $repeat
    $armed = New-ScheduledTaskTrigger -Once -At (Get-Date)
    $armed.Repetition = $repeat
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -Priority 4
    Register-ScheduledTask -TaskName 'daisyloop-watchdog' -Action $action -Trigger @($boot, $armed) -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName 'daisyloop-watchdog'
    Start-Sleep -Seconds 3
    $t0 = (Get-ScheduledTaskInfo -TaskName 'daisyloop-watchdog').LastRunTime
    Start-Sleep -Seconds 22
    $p = Get-CimInstance Win32_Process -Filter "Name='daisyloop.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
    if (-not $p) { throw 'new collector is not running 25 s after the watchdog start' }
    $beat = Join-Path $base 'heartbeat'
    $age = if (Test-Path $beat) { [int]((Get-Date) - (Get-Item $beat).LastWriteTime).TotalSeconds } else { -1 }
    if ($age -lt 0 -or $age -gt 30) { throw "heartbeat age ${age}s" }
    Start-Sleep -Seconds 70
    $t1 = (Get-ScheduledTaskInfo -TaskName 'daisyloop-watchdog').LastRunTime
    if ($t1 -lt $t0.AddSeconds(30)) { throw "watchdog LastRunTime did not advance ($t0 -> $t1), repetition is not armed" }
    if (-not (Procs $exe)) { throw 'collector died during verification' }
    if (-not (Procs $sidecarExe)) { throw 'sidecar not running' }
    if (Procs $oldExe) { throw 'old collector came back' }
    $log = @(Get-Content (Join-Path $base 'collector.log') -Tail 500)
    $from = ($log | Select-String 'collector started, data' | Select-Object -Last 1).LineNumber
    if (-not $from) { throw 'no start line from the new collector' }
    $fresh = @($log | Select-Object -Skip $from)
    foreach ($src in 'd5next', 'nvml', 'lhm') {
        $last = $fresh | Where-Object { $_ -match "\] $src (unavailable|recovered|first data)" } | Select-Object -Last 1
        if ($last -match 'unavailable') { throw "$src unavailable: $last" }
        if ($src -eq 'lhm' -and -not $last) { throw 'no LHM data from the sidecar' }
    }
    if (-not ($fresh -match '\] committed [1-9]\d* telemetry')) { throw 'no commit by the new collector' }
    $cutover = $true
    Unregister-ScheduledTask -TaskName 'daisyloop' -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName 'LibreHardwareMonitor' -Confirm:$false -ErrorAction SilentlyContinue
    $sid = (New-Object Security.Principal.NTAccount($userName)).Translate([Security.Principal.SecurityIdentifier]).Value
    $run = "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Run"
    if (Test-Path $run) { foreach ($n in 'LibreHardwareMonitor', 'daisyloop') { Remove-ItemProperty -Path $run -Name $n -ErrorAction SilentlyContinue } }
    Get-Process LibreHardwareMonitor -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $lhmExe } | Stop-Process -Force -ErrorAction Continue
    if (Test-Path $oldBin) { Remove-Item $oldBin -Recurse -Force -ErrorAction Continue }
    $p = Get-CimInstance Win32_Process -Filter "Name='daisyloop.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
    $owner = if ($p) { (Invoke-CimMethod -InputObject $p -MethodName GetOwner).User } else { 'none' }
    "task: $((Get-ScheduledTask -TaskName 'daisyloop-watchdog').State), LastRunTime $t0 -> $t1"
    "collector: pid $($p.ProcessId) owner $owner, heartbeat age ${age}s, exe $exe"
    "sidecar: $(@(Get-Process daisyloop-lhm -ErrorAction SilentlyContinue).Count) process"
    "user task daisyloop: $(if (Get-ScheduledTask -TaskName 'daisyloop' -ErrorAction SilentlyContinue) { 'STILL PRESENT' } else { 'removed' })"
    "old bin: $(if (Test-Path $oldBin) { 'STILL PRESENT' } else { 'removed' })"
    "install dir ACL:"
    & icacls $install
    Get-Content (Join-Path $base 'collector.log') -Tail 4
    $ok = $true
} catch {
    if ($cutover) {
        Write-Host "CLEANUP INCOMPLETE: $($_.Exception.Message)" -ForegroundColor Yellow
        $ok = $true
    } else {
        Write-Host "INSTALL FAILED: $($_.Exception.Message)" -ForegroundColor Red
        $dying = @(Procs $exe) + @(Procs $sidecarExe)
        $steps = @(
            { Disable-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue | Out-Null },
            { Stop-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue },
            { $dying | Stop-Process -Force -ErrorAction SilentlyContinue; $dying | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue },
            { if ($swappedExe -and (Test-Path $prev)) { Copy-Item $prev $exe -Force } },
            { if ($taskXml) { Register-ScheduledTask -TaskName 'daisyloop-watchdog' -Xml ($taskXml | Out-String) -Force | Out-Null } },
            { if ($userTaskDisabled) { Enable-ScheduledTask -TaskName 'daisyloop' | Out-Null } },
            {
                if ($oldRunning -and (Test-Path $oldExe)) {
                    Start-Process $oldExe -WorkingDirectory $oldBin -WindowStyle Hidden
                    Write-Host 'previous collector restarted from the old location'
                } elseif ($oldRunning -and (Test-Path $prev)) {
                    Start-Process $exe -WorkingDirectory $install -WindowStyle Hidden
                    Write-Host 'previous collector restarted from the install dir'
                }
            }
        )
        foreach ($step in $steps) {
            try { & $step } catch { Write-Host "ROLLBACK STEP FAILED: $($_.Exception.Message)" -ForegroundColor Red }
        }
    }
} finally {
    Stop-Transcript | Out-Null
}
if (-not $ok) {
    Read-Host 'Install failed, see install.log. Press Enter to close'
    exit 1
}
