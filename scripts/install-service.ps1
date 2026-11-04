$ErrorActionPreference = 'Stop'
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'run as administrator' }
$repo = Split-Path $PSScriptRoot -Parent
$userName = 'user'
$base = 'C:\Users\user\AppData\Local\daisyloop'
$bin = Join-Path $base 'bin'
$exe = Join-Path $bin 'daisyloop.exe'
$lhmExe = 'C:\Users\user\AppData\Local\LibreHardwareMonitor\LibreHardwareMonitor.exe'
$newExe = Join-Path $repo 'target\release\daisyloop.exe'
$newSidecar = Join-Path $repo 'sidecar\publish'
foreach ($p in $newExe, (Join-Path $newSidecar 'daisyloop-lhm.exe'), (Join-Path $PSScriptRoot 'watchdog.ps1')) { if (-not (Test-Path $p)) { throw "missing $p (run scripts\install.ps1 build first)" } }
New-Item -ItemType Directory -Force $bin | Out-Null
Get-ScheduledTask -TaskName 'daisyloop-watchdog' -ErrorAction SilentlyContinue | Stop-ScheduledTask
$stop = Join-Path $base 'stop'
$running = @(Get-Process daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe })
if ($running.Count -gt 0) {
    Set-Content $stop ''
    $running | Wait-Process -Timeout 240 -ErrorAction SilentlyContinue
    $running | Where-Object { -not $_.HasExited } | Stop-Process -Force
    Remove-Item $stop -ErrorAction SilentlyContinue
}
Get-Process daisyloop-lhm -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $bin 'sidecar\daisyloop-lhm.exe') } | Stop-Process -Force
Copy-Item $newExe $exe -Force
Copy-Item (Join-Path $PSScriptRoot 'watchdog.ps1') (Join-Path $bin 'watchdog.ps1') -Force
New-Item -ItemType Directory -Force (Join-Path $bin 'sidecar') | Out-Null
Copy-Item (Join-Path $newSidecar '*') (Join-Path $bin 'sidecar') -Recurse -Force
& icacls $bin /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' "${userName}:(OI)(CI)RX" | Out-Null
Unregister-ScheduledTask -TaskName 'daisyloop' -Confirm:$false -ErrorAction SilentlyContinue
Unregister-ScheduledTask -TaskName 'LibreHardwareMonitor' -Confirm:$false -ErrorAction SilentlyContinue
$sid = (New-Object Security.Principal.NTAccount($userName)).Translate([Security.Principal.SecurityIdentifier]).Value
$run = "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Run"
if (Test-Path $run) { foreach ($n in 'LibreHardwareMonitor', 'daisyloop') { Remove-ItemProperty -Path $run -Name $n -ErrorAction SilentlyContinue } }
Get-Process LibreHardwareMonitor -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $lhmExe } | Stop-Process -Force
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$(Join-Path $bin 'watchdog.ps1')`""
$trigger = New-ScheduledTaskTrigger -AtStartup
$trigger.Repetition = (New-ScheduledTaskTrigger -Once -At '2000-01-01' -RepetitionInterval (New-TimeSpan -Minutes 1)).Repetition
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
Register-ScheduledTask -TaskName 'daisyloop-watchdog' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName 'daisyloop-watchdog'
Start-Sleep -Seconds 25
$p = Get-CimInstance Win32_Process -Filter "Name='daisyloop.exe'" | Where-Object { $_.ExecutablePath -eq $exe }
$beat = Join-Path $base 'heartbeat'
$age = if (Test-Path $beat) { [int]((Get-Date) - (Get-Item $beat).LastWriteTime).TotalSeconds } else { -1 }
$owner = if ($p) { (Invoke-CimMethod -InputObject $p -MethodName GetOwner).User } else { 'none' }
"task: $((Get-ScheduledTask -TaskName 'daisyloop-watchdog').State)"
"collector: pid $($p.ProcessId) owner $owner, heartbeat age ${age}s"
"sidecar: $(@(Get-Process daisyloop-lhm -ErrorAction SilentlyContinue).Count) process"
"user task daisyloop: $(if (Get-ScheduledTask -TaskName 'daisyloop' -ErrorAction SilentlyContinue) { 'STILL PRESENT' } else { 'removed' })"
Get-Content (Join-Path $base 'collector.log') -Tail 4
