# Convenience entry — forwards to scripts\install.ps1 (Unsloth-style: run from repo root).
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
& (Join-Path $here "scripts\install.ps1") @args
