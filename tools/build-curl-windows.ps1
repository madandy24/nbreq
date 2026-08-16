[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$curlVersion = '8.21.0'
$sourceSha256 = 'AA1B66A70EACE83DC624508745646C08AE561DE512AB403ADFFB93AC87FC72E6'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$artifactRoot = Join-Path $workspace 'target\curl-pilot\windows-x64'
$downloadDir = Join-Path $artifactRoot 'download'
$sourceRoot = Join-Path $artifactRoot 'source'
$buildRoot = Join-Path $artifactRoot 'build'
$runtimeRoot = Join-Path $artifactRoot 'runtime'
$vcpkgRoot = Join-Path $artifactRoot 'vcpkg'
$archive = Join-Path $downloadDir "curl-$curlVersion.tar.xz"
$sourceDir = Join-Path $sourceRoot "curl-$curlVersion"

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw 'Visual Studio Installer (vswhere.exe) was not found.'
}
$vsRoot = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
if (-not $vsRoot) {
    throw 'Visual Studio with the x64 C++ build tools was not found.'
}
$cmake = Join-Path $vsRoot 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
if (-not (Test-Path -LiteralPath $cmake)) {
    throw "Visual Studio's bundled CMake was not found at $cmake"
}

New-Item -ItemType Directory -Force $downloadDir, $sourceRoot, $buildRoot, $runtimeRoot | Out-Null
if (-not (Test-Path -LiteralPath $archive)) {
    & curl.exe --fail --location --output $archive "https://curl.se/download/curl-$curlVersion.tar.xz"
    if ($LASTEXITCODE -ne 0) {
        throw "curl source download failed with exit code $LASTEXITCODE"
    }
}
$actualSourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash
if ($actualSourceHash -ne $sourceSha256) {
    throw "curl source SHA-256 mismatch: expected $sourceSha256, got $actualSourceHash"
}
if (-not (Test-Path -LiteralPath $sourceDir)) {
    & tar.exe -xf $archive -C $sourceRoot
    if ($LASTEXITCODE -ne 0) {
        throw "curl source extraction failed with exit code $LASTEXITCODE"
    }
}

$configure = @(
    '-S', $sourceDir,
    '-B', $buildRoot,
    '-G', 'Visual Studio 17 2022',
    '-A', 'x64',
    '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded',
    '-DBUILD_SHARED_LIBS=ON',
    '-DBUILD_STATIC_LIBS=OFF',
    '-DBUILD_CURL_EXE=OFF',
    '-DBUILD_EXAMPLES=OFF',
    '-DBUILD_TESTING=OFF',
    '-DHTTP_ONLY=ON',
    '-DCURL_USE_SCHANNEL=ON',
    '-DCURL_ZLIB=OFF',
    '-DCURL_BROTLI=OFF',
    '-DCURL_ZSTD=OFF',
    '-DUSE_NGHTTP2=OFF',
    '-DCURL_USE_LIBPSL=OFF',
    '-DCURL_USE_LIBSSH2=OFF',
    '-DCURL_DISABLE_COOKIES=ON',
    '-DCURL_DISABLE_CA_SEARCH=ON'
)
& $cmake @configure
if ($LASTEXITCODE -ne 0) {
    throw "curl CMake configuration failed with exit code $LASTEXITCODE"
}
& $cmake --build $buildRoot --config Release --parallel
if ($LASTEXITCODE -ne 0) {
    throw "curl build failed with exit code $LASTEXITCODE"
}
& $cmake --install $buildRoot --config Release --prefix $runtimeRoot
if ($LASTEXITCODE -ne 0) {
    throw "curl installation failed with exit code $LASTEXITCODE"
}

# curl-sys uses vcpkg-rs for dynamic MSVC discovery. This controlled tree contains only our pinned
# build; installing the vcpkg program globally is unnecessary.
$tripletRoot = Join-Path $vcpkgRoot 'installed\x64-windows'
$statusRoot = Join-Path $vcpkgRoot 'installed\vcpkg'
New-Item -ItemType Directory -Force (Join-Path $tripletRoot 'bin'), (Join-Path $tripletRoot 'lib'), (Join-Path $tripletRoot 'include'), (Join-Path $statusRoot 'info'), (Join-Path $statusRoot 'updates') | Out-Null
New-Item -ItemType File -Force (Join-Path $vcpkgRoot '.vcpkg-root') | Out-Null
Copy-Item -LiteralPath (Join-Path $runtimeRoot 'bin\libcurl.dll') -Destination (Join-Path $tripletRoot 'bin\libcurl.dll') -Force
Copy-Item -LiteralPath (Join-Path $runtimeRoot 'lib\libcurl_imp.lib') -Destination (Join-Path $tripletRoot 'lib\libcurl_imp.lib') -Force
Copy-Item -LiteralPath (Join-Path $runtimeRoot 'include\curl') -Destination (Join-Path $tripletRoot 'include') -Recurse -Force

$status = @"
Package: curl
Architecture: x64-windows
Version: $curlVersion
Status: install ok installed

"@
Set-Content -LiteralPath (Join-Path $statusRoot 'status') -Value $status -Encoding ascii
$manifest = @('x64-windows/bin/libcurl.dll', 'x64-windows/lib/libcurl_imp.lib')
$manifest += Get-ChildItem -LiteralPath (Join-Path $tripletRoot 'include\curl') -File | ForEach-Object { "x64-windows/include/curl/$($_.Name)" }
$manifest | Sort-Object | Set-Content -LiteralPath (Join-Path $statusRoot "info\curl_${curlVersion}_x64-windows.list") -Encoding ascii

$dll = Join-Path $runtimeRoot 'bin\libcurl.dll'
$dllHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $dll).Hash
Write-Host "Built curl $curlVersion (Schannel, HTTP/HTTPS, dynamic)"
Write-Host "Runtime: $dll"
Write-Host "SHA-256: $dllHash"
