param(
    [string]$AppBinary = 'C:\mydaw-target\debug\mydaw-app.exe',
    [string]$CliBinary = 'C:\mydaw-target\debug\mydaw.exe',
    [string]$FixtureBinary = 'C:\mydaw-target\debug\mydaw_fixture_clap_gain.dll',
    [string]$OutputDirectory = (Join-Path ([IO.Path]::GetTempPath()) ('mydaw-cli-' + [guid]::NewGuid())),
    [switch]$InstalledVst3
)
$ErrorActionPreference = 'Stop'
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$runtime = Join-Path $OutputDirectory 'runtime'
New-Item -ItemType Directory -Path $runtime -Force | Out-Null
$ownedBinary = Join-Path $runtime 'mydaw-app.exe'
Copy-Item -LiteralPath $AppBinary -Destination $ownedBinary
$previousDirectory = $env:MYDAW_CONTROL_DIR
$env:MYDAW_CONTROL_DIR = Join-Path $OutputDirectory 'sessions'
$script:revision = 0
$checks = [Collections.Generic.List[string]]::new()

function Check([bool]$Condition, [string]$Description) {
    if (!$Condition) { throw $Description }
    $checks.Add($Description)
}
function Call-Session {
    param([string]$Command, [hashtable]$Data = @{}, $Revision = $null, [string]$Key, [string]$ExpectedError)
    $argumentPath = Join-Path $OutputDirectory 'arguments.json'
    $Data | ConvertTo-Json -Depth 50 -Compress | Set-Content -LiteralPath $argumentPath -Encoding utf8NoBOM
    $arguments = @('call', $Command, '--file', $argumentPath, '--timeout', '60')
    if ($Command -in @('render', 'analyze')) { $arguments += '--no-wait' }
    if ($null -ne $Revision) { $arguments += @('--if-revision', [string]$Revision) }
    if ($Key) { $arguments += @('--idempotency-key', $Key) }
    $output = & $CliBinary @arguments
    $reply = $output | ConvertFrom-Json
    if ($ExpectedError) {
        Check ($reply.error.code -eq $ExpectedError) $ExpectedError
    } elseif ($reply.error) {
        throw "$Command : $($reply.error.code) : $($reply.error.message)"
    } elseif ($LASTEXITCODE -ne 0) { throw "$Command exited $LASTEXITCODE" }
    $script:revision = $reply.revision
    return $reply
}
function Edit([string]$Command, [hashtable]$Data) {
    Call-Session -Command $Command -Data $Data -Revision $script:revision
}
function Wait-Artifact($Reply) {
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    while ($Reply.data.status -eq 'running') {
        if ([DateTime]::UtcNow -gt $deadline) { throw 'worker timeout' }
        Start-Sleep -Milliseconds 100
        $Reply = Call-Session 'job_status' @{jobId=$Reply.data.jobId}
    }
    Check ($Reply.data.status -eq 'completed') 'snapshot worker completed'
    Check (Test-Path -LiteralPath $Reply.data.audio) 'WAV artifact exists'
    Check (Test-Path -LiteralPath $Reply.data.image) 'waveform and spectrum artifact exists'
    Check ($Reply.data.summary[0].peak -gt 0.001) 'render contains measurable audio'
    return $Reply.data
}

