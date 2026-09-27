# Build a minimal Windows NSIS installer (app only — no GGUF models bundled).
# WebView2 is fetched at install time via Tauri downloadBootstrapper.
# Usage:
#   .\scripts\build-installer.ps1
#   .\scripts\build-installer.ps1 -Cuda
#   .\scripts\build-installer.ps1 -SkipNpm

param(
    [switch]$Cuda,
    [switch]$SkipNpm
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Require-Command($Name, $Hint) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Missing '$Name'. $Hint"
    }
}

Write-Host "============================================" -ForegroundColor Cyan
Write-Host "  AI Editor — build Windows installer" -ForegroundColor Cyan
Write-Host "============================================" -ForegroundColor Cyan
Write-Host "Repo: $Root"
Write-Host ""

Require-Command node "Install Node.js 20+ LTS from https://nodejs.org/"
Require-Command npm "Comes with Node.js"
Require-Command cargo "Install Rust from https://rustup.rs/ (MSVC toolchain on Windows)"

if (-not $SkipNpm) {
    Write-Host "[1/3] npm install (root + ui)..." -ForegroundColor Yellow
    npm install
    npm --prefix ui install
} else {
    Write-Host "[1/3] Skipping npm install (-SkipNpm)" -ForegroundColor DarkGray
}

$features = "custom-protocol"
if ($Cuda) {
    $features += ",gpu-cuda"
    Write-Host "CUDA build enabled (NVIDIA toolkit required at compile time)." -ForegroundColor Yellow
}

Write-Host "[2/3] tauri build (release, features=$features)..." -ForegroundColor Yellow
Write-Host "      First build compiles llama.cpp — expect 10–30+ minutes." -ForegroundColor DarkGray
$env:CARGO_TERM_COLOR = "always"
npm run tauri -- build --features $features

Write-Host "[3/3] Locating installer..." -ForegroundColor Yellow
$bundleDir = Join-Path $Root "src-tauri\target\release\bundle\nsis"
if (-not (Test-Path $bundleDir)) {
    throw "Bundle folder not found: $bundleDir"
}

$setup = Get-ChildItem -Path $bundleDir -Filter "*-setup.exe" -File | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $setup) {
    $setup = Get-ChildItem -Path $bundleDir -Filter "*.exe" -File | Sort-Object LastWriteTime -Descending | Select-Object -First 1
}
if (-not $setup) {
    throw "No .exe installer found under $bundleDir"
}

$outDir = Join-Path $Root "dist-installer"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$dest = Join-Path $outDir $setup.Name
Copy-Item -LiteralPath $setup.FullName -Destination $dest -Force

$mb = [math]::Round((Get-Item -LiteralPath $dest).Length / 1MB, 1)
Write-Host ""
Write-Host "============================================" -ForegroundColor Green
Write-Host "  Build complete" -ForegroundColor Green
Write-Host "============================================" -ForegroundColor Green
Write-Host "Installer: $dest"
Write-Host "Size:      $mb MB"
Write-Host ""
Write-Host "Ship this .exe to users, or run:" -ForegroundColor Cyan
Write-Host "  .\scripts\install.ps1 -LocalInstaller `"$dest`"" -ForegroundColor White
Write-Host ""
Write-Host "Models are NOT included — users download GGUF from Models / Hub in the app." -ForegroundColor DarkGray
