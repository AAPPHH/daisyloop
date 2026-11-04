$ErrorActionPreference = 'Stop'
$exe = 'C:\Users\user\AppData\Local\LibreHardwareMonitor\LibreHardwareMonitor.exe'
$log = 'C:\Users\user\AppData\Local\daisyloop\lhm-autostart.log'
$user = 'user'
Start-Transcript -Path $log -Force | Out-Null
try {
    if (-not (Test-Path $exe)) { throw "missing $exe" }
    $every = New-TimeSpan -Minutes 1
    $logon = New-ScheduledTaskTrigger -AtLogOn -User $user
    $logon.Repetition = (New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval $every).Repetition
    $now = New-ScheduledTaskTrigger -Once -At (Get-Date).AddSeconds(5) -RepetitionInterval $every
    $action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory (Split-Path $exe)
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -StartWhenAvailable
    Register-ScheduledTask -TaskName 'LibreHardwareMonitor' -Action $action -Trigger @($logon, $now) -Principal $principal -Settings $settings -Force | Out-Null
    Get-Process LibreHardwareMonitor -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
    Start-Sleep -Seconds 2
    Start-ScheduledTask -TaskName 'LibreHardwareMonitor'
    $key = 'Registry::HKEY_USERS\' + ([Security.Principal.NTAccount]$user).Translate([Security.Principal.SecurityIdentifier]).Value + '\Software\Microsoft\Windows\CurrentVersion\Run'
    Remove-ItemProperty -Path $key -Name 'LibreHardwareMonitor' -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 3
    Get-ScheduledTask -TaskName 'LibreHardwareMonitor' | Select-Object TaskName, State | Format-List
    Get-Process LibreHardwareMonitor | Select-Object Id, Path, StartTime | Format-List
    'OK'
} catch {
    "FAILED: $_"
    exit 1
} finally {
    Stop-Transcript | Out-Null
}
