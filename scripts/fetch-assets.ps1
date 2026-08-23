#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Installs MYDAW's bundled sample content, pinned and checksummed.

.DESCRIPTION
    Reads assets-manifest.json -- the same file the in-app downloader has
    compiled into it -- and makes assets/instruments/<id>/ match it, for every
    instrument the file offers:

      * every sample downloaded from its pinned URL,
      * every file verified against its SHA-256,
      * manifest.json written last, so its presence means the samples beside
        it are complete,
      * assets/ATTRIBUTION.md written, covering every installed instrument.

    Idempotent. A file already present at its recorded size is left alone; use
    -Verify to re-hash everything, or -Force to re-download it.

    Every download lands as <name>.part and is renamed onto its final name only
    after its hash matches, so an interrupted run never leaves a truncated
    sample where the loader would decode it.

    assets/ is gitignored. Nothing this script writes is committed.

.PARAMETER Instrument
    Install only these instrument ids (see assets-manifest.json). Default: all
    of them. The kits are separate downloads; nobody has to take the marimba to
    get the drum machine.

.PARAMETER Verify
    Re-hash files that are already present, instead of trusting their size.

.PARAMETER Force
    Re-download everything.

.PARAMETER List
    Print what the manifest offers and exit. No network.

.EXAMPLE
    pwsh -File scripts/fetch-assets.ps1

.EXAMPLE
    pwsh -File scripts/fetch-assets.ps1 -Instrument 808-kit,808-bass

.EXAMPLE
    pwsh -File scripts/fetch-assets.ps1 -Verify
