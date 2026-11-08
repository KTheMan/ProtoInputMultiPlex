param(
    [ValidateSet('x64', 'x86', 'both')][string]$Architecture = 'both',
    [string]$Toolset = 'v143',
    [string]$WindowsSdk = '10.0',
    [switch]$PrepareOnly
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (!(Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio Build Tools with Desktop development with C++.' }
$msbuild = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
if (!$msbuild) { throw 'MSBuild was not found.' }
$vsRoot = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -property installationPath
$atl = Get-ChildItem -Path (Join-Path $vsRoot 'VC\Tools\MSVC\*\atlmfc\include\atlbase.h') -ErrorAction SilentlyContinue
if (!$atl) {
    $atl = Get-ChildItem -Path (Join-Path $repoRoot '.native-tools\atl-Headers\Contents\VC\Tools\MSVC\*\atlmfc\include\atlbase.h') -ErrorAction SilentlyContinue
    if (!$atl) { throw 'Run scripts/prepare-atl.ps1 or install the C++ ATL component matching your toolset.' }
}
$solution = Join-Path $repoRoot 'vendor\ProtoInput\src\ProtoInput\ProtoInput.sln'
if (!(Test-Path -LiteralPath $solution)) { throw 'Run git submodule update --init --recursive first.' }
$vendor = Join-Path $repoRoot 'vendor\ProtoInput'
$lifecyclePatch = Join-Path $repoRoot 'patches\lifecycle\0004-clean-restart.patch'
$keyboardPatch = Join-Path $repoRoot 'patches\keyboard-state\0005-consumed-keyboard-state.patch'
$setupAckPatch = Join-Path $repoRoot 'patches\lifecycle\0006-checked-message-filter-setup.patch'
$stdioPatch = Join-Path $repoRoot 'patches\stdio\0007-preserve-target-stdio.patch'
# Overlays are relative to their predecessor. Remove in reverse order before
# checking the frozen base, then restore in order even if base verification fails.
$overlays = @($lifecyclePatch, $keyboardPatch, $setupAckPatch, $stdioPatch)
$removedOverlays = @()
$baseVerified = $false
try {
    for ($overlayIndex = $overlays.Count - 1; $overlayIndex -ge 0; $overlayIndex--) {
        $overlay = $overlays[$overlayIndex]
        if (Test-Path -LiteralPath $overlay) {
            & git -C $vendor apply --ignore-space-change --reverse --check $overlay 2>$null
            if ($LASTEXITCODE -eq 0) {
                & git -C $vendor apply --ignore-space-change --reverse --whitespace=nowarn $overlay
                if ($LASTEXITCODE -ne 0) { throw "Could not verify compatibility overlay: $overlay" }
                $removedOverlays += $overlay
            }
        }
    }
    foreach ($patch in Get-ChildItem -LiteralPath (Join-Path $repoRoot 'patches') -Filter '*.patch') {
        & git -C $vendor apply --ignore-space-change --reverse --check $patch.FullName 2>$null
        if ($LASTEXITCODE -ne 0) {
            & git -C $vendor apply --ignore-space-change --check $patch.FullName
            if ($LASTEXITCODE -ne 0) { throw "Patch conflict: $($patch.Name)" }
            & git -C $vendor apply --ignore-space-change $patch.FullName
            if ($LASTEXITCODE -ne 0) { throw "Could not apply $($patch.Name)" }
        }
    }
    $baseVerified = $true
}
finally {
    foreach ($overlay in $overlays) {
        if ((Test-Path -LiteralPath $overlay) -and ($baseVerified -or $removedOverlays -contains $overlay)) {
            & git -C $vendor apply --ignore-space-change --check $overlay
            if ($LASTEXITCODE -ne 0) { throw "Compatibility overlay conflict; source was preserved: $overlay" }
            & git -C $vendor apply --ignore-space-change --whitespace=nowarn $overlay
            if ($LASTEXITCODE -ne 0) { throw "Could not apply compatibility overlay: $overlay" }
        }
    }
}
if ($PrepareOnly) { return }
$platforms = if ($Architecture -eq 'both') { @('x64', 'x86') } else { @($Architecture) }
foreach ($platform in $platforms) {
    $atlInclude = Split-Path -Parent ($atl | Select-Object -First 1 -ExpandProperty FullName)
    $atlLib = Join-Path (Split-Path -Parent $atlInclude) "lib\$platform"
    if (!(Test-Path -LiteralPath $atlLib)) {
        $atlLib = Get-ChildItem -Path (Join-Path $repoRoot ".native-tools\atl-$platform\Contents\VC\Tools\MSVC\*\atlmfc\lib\$platform") | Select-Object -First 1 -ExpandProperty FullName
    }
    # Upstream InjectorProxy has no solution dependency on the EasyHook static library it links.
    foreach ($target in @('EasyHookDll', 'BlackBone', 'ProtoInputLoader')) {
        & $msbuild $solution /m:2 "/t:$target" /p:Configuration=Release "/p:Platform=$platform" "/p:PlatformToolset=$Toolset" "/p:WindowsTargetPlatformVersion=$WindowsSdk" "/p:ForceImportBeforeCppTargets=$PSScriptRoot\native-compat.targets" "/p:MuxAtlInclude=$atlInclude" "/p:MuxAtlLib=$atlLib" /verbosity:minimal
        if ($LASTEXITCODE -ne 0) { throw "ProtoInput $platform $target build failed. See MSBuild diagnostics above." }
    }
}
$output = Join-Path $repoRoot 'vendor\ProtoInput\src\ProtoInput\Release'
$runtime = Join-Path $repoRoot 'runtime'
New-Item -ItemType Directory -Path $runtime -Force | Out-Null
Get-ChildItem -LiteralPath $output -File | Where-Object { $_.Extension -in '.dll', '.exe' } | Copy-Item -Destination $runtime
Copy-Item -LiteralPath (Join-Path $repoRoot 'vendor\ProtoInput\LICENSE') -Destination (Join-Path $runtime 'ProtoInput-LICENSE.txt')
Write-Host "Backend runtime staged in $runtime"
