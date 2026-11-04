$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ("dl-wd-test-" + [guid]::NewGuid().ToString('N'))
$bin = Join-Path $root 'bin path'
$data = Join-Path $root 'data'
New-Item -ItemType Directory -Force $bin, $data | Out-Null
$exe = Join-Path $bin 'daisyloop.exe'
Copy-Item (Join-Path $env:SystemRoot 'System32\PING.EXE') $exe
Copy-Item (Join-Path $PSScriptRoot 'watchdog.ps1') $bin
$failed = @()
function Dummy { Start-Process $exe -ArgumentList '-n', '120', '127.0.0.1' -WindowStyle Hidden -PassThru }
function Watch { & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $bin 'watchdog.ps1') -DataDir $data -MinAgeSecs 0 | Out-Null; Get-Content (Join-Path $bin 'state\watchdog.log') -Tail 1 }
function Expect($name, $cond) { if (-not $cond) { $script:failed += $name } }
try {
    $a = Dummy
    Start-Sleep -Seconds 1
    Expect 'missing heartbeat restarts' ((Watch) -match 'restarted collector: no heartbeat')
    Expect 'old pid replaced' ($a.HasExited)
    Get-Process -Name daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
    $a = Dummy
    Start-Sleep -Seconds 1
    Set-Content (Join-Path $data 'heartbeat') '0'
    (Get-Item (Join-Path $data 'heartbeat')).LastWriteTime = (Get-Date).AddMinutes(-10)
    Expect 'stale heartbeat restarts' ((Watch) -match 'restarted collector: heartbeat stale')
    Expect 'stale: old pid replaced' ($a.HasExited)
    Get-Process -Name daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
    $a = Dummy
    Start-Sleep -Seconds 1
    Set-Content (Join-Path $data 'heartbeat') '0'
    Watch | Out-Null
    Expect 'fresh heartbeat leaves the collector alone' (-not $a.HasExited)
    Expect 'no watchdog error' (-not ((Get-Content (Join-Path $bin 'state\watchdog.log')) -match 'watchdog error'))
} finally {
    Get-Process -Name daisyloop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
    Start-Sleep -Seconds 1
    Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
}
if ($failed) { Write-Host "FAILED: $($failed -join '; ')"; exit 1 }
'watchdog tests passed'
