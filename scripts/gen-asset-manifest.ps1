#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Regenerates assets-manifest.json from the pinned Salamander Grand Piano SFZ.

.DESCRIPTION
    This is the *generator*, not the installer. It runs rarely -- when the
    pinned commit or the chosen subset changes -- and its output,
    assets-manifest.json, is committed. Day to day you want fetch-assets.ps1,
    which only reads that file.

    What it does:

      1. Fetches the three SFZ include files that actually describe the
         mapping, from the pinned commit:
             Data/region.txt    key ranges, root keys and sample names
             Data/notes.txt     the sixteen velocity layers and their spans
             Data/tune_nat.txt  per-region detune (all zero in the natural set)
      2. Parses the small subset of SFZ they use. This is NOT an SFZ parser:
         it understands `lokey`, `hikey`, `pitch_keycenter`, `sample`, `lovel`,
         `hivel` and `#define`, which is all these three files contain. That is
         the point -- the ambiguity of SFZ is resolved here, once, offline, and
         the runtime reads a format it fully implements.
      3. Picks a subset (see -Layers) and re-tiles the chosen layers' velocity
         spans so they cover 1..127 with no gaps.
      4. Downloads each chosen .flac, hashes it, and records url + sha256 +
         bytes alongside the mapping.
      5. Writes assets-manifest.json.

    Downloaded files are kept under assets/instruments/<id>/, so a run of this
    script also *installs* the content -- fetch-assets.ps1 afterwards is a
    verification pass.

.PARAMETER Layers
    Which of Salamander's sixteen velocity layers to keep, softest first.
    Three layers of thirty notes is ~138 MB; see docs for the budget.

.PARAMETER Force
    Re-download files that are already present at the right size.

.EXAMPLE
    pwsh -File scripts/gen-asset-manifest.ps1
