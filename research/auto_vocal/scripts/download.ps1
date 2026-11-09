# Fetch the public evaluation sets into D:\mydaw-data\raw (never into the repo).
# Usage: powershell -File download.ps1 [name ...]   (no names = all)
# MUSDB18 is not downloaded whole: av/musdb_fetch.py reads it track by track (range requests).
param([string[]]$Only)
$ErrorActionPreference = 'Continue'
$raw = 'D:\mydaw-data\raw'
New-Item -ItemType Directory -Force $raw | Out-Null
$sets = [ordered]@{
  'vocadito.zip'      = 'https://zenodo.org/record/5578807/files/vocadito.zip?download=1'
  'gs_keys.zip'       = 'https://zenodo.org/record/1095691/files/keys.zip?download=1'
  'gs_meta.zip'       = 'https://zenodo.org/record/1095691/files/original_metadata.zip?download=1'
  'gs_tempo_ann.zip'  = 'https://github.com/GiantSteps/giantsteps-tempo-dataset/archive/0b7d47ba8cae59d3535a02e3db69e2cf6d0af5bb.zip'
  'gtzan_tempo.zip'   = 'https://github.com/TempoBeatDownbeat/gtzan_tempo_beat/archive/refs/heads/main.zip'
  'gtzan_key.zip'     = 'https://github.com/alexanderlerch/gtzan_key/archive/refs/heads/master.zip'
  'gs_key_audio.zip'  = 'https://zenodo.org/record/1095691/files/audio.zip?download=1'
}
foreach ($k in $sets.Keys) {
  if ($Only -and -not ($Only -contains $k)) { continue }
  $dst = Join-Path $raw $k
  if (Test-Path $dst) { Write-Output "skip $k"; continue }
  Write-Output "get $k"
  curl.exe -sSL --retry 3 -o "$dst.part" $sets[$k]
  if ($LASTEXITCODE -eq 0) { Move-Item "$dst.part" $dst } else { Write-Output "FAIL $k" }
}
# the annotation sets and Vocadito are read unpacked (GiantSteps keys/audio are read from the zips)
foreach ($z in 'vocadito', 'gs_tempo_ann', 'gtzan_tempo', 'gtzan_key') {
  if ((Test-Path "$raw\$z.zip") -and -not (Test-Path "D:\mydaw-data\$z")) {
    Expand-Archive "$raw\$z.zip" -DestinationPath "D:\mydaw-data\$z"
  }
}
Write-Output DONE
