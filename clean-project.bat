@echo off
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"
set "ERR=0"

set "AUTO=0"
if /i "%~1"=="/Y" set "AUTO=1"
if /i "%~1"=="-Y" set "AUTO=1"

echo ============================================
echo   AI Editor - project cleanup
echo ============================================
echo.
echo Repository: %CD%
echo.
echo Removes rebuildable artifacts only (no source, no models):
echo   node_modules
echo   ui\node_modules
echo   dist
echo   ui\dist
echo   src-tauri\target
echo   src-tauri\gen
echo   *.log under src, ui\src, src-tauri
echo.
echo Does NOT touch: Windows system files, Recycle Bin, or your GGUF models.
echo Close running "tauri dev" / Node processes first if removal fails.
echo.

if "%AUTO%"=="0" (
  choice /C YN /N /M "Continue? (Y/N): "
  if errorlevel 2 (
    echo Cancelled.
    exit /b 0
  )
)

echo.

call :remove_dir "node_modules"
call :remove_dir "ui\node_modules"
call :remove_dir "dist"
call :remove_dir "ui\dist"
call :remove_dir "src-tauri\target"
call :remove_dir "src-tauri\gen"

echo.
echo Removing log files...
for %%R in ("src" "ui\src" "src-tauri") do (
  if exist %%~R (
    for /r %%~R %%F in (*.log) do del /f /q "%%F" 2>nul
  )
)
for %%F in (*.log) do del /f /q "%%F" 2>nul

echo.
if "%ERR%"=="1" (
  echo ============================================
  echo   Cleanup finished with warnings
  echo ============================================
  echo Some folders could not be removed. Stop dev servers and retry.
) else (
  echo ============================================
  echo   Cleanup complete
  echo ============================================
)
echo.
echo Reinstall:  npm install
echo              npm --prefix ui install
echo Rebuild:    npm run tauri:build
echo.
exit /b !ERR!

:remove_dir
set "TARGET=%~1"
if not exist "%TARGET%" (
  echo [skip] %TARGET%
  exit /b 0
)
echo [remove] %TARGET% ...
rd /s /q "%TARGET%" 2>nul
if exist "%TARGET%" (
  echo   [warn] still present - files may be in use
  set "ERR=1"
) else (
  echo   [ok]
)
exit /b 0
