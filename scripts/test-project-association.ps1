$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$build = Join-Path $PSScriptRoot '../target/debug'
$installer = Get-ChildItem (Join-Path $build 'bundle/nsis/*-setup.exe') | Select-Object -First 1
$cli = Join-Path $build 'wave.exe'
$installDir = Join-Path $env:RUNNER_TEMP 'WAVE-Association-Test'
$exe = Join-Path $installDir 'wave-app.exe'
$projects = New-Item -ItemType Directory -Path (Join-Path $env:RUNNER_TEMP 'WAVE test projects')
$fixture = Join-Path $PSScriptRoot '../crates/engine/tests/fixtures/demo_v1.wave'
$first = Join-Path $projects.FullName 'Nowy utwór.WAVE'
$second = Join-Path $projects.FullName 'Second song.wave'
Copy-Item $fixture $first
Copy-Item $fixture $second

function Wait-ForProject($path) {
    for ($attempt = 0; $attempt -lt 90; $attempt++) {
        $reply = & $cli status 2>$null
        if ($LASTEXITCODE -eq 0 -and ($reply | ConvertFrom-Json).path -eq $path) { return }
        Start-Sleep -Seconds 1
    }
    throw "WAVE did not open $path. Last CLI response: $reply"
}

function Run-Installer($program, $arguments) {
    $process = Start-Process -FilePath $program -ArgumentList $arguments -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw "$program failed with exit code $($process.ExitCode)" }
}

$extension = 'HKCU:\Software\Classes\.wave'
$progId = 'HKCU:\Software\Classes\WAVE.Project'
$originalProgId = 'WAVE.AssociationTest.Previous'
New-Item -Path $extension -Force | Out-Null
Set-Item -LiteralPath $extension -Value $originalProgId
try {
    Run-Installer $installer.FullName "/S /D=$installDir"
    if ((Get-Item -LiteralPath $extension).GetValue('') -ne 'WAVE.Project') { throw 'Missing .wave association' }
    $icon = Join-Path $installDir 'wave-project.ico'
    if (!(Test-Path -LiteralPath $icon) -or (Get-Item -LiteralPath "$progId\DefaultIcon").GetValue('') -ne "`"$icon`",0") {
        throw 'Missing project icon registration'
    }

    Invoke-Item -LiteralPath $first
    Wait-ForProject $first
    $pidBefore = (Get-Process -Name 'wave-app').Id
    Invoke-Item -LiteralPath $second
    Wait-ForProject $second
    if ((Get-Process -Name 'wave-app').Id -ne $pidBefore) { throw 'Opening the second project changed the WAVE process' }

    Stop-Process -Id $pidBefore
    Wait-Process -Id $pidBefore -Timeout 15 -ErrorAction SilentlyContinue
    Run-Installer $installer.FullName "/S /UPDATE /D=$installDir"
    if ((Get-Item -LiteralPath $extension).GetValue('WAVE.Project_backup') -ne $originalProgId) {
        throw 'Updating lost the previous file association'
    }
    Run-Installer (Join-Path $installDir 'uninstall.exe') "/S _?=$installDir"
    if ((Test-Path -LiteralPath $progId) -or (Test-Path -LiteralPath $icon) -or (Get-Item -LiteralPath $extension).GetValue('') -ne $originalProgId) {
        throw 'Uninstall did not remove the icon and restore the previous association'
    }
} finally {
    Get-Process -Name 'wave-app' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $exe } | Stop-Process -ErrorAction SilentlyContinue
}
