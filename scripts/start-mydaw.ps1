param(
    [string]$TargetDir = 'C:\mydaw-target'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$appExe = Join-Path $TargetDir 'debug\mydaw-app.exe'
if (-not (Test-Path -LiteralPath $appExe)) {
    throw "Build MYDAW first: pnpm --dir ui build; cargo build -p mydaw-app --features custom-protocol. Pass -TargetDir for your Cargo target directory."
}
$env:MYDAW_ASSETS_DIR = Join-Path $repoRoot 'assets'
# This is the interactive application the user is opening.
Start-Process -FilePath $appExe -WorkingDirectory $repoRoot
