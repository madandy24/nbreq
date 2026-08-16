[CmdletBinding()]
param(
    [ValidateRange(1, 1000)]
    [int]$DllIterations = 25
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$bundle = (Resolve-Path $PSScriptRoot).Path
$transcript = Join-Path $bundle 'win10-proof.txt'
$manifest = Join-Path $bundle 'manifest.sha256'

if (-not (Test-Path -LiteralPath $manifest)) {
    throw 'manifest.sha256 is missing from the Win10 proof bundle.'
}

Start-Transcript -LiteralPath $transcript -Force | Out-Null
try {
    Write-Host 'NBReq Windows 10 proof'
    Write-Host "Started: $([DateTimeOffset]::Now.ToString('o'))"
    Write-Host "Bundle: $bundle"
    Write-Host "User: $([System.Security.Principal.WindowsIdentity]::GetCurrent().Name)"
    Write-Host "64-bit OS: $([Environment]::Is64BitOperatingSystem)"
    Write-Host "64-bit process: $([Environment]::Is64BitProcess)"

    $windows = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $build = [int]$windows.CurrentBuild
    $ubr = if ($null -ne $windows.UBR) { [int]$windows.UBR } else { 0 }
    $release = if ($windows.PSObject.Properties.Name -contains 'DisplayVersion') {
        $windows.DisplayVersion
    }
    else {
        $windows.ReleaseId
    }
    Write-Host "Windows product: $($windows.ProductName)"
    Write-Host "Windows release: $release"
    Write-Host "Windows build: $build.$ubr"
    if ($build -lt 10240 -or $build -ge 22000) {
        throw "This proof requires Windows 10 (build 10240-21999); observed build $build.$ubr."
    }
    if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
        throw 'This proof requires an x64 OS and x64 PowerShell process.'
    }

    Write-Host 'Verifying bundle hashes...'
    foreach ($line in Get-Content -LiteralPath $manifest) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -notmatch '^([0-9A-Fa-f]{64})  (.+)$') {
            throw "Invalid manifest line: $line"
        }
        $expected = $Matches[1].ToUpperInvariant()
        $name = $Matches[2]
        $path = Join-Path $bundle $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Bundle file is missing: $name"
        }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash
        if ($actual -ne $expected) {
            throw "SHA-256 mismatch for ${name}: expected $expected, got $actual"
        }
        Write-Host "SHA-256 OK: $name $actual"
    }

    $unitTests = Join-Path $bundle 'nbreq-curl-tests.exe'
    $contractTests = Join-Path $bundle 'nbreq-public-contract-tests.exe'
    $curlDll = Join-Path $bundle 'libcurl.dll'
    $probeDll = Join-Path $bundle 'nbreq_curl_dll_probe.dll'
    $hostExe = Join-Path $bundle 'nbreq-curl-dll-host.exe'
    $env:PATH = "$bundle;$env:PATH"
    $env:NBREQ_EXPECT_DYNAMIC_CURL = '1'

    Write-Host 'Running NBReq curl, lifecycle, TLS, and cancellation tests...'
    & $unitTests --test-threads=1 --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw "NBReq unit test executable failed with exit code $LASTEXITCODE"
    }

    Write-Host 'Running public contract tests...'
    & $contractTests --test-threads=1 --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw "NBReq public contract test executable failed with exit code $LASTEXITCODE"
    }

    Write-Host "Running $DllIterations fresh-process DLL load/use/exit iterations..."
    for ($iteration = 1; $iteration -le $DllIterations; $iteration++) {
        & $hostExe $curlDll $probeDll
        if ($LASTEXITCODE -ne 0) {
            throw "DLL lifecycle iteration $iteration failed with exit code $LASTEXITCODE"
        }
    }

    Write-Host "PASS: NBReq Windows 10 proof completed at $([DateTimeOffset]::Now.ToString('o'))"
}
catch {
    Write-Host "FAIL: $($_.Exception.Message)"
    throw
}
finally {
    Stop-Transcript | Out-Null
    Write-Host "Proof transcript: $transcript"
}
