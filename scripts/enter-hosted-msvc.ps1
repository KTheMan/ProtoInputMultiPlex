param(
    [Parameter(Mandatory)]
    [ValidateSet('amd64', 'x86')]
    [string]$Architecture
)

$ErrorActionPreference = 'Stop'
if ($env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or !$env:GITHUB_ENV) {
    throw 'Hosted Actions runner environment required.'
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or !$vsRoot) {
    throw 'Visual Studio C++ build tools were not found.'
}

$before = @{}
Get-ChildItem Env: | ForEach-Object { $before[$_.Name] = $_.Value }
$devShell = Join-Path $vsRoot 'Common7/Tools/Launch-VsDevShell.ps1'
& $devShell -Arch $Architecture -HostArch amd64 -SkipAutomaticLocation
$target = if ($Architecture -eq 'amd64') { 'x64' } else { 'x86' }
$compiler = (Get-Command cl.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
if (!$compiler.EndsWith("\bin\Hostx64\$target\cl.exe", [StringComparison]::OrdinalIgnoreCase)) {
    throw "Visual Studio did not select the expected $target compiler."
}

$after = @{}
Get-ChildItem Env: | ForEach-Object { $after[$_.Name] = $_.Value }
$names = @($before.Keys) + @($after.Keys) | Sort-Object -Unique
foreach ($name in $names) {
    if ($before[$name] -ceq $after[$name]) { continue }
    if ($name -match '^(GITHUB_|RUNNER_|NODE_OPTIONS$)' -or $name -notmatch '^[A-Za-z_][A-Za-z0-9_()]*$' -or $after[$name] -match '[\r\n]') {
        throw 'Visual Studio changed an unsupported environment variable.'
    }
    "$name=$($after[$name])" >> $env:GITHUB_ENV
}
