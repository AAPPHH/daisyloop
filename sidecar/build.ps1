$ErrorActionPreference = 'Stop'
$dotnet = Join-Path $env:LOCALAPPDATA 'dotnet-sdk\dotnet.exe'
if (-not (Test-Path $dotnet)) {
    $s = Join-Path $env:TEMP 'dotnet-install.ps1'
    Invoke-WebRequest https://dot.net/v1/dotnet-install.ps1 -OutFile $s
    & $s -Channel 8.0 -InstallDir (Join-Path $env:LOCALAPPDATA 'dotnet-sdk') -NoPath
}
$out = Join-Path $PSScriptRoot 'publish'
& $dotnet publish (Join-Path $PSScriptRoot 'daisyloop-lhm.csproj') -c Release -r win-x64 --self-contained false -o $out --nologo -v q
if ($LASTEXITCODE -ne 0) { throw 'sidecar build failed' }
