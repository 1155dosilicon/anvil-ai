# Anvil installer for Windows x86_64.
# Usage: powershell -ExecutionPolicy Bypass -File .\install.ps1 [extra `anvil install` flags]
$ErrorActionPreference = 'Stop'
$anvil = Join-Path $PSScriptRoot 'anvil.exe'
& $anvil install --from $PSScriptRoot --yes @args
exit $LASTEXITCODE
