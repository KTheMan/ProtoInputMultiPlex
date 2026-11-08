param(
    [ValidateSet('x64', 'x86', 'both')][string]$Architecture = 'both',
    # Optional MinGW C++ compiler for a smoke check. MSVC is the qualification path.
    [string]$GnuCxx,
    [string]$OutputDirectory,
    # Explicit MSVC environment wrapper, including a portable per-user toolchain.
    [string]$VsDevCmd
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repoRoot 'runtime\wire-abi' }
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
$source = Join-Path $PSScriptRoot 'native\wire-abi.cpp'
$goldenPath = Join-Path $PSScriptRoot 'native\wire-abi-golden.txt'
$expected = ([System.IO.File]::ReadAllText($goldenPath) -replace "`r`n", "`n").TrimEnd()
$platforms = if ($Architecture -eq 'both') { @('x64', 'x86') } else { @($Architecture) }
if (!$GnuCxx) {
    if ($VsDevCmd) {
        $devcmd = [System.IO.Path]::GetFullPath($VsDevCmd)
        if (!(Test-Path -LiteralPath $devcmd -PathType Leaf)) { throw "MSVC environment wrapper not found: $devcmd" }
    }
    else {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (!(Test-Path -LiteralPath $vswhere)) { throw 'MSVC qualification requires Visual Studio C++ Build Tools (vswhere not found).' }
        $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (!$vsRoot) { throw 'Install Visual Studio C++ x86/x64 tools for native ABI qualification.' }
        $devcmd = Join-Path $vsRoot 'Common7\Tools\VsDevCmd.bat'
    }
}
foreach ($platform in $platforms) {
    $executable = Join-Path $outputRoot "wire-abi-$platform.exe"
    if ($GnuCxx) {
        $mode = if ($platform -eq 'x64') { '-m64' } else { '-m32' }
        & $GnuCxx $mode '-std=c++17' '-Wall' '-Wextra' '-Werror' $source '-o' $executable
        if ($LASTEXITCODE -ne 0) { throw "MinGW $platform ABI fixture build failed." }
    }
    else {
        # A dedicated command file initializes the matching compiler environment.
        # No source files or existing native build outputs are changed.
        $object = Join-Path $outputRoot "wire-abi-$platform.obj"
        $commandFile = Join-Path $outputRoot "build-wire-abi-$platform.cmd"
        $commands = @"
@echo off
call "$devcmd" -no_logo -arch=$platform -host_arch=x64 >nul
if errorlevel 1 exit /b %errorlevel%
cl.exe /nologo /std:c++17 /EHsc /W4 /WX "$source" /Fo"$object" /Fe"$executable"
exit /b %errorlevel%
"@
        [System.IO.File]::WriteAllText($commandFile, $commands, [System.Text.Encoding]::Default)
        & $env:ComSpec /d /c $commandFile
        if ($LASTEXITCODE -ne 0) { throw "MSVC $platform ABI fixture build failed." }
    }
    # Verify the produced architecture independently of a supplied compiler or wrapper.
    $image = [System.IO.File]::ReadAllBytes($executable)
    if ($image.Length -lt 64 -or [BitConverter]::ToUInt16($image, 0) -ne 0x5a4d) { throw 'ABI fixture is not a PE executable.' }
    $peOffset = [BitConverter]::ToUInt32($image, 0x3c)
    if ($peOffset -gt $image.Length - 6 -or [BitConverter]::ToUInt32($image, $peOffset) -ne 0x00004550) { throw 'ABI fixture has an invalid PE header.' }
    $machine = [BitConverter]::ToUInt16($image, $peOffset + 4)
    $expectedMachine = if ($platform -eq 'x64') { 0x8664 } else { 0x014c }
    if ($machine -ne $expectedMachine) { throw "ABI fixture machine 0x$($machine.ToString('X4')) does not match $platform." }
    $actual = (& $executable) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Native $platform ABI fixture execution failed." }
    if ($actual.TrimEnd() -cne $expected) { throw "Native $platform bytes differ from the shared Rust golden fixture." }
    $compilerKind = if ($GnuCxx) { 'MinGW smoke' } else { 'MSVC' }
    Write-Host "$compilerKind $platform native layout/bytes match all 22 shared vectors."
}
