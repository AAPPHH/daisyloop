$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$base = Join-Path $env:LOCALAPPDATA 'daisyloop'
$bin = Join-Path $base 'bin'
$exe = Join-Path $bin 'daisyloop.exe'
Push-Location $repo
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
Pop-Location
New-Item -ItemType Directory -Force $bin | Out-Null
Set-Content (Join-Path $base 'stop') ''
Start-Sleep -Seconds 4
Get-Process daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Copy-Item (Join-Path $repo 'target\release\daisyloop.exe') $exe -Force
Remove-Item (Join-Path $base 'stop') -ErrorAction SilentlyContinue
$mechanism = 'ScheduledTask'
try {
    $user = "$env:USERDOMAIN\$env:USERNAME"
    $action = New-ScheduledTaskAction -Execute $exe
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName 'daisyloop' -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name daisyloop -ErrorAction SilentlyContinue
} catch {
    $mechanism = 'RunKey'
    Set-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name daisyloop -Value "`"$exe`""
}
Start-Process $exe -WindowStyle Hidden
Write-Output $mechanism
