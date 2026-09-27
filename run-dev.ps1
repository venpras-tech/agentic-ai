$env:Path = "C:\Users\durga\.cargo\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;C:\Users\durga\AppData\Local\LLVM-portable\bin;" + $env:Path
$env:LIBCLANG_PATH = "C:\Users\durga\AppData\Local\LLVM-portable\bin"
Set-Location "D:\ai"
$log = "D:\ai\run-dev.log"
if (Test-Path $log) { Remove-Item $log -Force }
$proc = Start-Process -FilePath "npm.cmd" -ArgumentList "run", "tauri:dev" -WorkingDirectory "D:\ai" -RedirectStandardOutput $log -RedirectStandardError "D:\ai\run-dev.err.log" -PassThru -NoNewWindow
$proc.Id | Set-Content "D:\ai\run-dev.pid"
"STARTED_PID=$($proc.Id)"