#>
[CmdletBinding()]
param(
    [int[]] $Layers = @(4, 9, 14),
    [switch] $Force
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# --- what we are pinned to -------------------------------------------------
$Repo      = 'https://github.com/sfzinstruments/SalamanderGrandPiano'
$Commit    = '3382bf9496bba2486f5ab0de55a264d1dfc38404'
$RawBase   = "https://raw.githubusercontent.com/sfzinstruments/SalamanderGrandPiano/$Commit"
$License   = 'CC-BY-3.0'
$LicenseUrl= 'https://creativecommons.org/licenses/by/3.0/'
$Author    = 'Alexander Holm'

$InstrumentId   = 'salamander-piano'
$InstrumentName = 'Salamander Grand Piano'

# Memory budget, in the manifest so the loader does not hard-code it.
# 30 notes x 3 layers x 7 s x 48 kHz x 2 ch x 4 B = 242 MB decoded.
$MaxSeconds      = 7.0
$FadeOutSeconds  = 1.0
# A grand piano's damper. Longer than the synth's 120 ms, and audibly right.
$ReleaseMs       = 400

$RepoRoot    = Split-Path -Parent $PSScriptRoot
$InstallDir  = Join-Path $RepoRoot "assets/instruments/$InstrumentId"
$ManifestOut = Join-Path $RepoRoot 'assets-manifest.json'

function Get-TextFile([string] $RelativePath) {
    $url = "$RawBase/" + ($RelativePath -replace ' ', '%20')
    Write-Host "  reading $RelativePath"
    (Invoke-WebRequest -Uri $url -UseBasicParsing).Content
}

# --- 1. fetch the mapping --------------------------------------------------
Write-Host "Reading the SFZ mapping from $Commit"
$regionText = Get-TextFile 'Data/region.txt'
$notesText  = Get-TextFile 'Data/notes.txt'
$tuneText   = Get-TextFile 'Data/tune_nat.txt'

# --- 2. parse the subset ---------------------------------------------------
# `<region> region_label=NN tune=$TUNENN ... lokey=21 hikey=22
#  pitch_keycenter=21 sample=A0$VEL.$EXT`
$regions = foreach ($line in ($regionText -split "`r?`n")) {
    if ($line -notmatch '<region>') { continue }
    $label = if ($line -match 'region_label=(\d+)')     { $Matches[1] } else { $null }
    $lokey = if ($line -match 'lokey=(\d+)')            { [int]$Matches[1] } else { $null }
    $hikey = if ($line -match 'hikey=(\d+)')            { [int]$Matches[1] } else { $null }
    $root  = if ($line -match 'pitch_keycenter=(\d+)')  { [int]$Matches[1] } else { $null }
    $note  = if ($line -match 'sample=([A-G]#?-?\d+)\$VEL') { $Matches[1] } else { $null }
    if ($null -eq $lokey -or $null -eq $hikey -or $null -eq $root -or $null -eq $note) { continue }
    [pscustomobject]@{ Label = $label; LoKey = $lokey; HiKey = $hikey; Root = $root; Note = $note }
}
if ($regions.Count -eq 0) { throw 'no <region> lines were parsed; the upstream layout changed' }
Write-Host "  $($regions.Count) regions, keys $($regions[0].LoKey)..$($regions[-1].HiKey)"

# `<group> #include "Data/vel_01.txt" lovel=1 hivel=26 #include "Data/region.txt"`
$allLayers = foreach ($line in ($notesText -split "`r?`n")) {
    if ($line -notmatch '<group>') { continue }
    if ($line -notmatch 'vel_(\d+)\.txt') { continue }
    $index = [int]$Matches[1]
    $lovel = if ($line -match 'lovel=(\d+)') { [int]$Matches[1] } else { 1 }
    $hivel = if ($line -match 'hivel=(\d+)') { [int]$Matches[1] } else { 127 }
    [pscustomobject]@{ Index = $index; LoVel = $lovel; HiVel = $hivel }
}
if ($allLayers.Count -eq 0) { throw 'no <group> velocity layers were parsed' }
Write-Host "  $($allLayers.Count) velocity layers upstream; keeping $($Layers -join ', ')"

# `#define $TUNE01 0`
$tune = @{}
foreach ($line in ($tuneText -split "`r?`n")) {
    if ($line -match '#define\s+\$TUNE(\d+)\s+(-?[\d.]+)') {
        $tune[$Matches[1]] = [double]$Matches[2]
    }
}

# --- 3. re-tile the chosen layers over 1..127 ------------------------------
# Each kept layer keeps its recorded timbre but is widened to cover the
# velocities between it and its neighbours -- split at the midpoint between
# adjacent layers' recorded centres. Without this the gaps between kept layers
# would be silent.
$kept = foreach ($index in ($Layers | Sort-Object)) {
    $layer = $allLayers | Where-Object Index -eq $index
    if (-not $layer) { throw "velocity layer $index does not exist upstream" }
    [pscustomobject]@{
        Index  = $index
        Name   = "v$index"
        Centre = ($layer.LoVel + $layer.HiVel) / 2.0
    }
}
$kept = @($kept)
for ($i = 0; $i -lt $kept.Count; $i++) {
    $lo = if ($i -eq 0) { 1 } else {
        [int][math]::Round(($kept[$i - 1].Centre + $kept[$i].Centre) / 2.0) + 1
    }
    $hi = if ($i -eq $kept.Count - 1) { 127 } else {
        [int][math]::Round(($kept[$i].Centre + $kept[$i + 1].Centre) / 2.0)
    }
    $kept[$i] | Add-Member -NotePropertyName LoVel -NotePropertyValue $lo -Force
    $kept[$i] | Add-Member -NotePropertyName HiVel -NotePropertyValue $hi -Force
    Write-Host "  layer $($kept[$i].Name) covers velocity $lo..$hi"
}

# --- 4. download, hash, and build the entries ------------------------------
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$samples = [System.Collections.Generic.List[object]]::new()
$total = $regions.Count * $kept.Count
$done = 0

foreach ($layer in $kept) {
    foreach ($region in $regions) {
        $done++
        $name = "$($region.Note)$($layer.Name).flac"
        $url  = "$RawBase/Samples/" + ($name -replace '#', '%23')
        $path = Join-Path $InstallDir $name

        if ($Force -or -not (Test-Path $path)) {
            Write-Progress -Activity 'Fetching samples' -Status $name `
                -PercentComplete (100 * $done / $total)
            Invoke-WebRequest -Uri $url -OutFile $path -UseBasicParsing
        }

        $item = Get-Item $path
        $hash = (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
        $cents = if ($region.Label -and $tune.ContainsKey($region.Label)) { $tune[$region.Label] } else { 0 }

        $samples.Add([ordered]@{
            file       = $name
            root_key   = $region.Root
            key_lo     = $region.LoKey
            key_hi     = $region.HiKey
            vel_lo     = $layer.LoVel
            vel_hi     = $layer.HiVel
            tune_cents = $cents
            url        = $url
            sha256     = $hash
            bytes      = $item.Length
        })
    }
}
Write-Progress -Activity 'Fetching samples' -Completed

# --- 5. emit ---------------------------------------------------------------
# `Measure-Object -Property` cannot see into an ordered dictionary, which is
# what each entry is; project the value out first.
$bytes = ($samples | ForEach-Object { $_.bytes } | Measure-Object -Sum).Sum
$manifest = [ordered]@{
    id          = $InstrumentId
    name        = $InstrumentName
    description = "Yamaha C5 grand, $($regions.Count) sampled notes x $($kept.Count) velocity layers"
    release_ms  = $ReleaseMs
    max_seconds = $MaxSeconds
    fade_out_seconds = $FadeOutSeconds
    attribution = "$InstrumentName V3 by $Author, licensed $License. Source: $Repo"
    source      = [ordered]@{
        repo        = $Repo
        commit      = $Commit
        license     = $License
        license_url = $LicenseUrl
        author      = $Author
    }
    samples     = $samples
}

# Merge, rather than replace: assets-manifest.json also carries the factory
# kits, whose generator is scripts/gen-factory-kits.mjs. Every instrument this
# script does not own is copied through untouched, and the file is written in
# the container shape (`schema: 2`) whether or not it was in one before.
$existing = if (Test-Path $ManifestOut) {
    Get-Content -Path $ManifestOut -Raw | ConvertFrom-Json
} else { $null }
$others = if ($null -eq $existing) {
    @()
} elseif ($existing.PSObject.Properties.Name -contains 'instruments') {
    @($existing.instruments | Where-Object { $_.id -ne $InstrumentId })
} elseif ($existing.id -ne $InstrumentId) {
    @($existing)
} else {
    @()
}

[ordered]@{
    schema      = 2
    instruments = @($manifest) + $others
} | ConvertTo-Json -Depth 8 | Set-Content -Path $ManifestOut -Encoding utf8NoBOM

Write-Host ''
Write-Host "Wrote $ManifestOut"
Write-Host "  $(1 + $others.Count) instrument(s); $($others.Count) left untouched"
Write-Host "  $($samples.Count) samples, $([math]::Round($bytes / 1MB, 1)) MB on disk"
Write-Host "  decoded worst case: $([math]::Round($samples.Count * $MaxSeconds * 48000 * 2 * 4 / 1MB, 0)) MB"
