# Layer 3 in order: EQ/chain choice on train -> dynamics calibration -> test evaluation ->
# listening samples. Each step first waits until no chain step is running.
# usage: run_layer3.ps1 [tune|dyn|eval|samples]   (start from that step; default tune)
$py = "$PSScriptRoot\..\.venv\Scripts\python.exe"
Set-Location "$PSScriptRoot\.."
$env:PYTHONIOENCODING = 'utf-8'
function Wait-For($pat) {
  while (Get-CimInstance Win32_Process -Filter "Name='python.exe'" | Where-Object { $_.CommandLine -match $pat }) { Start-Sleep 20 }
}
$from = if ($args.Count) { $args[0] } else { 'tune' }
# fx (av.fx_calib) is not in the chain: the reverb setting is kept from its first run (owner)
$steps = @('tune', 'dyn', 'eval', 'samples')
$mods = @{ tune = 'av.layer3_tune'; dyn = 'av.dyn_calib'; eval = 'av.layer3_eval'; fx = 'av.fx_calib'; samples = 'av.make_samples' }
foreach ($s in $steps[$steps.IndexOf($from)..($steps.Count - 1)]) {
  Wait-For 'av\.(layer3_tune|dyn_calib|layer3_eval|fx_calib|make_samples)'  # a step started by hand
  if ($s -eq 'eval') { Remove-Item 'D:\mydaw-data\auto_vocal_cache\l3_paired.jsonl' -ErrorAction SilentlyContinue }
  & $py -m $mods[$s]
  if ($LASTEXITCODE -ne 0) { Write-Output "STEP $s FAILED"; exit 1 }
}
Write-Output "L3 CHAIN DONE"
