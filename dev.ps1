$env:Path = "C:\Users\durga\.cargo\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;C:\Users\durga\AppData\Local\LLVM-portable\bin;" + $env:Path
$env:LIBCLANG_PATH = "C:\Users\durga\AppData\Local\LLVM-portable\bin"
Set-Location "D:\ai"
npm run tauri:dev *> "D:\ai\dev.log" 2>&1
"DEV_EXIT=$LASTEXITCODE" >> "D:\ai\dev.log"
