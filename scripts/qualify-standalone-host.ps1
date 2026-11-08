param(
    [Parameter(Mandatory = $true)][string]$ArtifactsRoot,
    [Parameter(Mandatory = $true)][string]$EvidenceRoot,
    [Parameter(Mandatory = $true)][string]$SourceCommit
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_OS -cne 'Windows') { throw 'Disposable GitHub-hosted Windows runner required' }
if ($SourceCommit -cnotmatch '^[0-9a-f]{40}$' -or $SourceCommit -cne $env:GITHUB_SHA) { throw 'Workflow source identity mismatch' }
if (-not [IO.Path]::IsPathFullyQualified($ArtifactsRoot) -or -not [IO.Path]::IsPathFullyQualified($EvidenceRoot)) { throw 'Absolute owned paths required' }
$runnerRoot = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
foreach ($candidate in @($ArtifactsRoot, $EvidenceRoot)) {
    if (-not [IO.Path]::GetFullPath($candidate).StartsWith($runnerRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Qualification paths must remain under runner TEMP' }
}
if (Test-Path -LiteralPath $EvidenceRoot) { throw 'Evidence directory already exists; never replay a specimen' }
New-Item -ItemType Directory -Path $EvidenceRoot | Out-Null
function Assert-Fields($Object, [string[]]$Keys) {
    if ($Object -isnot [pscustomobject]) { throw 'Expected JSON object' }
    $actual = @($Object.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    $expectedKeys = @($Keys | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $expectedKeys.Count -or ($actual -join ',') -cne ($expectedKeys -join ',')) { throw 'Unexpected JSON fields' }
}
function Assert-Detail($Object, [System.Collections.IDictionary]$Expected) {
    Assert-Fields $Object @($Expected.Keys)
    foreach ($key in $Expected.Keys) {
        $actual = $Object.$key
        $value = $Expected[$key]
        if ($value -is [bool]) {
            if ($actual -isnot [bool] -or $actual -ne $value) { throw "Boolean detail mismatch: $key" }
        } elseif ($value -is [int]) {
            if (($actual -isnot [int] -and $actual -isnot [long]) -or $actual -ne $value) { throw "Integer detail mismatch: $key" }
        } elseif ($actual -isnot [string] -or $actual -cne $value) { throw "String detail mismatch: $key" }
    }
}
function Normalize-DllPath([string]$Value) {
    if ($Value.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) { $Value = '\\' + $Value.Substring(8) }
    elseif ($Value.StartsWith('\\?\', [StringComparison]::Ordinal)) { $Value = $Value.Substring(4) }
    if (-not [IO.Path]::IsPathFullyQualified($Value)) { throw 'Reported DLL path must be absolute' }
    return [IO.Path]::GetFullPath($Value)
}
$expected = @('started', 'dll_hash_verified', 'base_exports_resolved', 'host_created',
    'trusted_setup_retained_reply_checked', 'host_destroyed_retained_reply_retired',
    'dll_unloaded_owner_still_alive', 'post_unload_barrier_acknowledged', 'owner_ready_to_exit_after_unload')
foreach ($architecture in @(
    @{ Name = 'x64'; Triple = 'x86_64-pc-windows-msvc' },
    @{ Name = 'x86'; Triple = 'i686-pc-windows-msvc' }
)) {
    $name = $architecture.Name
    $controller = [ordered]@{ status = 'FAIL'; reason = 'validation incomplete'; source_commit = $SourceCommit;
        architecture = $name; deadline_ms = 120000; exit_code = $null; timed_out = $false;
        scope = 'standalone Host C ABI DLL lifetime only'; standalone_host_unload_qualified = $false }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        $root = Join-Path $ArtifactsRoot ('sdk-preview/' + $name)
        $metadata = Get-Content -LiteralPath (Join-Path $root 'metadata.json') -Raw | ConvertFrom-Json
        if ($metadata.source_commit -cne $SourceCommit -or $metadata.target -cne $architecture.Triple -or
            $metadata.profile -cne 'debug' -or $metadata.default_features -ne $true -or
            $metadata.isolated_sdk_build -ne $true -or $metadata.header_compile_and_native_exports_checked -ne $true -or
            (@($metadata.features) -join ',') -cne 'native-ffi,native-windows,native-client') { throw 'Metadata identity mismatch' }
        foreach ($entry in @(
            @{ Value = $metadata.dll; File = 'mux_application_sdk.dll' },
            @{ Value = $metadata.header; File = 'protoinputmultiplex_application_sdk.h' },
            @{ Value = $metadata.standalone_host_fixture; File = 'standalone_host_dll_lifecycle.exe' }
        )) {
            if ($entry.Value.file -cne $entry.File -or $entry.Value.sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid artifact metadata' }
            $actual = (Get-FileHash -LiteralPath (Join-Path $root $entry.File) -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -cne $entry.Value.sha256) { throw "Artifact digest mismatch: $($entry.File)" }
        }
        $controller.artifacts = $metadata
        $exe = Join-Path $root 'standalone_host_dll_lifecycle.exe'
        $dll = Join-Path $root 'mux_application_sdk.dll'
        $report = Join-Path $EvidenceRoot ($name + '-standalone-host.jsonl')
        $arguments = '--dll "{0}" --dll-sha256 {1} --report "{2}"' -f $dll, $metadata.dll.sha256, $report
        $process = Start-Process -FilePath $exe -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $EvidenceRoot ($name + '-stdout.log')) -RedirectStandardError (Join-Path $EvidenceRoot ($name + '-stderr.log'))
        $controller.pid = $process.Id
        if (-not $process.WaitForExit(120000)) {
            $controller.timed_out = $true
            throw 'External deadline exceeded; preserve original specimen without kill, retry or unload claim'
        }
        $process.Refresh()
        $controller.exit_code = $process.ExitCode
        if ($process.ExitCode -ne 0) { throw 'Fixture did not exit normally with code zero' }
        if ((Get-Item -LiteralPath $report).Length -gt 1048576) { throw 'Fixture report exceeds one MiB bound' }
        $records = @(Get-Content -LiteralPath $report | ForEach-Object { $_ | ConvertFrom-Json })
        if ($records.Count -ne 18) { throw 'Missing or extra standalone Host milestones' }
        for ($cycle = 1; $cycle -le 2; $cycle++) {
            $offset = ($cycle - 1) * 9
            $details = @(
                @{ cycles = 2; expected_sha256 = $metadata.dll.sha256; previous_owner_os_exit_and_join_confirmed = ($cycle -eq 2); runtime_claim = 'standalone Host DLL lifetime prerequisite only' },
                @{ path = ''; sha256 = $metadata.dll.sha256 },
                @{ base_exports = 8; calling_convention = 'C'; base_abi_version = 1 },
                @{ export = 'pim_app_host_create'; workers = 0 },
                @{ short_copy_retained = $true; pending_call_busy = $true; bound_identity_from_host_reply = $true },
                @{ destroy_status = 0; stale_handle_status = 2; client_call_exercised = $true; response_discard_exercised = $true; virtual_users_created = 0 },
                @{ live_host_handles = 0; outstanding_sdk_calls = 0; future_sdk_entry_fenced = $true },
                @{ parent_observed_owner_os_thread_alive = $true },
                @{ result = 'REQUIRES_PARENT_OS_WAIT_JOIN_AND_EXTERNAL_EXIT_0'; gui_service_qualified = $false; native_web_same_users_qualified = $false; hardware_qualified = $false; full_sdk_complete = $false }
            )
            for ($i = 0; $i -lt 9; $i++) {
                $row = $records[$offset + $i]
                Assert-Fields $row @('schema', 'cycle', 'sequence', 'stage', 'detail')
                if ($row.schema -isnot [string] -or $row.schema -cne 'pim.standalone_host_dll_lifecycle.v1' -or
                    ($row.cycle -isnot [int] -and $row.cycle -isnot [long]) -or $row.cycle -ne $cycle -or
                    ($row.sequence -isnot [int] -and $row.sequence -isnot [long]) -or $row.sequence -ne ($i + 1) -or
                    $row.stage -isnot [string] -or $row.stage -cne $expected[$i]) { throw 'Milestone schema/order mismatch' }
                if ($i -eq 1) {
                    Assert-Fields $row.detail @('path', 'sha256')
                    if ($row.detail.path -isnot [string] -or
                        -not [string]::Equals((Normalize-DllPath $row.detail.path), (Normalize-DllPath $dll), [StringComparison]::OrdinalIgnoreCase)) { throw 'Reported DLL path mismatch' }
                    $details[$i].path = $row.detail.path
                }
                Assert-Detail $row.detail $details[$i]
            }
        }
        $controller.status = 'PASS'
        $controller.reason = 'Two actual base ABI cycles, parent OS-thread wait/join and external exit zero observed'
        $controller.standalone_host_unload_qualified = $true
    } catch {
        $controller.reason = $_.Exception.Message
        throw
    } finally {
        $controller.elapsed_ms = $timer.ElapsedMilliseconds
        $controller | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $EvidenceRoot ($name + '-controller.json')) -Encoding utf8
    }
}