$ownedApp = Start-Process -FilePath $ownedBinary -WorkingDirectory (Split-Path $PSScriptRoot -Parent) -WindowStyle Hidden -RedirectStandardOutput (Join-Path $OutputDirectory 'app.out') -RedirectStandardError (Join-Path $OutputDirectory 'app.log') -PassThru
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $sessions = & $CliBinary sessions | ConvertFrom-Json
        if ([DateTime]::UtcNow -gt $deadline) { throw 'isolated native session did not start' }
    } while (!$sessions)
    $initial = Call-Session 'context' @{detail='musical'}
    $trackId = $initial.data.tracks[0].id
    $patternId = $initial.data.patterns[0].id
    $notes = @(
        @{key=65;start=0;length=720;velocity=0.7},
        @{key=68;start=960;length=480;velocity=0.65},
        @{key=72;start=1920;length=720;velocity=0.72},
        @{key=68;start=2880;length=480;velocity=0.6}
    )
    $batch = @{operations=@(
        @{command='patch_notes';args=@{patternId=$patternId;trackId=$trackId;notes=$notes;append=$false}},
        @{command='set_bpm';args=@{bpm=140}}
    )}
    $beforeRevision = $script:revision
    $first = Call-Session 'apply' $batch $beforeRevision 'melody-once'
    $retry = Call-Session 'apply' $batch $beforeRevision 'melody-once'
    Check ($retry.revision -eq $first.revision) 'idempotent retry does not apply twice'
    $null = Call-Session 'set_bpm' @{bpm=99} $beforeRevision $null 'REVISION_CONFLICT'
    $null = Call-Session 'apply' @{operations=@(@{command='set_bpm';args=@{bpm=99}})} $beforeRevision 'melody-once' 'IDEMPOTENCY_CONFLICT'
    $null = Call-Session 'set_bpm' @{bpm=140;typo=1} $script:revision $null 'INVALID_ARGUMENTS'
    $melody = (Call-Session 'context' @{detail='musical';trackId=$trackId}).data.music | ConvertTo-Json -Depth 50 -Compress
    $added = Edit 'add_track' @{source='sampler:808-kit';name='Agent drums'}
    $drumId = $added.data.createdTrackIds[0]
    $drums = @(0,960,1920,2880 | ForEach-Object { @{key=36;start=$_;length=120;velocity=0.75} })
    $null = Edit 'patch_notes' @{patternId=$patternId;trackId=$drumId;notes=$drums;append=$false}
    $afterMelody = (Call-Session 'context' @{detail='musical';trackId=$trackId}).data.music | ConvertTo-Json -Depth 50 -Compress
    Check ($melody -eq $afterMelody) 'adding drums preserves the human melody'
    $invalidBatch = @{operations=@(@{command='set_bpm';args=@{bpm=99}},@{command='set_track_gain';args=@{trackId=4294967295;gainDb=-12}})}
    $null = Call-Session 'apply' $invalidBatch $script:revision $null 'INVALID_ARGUMENTS'
    $context = Call-Session 'context' @{detail='musical'}
    Check ($context.data.bpm -eq 140) 'failed batch restores the tempo'
    $null = Edit 'undo' @{}
    $withoutDrums = Call-Session 'context' @{detail='musical';trackId=$drumId}
    Check ($withoutDrums.data.music.channels[0].notes.Count -eq 0) 'shared undo removes the drum edit'
    $null = Edit 'redo' @{}
    $withDrums = Call-Session 'context' @{detail='musical';trackId=$drumId}
    Check ($withDrums.data.music.channels[0].notes.Count -eq 4) 'shared redo restores the drum edit'
    $drumAudio = Wait-Artifact (Call-Session 'render' @{options=@{mode='pattern';bars=1;tailSeconds=0.25;trackId=$drumId}})
    Check ($drumAudio.summary[0].peak -gt 0.01) 'sampled drums are audible in the offline graph'

    $fixturePath = Join-Path $OutputDirectory 'fixture.clap'
    Copy-Item -LiteralPath $FixtureBinary -Destination $fixturePath
    $null = Edit 'set_track_source_clap' @{trackId=$trackId;bundlePath=$fixturePath;pluginId='dev.mydaw.fixture.synth'}
    $parameters = Call-Session 'plugin_parameters' @{format='clap';trackId=$trackId;query='Detune'}
    Check ($parameters.data.parameters[0].max -eq 24) 'CLAP exposes native parameter units'
    $null = Edit 'set_plugin_parameter' @{format='clap';trackId=$trackId;paramId=0;text='12 st'}
    $parameters = Call-Session 'plugin_parameters' @{format='clap';trackId=$trackId}
    Check ($parameters.data.parameters[0].value -eq 12 -and !$parameters.data.parameters[0].pending) 'CLAP parameter reaches the processor'
    $null = Edit 'undo' @{}
    $parameters = Call-Session 'plugin_parameters' @{format='clap';trackId=$trackId}
    Check ($parameters.data.parameters[0].value -eq 0) 'plugin state participates in shared undo'
    $null = Edit 'redo' @{}
    $preset = Join-Path $OutputDirectory 'synth-state.clap-preset'
    $null = Call-Session 'save_plugin_preset' @{format='clap';trackId=$trackId;path=$preset}
    $null = Edit 'set_plugin_parameter' @{format='clap';trackId=$trackId;paramId=0;value=0}
    $null = Edit 'load_plugin_preset' @{format='clap';trackId=$trackId;path=$preset}
    $parameters = Call-Session 'plugin_parameters' @{format='clap';trackId=$trackId}
    Check ($parameters.data.parameters[0].value -eq 12) 'preset restores the processor state'
    $render = Wait-Artifact (Call-Session 'render' @{options=@{mode='pattern';bars=1;tailSeconds=0.25}})
    $analysis = Wait-Artifact (Call-Session 'analyze' @{path=$render.audio;images=$true})
    Check ($render.frames -eq $analysis.frames) 'analysis reads the rendered audio'

    $vstArtifact = $null
    if ($InstalledVst3) {
        $installed = (Call-Session 'get_vst3_plugins' @{rescan=$false}).data | Where-Object name -eq 'Serum' | Select-Object -First 1
        if (!$installed) { throw 'requested installed Serum verification but it is unavailable' }
        $null = Edit 'set_track_source_vst3' @{trackId=$trackId;modulePath=$installed.path;cid=$installed.id}
        $parameters = Call-Session 'plugin_parameters' @{format='vst3';trackId=$trackId;query='Master';limit=20}
        $parameter = $parameters.data.parameters | Where-Object { !$_.readOnly -and $_.name -match '(vol|gain)' } | Select-Object -First 1
        Check ($null -ne $parameter) 'real VST3 exposes controller metadata'
        $vstBaseline = Wait-Artifact (Call-Session 'render' @{options=@{mode='pattern';bars=1;tailSeconds=0.25;trackId=$trackId}})
        $null = Edit 'set_plugin_parameter' @{format='vst3';trackId=$trackId;paramId=$parameter.id;value=0.3}
        $parameters = Call-Session 'plugin_parameters' @{format='vst3';trackId=$trackId;query='Master';limit=20}
        Check ([Math]::Abs(($parameters.data.parameters | Where-Object id -eq $parameter.id).value - 0.3) -lt 0.001) 'real VST3 parameter reaches the processor'
        $vstArtifact = Wait-Artifact (Call-Session 'render' @{options=@{mode='pattern';bars=1;tailSeconds=0.25;trackId=$trackId}})
        Check ([Math]::Abs($vstBaseline.summary[0].rms - $vstArtifact.summary[0].rms) -gt 0.001) 'real VST3 parameter changes rendered audio'
    }
    $project = Join-Path $OutputDirectory 'collaboration.mydaw'
    $null = Call-Session 'save_project' @{path=$project;saveAs=$true}
    Check (Test-Path -LiteralPath $project) 'session and plugin state saved'
    $null = Edit 'new_project' @{}
    $null = Edit 'open_project' @{path=$project}
    $reopened = Call-Session 'context' @{detail='musical';trackId=$trackId}
    Check (($reopened.data.music | ConvertTo-Json -Depth 50 -Compress) -eq $melody) 'saved human melody survives reopening'
    if ($InstalledVst3) {
        $parameters = Call-Session 'plugin_parameters' @{format='vst3';trackId=$trackId;query='Master';limit=20}
        Check ([Math]::Abs(($parameters.data.parameters | Where-Object id -eq $parameter.id).value - 0.3) -lt 0.001) 'VST3 component and controller state survive reopening'
        $null = Edit 'set_track_source_clap' @{trackId=$trackId;bundlePath=$fixturePath;pluginId='dev.mydaw.fixture.synth'}
        $null = Edit 'load_plugin_preset' @{format='clap';trackId=$trackId;path=$preset}
        $reverse = Wait-Artifact (Call-Session 'render' @{options=@{mode='pattern';bars=1;tailSeconds=0.25;trackId=$trackId}})
        Check ([Math]::Abs($reverse.summary[0].topBins[0].frequencyHz - 698.46) -lt 5) 'switching from VST3 back to CLAP captures the correct plugin state'
    }
    $endpoint = Get-Content -LiteralPath (Join-Path $env:MYDAW_CONTROL_DIR ($sessions[0].sessionId + '.json')) | ConvertFrom-Json
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $endpoint.socket, [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect(1500)
        # An authenticated server must also release an incomplete frame.
        $pipe.Write([byte[]]@(1,0), 0, 2)
        Start-Sleep -Milliseconds 5500
        $closed = $false
        try { $pipe.Write([byte[]]@(0,0), 0, 2) } catch { $closed = $true }
        Check $closed 'incomplete local frames expire without occupying a client forever'
    } finally { $pipe.Dispose() }
    $null = Call-Session 'context'
    $cancelled = Call-Session 'render' @{options=@{mode='pattern';bars=50;tailSeconds=0}}
    $null = Call-Session 'cancel_job' @{jobId=$cancelled.data.jobId}
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 50
        $cancelStatus = Call-Session 'job_status' @{jobId=$cancelled.data.jobId}
        if ([DateTime]::UtcNow -gt $deadline) { throw 'cancelled worker did not stop' }
    } while ($cancelStatus.data.status -eq 'running')
    Check ($cancelStatus.data.status -eq 'cancelled') 'cancellation stops the owned snapshot worker'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $compact = & $CliBinary context
    $timer.Stop()
    $report = @{checks=$checks.ToArray();revision=$script:revision;contextBytes=[Text.Encoding]::UTF8.GetByteCount($compact);contextMilliseconds=$timer.Elapsed.TotalMilliseconds;project=$project;render=$render;vstRender=$vstArtifact}
    $report | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.json') -Encoding utf8NoBOM
    $report | ConvertTo-Json -Depth 20 -Compress
} finally {
    # The handle belongs to the process launched here, never an existing DAW.
    if (!$ownedApp.HasExited) { Stop-Process -Id $ownedApp.Id }
    $env:MYDAW_CONTROL_DIR = $previousDirectory
}
