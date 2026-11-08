param(
    [Parameter(Mandatory)][string]$CargoTargetDir,
    [Parameter(Mandatory)][string]$OutputDir,
    [Parameter(Mandatory)][string]$WasmBindgenArchive
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
$sdkBuild = Join-Path $target 'sdk-release-build'
if (Test-Path -LiteralPath $sdkBuild) { throw 'SDK build directory must be fresh; choose a fresh CargoTargetDir.' }
if (Test-Path -LiteralPath (Join-Path $target 'sdk-release')) { throw 'SDK release staging directory must be fresh.' }
$archive = (Resolve-Path -LiteralPath $WasmBindgenArchive).Path
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -cne '79e348c169d0c10c0b287647f1e753d0985353ca96800bc29be8f4261baf13e6') {
    throw 'Expected official wasm-bindgen 0.2.129 x86_64-pc-windows-msvc archive.'
}
$oldTarget = $env:CARGO_TARGET_DIR
$oldWrapper = $env:RUSTC_WRAPPER
$oldFlags = $env:RUSTFLAGS
Push-Location $repo
try {
    $env:CARGO_TARGET_DIR = $target
    # A local sccache service is not a distribution prerequisite.
    $env:RUSTC_WRAPPER = ''
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    # Notice inventory covers the entire workspace on every SDK platform,
    # including dependencies of packages not compiled for that platform. Warm
    # the exact final metadata graphs online before source freeze/build work.
    foreach ($triple in @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc', 'wasm32-unknown-unknown')) {
        & cargo --config 'build.rustc-wrapper=""' metadata --locked --features mux-application-sdk/native-client --format-version 1 --filter-platform $triple | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Release metadata prefetch failed: $triple" }
    }
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
    # Isolated fresh target directories make Cargo JSON identify one exact header
    # from the same full-default-feature release invocation as each DLL.
    New-Item -ItemType Directory -Path $sdkBuild | Out-Null
    foreach ($architecture in @(
        @{ Name = 'x64'; Triple = 'x86_64-pc-windows-msvc' },
        @{ Name = 'x86'; Triple = 'i686-pc-windows-msvc' }
    )) {
        $buildDir = Join-Path $sdkBuild $architecture.Name
        $log = Join-Path $sdkBuild ($architecture.Name + '.jsonl')
        & cargo --config 'build.rustc-wrapper=""' build --release --locked --target $architecture.Triple --target-dir $buildDir -p mux-application-sdk --lib --features native-client --message-format=json-render-diagnostics | Out-File -LiteralPath $log -Encoding utf8
        if ($LASTEXITCODE -ne 0) { throw "SDK release build failed: $($architecture.Name)" }
        & python (Join-Path $PSScriptRoot 'package-portable.py') --record-sdk-native $log --sdk-target $architecture.Triple --target-dir $target --build-inputs $inputs
        if ($LASTEXITCODE -ne 0) { throw 'SDK artifact recording failed.' }
    }
    $toolRoot = Join-Path $sdkBuild 'wasm-bindgen-tool'
    New-Item -ItemType Directory -Path $toolRoot | Out-Null
    $frozenArchive = Join-Path $toolRoot 'upstream.tar.gz'
    Copy-Item -LiteralPath $archive -Destination $frozenArchive
    if ((Get-FileHash -LiteralPath $frozenArchive -Algorithm SHA256).Hash.ToLowerInvariant() -cne '79e348c169d0c10c0b287647f1e753d0985353ca96800bc29be8f4261baf13e6') { throw 'Binding archive changed while copying.' }
    & tar -xzf $frozenArchive -C $toolRoot
    if ($LASTEXITCODE -ne 0) { throw 'Binding tool extraction failed.' }
    $bindgen = Join-Path $toolRoot 'wasm-bindgen-0.2.129-x86_64-pc-windows-msvc/wasm-bindgen.exe'
    $version = & $bindgen --version
    if ($LASTEXITCODE -ne 0 -or $version -cne 'wasm-bindgen 0.2.129') { throw 'Binding tool version mismatch.' }
    $env:RUSTFLAGS = '' # Windows static CRT flags do not apply to wasm.
    & cargo --config 'build.rustc-wrapper=""' build --release --locked --target wasm32-unknown-unknown --target-dir (Join-Path $sdkBuild 'web') -p mux-application-web
    if ($LASTEXITCODE -ne 0) { throw 'Browser SDK release build failed.' }
    $web = Join-Path $target 'sdk-release/web'
    New-Item -ItemType Directory -Path $web | Out-Null
    & $bindgen --target web --out-dir (Join-Path $web 'pkg') --out-name mux_application_web (Join-Path $sdkBuild 'web/wasm32-unknown-unknown/release/mux_application_web.wasm')
    if ($LASTEXITCODE -ne 0) { throw 'Browser binding generation failed.' }
    Copy-Item -LiteralPath (Join-Path $repo 'crates/mux-application-web/example/index.html'),(Join-Path $repo 'crates/mux-application-web/example/bootstrap.js') -Destination $web
    & python (Join-Path $PSScriptRoot 'package-portable.py') --record-sdk-web --target-dir $target --build-inputs $inputs
    if ($LASTEXITCODE -ne 0) { throw 'Browser SDK recording failed.' }
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
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
