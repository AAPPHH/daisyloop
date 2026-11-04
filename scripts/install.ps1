$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
Pop-Location
& (Join-Path $repo 'sidecar\build.ps1')
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$script = Join-Path $PSScriptRoot 'install-service.ps1'
if ((New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    & $script
} else {
    Start-Process powershell -Verb RunAs -Wait -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$script`""
}