#>
[CmdletBinding()]
param(
    [string[]] $Instrument,
    [switch] $Verify,
    [switch] $Force,
    [switch] $List
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$RepoRoot     = Split-Path -Parent $PSScriptRoot
$ManifestPath = Join-Path $RepoRoot 'assets-manifest.json'
$AssetsRoot   = Join-Path $RepoRoot 'assets'

if (-not (Test-Path $ManifestPath)) {
    throw "no manifest at $ManifestPath; run scripts/gen-asset-manifest.ps1 first"
}

$document = Get-Content -Path $ManifestPath -Raw | ConvertFrom-Json

# Two shapes, one file. `{ schema: 2, instruments: [...] }` is the container the
# factory library needs; a bare instrument object is what this file was before
# there was more than one, and still installs. See `AssetCatalog` in
# plugins/sampler/src/download.rs, which reads the same two shapes.
$catalog = if ($document.PSObject.Properties.Name -contains 'instruments') {
    @($document.instruments)
} else {
    @($document)
}
if ($catalog.Count -eq 0) {
    throw "$ManifestPath offers no instruments"
}

if ($List) {
    Write-Host "$ManifestPath offers $($catalog.Count) instrument(s):"
    foreach ($entry in $catalog) {
        $bytes = ($entry.samples | Measure-Object -Property bytes -Sum).Sum
        Write-Host ("  {0,-18} {1,4} samples  {2,7:N1} MB  {3}" -f `
            $entry.id, $entry.samples.Count, ($bytes / 1MB), $entry.source.license)
    }
    return
}

if ($Instrument) {
    # `pwsh -File script.ps1 -Instrument a,b` hands the parameter the single
    # string "a,b" -- argument binding only splits arrays in `-Command` mode --
    # and the documented invocation is `-File`. So split here, and accept both
    # `-Instrument a,b` and `-Instrument a b`.
    $Instrument = @($Instrument -split ',' | ForEach-Object { $_.Trim() } |
        Where-Object { $_.Length -gt 0 })
    $known = @($catalog | ForEach-Object { $_.id })
    $missing = @($Instrument | Where-Object { $known -notcontains $_ })
    if ($missing.Count -gt 0) {
        throw "no such instrument: $($missing -join ', '). Try -List."
    }
    $selected = @($catalog | Where-Object { $Instrument -contains $_.id })
} else {
    $selected = $catalog
}

$totalBytes = ($selected | ForEach-Object { $_.samples } | Measure-Object -Property bytes -Sum).Sum
$totalFiles = ($selected | ForEach-Object { $_.samples.Count } | Measure-Object -Sum).Sum
Write-Host "$($selected.Count) instrument(s), $totalFiles samples, $([math]::Round($totalBytes / 1MB, 1)) MB"
Write-Host "  into $(Join-Path $AssetsRoot 'instruments')"
Write-Host ''

$fetched = 0
$skipped = 0
$done = 0

foreach ($manifest in $selected) {
    $installDir = Join-Path $AssetsRoot "instruments/$($manifest.id)"
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null

    $instrumentBytes = ($manifest.samples | Measure-Object -Property bytes -Sum).Sum
    Write-Host "$($manifest.name) [$($manifest.id)]: $($manifest.samples.Count) samples, $([math]::Round($instrumentBytes / 1MB, 1)) MB"
    Write-Host "  from $($manifest.source.repo) @ $($manifest.source.commit) ($($manifest.source.license))"

    foreach ($sample in $manifest.samples) {
        $done++
        $path = Join-Path $installDir $sample.file
        $part = "$path.part"

        $present = (Test-Path $path) -and ((Get-Item $path).Length -eq $sample.bytes)
        if ($present -and $Verify) {
            $hash = (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($hash -ne $sample.sha256.ToLowerInvariant()) {
                Write-Warning "$($manifest.id)/$($sample.file) failed verification; re-fetching"
                Remove-Item -Path $path -Force
                $present = $false
            }
        }
        if ($present -and -not $Force) {
            $skipped++
            continue
        }

        Write-Progress -Activity 'Installing samples' -Status "$($manifest.id)/$($sample.file)" `
            -PercentComplete (100 * $done / $totalFiles)
        Invoke-WebRequest -Uri $sample.url -OutFile $part -UseBasicParsing

        $hash = (Get-FileHash -Path $part -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne $sample.sha256.ToLowerInvariant()) {
            Remove-Item -Path $part -Force
            throw "$($manifest.id)/$($sample.file) does not match its pinned checksum (expected $($sample.sha256), got $hash); the download was discarded"
        }
        Move-Item -Path $part -Destination $path -Force
        $fetched++
    }

    # The runtime manifest is this instrument's slice of the download manifest,
    # verbatim. Unknown fields (url, sha256, bytes, note) are ignored by the
    # loader, and one fewer transformation is one fewer thing to get wrong.
    # Written last: its presence means the samples beside it are complete.
    $runtimeManifest = Join-Path $installDir 'manifest.json'
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -Path "$runtimeManifest.part" -Encoding utf8NoBOM
    Move-Item -Path "$runtimeManifest.part" -Destination $runtimeManifest -Force
}
Write-Progress -Activity 'Installing samples' -Completed

# --- credits ---------------------------------------------------------------
# Some of this content is CC0, which asks for nothing. Some is CC-BY, which
# requires the credit to travel with it. The file covers both, because a user
# who wants to know where a sound came from should not have to know which.
$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('# Bundled sample content -- credits')
$lines.Add('')
$lines.Add('This directory is generated by `scripts/fetch-assets.ps1` (or by the in-app')
$lines.Add('downloader). It is **not** part of the MYDAW source tree and is excluded from')
$lines.Add('version control; every file here was downloaded from a pinned commit of the')
$lines.Add('source named below and verified against a SHA-256 recorded in')
$lines.Add('`assets-manifest.json`.')
$lines.Add('')
$lines.Add('| Instrument | Samples | Author | Source | Pinned commit | Licence |')
$lines.Add('| --- | ---: | --- | --- | --- | --- |')
foreach ($manifest in $catalog) {
    $installed = Test-Path (Join-Path $AssetsRoot "instruments/$($manifest.id)/manifest.json")
    $mark = if ($installed) { '' } else { ' *(not installed)*' }
    $lines.Add(("| {0}{1} | {2} | {3} | <{4}> | ``{5}`` | [{6}]({7}) |" -f `
        $manifest.name, $mark, $manifest.samples.Count, $manifest.source.author, `
        $manifest.source.repo, $manifest.source.commit.Substring(0, 12), `
        $manifest.source.license, $manifest.source.license_url))
}
$lines.Add('')
foreach ($manifest in $catalog) {
    $lines.Add("## $($manifest.name)")
    $lines.Add('')
    $lines.Add("> $($manifest.attribution)")
    $lines.Add('')
}
$lines.Add('## What the licences require')
$lines.Add('')
$lines.Add('**CC0-1.0** is a public-domain dedication. It requires nothing -- no credit, no')
$lines.Add('royalties, no share-alike -- of you or of anything you make with these sounds.')
$lines.Add('The credits above are given anyway, because the people who recorded them')
$lines.Add('earned them.')
$lines.Add('')
$lines.Add('**CC-BY** is an *attribution* licence: any distribution of that content, or of')
$lines.Add('a work built from it, must carry the credit above. MYDAW does not redistribute')
$lines.Add('these files -- it downloads them from the sources named here -- but anything you')
$lines.Add('export that contains their sound does.')
Set-Content -Path (Join-Path $AssetsRoot 'ATTRIBUTION.md') -Value ($lines -join "`n") -Encoding utf8NoBOM

Write-Host ''
Write-Host "Done: $fetched fetched, $skipped already present."
Write-Host "  $(Join-Path $AssetsRoot 'ATTRIBUTION.md')"
