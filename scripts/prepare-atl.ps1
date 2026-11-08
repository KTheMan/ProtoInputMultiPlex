param([string]$CatalogPath)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (!$CatalogPath) {
    $CatalogPath = Get-ChildItem 'C:\ProgramData\Microsoft\VisualStudio\Packages\_Instances\*\catalog.json' | Select-Object -First 1 -ExpandProperty FullName
}
if (!$CatalogPath) { throw 'No installed Visual Studio catalog found. Install C++ ATL with Visual Studio Installer.' }
$catalog = Get-Content -Raw -LiteralPath $CatalogPath | ConvertFrom-Json
$cache = Join-Path $repoRoot '.native-tools'
New-Item -ItemType Directory -Path $cache -Force | Out-Null
foreach ($part in @('Headers', 'X64', 'X86')) {
    $package = $catalog.packages | Where-Object { $_.id -eq "Microsoft.VC.14.44.17.14.ATL.$part.base" } | Select-Object -First 1
    if (!$package) { throw "ATL $part package missing from catalog. This fallback targets MSVC 14.44; install ATL for other toolsets normally." }
    $payload = $package.payloads[0]
    if (([uri]$payload.url).Host -ne 'download.visualstudio.microsoft.com') { throw 'Unexpected ATL download host' }
    $archive = Join-Path $cache $payload.fileName
    if (!(Test-Path -LiteralPath $archive)) { Invoke-WebRequest -Uri $payload.url -OutFile $archive }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $payload.sha256) { throw "Checksum mismatch: $archive" }
    $destination = Join-Path $cache "atl-$part"
    if (!(Test-Path -LiteralPath $destination)) {
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $destination)
    }
    Write-Host "Verified and extracted $($package.id)"
}
