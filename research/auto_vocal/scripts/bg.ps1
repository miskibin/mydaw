# Start a research job detached, at below-normal priority, logging to D:\mydaw-data\logs.
# usage: bg.ps1 <name> <python args...>
param([string]$Name, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)
$here = Split-Path -Parent $PSScriptRoot
$env:PYTHONIOENCODING = 'utf-8'
New-Item -ItemType Directory -Force D:\mydaw-data\logs | Out-Null
$p = Start-Process -FilePath "$here\.venv\Scripts\python.exe" -ArgumentList $Rest -WorkingDirectory $here `
  -RedirectStandardOutput "D:\mydaw-data\logs\$Name.out" -RedirectStandardError "D:\mydaw-data\logs\$Name.err" `
  -WindowStyle Hidden -PassThru
$p.PriorityClass = 'BelowNormal'
Write-Output "$Name pid=$($p.Id)"
