#requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'The native isolation prerequisite probe requires Windows.' }
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskOutput = Join-Path $taskRoot 'dist/native'
New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
$taskVswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$taskVs = (& $taskVswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
if ($LASTEXITCODE -ne 0 -or -not $taskVs) { throw 'MSVC Build Tools are required for the Windows isolation probe.' }
$taskVcvars = Join-Path $taskVs 'VC/Auxiliary/Build/vcvars64.bat'
$taskSource = Join-Path $taskRoot 'native/worker-probe.cpp'
foreach ($taskPath in @($taskVcvars, $taskSource, $taskOutput)) {
    if ($taskPath -match '["\r\n%]') { throw 'Unsupported build path.' }
}
$taskBuildDirectory = [IO.Path]::GetFullPath((Join-Path $taskRoot ('../../.local/prometheus/native-build/' + [Guid]::NewGuid().ToString())))
New-Item -ItemType Directory -Path $taskBuildDirectory -Force | Out-Null
$taskBuild = Join-Path $taskBuildDirectory 'build-probe.cmd'
@('@echo off', ('call "' + $taskVcvars + '" >nul'), 'if errorlevel 1 exit /b 1', ('cl.exe /nologo /std:c++17 /EHsc /W4 /WX /O2 /MT /guard:cf "' + $taskSource + '" /Fe:worker-probe.exe /Fo:worker-probe.obj /link /DYNAMICBASE /NXCOMPAT userenv.lib advapi32.lib ws2_32.lib'), 'if errorlevel 1 exit /b 1', ('cl.exe /nologo /std:c++17 /EHsc /W4 /WX /O2 /MT /guard:cf /DPROMETHEUS_PROBE_CHILD "' + $taskSource + '" /Fe:probe-child.exe /Fo:probe-child.obj /link /DYNAMICBASE /NXCOMPAT advapi32.lib ws2_32.lib')) | Set-Content -LiteralPath $taskBuild -Encoding ascii
Push-Location $taskBuildDirectory
try {
    & $env:ComSpec /d /c $taskBuild
    if ($LASTEXITCODE -ne 0) { throw 'Native worker probe build failed; do not change application control policy.' }
    foreach ($taskName in @('worker-probe.exe', 'probe-child.exe')) {
        Copy-Item -LiteralPath (Join-Path $taskBuildDirectory $taskName) -Destination (Join-Path $taskOutput $taskName)
    }
} finally { Pop-Location }
