# Layer 3 in order: chain choice on train -> test evaluation -> FX calibration -> samples.
# Each step waits for a running copy of the previous one (started separately) to finish.
$py = "$PSScriptRoot\..\.venv\Scripts\python.exe"
Set-Location "$PSScriptRoot\.."
$env:PYTHONIOENCODING = 'utf-8'
function Wait-For($pat) {
  while (Get-CimInstance Win32_Process -Filter "Name='python.exe'" | Where-Object { $_.CommandLine -match $pat }) { Start-Sleep 20 }
}
if ($args -notcontains '--samples-only') {
  & $py -m av.layer3_tune
  & $py -m av.layer3_eval
  & $py -m av.fx_calib
}
Wait-For 'fx_calib'
& $py -m av.make_samples
Write-Output "L3 CHAIN DONE"
