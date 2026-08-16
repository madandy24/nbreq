[CmdletBinding()]
param(
    [switch]$SkipCurlBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$artifactRoot = Join-Path $workspace 'target\curl-pilot\windows-x64'
$vcpkgRoot = Join-Path $artifactRoot 'vcpkg'
$runtimeBin = Join-Path $artifactRoot 'runtime\bin'
$curlDll = Join-Path $runtimeBin 'libcurl.dll'
$buildRoot = Join-Path $workspace 'target\curl-pilot\win10-proof-build'
$probeBuildRoot = Join-Path $workspace 'target\curl-pilot\win10-proof-probe'
$bundle = Join-Path $workspace 'target\curl-pilot\win10-proof'
$zip = Join-Path $workspace 'target\curl-pilot\nbreq-win10-proof.zip'

$sourceCommit = (& git -C $workspace rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $sourceCommit) {
    throw 'Could not determine the NBReq source commit.'
}
$sourceStatus = @(& git -C $workspace status --porcelain)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not determine whether the NBReq source tree is clean.'
}
if ($sourceStatus.Count -ne 0) {
    throw 'Refusing to package Win10 proof from a dirty source tree. Commit or stash changes first.'
}

if (-not $SkipCurlBuild) {
    & (Join-Path $PSScriptRoot 'build-curl-windows.ps1')
}
if (-not (Test-Path -LiteralPath $curlDll -PathType Leaf)) {
    throw 'The pinned curl DLL is absent. Run without -SkipCurlBuild first.'
}

$env:VCPKG_ROOT = $vcpkgRoot
$env:VCPKGRS_DYNAMIC = '1'
$env:CARGO_TARGET_DIR = $buildRoot
$env:PATH = "$runtimeBin;$env:PATH"

Write-Host 'Building current dynamically linked test executables...'
$jsonLines = & cargo test --features curl-pilot --no-run --message-format=json
if ($LASTEXITCODE -ne 0) {
    throw "NBReq proof build failed with exit code $LASTEXITCODE"
}
$artifacts = foreach ($line in $jsonLines) {
    try {
        $message = $line | ConvertFrom-Json -ErrorAction Stop
        if ($message.reason -eq 'compiler-artifact' -and $null -ne $message.executable) {
            $message
        }
    }
    catch {
        # Cargo may emit non-JSON status text; only compiler-artifact messages are relevant.
    }
}
$unitTests = $artifacts |
    Where-Object { $_.target.name -eq 'nbreq' -and $_.target.kind -contains 'lib' } |
    Select-Object -Last 1
$contractTests = $artifacts |
    Where-Object { $_.target.name -eq 'public_contract' -and $_.target.kind -contains 'test' } |
    Select-Object -Last 1
if ($null -eq $unitTests -or $null -eq $contractTests) {
    throw 'Cargo did not report both required test executables.'
}

Write-Host 'Building current controlled DLL lifecycle probe...'
$env:CARGO_TARGET_DIR = $probeBuildRoot
$probeManifest = Join-Path $workspace 'experiments\windows-curl-dll\Cargo.toml'
& cargo build --manifest-path $probeManifest --release
if ($LASTEXITCODE -ne 0) {
    throw "DLL proof build failed with exit code $LASTEXITCODE"
}

if (Test-Path -LiteralPath $bundle) {
    $resolvedBundle = (Resolve-Path -LiteralPath $bundle).Path
    $expectedParent = (Resolve-Path -LiteralPath (Join-Path $workspace 'target\curl-pilot')).Path
    if (-not $resolvedBundle.StartsWith($expectedParent + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to replace bundle outside the expected target directory: $resolvedBundle"
    }
    Remove-Item -LiteralPath $resolvedBundle -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $bundle | Out-Null

$files = [ordered]@{
    'nbreq-curl-tests.exe' = [string]$unitTests.executable
    'nbreq-public-contract-tests.exe' = [string]$contractTests.executable
    'libcurl.dll' = $curlDll
    'nbreq-curl-dll-host.exe' = Join-Path $probeBuildRoot 'release\nbreq-curl-dll-host.exe'
    'nbreq_curl_dll_probe.dll' = Join-Path $probeBuildRoot 'release\nbreq_curl_dll_probe.dll'
    'run-win10-proof.ps1' = Join-Path $PSScriptRoot 'run-win10-proof.ps1'
}
foreach ($entry in $files.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) {
        throw "Required proof artifact is missing: $($entry.Value)"
    }
    Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $bundle $entry.Key) -Force
}

$readme = @'
NBReq Windows 10 proof bundle

1. Copy this entire folder to the Windows 10 x64 test machine.
2. Open an ordinary non-administrator PowerShell window in the folder.
3. Run:

   Set-ExecutionPolicy -Scope Process Bypass
   .\run-win10-proof.ps1

4. Return win10-proof.txt to the NBReq workspace/reviewer.

No Rust toolchain, Visual Studio, internet access, certificate installation, or machine-wide change
is required. The runner verifies Windows 10 build range, x64 execution, every bundle hash, the full
NBReq unit/public-contract suites, and 25 fresh-process DLL load/use/exit iterations.

Some containment tests deliberately panic inside a callback, reactor, or manual drive and print a
panic message. Those lines are expected when the named test remains "ok" and the final result is
PASS.
'@
Set-Content -LiteralPath (Join-Path $bundle 'README.txt') -Value $readme -Encoding ascii

$buildInfo = @"
NBReq source commit: $sourceCommit
Packaged at: $([DateTimeOffset]::Now.ToString('o'))
Packager OS: $([Environment]::OSVersion.VersionString)
Rust: $(& rustc --version)
Cargo: $(& cargo --version)
Pinned libcurl SHA-256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $curlDll).Hash)
"@
Set-Content -LiteralPath (Join-Path $bundle 'BUILD-INFO.txt') -Value $buildInfo -Encoding ascii

$manifestNames = @($files.Keys) + @('README.txt', 'BUILD-INFO.txt')
$manifestLines = foreach ($name in $manifestNames) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $bundle $name)).Hash
    "$hash  $name"
}
Set-Content -LiteralPath (Join-Path $bundle 'manifest.sha256') -Value $manifestLines -Encoding ascii

if (Test-Path -LiteralPath $zip) {
    Remove-Item -LiteralPath $zip -Force
}
Compress-Archive -Path (Join-Path $bundle '*') -DestinationPath $zip -CompressionLevel Optimal

Write-Host "Win10 proof folder: $bundle"
Write-Host "Win10 proof archive: $zip"
Write-Host "Pinned libcurl SHA-256: $((Get-FileHash -Algorithm SHA256 -LiteralPath $curlDll).Hash)"
