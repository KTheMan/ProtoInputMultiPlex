param(
    [Parameter(Mandatory)][string]$BundleDir,
    [Parameter(Mandatory)][string]$OutputFile,
    [string]$MakeNsis = 'C:\Program Files (x86)\NSIS\Bin\makensis.exe',
    [ValidatePattern('^[A-Za-z0-9_-]{1,64}$')][string]$ShellTestId
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$bundle = (Resolve-Path -LiteralPath $BundleDir).Path
$output = [IO.Path]::GetFullPath($OutputFile)
if (Test-Path -LiteralPath $output) { throw 'OutputFile must be fresh.' }
if ($output.StartsWith($bundle.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Keep the installer output outside the immutable portable bundle.' }
if (!(Test-Path -LiteralPath (Join-Path $bundle 'bundle-manifest.json'))) { throw 'Stage the complete portable distribution first.' }
if (!(Test-Path -LiteralPath $MakeNsis)) { throw 'Install NSIS 3 or pass -MakeNsis.' }
$manifest = Get-Content -LiteralPath (Join-Path $bundle 'bundle-manifest.json') -Raw | ConvertFrom-Json
if ($manifest.version -notmatch '^\d+\.\d+\.\d+([-.][A-Za-z0-9.-]+)?$') { throw 'Invalid manifest application version.' }
$numericParts = (($manifest.version -split '[-+]')[0] -split '\.')
foreach ($part in $numericParts) {
    if ([uint64]$part -gt [uint16]::MaxValue) { throw 'Application version exceeds the Windows version field range.' }
}
$numericVersion = ($numericParts + @('0')) -join '.'
$parent = Split-Path -Parent $output
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$nsisArgs = @('/V2', "/DBUNDLE_DIR=$bundle", "/DOUTPUT_FILE=$output", "/DAPP_VERSION=$($manifest.version)", "/DAPP_VERSION_NUMERIC=$numericVersion", "/DSETUP_FILENAME=$(Split-Path -Leaf $output)")
if ($ShellTestId) {
    $nsisArgs += "/DSHELL_REG_KEY=Software\ProtoInputMultiPlex\Tests\Installer-$ShellTestId"
    $nsisArgs += '/DSHELL_MENU_ROOT=$INSTDIR\test-shell-menu'
}
& $MakeNsis @nsisArgs (Join-Path $PSScriptRoot '..\packaging\installer.nsi')
if ($LASTEXITCODE -ne 0) { throw 'NSIS installer build failed.' }
Get-FileHash -LiteralPath $output -Algorithm SHA256
