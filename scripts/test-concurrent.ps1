param([Parameter(Mandatory)][string]$Executable,[string]$SecondExecutable=$Executable)
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
$run=Join-Path $root ('runtime/concurrent-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $run -Force | Out-Null
# Run a snapshot so later builds cannot overwrite or be blocked by active test images.
$mainImage=Join-Path $run 'control.exe'
$probeImage=Join-Path $run 'second-probe.exe'
Copy-Item -LiteralPath $Executable -Destination $mainImage
Copy-Item -LiteralPath $SecondExecutable -Destination $probeImage
$Executable=$mainImage
$SecondExecutable=$probeImage
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class CursorWindows {
 [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
}
"@
$children=@()
try {
 foreach($entry in @(@($Executable,'a'),@($SecondExecutable,'b'))) {
  $children+=Start-Process -FilePath $entry[0] -ArgumentList @('probe','--report',('"'+(Join-Path $run ($entry[1]+'.json'))+'"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
 }
 # A copied executable can start slowly on a cold Windows cache. Wait for real windows.
 $readyDeadline=[DateTime]::UtcNow.AddSeconds(60)
 while($true) {
  $ready=$true
  foreach($child in $children){$child.Refresh();if($child.HasExited){throw 'Probe exited during startup'};if($child.MainWindowHandle -eq [IntPtr]::Zero){$ready=$false}}
  if($ready){break}
  if([DateTime]::UtcNow -gt $readyDeadline){throw 'Probe window startup timed out'}
  Start-Sleep -Milliseconds 200
 }
 @{version=1;concurrent=$true;seats=@(
  @{name='Virtual Blue';mouse='virtual://mouse/1';keyboard='virtual://keyboard/1';target_pid=$children[0].Id;cursor=@{color=@(96,165,250)}},
  @{name='Virtual Coral';mouse='virtual://mouse/2';keyboard='virtual://keyboard/2';target_pid=$children[1].Id;cursor=@{color=@(248,113,113);swap_buttons=$true;invert_wheel=$true;wheel_scale=2.0;speed_y=1.5}}
 )} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'profile.json') -Encoding utf8
 $steps=@(
  @{button='Start routing';wait_ms=4000},
  @{button='Hold all users';wait_ms=15000},
  @{button='1 Right';wait_ms=3000},
  @{button='2 Down';wait_ms=3000},
  @{button='1 Wheel';wait_ms=3000},
  @{button='2 Wheel';wait_ms=3000},
  @{button='Pause';wait_ms=3000},
  @{button='1 Right';wait_ms=3000},
  @{button='Resume';wait_ms=3000},
  @{button='1 Left';wait_ms=3000},
  @{button='Hold all users';wait_ms=3000},
  @{button='1 Tap W';wait_ms=3000},
  @{button='Release all users';wait_ms=3000},
  @{button='Configure user';wait_ms=3000},
  @{button='Theme';wait_ms=3000},
  @{button='Apply cursor settings';wait_ms=4000},
  @{button='Stop routing';wait_ms=4000}
 )
 $steps | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'ui.json') -Encoding utf8
 $children+=Start-Process -FilePath $Executable -ArgumentList @('gui','--profile',('"'+(Join-Path $run 'profile.json')+'"'),'--ui-script',('"'+(Join-Path $run 'ui.json')+'"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
 Write-Output "Concurrent virtual validation: $run; PIDs $($children.Id -join ', ')"
 $deadline=[DateTime]::UtcNow.AddSeconds(120);$lastCount=-1;$changedAt=[DateTime]::UtcNow;$samples=@{}
 while([DateTime]::UtcNow -lt $deadline) {
  Start-Sleep -Milliseconds 150
  if($children[2].HasExited){throw "Dashboard exited before validation completed: $run"}
  $log=Join-Path $run 'ui.events.jsonl'
  $count=if(Test-Path -LiteralPath $log){@(Get-Content -LiteralPath $log).Count}else{0}
  if($count -ne $lastCount){$lastCount=$count;$changedAt=[DateTime]::UtcNow}
  $settle=if($count -eq 1){12}else{1.5}
  if($count -gt 0 -and -not $samples.ContainsKey($count) -and ([DateTime]::UtcNow-$changedAt).TotalSeconds -gt $settle) {
   try {
    $a=Get-Content -Raw -LiteralPath (Join-Path $run 'a.json') | ConvertFrom-Json
    $b=Get-Content -Raw -LiteralPath (Join-Path $run 'b.json') | ConvertFrom-Json
    $state=@(Get-Content -Raw -LiteralPath (Join-Path $run 'ui.state.json') | ConvertFrom-Json)
    if($null -eq $a -or $null -eq $b -or $state.Count -ne 2 -or $a.pid -ne $children[0].Id -or $b.pid -ne $children[1].Id){continue}
    $samples[$count]=@{a=$a;b=$b;state=$state;button=$steps[$count-1].button}
    Write-Output "Step $count $($steps[$count-1].button): A=$($a.cursor -join ',') mouse=$($a.buttons) keys=$($a.test_keys) wheel=$($a.wheel_events); B=$($b.cursor -join ',') mouse=$($b.buttons) keys=$($b.test_keys) wheel=$($b.wheel_events)"
    if($count -eq 16 -and (Test-Path -LiteralPath (Join-Path $run 'ui.png'))){Copy-Item -LiteralPath (Join-Path $run 'ui.png') -Destination (Join-Path $run 'theme.png')}
   }catch{continue}
  }
  if($samples.ContainsKey($steps.Count)){break}
 }
 $serializable=@{};foreach($key in $samples.Keys){$serializable[[string]$key]=$samples[$key]}
 $serializable | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $run 'samples.json') -Encoding utf8
 function Assert-Check($condition,$message){if(-not $condition){throw $message}}
 function Same-Cursor($x,$y){($x.cursor -join ',') -eq ($y.cursor -join ',')}
 Assert-Check ($samples.Count -eq $steps.Count) 'UI script did not complete all controls'
 foreach($i in 1..16){
  foreach($j in 0..1){
   $s=$samples[$i];$probe=if($j -eq 0){$s.a}else{$s.b};$overlay=$s.state[$j].overlay
   Assert-Check ($overlay.x -eq $probe.cursor[0] -and $overlay.y -eq $probe.cursor[1]) "Overlay is not aligned at step $i user $j"
   Assert-Check ($overlay.visible -and $overlay.no_activate -and $overlay.click_through -and $overlay.painted_pixels -gt 20) "Invalid overlay at step $i user $j"
  }
 }
 Assert-Check ($samples[2].a.mouse_clicks -ge 1 -and $samples[2].b.mouse_clicks -ge 1) 'Mouse button window events were not delivered'
 foreach($i in 2..6){Assert-Check ($samples[$i].a.buttons -eq 1 -and $samples[$i].b.buttons -eq 2 -and $samples[$i].a.test_keys -eq 1 -and $samples[$i].b.test_keys -eq 1) "Concurrent holds lost at step $i"}
 Assert-Check ($samples[3].a.cursor[0] -eq $samples[2].a.cursor[0]+40 -and (Same-Cursor $samples[3].b $samples[2].b)) 'A motion isolation failed'
 Assert-Check ($samples[4].b.cursor[1] -eq $samples[3].b.cursor[1]+60 -and (Same-Cursor $samples[4].a $samples[3].a)) 'B motion isolation failed'
 Assert-Check ($samples[3].a.drag_moves -gt $samples[2].a.drag_moves -and $samples[4].b.drag_moves -gt $samples[3].b.drag_moves) 'Concurrent drag events missing'
 Assert-Check ($samples[5].a.wheel_events -gt $samples[4].a.wheel_events -and $samples[5].b.wheel_events -eq $samples[4].b.wheel_events) 'A wheel isolation failed'
 Assert-Check ($samples[6].b.wheel_events -gt $samples[5].b.wheel_events -and $samples[6].a.wheel_events -eq $samples[5].a.wheel_events) 'B wheel isolation failed'
 Assert-Check ($samples[5].a.wheel_delta -gt $samples[4].a.wheel_delta -and $samples[6].b.wheel_delta -lt $samples[5].b.wheel_delta) 'Wheel direction transform failed'
 Assert-Check ($samples[7].a.buttons -eq 0 -and $samples[7].b.buttons -eq 0 -and $samples[7].a.test_keys -eq 0 -and $samples[7].b.test_keys -eq 0) 'Pause did not release all holds'
 Assert-Check (Same-Cursor $samples[8].a $samples[7].a) 'Paused motion was delivered'
 Assert-Check ($samples[10].a.cursor[0] -eq $samples[9].a.cursor[0]-40) 'Resume failed'
 Assert-Check ($samples[12].a.test_keys -eq 1 -and $samples[12].b.test_keys -eq 1) 'A timed tap released an existing hold'
 Assert-Check ($samples[13].a.buttons -eq 0 -and $samples[13].b.buttons -eq 0 -and $samples[13].a.test_keys -eq 0 -and $samples[13].b.test_keys -eq 0) 'Explicit release failed'
 foreach($state in $samples[16].state){Assert-Check (-not [CursorWindows]::IsWindow([IntPtr][long]$state.overlay.hwnd)) 'Overlay window survived stop'}
 Write-Output 'PASS: concurrent virtual holds, independent drags/wheels, desktop overlays, pause/release/resume/stop, Ouroboros settings.'
}finally{foreach($child in $children){if(-not $child.HasExited){Stop-Process -Id $child.Id -ErrorAction SilentlyContinue}}}
