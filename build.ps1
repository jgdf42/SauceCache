param([switch]$Check, [switch]$Test, [switch]$Release, [switch]$Lint)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$env:PATH = "$cargoBin;$env:PATH"
$toolchain = @()
$tools = Join-Path $PSScriptRoot '.tools'
if (Test-Path -LiteralPath $tools) {
    $compiler = Get-ChildItem -LiteralPath $tools -Directory | Where-Object { $_.Name -like 'llvm-mingw-*' } | Select-Object -First 1
    if ($compiler) {
        $env:PATH = "$($compiler.FullName)\bin;$env:PATH"
        $env:CC = 'x86_64-w64-mingw32-clang'
        $env:CXX = 'x86_64-w64-mingw32-clang++'
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = 'x86_64-w64-mingw32-clang'
        $toolchain = @('+stable-x86_64-pc-windows-gnullvm')
    }
}
if ($Test) { cargo @toolchain test --locked }
elseif ($Check) { cargo @toolchain check --locked }
elseif ($Lint) { cargo @toolchain clippy --locked -- -D warnings }
elseif ($Release) { cargo @toolchain build --release --locked }
else { cargo @toolchain build --locked }
if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }
