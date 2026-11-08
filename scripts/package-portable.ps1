param(
    [Parameter(Mandatory)][string]$CargoTargetDir,
    [Parameter(Mandatory)][string]$OutputDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent $PSScriptRoot
$target = [IO.Path]::GetFullPath($CargoTargetDir)
$output = [IO.Path]::GetFullPath($OutputDir)
$sharedTarget = [IO.Path]::GetFullPath('E:\.cargo\target').TrimEnd('\')
if ($target.TrimEnd('\') -ieq $sharedTarget -or $target.StartsWith($sharedTarget + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Choose an isolated CargoTargetDir, never E:\.cargo\target.'
}
if (Test-Path -LiteralPath $output) { throw 'OutputDir must not exist; choose a fresh distribution directory.' }
$oldTarget = $env:CARGO_TARGET_DIR
$oldWrapper = $env:RUSTC_WRAPPER
$oldFlags = $env:RUSTFLAGS
Push-Location $repo
try {
    $env:CARGO_TARGET_DIR = $target
    # A local sccache service is not a distribution prerequisite.
    $env:RUSTC_WRAPPER = ''
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    # Prepare existing compatibility patches before freezing compilation inputs.
    & (Join-Path $PSScriptRoot 'build-backend.ps1') -PrepareOnly
    if (!$?) { throw 'ProtoInput source preparation failed.' }
    $inputs = Join-Path $target 'portable-build-inputs.json'
    & python (Join-Path $PSScriptRoot 'package-portable.py') --record-inputs $inputs
    if ($LASTEXITCODE -ne 0) { throw 'Could not record frozen source inputs.' }
    & cargo --config 'build.rustc-wrapper=""' build --release --locked --target x86_64-pc-windows-msvc -p protoinputmultiplex -p cursor-observer
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
    & cargo --config 'build.rustc-wrapper=""' build --release --locked --target i686-pc-windows-msvc -p cursor-observer
    if ($LASTEXITCODE -ne 0) { throw '32-bit cursor observer build failed.' }
    & (Join-Path $PSScriptRoot 'build-backend.ps1') -Architecture both
    if (!$?) { throw 'ProtoInput release build failed.' }
    & python (Join-Path $PSScriptRoot 'package-portable.py') --target-dir $target --output $output --build-inputs $inputs
    if ($LASTEXITCODE -ne 0) { throw 'Portable staging failed; inspect the incomplete output before retrying with a fresh path.' }
} finally {
    Pop-Location
    $env:CARGO_TARGET_DIR = $oldTarget
    $env:RUSTC_WRAPPER = $oldWrapper
    $env:RUSTFLAGS = $oldFlags
}
