$ErrorActionPreference = "Continue"
$env:Path = "C:\Users\durga\.cargo\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;C:\Users\durga\AppData\Local\LLVM-portable\bin;" + $env:Path
$env:LIBCLANG_PATH = "C:\Users\durga\AppData\Local\LLVM-portable\bin"
Set-Location "D:\ai\src-tauri"
cargo check *> "D:\ai\src-tauri\build.log" 2>&1
"BUILD_EXIT=$LASTEXITCODE" >> "D:\ai\src-tauri\build.log"
