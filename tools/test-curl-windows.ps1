[CmdletBinding()]
param(
    [switch]$SkipCurlBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $SkipCurlBuild) {
    & (Join-Path $PSScriptRoot 'build-curl-windows.ps1')
}
$artifactRoot = Join-Path $workspace 'target\curl-pilot\windows-x64'
$vcpkgRoot = Join-Path $artifactRoot 'vcpkg'
$runtimeBin = Join-Path $artifactRoot 'runtime\bin'
$expectedDll = Join-Path $runtimeBin 'libcurl.dll'
if (-not (Test-Path -LiteralPath $expectedDll)) {
    throw 'The pinned curl DLL is absent. Run without -SkipCurlBuild first.'
}

$env:VCPKG_ROOT = $vcpkgRoot
$env:VCPKGRS_DYNAMIC = '1'
$env:CARGO_TARGET_DIR = Join-Path $workspace 'target\curl-pilot\rust-dynamic'
$env:PATH = "$runtimeBin;$env:PATH"
$env:NBREQ_EXPECT_DYNAMIC_CURL = '1'
& cargo test --features curl 'curl_' -- --nocapture
if ($LASTEXITCODE -ne 0) {
    throw "dynamic curl test suite failed with exit code $LASTEXITCODE"
}

$expectedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $expectedDll).Hash
$copiedDlls = Get-ChildItem -LiteralPath $env:CARGO_TARGET_DIR -Recurse -Filter libcurl.dll
if (-not $copiedDlls) {
    throw 'curl-sys did not record/copy a dynamically discovered libcurl DLL.'
}
foreach ($copy in $copiedDlls) {
    $copyHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $copy.FullName).Hash
    if ($copyHash -ne $expectedHash) {
        throw "Rust build used an unexpected libcurl DLL at $($copy.FullName)"
    }
}
Write-Host "Dynamic Rust tests used pinned libcurl SHA-256 $expectedHash"
