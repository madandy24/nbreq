param(
    [ValidateSet("CurrentDefault", "CurrentNative", "V011")]
    [string]$Mode = "CurrentDefault",
    [ValidateRange(1, 1000)]
    [int]$Samples = 3,
    [ValidateRange(0, 1000000)]
    [int]$Warmups = 8,
    [ValidateRange(1, 1000000000)]
    [int]$RequestsPerSample = 250,
    [ValidateRange(1, 8388608)]
    [int]$BodyBytes = 128,
    [ValidateRange(1, 86400)]
    [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = "Stop"
$toolRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "..\..")).Path
$currentManifest = Join-Path $toolRoot "Cargo.toml"
$v011Manifest = Join-Path $toolRoot "v011\Cargo.toml"
$resultRoot = Join-Path $repoRoot "target\f5-observe\results"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null

if ($Mode -eq "V011") {
    $manifest = $v011Manifest
    $sourceCommit = (& git -C $repoRoot rev-list -n 1 v0.1.1).Trim()
    if ($LASTEXITCODE -ne 0 -or $sourceCommit.Length -eq 0) {
        throw "could not resolve immutable v0.1.1 provenance"
    }
    $sourceVersion = "0.1.1"
} else {
    $manifest = $currentManifest
    $sourceCommit = (& git -C $repoRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "git rev-parse failed"
    }
    if ((& git -C $repoRoot status --porcelain --untracked-files=normal).Count -ne 0) {
        $sourceCommit += "+dirty"
    }
    $manifestText = Get-Content -LiteralPath (Join-Path $repoRoot "Cargo.toml") -Raw
    if ($manifestText -notmatch '(?m)^version = "([^"]+)"') {
        throw "could not read the NBReq package version"
    }
    $sourceVersion = $Matches[1]
}
$timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")

function Build-Observer {
    param([bool]$Instrumented)

    $kind = if ($Instrumented) { "instrumented" } else { "plain" }
    $targetDir = Join-Path $repoRoot "target\f5-observe\windows-$($Mode.ToLowerInvariant())-$kind"
    $arguments = @("build", "--manifest-path", $manifest, "--release", "--target-dir", $targetDir)
    $features = @()
    if ($Mode -ne "CurrentDefault") {
        $arguments += "--no-default-features"
    }
    if ($Instrumented) {
        $features += "alloc-stats"
    }
    if ($features.Count -ne 0) {
        $arguments += @("--features", ($features -join ","))
    }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "observer build failed for $kind"
    }
    return Join-Path $targetDir "release\nbreq-f5-observe.exe"
}

function Set-Summary {
    param($Measures, [string]$Name, [string]$Unit, [double]$Value)

    $Measures.$Name = [pscustomobject]@{
        available = $true
        unit = $Unit
        median = $Value
        min = $Value
        max = $Value
    }
}

function Assert-NoObserverProcess {
    $remaining = @(Get-Process -Name "nbreq-f5-observe" -ErrorAction SilentlyContinue)
    if ($remaining.Count -ne 0) {
        throw "one or more nbreq-f5-observe processes remain"
    }
}

function Invoke-Observer {
    param([string]$Executable, [bool]$Instrumented)

    $kind = if ($Instrumented) { "instrumented" } else { "plain" }
    $stem = "$timestamp-$($Mode.ToLowerInvariant())-$kind"
    $stdoutPath = Join-Path $resultRoot "$stem.inner.json"
    $stderrPath = Join-Path $resultRoot "$stem.log"
    $finalPath = Join-Path $resultRoot "$stem.json"
    $arguments = @(
        "--source-commit", $sourceCommit,
        "--source-version", $sourceVersion,
        "--samples", $Samples,
        "--warmups", $Warmups,
        "--requests-per-sample", $RequestsPerSample,
        "--body-bytes", $BodyBytes
    )
    $processWall = [System.Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process -FilePath $Executable -ArgumentList $arguments `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
        -WindowStyle Hidden -PassThru
    # Force Windows PowerShell to retain the native process handle before a short-lived child exits;
    # otherwise ExitCode can remain unset even after WaitForExit.
    $null = $process.Handle
    $peakWorkingSet = 0L
    $peakPrivateBytes = 0L
    $cpuMs = 0.0
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $process.WaitForExit(10)) {
        if ([DateTime]::UtcNow -ge $deadline) {
            Stop-Process -Id $process.Id -Force
            $process.WaitForExit()
            Assert-NoObserverProcess
            throw "observer $kind exceeded the $TimeoutSeconds second bound"
        }
        try {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.WorkingSet64)
            $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
            $cpuMs = [Math]::Max($cpuMs, $process.TotalProcessorTime.TotalMilliseconds)
        } catch {
            # The process may exit between WaitForExit and Refresh; the last sample remains valid.
        }
    }
    $process.WaitForExit()
    $processWall.Stop()
    $process.Refresh()
    try {
        $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
        $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
        $cpuMs = [Math]::Max($cpuMs, $process.TotalProcessorTime.TotalMilliseconds)
    } catch {
        # The sampled maxima remain explicit if this platform cannot read final process counters.
    }
    $exitCode = $process.ExitCode
    if ($exitCode -ne 0) {
        throw "observer $kind failed with exit code $exitCode; see $stderrPath"
    }

    $innerLines = @(Get-Content -LiteralPath $stdoutPath | Where-Object { $_.Trim().Length -ne 0 })
    if ($innerLines.Count -ne 1) {
        throw "observer $kind did not emit exactly one JSON record"
    }
    $record = $innerLines[0] | ConvertFrom-Json
    if ($record.schema -ne "nbreq-f5-observation-v1") {
        throw "observer $kind emitted an unexpected schema"
    }
    if (-not $Instrumented) {
        Set-Summary $record.measures "process_wall_ms" "ms" $processWall.Elapsed.TotalMilliseconds
        Set-Summary $record.measures "process_cpu_ms" "ms" $cpuMs
        Set-Summary $record.measures "process_peak_working_set_bytes" "bytes" ([double]$peakWorkingSet)
        Set-Summary $record.measures "process_sampled_peak_private_bytes" "bytes" ([double]$peakPrivateBytes)
    }
    $record.checks.no_leftover_process = $null -eq (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)
    if (-not $record.checks.no_leftover_process) {
        throw "observer $kind process remained after WaitForExit"
    }
    Assert-NoObserverProcess
    $record.process_observation = [pscustomobject]@{
        scope = "observer_with_in_process_loopback_fixture"
        fixture_accept_poll_ms = 1
        process_sample_interval_ms = 10
        working_set_method = "PeakWorkingSet64"
        private_bytes_method = "sampled_PrivateMemorySize64"
    }
    $finalJson = $record | ConvertTo-Json -Depth 8 -Compress
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($finalPath, $finalJson + [Environment]::NewLine, $utf8WithoutBom)
    Remove-Item -LiteralPath $stdoutPath
    Write-Host "F5_RECORD $finalPath"
    Write-Output $finalJson
}

$plain = Build-Observer $false
$instrumented = Build-Observer $true
Invoke-Observer $plain $false
Invoke-Observer $instrumented $true
