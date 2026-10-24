param([Parameter(Mandatory=$true)][ValidatePattern('^[0-9a-f]{40}$')][string]$Commit)
$ErrorActionPreference = 'Stop'
$taskRepo = Split-Path -Parent $PSScriptRoot
if ((git -C $taskRepo rev-parse HEAD).Trim() -ne $Commit) { throw 'Build only the current, committed frontend revision' }
Push-Location (Join-Path $taskRepo 'web/admin')
try {
    $env:VITE_BUILD_SHA = $Commit
    npm.cmd ci
    if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
    npm.cmd run build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
} finally { Pop-Location; Remove-Item Env:VITE_BUILD_SHA -ErrorAction SilentlyContinue }
$taskArtifacts = Join-Path $taskRepo '.git/artifacts'
New-Item -ItemType Directory -Force -Path $taskArtifacts | Out-Null
$taskArchive = Join-Path $taskArtifacts "admin-$Commit.tar.gz"
$taskDist = Join-Path $taskRepo 'web/admin/dist'
@'
import json,pathlib,sys,tarfile
source,sha,destination=sys.argv[1:]
root=pathlib.Path(source)
(root/'release.json').write_text(json.dumps({'commit':sha,'api_version':1}))
with tarfile.open(destination,'w:gz') as tf:
    for item in sorted(root.rglob('*')):
        if item.is_file(): tf.add(item,arcname=item.relative_to(root).as_posix())
'@ | python -S - $taskDist $Commit $taskArchive
if ($LASTEXITCODE -ne 0) { throw 'Packaging failed' }
Get-FileHash -Algorithm SHA256 -LiteralPath $taskArchive | Select-Object Path,Hash
