# Runs once at the first logon, from the setup disc: installs the QA agent and
# starts it. The agent registers itself to start at every later logon.
$ErrorActionPreference = 'Continue'
$source = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = 'C:\textloom-qa'
New-Item -ItemType Directory -Force $root | Out-Null
Copy-Item -LiteralPath (Join-Path $source 'agent.ps1'), (Join-Path $source 'agent.json') -Destination $root -Force
# Files copied from a disc are read-only; `cargo winvm provision` replaces the agent later.
Get-ChildItem -LiteralPath $root -File | ForEach-Object { $_.IsReadOnly = $false }
Start-Process -FilePath 'powershell.exe' -WindowStyle Hidden `
    -ArgumentList "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File $root\agent.ps1"
