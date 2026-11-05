#requires -Version 7.0
param(
    [Parameter(Mandatory = $true)][string]$DataDir,
    [string]$Listen = '127.0.0.1:8419',
    [string]$Origin = 'http://127.0.0.1:8419',
    [string]$StageModelDir,
    [string]$StageCoreDir,
    [switch]$BootstrapToken,
    [switch]$SecureCookie
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$taskRuntime = $PSScriptRoot
$taskManifestPath = Join-Path $taskRuntime 'release.json'
if (-not (Test-Path -LiteralPath $taskManifestPath)) { throw 'Use the Start-Prometheus.ps1 copied into a packaged runtime.' }
$taskRelease = Get-Content -LiteralPath $taskManifestPath -Raw | ConvertFrom-Json
foreach ($taskFile in $taskRelease.files) {
    $taskPath = [IO.Path]::GetFullPath((Join-Path $taskRuntime $taskFile.path))
    if (-not $taskPath.StartsWith($taskRuntime.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid manifest path.' }
    if (-not (Test-Path -LiteralPath $taskPath -PathType Leaf)) { throw ('Missing release file: ' + $taskFile.path) }
    if ((Get-FileHash -LiteralPath $taskPath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $taskFile.sha256) { throw ('Artifact hash mismatch: ' + $taskFile.path) }
}
$taskData = [IO.Path]::GetFullPath($DataDir)
if ($taskData.Equals($taskRuntime, [StringComparison]::OrdinalIgnoreCase) -or $taskData.StartsWith($taskRuntime.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Keep mutable data outside the release runtime.' }
$taskCli = Join-Path $taskRuntime 'apps/server/dist/cli.js'
if ($BootstrapToken) {
    & node.exe $taskCli bootstrap-token --data-dir $taskData
    if ($LASTEXITCODE -ne 0) { throw 'Initialization token creation failed.' }
    return
}
$taskArguments = @($taskCli, 'serve', '--data-dir', $taskData, '--listen', $Listen, '--origin', $Origin, '--web-dir', (Join-Path $taskRuntime 'apps/admin/dist'), '--stage-dir', (Join-Path $taskRuntime 'apps/stage/dist'))
foreach ($taskStageOption in @(
    @{ Argument = '--stage-model-dir'; Directory = $StageModelDir }
    @{ Argument = '--stage-core-dir'; Directory = $StageCoreDir }
)) {
    if (-not $taskStageOption.Directory) { continue }
    $taskStagePath = [IO.Path]::GetFullPath($taskStageOption.Directory)
    if ($taskStagePath.Equals($taskRuntime, [StringComparison]::OrdinalIgnoreCase) -or $taskStagePath.StartsWith($taskRuntime.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Keep Live2D model and Core files outside the release runtime.' }
    $taskArguments += @($taskStageOption.Argument, $taskStagePath)
}
if ($SecureCookie) { $taskArguments += '--secure-cookie' }
Write-Output ('Starting Prometheus ' + $taskRelease.commit + ' in the foreground. Ctrl+C requests a clean exit.')
& node.exe @taskArguments
if ($LASTEXITCODE -ne 0) { throw 'Prometheus exited with an error.' }
