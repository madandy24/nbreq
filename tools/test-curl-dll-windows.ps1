[CmdletBinding()]
param(
    [ValidateRange(1, 1000)]
    [int]$Iterations = 25,
    [switch]$SkipCurlBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$artifactRoot = Join-Path $workspace 'target\curl-pilot\windows-x64'
if (-not $SkipCurlBuild) {
    & (Join-Path $PSScriptRoot 'build-curl-windows.ps1')
}
$vcpkgRoot = Join-Path $artifactRoot 'vcpkg'
$curlDll = Join-Path $artifactRoot 'runtime\bin\libcurl.dll'
if (-not (Test-Path -LiteralPath $curlDll)) {
    throw 'The pinned curl DLL is absent. Run without -SkipCurlBuild first.'
}

$env:VCPKG_ROOT = $vcpkgRoot
$env:VCPKGRS_DYNAMIC = '1'
$env:CARGO_TARGET_DIR = Join-Path $workspace 'target\curl-pilot\dll-probe'
$manifest = Join-Path $workspace 'experiments\windows-curl-dll\Cargo.toml'
& cargo build --manifest-path $manifest --release
if ($LASTEXITCODE -ne 0) {
    throw "DLL probe build failed with exit code $LASTEXITCODE"
}

$hostExe = Join-Path $env:CARGO_TARGET_DIR 'release\nbreq-curl-dll-host.exe'
$probeDll = Join-Path $env:CARGO_TARGET_DIR 'release\nbreq_curl_dll_probe.dll'
for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
    & $hostExe $curlDll $probeDll
    if ($LASTEXITCODE -ne 0) {
        throw "DLL host process $iteration failed with exit code $LASTEXITCODE"
    }
}
Write-Host "Passed $Iterations controlled process load/use/exit iterations with the pinned curl DLL."
