$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
Pop-Location
& (Join-Path $repo 'sidecar\build.ps1')
if ($LASTEXITCODE -ne 0) { throw 'sidecar build failed' }
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$script = Join-Path $PSScriptRoot 'install-service.ps1'
if ((New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    & $script
    if ($LASTEXITCODE -ne 0) { throw 'install-service failed' }
} else {
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$script`""
    if ($p.ExitCode -ne 0) { throw "install-service failed with exit code $($p.ExitCode), see install.log" }
}
