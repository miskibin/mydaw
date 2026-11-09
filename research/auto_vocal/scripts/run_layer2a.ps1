# Layer 2a pitch-tracking benchmark as it was run (resumable). CREPE-full costs ~30x
# CREPE-tiny on CPU, so it is scored on Vocadito and 40 MIR-1K clips only.
$py = "$PSScriptRoot\..\.venv\Scripts\python.exe"
Set-Location "$PSScriptRoot\.."
$env:PYTHONIOENCODING = 'utf-8'
& $py -m av.layer2_pitch vocadito
& $py -m av.layer2_pitch mir1k --every 10 --limit 40
& $py -m av.layer2_pitch vocalset --n 140 --methods pyin,crepe_tiny,wave_yin,wave_rmvpe
& $py -m av.layer2_pitch mir1k_mix --every 10 --methods pyin,crepe_tiny,wave_yin,wave_rmvpe
Write-Output "L2A DONE"
