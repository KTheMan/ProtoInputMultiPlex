param(
    [ValidateSet('x64', 'x86', 'both')][string]$Architecture = 'both',
    [Parameter(Mandatory = $true)][string]$VsDevCmd,
    [string]$OutputDirectory = (Join-Path $env:TEMP 'protoinputmux-port-b-fixture')
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$source = Join-Path $PSScriptRoot 'native\setup-ack.cpp'
$golden = ([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot 'native\setup-ack-golden.txt')) -replace "`r`n", "`n").Trim()
$devcmd = [System.IO.Path]::GetFullPath($VsDevCmd)
if (!(Test-Path -LiteralPath $devcmd -PathType Leaf)) { throw 'MSVC environment wrapper is missing.' }
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
$platforms = if ($Architecture -eq 'both') { @('x64', 'x86') } else { @($Architecture) }
foreach ($platform in $platforms) {
    $executable = Join-Path $outputRoot "mux-config-contract-$platform.exe"
    $object = Join-Path $outputRoot "mux-config-contract-$platform.obj"
    $command = Join-Path $outputRoot "build-config-contract-$platform.cmd"
    $commands = @"
@echo off
call "$devcmd" -no_logo -arch=$platform -host_arch=x64 >nul
if errorlevel 1 exit /b %errorlevel%
cl.exe /nologo /std:c++17 /EHsc /W4 /WX "$source" /Fo"$object" /Fe"$executable"
exit /b %errorlevel%
"@
    [System.IO.File]::WriteAllText($command, $commands, [System.Text.Encoding]::Default)
    & $env:ComSpec /d /c $command
    if ($LASTEXITCODE -ne 0) { throw "MSVC $platform setup ACK fixture build failed." }
    $image = [System.IO.File]::ReadAllBytes($executable)
    if ($image.Length -lt 64 -or [BitConverter]::ToUInt16($image, 0) -ne 0x5a4d) { throw 'Fixture is not a PE executable.' }
    $pe = [BitConverter]::ToUInt32($image, 0x3c)
    if ($pe -gt $image.Length - 6 -or [BitConverter]::ToUInt32($image, $pe) -ne 0x00004550) { throw 'Fixture PE header is invalid.' }
    $expectedMachine = if ($platform -eq 'x64') { 0x8664 } else { 0x014c }
    if ([BitConverter]::ToUInt16($image, $pe + 4) -ne $expectedMachine) { throw "Fixture does not match $platform." }
    $info = [System.Diagnostics.ProcessStartInfo]::new($executable)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::Start($info)
    if (!$process.WaitForExit(10000)) { $process.Kill(); $process.WaitForExit(); throw 'Owned native fixture timed out.' }
    $actual = ($process.StandardOutput.ReadToEnd() -replace "`r`n", "`n").Trim()
    $diagnostics = $process.StandardError.ReadToEnd()
    if ($process.ExitCode -ne 0 -or $actual -cne $golden) { throw "Native $platform contract failed: $diagnostics`n$actual" }
    Write-Host "MSVC $platform checked-setup bytes/status/duplicate fixtures passed; PE architecture verified."
}
