#requires -Version 7.0
param(
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-f]{40}$')][string]$Commit,
    [ValidatePattern('^[0-9a-f]{40}$')][string]$PreviousVerifiedCommit,
    [string]$ReleaseRoot = '.local/prometheus/releases',
    [switch]$IncludeWorkerProbe
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$taskRepo = (git rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Run inside the Prometheus Git repository.' }
$taskRepo = [IO.Path]::GetFullPath($taskRepo)
$taskRemote = @(git ls-remote --heads origin)
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify the pushed commit on origin.' }
if (-not ($taskRemote | Where-Object { ($_ -split '\s+')[0] -eq $Commit })) { throw 'The release commit must be the verified tip of a branch on origin.' }
$taskBase = [IO.Path]::GetFullPath((Join-Path $taskRepo $ReleaseRoot))
$taskBoundary = $taskRepo.TrimEnd('\') + '\'
if (-not $taskBase.StartsWith($taskBoundary, [StringComparison]::OrdinalIgnoreCase)) { throw 'ReleaseRoot must stay inside this repository.' }
if ($PreviousVerifiedCommit) {
    $taskPriorManifest = Join-Path $taskBase ($PreviousVerifiedCommit + '/runtime/release.json')
    if (-not (Test-Path -LiteralPath $taskPriorManifest)) { throw 'Previous Windows artifact was not found.' }
    $taskPrior = Get-Content -LiteralPath $taskPriorManifest -Raw | ConvertFrom-Json
    if ($taskPrior.commit -ne $PreviousVerifiedCommit) { throw 'Previous Windows artifact SHA does not match.' }
}
$taskRelease = Join-Path $taskBase $Commit
if (Test-Path -LiteralPath $taskRelease) { throw 'Release directory already exists; preserve it and choose a new commit.' }
$taskSource = Join-Path $taskRelease 'source'
$taskRuntime = Join-Path $taskRelease 'runtime'
New-Item -ItemType Directory -Path $taskSource -Force | Out-Null
New-Item -ItemType Directory -Path $taskRuntime -Force | Out-Null
$taskArchive = Join-Path $taskRelease 'source.tar'
git -C $taskRepo archive --format=tar --output=$taskArchive $Commit
if ($LASTEXITCODE -ne 0) { throw 'git archive failed.' }
& tar.exe -xf $taskArchive -C $taskSource
if ($LASTEXITCODE -ne 0) { throw 'Source extraction failed.' }
$taskPreviousServerSha = $env:PROMETHEUS_BUILD_SHA
$taskPreviousWebSha = $env:VITE_BUILD_SHA
$taskNode = (Get-Command node.exe).Source
try {
    $env:PROMETHEUS_BUILD_SHA = $Commit
    $env:VITE_BUILD_SHA = $Commit
    Push-Location $taskSource
    try {
        & npm.cmd ci --ignore-scripts
        if ($LASTEXITCODE -ne 0) { throw 'Locked dependency installation failed.' }
        & npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw 'Prometheus build failed.' }
        if ($IncludeWorkerProbe) {
            & npm.cmd run build:worker
            if ($LASTEXITCODE -ne 0) { throw 'Native worker prerequisite probe build failed.' }
        }
    } finally { Pop-Location }
    foreach ($taskName in @('server', 'admin')) {
        $taskApp = Join-Path $taskRuntime ('apps/' + $taskName)
        New-Item -ItemType Directory -Path $taskApp -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $taskSource ('apps/' + $taskName + '/dist')) -Destination $taskApp -Recurse
        Copy-Item -LiteralPath (Join-Path $taskSource ('apps/' + $taskName + '/package.json')) -Destination $taskApp
    }
    Copy-Item -LiteralPath (Join-Path $taskSource 'package.json') -Destination $taskRuntime
    Copy-Item -LiteralPath (Join-Path $taskSource 'package-lock.json') -Destination $taskRuntime
    Copy-Item -LiteralPath (Join-Path $taskSource 'deploy/prometheus/start-windows.ps1') -Destination (Join-Path $taskRuntime 'Start-Prometheus.ps1')
    Push-Location $taskRuntime
    try {
        & npm.cmd ci --omit=dev --ignore-scripts
        if ($LASTEXITCODE -ne 0) { throw 'Runtime dependency installation failed.' }
    } finally { Pop-Location }
    $taskFiles = @(Get-ChildItem -LiteralPath $taskRuntime -Recurse -File | ForEach-Object {
        [ordered]@{ path = [IO.Path]::GetRelativePath($taskRuntime, $_.FullName).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $taskManifest = [ordered]@{
        name = 'Prometheus'; version = '0.1.0'; commit = $Commit; built_at = (Get-Date).ToString('o');
        platform = [ordered]@{ os = [Environment]::OSVersion.VersionString; architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString(); node = (& $taskNode --version); node_executable = $taskNode };
        previous_verified_windows_version = $(if ($PreviousVerifiedCommit) { $PreviousVerifiedCommit } else { $null }); files = $taskFiles;
        data = 'Outside this release directory; no historical DSH database is imported.'
    }
    $taskServerBuild = Get-Content -LiteralPath (Join-Path $taskRuntime 'apps/server/dist/assets/build.json') -Raw | ConvertFrom-Json
    if ($taskServerBuild.PSObject.Properties.Name -contains 'data_schema') { $taskManifest['data_schema'] = $taskServerBuild.data_schema }
    $taskManifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $taskRuntime 'release.json') -Encoding utf8
    Compress-Archive -LiteralPath $taskRuntime -DestinationPath (Join-Path $taskRelease 'prometheus-windows.zip')
    Write-Output ('Prometheus release: ' + $taskRuntime)
    Write-Output ('Commit: ' + $Commit)
    Write-Output ('Archive SHA256: ' + (Get-FileHash -LiteralPath (Join-Path $taskRelease 'prometheus-windows.zip') -Algorithm SHA256).Hash.ToLowerInvariant())
} finally {
    $env:PROMETHEUS_BUILD_SHA = $taskPreviousServerSha
    $env:VITE_BUILD_SHA = $taskPreviousWebSha
}
