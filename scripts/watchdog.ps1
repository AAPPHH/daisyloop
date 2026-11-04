param([Parameter(Mandatory)][string]$DataDir)
$ErrorActionPreference = 'Stop'
$bin = $PSScriptRoot
$exe = Join-Path $bin 'daisyloop.exe'
$sidecar = Join-Path $bin 'sidecar\daisyloop-lhm.exe'
$beat = Join-Path $DataDir 'heartbeat'
$state = Join-Path $bin 'state'
New-Item -ItemType Directory -Force $state | Out-Null
$log = Join-Path $state 'watchdog.log'
function Note($m) {
    if ((Test-Path $log) -and (Get-Item $log).Length -gt 262144) { Set-Content $log (Get-Content $log -Tail 200) }
    Add-Content $log ('{0} {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $m)
}
function Running($path) { @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $path }) }
try {
    $procs = Running $exe
    $why = $null
    if ($procs.Count -eq 0) { $why = 'collector not running' }
    elseif (((Get-Date) - $procs[0].StartTime).TotalSeconds -gt 120) {
        if (-not (Test-Path $beat)) { $why = 'no heartbeat' }
        elseif (((Get-Date) - (Get-Item $beat).LastWriteTime).TotalSeconds -gt 90) { $why = 'heartbeat stale' }
    }
    if ($why) {
        $procs + (Running $sidecar) | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
        Start-Process $exe -WorkingDirectory $bin -WindowStyle Hidden
        Note "restarted collector: $why"
    }
} catch {
    Note "watchdog error: $($_.Exception.Message)"
}
