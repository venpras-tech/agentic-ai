# AI Editor — Windows setup (Unsloth Studio–style bootstrap)
# Small installer EXE + runtime deps downloaded on the machine (WebView2, models in-app).
#
# From a built repo:
#   powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1
#
# With a local setup EXE:
#   powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -LocalInstaller ".\dist-installer\AI Editor_0.1.0_x64-setup.exe"
#
# Remote (host the setup EXE and set URL):
#   powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -InstallerUrl "https://your.cdn/AI-Editor-setup.exe"
#
# One-liner after you publish install.ps1 (optional):
#   irm https://your.cdn/install.ps1 | iex

param(
    [string]$InstallerUrl = "",
    [string]$LocalInstaller = "",
    [switch]$SkipWebView2,
    [switch]$Launch
)

$ErrorActionPreference = "Stop"
$AppName = "AI Editor"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Staging = Join-Path $env:LOCALAPPDATA "AIEditor\install-cache"

function Write-Step($msg) {
    Write-Host "  $msg" -ForegroundColor Cyan
}

function Test-WebView2Installed {
    $keys = @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    )
    foreach ($k in $keys) {
        if (Test-Path $k) {
            $pv = (Get-ItemProperty -Path $k -ErrorAction SilentlyContinue).pv
            if ($pv) { return $true }
        }
    }
    return $false
}

function Install-WebView2Runtime {
    Write-Step "WebView2 Runtime not detected — installing (required for the UI)..."
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        Write-Step "Trying winget: Microsoft.EdgeWebView2Runtime"
        $p = Start-Process winget -ArgumentList @(
            "install", "--id", "Microsoft.EdgeWebView2Runtime",
            "-e", "--accept-package-agreements", "--accept-source-agreements"
        ) -Wait -PassThru -NoNewWindow
        if ($p.ExitCode -eq 0 -or (Test-WebView2Installed)) {
            Write-Host "  WebView2 ready." -ForegroundColor Green
            return
        }
    }
    $bootstrap = Join-Path $Staging "MicrosoftEdgeWebview2Setup.exe"
    New-Item -ItemType Directory -Force -Path $Staging | Out-Null
    $url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
    Write-Step "Downloading WebView2 bootstrapper..."
    Invoke-WebRequest -Uri $url -OutFile $bootstrap -UseBasicParsing
    Write-Step "Running WebView2 bootstrapper (silent)..."
    $p = Start-Process -FilePath $bootstrap -ArgumentList "/silent", "/install" -Wait -PassThru
    if (-not (Test-WebView2Installed) -and $p.ExitCode -ne 0) {
        throw "WebView2 install failed. Install manually: https://developer.microsoft.com/microsoft-edge/webview2/"
    }
    Write-Host "  WebView2 ready." -ForegroundColor Green
}

function Resolve-InstallerPath {
    if ($LocalInstaller -and (Test-Path -LiteralPath $LocalInstaller)) {
        return (Resolve-Path -LiteralPath $LocalInstaller).Path
    }
    $bundles = @(
        (Join-Path $RepoRoot "dist-installer\*-setup.exe"),
        (Join-Path $RepoRoot "src-tauri\target\release\bundle\nsis\*-setup.exe")
    )
    foreach ($pattern in $bundles) {
        $hit = Get-ChildItem -Path $pattern -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending |
            Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    if ($InstallerUrl) {
        New-Item -ItemType Directory -Force -Path $Staging | Out-Null
        $name = [IO.Path]::GetFileName(($InstallerUrl -split '\?')[0])
        if (-not $name -or $name -notmatch '\.exe$') { $name = "AI-Editor-setup.exe" }
        $dest = Join-Path $Staging $name
        Write-Step "Downloading installer from URL..."
        Invoke-WebRequest -Uri $InstallerUrl -OutFile $dest -UseBasicParsing
        return $dest
    }
    return $null
}

Write-Host ""
Write-Host "============================================" -ForegroundColor Cyan
Write-Host "  $AppName — Windows setup" -ForegroundColor Cyan
Write-Host "============================================" -ForegroundColor Cyan
Write-Host ""

if (-not $SkipWebView2) {
    if (Test-WebView2Installed) {
        Write-Host "WebView2 Runtime: already installed" -ForegroundColor Green
    } else {
        Install-WebView2Runtime
    }
} else {
    Write-Host "Skipping WebView2 check (-SkipWebView2)" -ForegroundColor DarkGray
}

$installer = Resolve-InstallerPath
if (-not $installer) {
    Write-Host ""
    Write-Host "No installer EXE found." -ForegroundColor Red
    Write-Host ""
    Write-Host "Option A — build on this machine:" -ForegroundColor Yellow
    Write-Host "  .\scripts\build-installer.ps1" -ForegroundColor White
    Write-Host "  .\scripts\install.ps1" -ForegroundColor White
    Write-Host ""
    Write-Host "Option B — download a published setup EXE:" -ForegroundColor Yellow
    Write-Host "  .\scripts\install.ps1 -InstallerUrl `"https://.../AI-Editor-setup.exe`"" -ForegroundColor White
    exit 1
}

Write-Host ""
Write-Step "Running installer: $installer"
Write-Host "  (GGUF models are downloaded inside the app — not bundled here.)" -ForegroundColor DarkGray
$p = Start-Process -FilePath $installer -Wait -PassThru
if ($p.ExitCode -ne 0) {
    Write-Host "Installer exited with code $($p.ExitCode)" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "============================================" -ForegroundColor Green
Write-Host "  Setup finished" -ForegroundColor Green
Write-Host "============================================" -ForegroundColor Green
Write-Host "1. Launch AI Editor from Start Menu" -ForegroundColor White
Write-Host "2. Open Models or Model Hub to download a GGUF (or use a remote API in Settings)" -ForegroundColor White
Write-Host "3. Set a project folder, then use Agent mode for code changes" -ForegroundColor White
Write-Host ""

if ($Launch) {
    $exe = Join-Path ${env:LOCALAPPDATA} "Programs\ai-editor\AI Editor.exe"
    if (Test-Path $exe) {
        Start-Process $exe
    } else {
        Write-Host "Could not auto-launch (path not found). Use Start Menu." -ForegroundColor DarkGray
    }
}
