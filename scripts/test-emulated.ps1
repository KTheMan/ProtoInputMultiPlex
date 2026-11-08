param(
    [Parameter(Mandatory)][string]$Executable,
    [string]$SecondExecutable = $Executable
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$run = Join-Path $root ('runtime/emulated-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $run -Force | Out-Null
$children = @()
try {
    foreach ($entry in @(@($Executable, 'a'), @($SecondExecutable, 'b'))) {
        $children += Start-Process -FilePath $entry[0] -ArgumentList @('probe', '--report', ('"' + (Join-Path $run ($entry[1]+'.json')) + '"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
    }
    $profile = @{version=1; seats=@(
        @{name='Virtual Blue';mouse='virtual://mouse/1';keyboard='virtual://keyboard/1';target_pid=$children[0].Id},
        @{name='Virtual Coral';mouse='virtual://mouse/2';keyboard='virtual://keyboard/2';target_pid=$children[1].Id}
    )}
    $profile | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $run 'profile.json') -Encoding utf8
    $steps = @(
        @{button='Start routing';wait_ms=3000},
        @{button='1 Right';wait_ms=15000},
        @{button='1 Tap W';wait_ms=3500},
        @{button='2 Down';wait_ms=3500},
        @{button='2 Tap D';wait_ms=3500},
        @{button='Pause';wait_ms=3500},
        @{button='1 Right';wait_ms=3500},
        @{button='2 Tap W';wait_ms=3500},
        @{button='Resume';wait_ms=3500},
        @{button='1 Left';wait_ms=3500},
        @{button='Stop routing';wait_ms=3500}
    )
    $steps | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'ui.json') -Encoding utf8
    $children += Start-Process -FilePath $Executable -ArgumentList @('gui','--profile',('"'+(Join-Path $run 'profile.json')+'"'),'--ui-script',('"'+(Join-Path $run 'ui.json')+'"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Write-Output "Emulated UI validation: $run; processes $($children.Id -join ', ')"
    $deadline = [DateTime]::UtcNow.AddSeconds(100)
    $lastCount = -1
    $changedAt = [DateTime]::UtcNow
    $samples = @{}
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $log = Join-Path $run 'ui.events.jsonl'
        $count = if (Test-Path -LiteralPath $log) { @(Get-Content -LiteralPath $log).Count } else { 0 }
        if ($count -ne $lastCount) { $lastCount = $count; $changedAt = [DateTime]::UtcNow }
        $settleSeconds = if ($count -eq 1) { 12 } else { 2 }
        if ($count -gt 0 -and -not $samples.ContainsKey($count) -and ([DateTime]::UtcNow - $changedAt).TotalSeconds -gt $settleSeconds) {
            try {
                $a = Get-Content -Raw -LiteralPath (Join-Path $run 'a.json') | ConvertFrom-Json
                $b = Get-Content -Raw -LiteralPath (Join-Path $run 'b.json') | ConvertFrom-Json
                if ($null -eq $a -or $null -eq $b -or $a.pid -ne $children[0].Id -or $b.pid -ne $children[1].Id) { continue }
                $samples[$count] = @{a=$a;b=$b;button=$steps[$count-1].button}
                Write-Output "Step $count $($steps[$count-1].button): A=$($a.cursor -join ',') keys=$($a.key_presses); B=$($b.cursor -join ',') keys=$($b.key_presses)"
            } catch { continue }
        }
        if ($samples.ContainsKey($steps.Count)) { break }
    }
    $serializable = @{}
    foreach ($key in $samples.Keys) { $serializable[[string]$key] = $samples[$key] }
    $serializable | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'samples.json') -Encoding utf8
    function Assert-Check($condition, $message) { if (-not $condition) { throw $message } }
    Assert-Check ($samples.Count -eq $steps.Count) 'UI script did not complete all controls'
    function Same-Cursor($x,$y) { ($x.cursor -join ',') -eq ($y.cursor -join ',') }
    Assert-Check ($samples[2].a.cursor[0] -eq $samples[1].a.cursor[0]+40) 'Seat A mouse did not move by 40'
    Assert-Check (Same-Cursor $samples[2].b $samples[1].b) 'Seat A mouse leaked into B'
    Assert-Check ($samples[3].a.key_presses -eq $samples[2].a.key_presses+1) 'Seat A key was not delivered'
    Assert-Check ($samples[3].b.key_presses -eq $samples[2].b.key_presses) 'Seat A key leaked into B'
    Assert-Check ($samples[4].b.cursor[1] -eq $samples[3].b.cursor[1]+40) 'Seat B mouse did not move by 40'
    Assert-Check (Same-Cursor $samples[4].a $samples[3].a) 'Seat B mouse leaked into A'
    Assert-Check ($samples[5].b.key_presses -eq $samples[4].b.key_presses+1) 'Seat B key was not delivered'
    Assert-Check ($samples[5].a.key_presses -eq $samples[4].a.key_presses) 'Seat B key leaked into A'
    Assert-Check (Same-Cursor $samples[7].a $samples[6].a) 'Pause did not freeze mouse input'
    Assert-Check ($samples[8].b.key_presses -eq $samples[6].b.key_presses) 'Pause did not suppress key input'
    Assert-Check ($samples[10].a.cursor[0] -eq $samples[9].a.cursor[0]-40) 'Resume did not restore mouse input'
    Assert-Check ($samples[10].a.test_keys -eq 0 -and $samples[10].b.test_keys -eq 0) 'Test key stuck after release'
    # Desktop motion can occur between the two reports. Both must leave their frozen routed position.
    Assert-Check (-not (Same-Cursor $samples[11].a $samples[10].a) -and -not (Same-Cursor $samples[11].b $samples[10].b)) 'Stop did not restore desktop cursor API'
    Write-Output 'PASS: real native mouse/keyboard isolation, pause, resume and stop through egui controls.'
} finally {
    foreach ($child in $children) {
        if (-not $child.HasExited) { Stop-Process -Id $child.Id -ErrorAction SilentlyContinue }
    }
}
