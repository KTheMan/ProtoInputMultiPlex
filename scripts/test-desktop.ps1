param([Parameter(Mandatory)][string]$Executable,[string]$SecondExecutable=$Executable)
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
$run=Join-Path $root ('runtime/desktop-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $run -Force | Out-Null
$mainImage=Join-Path $run 'control.exe';$secondImage=Join-Path $run 'probe32.exe'
Copy-Item -LiteralPath $Executable -Destination $mainImage
Copy-Item -LiteralPath $SecondExecutable -Destination $secondImage
Add-Type -TypeDefinition @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class DesktopFixtures {
 [StructLayout(LayoutKind.Sequential)] public struct RECT {public int left,top,right,bottom;}
 [StructLayout(LayoutKind.Sequential)] public struct POINT {public int x,y;}
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h,ref POINT p);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int w,int height,uint flags);
 [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
 public delegate bool Callback(IntPtr h,IntPtr p);
 [DllImport("user32.dll")] public static extern bool EnumWindows(Callback c,IntPtr p);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll",CharSet=CharSet.Unicode,EntryPoint="GetWindowTextW")] public static extern int GetWindowText(IntPtr h,StringBuilder b,int n);
 public static IntPtr FindOwned(uint pid,string prefix) {
  IntPtr result=IntPtr.Zero;
  EnumWindows((h,p)=>{uint owner;GetWindowThreadProcessId(h,out owner);if(owner==pid && IsWindowVisible(h)){var title=new StringBuilder(512);GetWindowText(h,title,512);if(title.ToString().StartsWith(prefix,StringComparison.Ordinal)){result=h;return false;}}return true;},IntPtr.Zero);
  return result;
 }

}
"@
function Window-Info($process) {
 $process.Refresh();$h=[DesktopFixtures]::FindOwned($process.Id,"ProtoInputMultiPlex Routing Test")
 $outer=New-Object DesktopFixtures+RECT;$client=New-Object DesktopFixtures+RECT;$origin=New-Object DesktopFixtures+POINT
 if(-not [DesktopFixtures]::GetWindowRect($h,[ref]$outer) -or -not [DesktopFixtures]::GetClientRect($h,[ref]$client) -or -not [DesktopFixtures]::ClientToScreen($h,[ref]$origin)){throw 'Read fixture window'}
 @{hwnd=[long]$h;left=$outer.left;top=$outer.top;right=$outer.right;bottom=$outer.bottom;x=$origin.x+180;y=$origin.y+160;caption_x=$outer.left+120;caption_y=[int](($outer.top+$origin.y)/2)}
}
function Read-Report($name){$report=Get-Content -Raw -LiteralPath (Join-Path $run ($name+'.json')) | ConvertFrom-Json;if($null -eq $report){throw 'Incomplete report'};$report}
$children=@();$samples=@{}
try {
 foreach($entry in @(@($mainImage,'a'),@($secondImage,'b'),@($mainImage,'c'))) {
  $children+=Start-Process -FilePath $entry[0] -ArgumentList @('probe','--report',('"'+(Join-Path $run ($entry[1]+'.json'))+'"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
 }
 $readyDeadline=[DateTime]::UtcNow.AddSeconds(60)
 while($true){$ready=$true;foreach($child in $children){$child.Refresh();if($child.HasExited){throw 'Fixture exited'};if([DesktopFixtures]::FindOwned($child.Id,"ProtoInputMultiPlex Routing Test") -eq [IntPtr]::Zero){$ready=$false}};if($ready){break};if([DateTime]::UtcNow -gt $readyDeadline){throw 'Fixture startup timeout'};Start-Sleep -Milliseconds 200}
 # Only test-owned windows are positioned. SWP_NOACTIVATE; no cursor or keyboard injection.
 [void][DesktopFixtures]::SetWindowPos([DesktopFixtures]::FindOwned($children[0].Id,"ProtoInputMultiPlex Routing Test"),[IntPtr](-1),40,50,400,360,0x10)
 [void][DesktopFixtures]::SetWindowPos([DesktopFixtures]::FindOwned($children[1].Id,"ProtoInputMultiPlex Routing Test"),[IntPtr](-1),500,50,400,360,0x10)
 [void][DesktopFixtures]::SetWindowPos([DesktopFixtures]::FindOwned($children[2].Id,"ProtoInputMultiPlex Routing Test"),[IntPtr](-1),40,500,400,360,0x10)
 $a=Window-Info $children[0];$b=Window-Info $children[1];$c=Window-Info $children[2]
 $pA=@([int]$a.x,[int]$a.y);$pB=@([int]$b.x,[int]$b.y);$pC=@([int]$c.x,[int]$c.y)
 $bMoved=@([int]($b.x+80),[int]($b.y+60));$bFinal=@([int]($b.x+110),[int]($b.y+80))
 @{version=1;concurrent=$true;desktop=@{mode='switched';multi_keyboard=$true;allowed_pids=@($children[0].Id,$children[1].Id,$children[2].Id)};seats=@(
  @{name='Virtual Blue';mouse='virtual://mouse/1';keyboard='virtual://keyboard/1';desktop_start=$pA;cursor=@{color=@(96,165,250);lock_window=$false}},
  @{name='Virtual Coral';mouse='virtual://mouse/2';keyboard='virtual://keyboard/2';desktop_start=$pB;cursor=@{color=@(248,113,113);lock_window=$false}}
 )} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $run 'profile.json') -Encoding utf8
 $steps=[System.Collections.Generic.List[object]]::new()
 function Step($id,$button,$point=$null,$user=0,$wait=1400) {
  $value=@{id=$id;button=$button;wait_ms=$wait;user=$user;wait_for_sample=$true};if($null -ne $point){$value.point=$point};$steps.Add($value)
 }
 Step 'start' 'Start routing' $null 0 3000
 Step 'select-a' '1 Hold click' $null 0 3000
 Step 'release-a' '1 Release click'
 Step 'select-b' '2 Hold click'
 Step 'release-b' '2 Release click'
 Step 'root-a' '1 Hold click'
 Step 'blocked-b' '2 Hold click'
 Step 'key-b' '2 Hold W'
 Step 'caption-b' '2 Move to point' @([int]$b.caption_x,[int]$b.caption_y) 1
 Step 'grab-b' '2 Hold click'
 Step 'move-b' '2 Move to point' @([int]($b.caption_x+80),[int]($b.caption_y+60)) 1
 Step 'drop-b' '2 Release click'
 Step 'corner-b' '2 Move to point' @([int]($b.right+80-5),[int]($b.bottom+60-5)) 1
 Step 'grab-resize' '2 Hold click'
 Step 'resize-b' '2 Move to point' @([int]($b.right+80-5+60),[int]($b.bottom+60-5+40)) 1
 Step 'drop-resize' '2 Release click'
 Step 'drag-a-over-b' '1 Move to point' $bFinal 0
 Step 'release-root-a' '1 Release click'
 Step 'release-key-b' '2 Release W'
 Step 'concurrent' 'Concurrent'
 Step 'return-a' '1 Move to point' $pA 0
 Step 'return-b' '2 Move to point' $bFinal 1
 Step 'both-held' 'Hold all users'
 Step 'drag-a' '1 Right'
 Step 'drag-b' '2 Down'
 Step 'wheel-a' '1 Wheel'
 Step 'wheel-b' '2 Wheel'
 Step 'pause' 'Pause'
 Step 'paused-move' '1 Right'
 Step 'resume' 'Resume'
 Step 'swap-a' '1 Move to point' $bFinal 0
 Step 'claim-b' '1 Hold click'
 Step 'drop-claim-b' '1 Release click'
 Step 'swap-b' '2 Move to point' $pA 1
 Step 'claim-a' '2 Hold click'
 Step 'drop-claim-a' '2 Release click'
 Step 'typing-b' '1 Hold W'
 Step 'typing-a' '2 Tap D'
 Step 'move-while-key' '1 Move to point' $pC 0
 Step 'claim-new-c' '1 Hold click'
 Step 'drop-new-c' '1 Release click'
 Step 'root-again' 'Switched'
 Step 'hold-a-again' '2 Hold click'
 Step 'hold-a-key' '2 Hold W'
 Step 'after-exit-move' '2 Move to point' $pC 1 $null
 $steps[$steps.Count-1].wait_ms=4000
 Step 'claim-c-after-exit' '2 Hold click'
 Step 'release-c-after-exit' '2 Release click'
 Step 'all-clear' 'Release all users'
 Step 'stop' 'Stop routing'
 $steps | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'ui.json') -Encoding utf8
 $children+=Start-Process -FilePath $mainImage -ArgumentList @('gui','--profile',('"'+(Join-Path $run 'profile.json')+'"'),'--ui-script',('"'+(Join-Path $run 'ui.json')+'"')) -WorkingDirectory $root -WindowStyle Hidden -PassThru
 $guiDeadline=[DateTime]::UtcNow.AddSeconds(60)
 while($true){$children[3].Refresh();if([DesktopFixtures]::FindOwned($children[3].Id,"ProtoInputMultiPlex") -ne [IntPtr]::Zero){break};if($children[3].HasExited -or [DateTime]::UtcNow -gt $guiDeadline){throw 'Dashboard startup timeout'};Start-Sleep -Milliseconds 100}
 [void][DesktopFixtures]::SetWindowPos([DesktopFixtures]::FindOwned($children[3].Id,"ProtoInputMultiPlex"),[IntPtr](-1),980,50,940,850,0x10)
 Write-Output "Desktop validation: $run; PIDs $($children.Id -join ', ')"
 $lastCount=-1;$changed=[DateTime]::UtcNow;$deadline=[DateTime]::UtcNow.AddSeconds(360);$closedA=$false
 while([DateTime]::UtcNow -lt $deadline) {
  Start-Sleep -Milliseconds 100
  if($children[3].HasExited){throw 'Dashboard exited'}
  $log=Join-Path $run 'ui.events.jsonl';$count=if(Test-Path -LiteralPath $log){@(Get-Content -LiteralPath $log).Count}else{0}
  if($count -ne $lastCount){$lastCount=$count;$changed=[DateTime]::UtcNow}
  if($count -gt 0 -and -not $samples.ContainsKey($steps[$count-1].id) -and ([DateTime]::UtcNow-$changed).TotalMilliseconds -gt 750) {
   try {
    if(-not (Get-Content -Raw -LiteralPath (Join-Path $run 'ui.ready.json') | ConvertFrom-Json)){continue}
    $ra=Read-Report 'a';$rb=Read-Report 'b';$rc=Read-Report 'c';$state=@(Read-Report 'ui.state')
    if($state.Count -ne 2){continue}
    $id=$steps[$count-1].id
    # Native delivery and probe reports follow worker acknowledgement asynchronously.
    # Sample only when each currently selected live probe agrees with routed held state.
    $settled=$true
    if($id -ne 'stop') {
     foreach($userState in $state) {
      $probe=if($userState.pid -eq $children[0].Id -and -not $children[0].HasExited){$ra}elseif($userState.pid -eq $children[1].Id){$rb}elseif($userState.pid -eq $children[2].Id){$rc}else{$null}
      if($null -eq $probe){continue}
      $buttons=0;foreach($flag in $userState.buttons){$buttons=$buttons -bor $(switch($flag){1{1}4{2}16{4}default{0}})}
      $keys=0;foreach($key in $userState.keys){$keys=$keys -bor $(switch($key){87{1}65{2}83{4}68{8}32{16}default{0}})}
      if($probe.buttons -ne $buttons -or $probe.test_keys -ne $keys){$settled=$false}
      # Switched non-root cursors can move visually without changing native hover.
      # Caption gestures do not create native button holds either.
      if($buttons -ne 0 -and ($probe.cursor[0] -ne $userState.overlay.x -or $probe.cursor[1] -ne $userState.overlay.y)){$settled=$false}
     }
    }
    if(-not $settled){continue}
    $samples[$id]=@{a=$ra;b=$rb;c=$rc;state=$state;button=$steps[$count-1].button;b_window=(Window-Info $children[1]);a_alive=(-not $children[0].HasExited)}
    Write-Output "$id`: targets=$($state.pid -join ',') root=$($state.root -join ',') A mouse=$($ra.buttons) key=$($ra.test_keys); B mouse=$($rb.buttons) key=$($rb.test_keys); status=$($state.status -join ' / ')"
    if($id -eq 'hold-a-key' -and -not $closedA){Stop-Process -Id $children[0].Id;$closedA=$true}
    if($id -eq 'both-held' -and (Test-Path -LiteralPath (Join-Path $run 'ui.png'))){Copy-Item -LiteralPath (Join-Path $run 'ui.png') -Destination (Join-Path $run 'concurrent.png')}
    [string]$count | Set-Content -LiteralPath (Join-Path $run 'ui.sampled.json') -Encoding utf8
   }catch{continue}
  }
  if($samples.ContainsKey('stop')){break}
 }
 $samples | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $run 'samples.json') -Encoding utf8
 function Assert-Check($condition,$message){if(-not $condition){throw "$message ($run)"}}
 function Same-Cursor($a,$b){($a.cursor -join ',') -eq ($b.cursor -join ',')}
 Assert-Check ($samples.Count -eq $steps.Count) 'Incomplete UI sequence'
 Assert-Check ($samples['start'].state[0].pid -eq 0 -and $samples['start'].state[1].pid -eq 0) 'Automatic mode unexpectedly preselected a process'
 Assert-Check ($samples['select-a'].state[0].pid -eq $children[0].Id -and $samples['select-a'].a.buttons -eq 1) 'First click did not automatically select A'
 Assert-Check ($samples['select-b'].state[1].pid -eq $children[1].Id -and $samples['select-b'].b.buttons -eq 1) 'Second cursor did not automatically select x86 B'
 Assert-Check ($samples['blocked-b'].b.mouse_clicks -eq $samples['root-a'].b.mouse_clicks -and $samples['blocked-b'].b.buttons -eq 0 -and $samples['blocked-b'].state[0].root) 'Switched click arbitration failed'
 Assert-Check ($samples['key-b'].b.test_keys -eq 1 -and $samples['key-b'].a.test_keys -eq 0) 'Independent typing failed while another user was root'
 Assert-Check ($samples['move-b'].b_window.left -eq $b.left+80 -and $samples['move-b'].b_window.top -eq $b.top+60 -and $samples['move-b'].a.buttons -eq 1) 'Window drag did not work while another user held a client click'
 Assert-Check ($samples['resize-b'].b_window.right -eq $b.right+80+60 -and $samples['resize-b'].b_window.bottom -eq $b.bottom+60+40) 'Virtual window resize failed'
 Assert-Check ($samples['drag-a-over-b'].state[0].pid -eq $children[0].Id -and $samples['drag-a-over-b'].a.cursor[0] -eq $bFinal[0] -and $samples['drag-a-over-b'].a.buttons -eq 1) 'Drag capture retargeted into the crossed window'
 Assert-Check ($samples['concurrent'].state[0].mode -eq 'concurrent') 'Live mode switch failed'
 Assert-Check ($samples['both-held'].a.buttons -eq 1 -and $samples['both-held'].b.buttons -eq 1 -and $samples['both-held'].a.test_keys -eq 1 -and $samples['both-held'].b.test_keys -eq 1) 'Concurrent holds failed'
 Assert-Check ($samples['drag-a'].a.cursor[0] -eq $samples['both-held'].a.cursor[0]+40 -and (Same-Cursor $samples['drag-a'].b $samples['both-held'].b)) 'A concurrent motion leaked'
 Assert-Check ($samples['drag-b'].b.cursor[1] -eq $samples['drag-a'].b.cursor[1]+40 -and (Same-Cursor $samples['drag-b'].a $samples['drag-a'].a)) 'B concurrent motion leaked'
 Assert-Check ($samples['wheel-a'].a.wheel_events -gt $samples['drag-b'].a.wheel_events -and $samples['wheel-a'].b.wheel_events -eq $samples['drag-b'].b.wheel_events) 'A wheel leaked'
 Assert-Check ($samples['wheel-b'].b.wheel_events -gt $samples['wheel-a'].b.wheel_events) 'B wheel missing'
 Assert-Check ($samples['pause'].a.buttons -eq 0 -and $samples['pause'].b.buttons -eq 0 -and $samples['pause'].a.test_keys -eq 0 -and $samples['pause'].b.test_keys -eq 0) 'Pause did not release all state'
 Assert-Check (Same-Cursor $samples['paused-move'].a $samples['pause'].a) 'Paused cursor moved'
 Assert-Check ($samples['claim-b'].state[0].pid -eq $children[1].Id -and $samples['claim-a'].state[1].pid -eq $children[0].Id) 'Automatic application handoff failed'
 Assert-Check ($samples['typing-b'].b.test_keys -eq 1 -and $samples['typing-b'].a.test_keys -eq 0) 'Keyboard did not follow clicked target'
 Assert-Check ($samples['typing-a'].a.key_presses -gt $samples['typing-b'].a.key_presses -and $samples['typing-a'].b.key_presses -eq $samples['typing-b'].b.key_presses) 'Keyboard routing after handoff leaked'
 Assert-Check ($samples['claim-new-c'].state[0].pid -eq $children[2].Id -and $samples['claim-new-c'].b.test_keys -eq 0) 'New application discovery did not release old held key'
 Assert-Check (-not $samples['after-exit-move'].a_alive -and $samples['after-exit-move'].state[1].pid -eq 0 -and $samples['after-exit-move'].state[1].keys.Count -eq 0) 'Target exit did not clear capture/focus'
 Assert-Check ($samples['claim-c-after-exit'].state[1].pid -eq $children[2].Id -and $samples['claim-c-after-exit'].c.buttons -eq 1) 'Routing did not recover after target exit'
 foreach($state in $samples['all-clear'].state){Assert-Check (-not [DesktopFixtures]::IsWindow([IntPtr][long]$state.overlay.hwnd)) 'Overlay survived stop'}
 Write-Output 'PASS: automatic targeting, switched ownership, window drag/resize, concurrent routing, keyboard handoff, capture, pause/resume, target-exit recovery and cleanup through Ouroboros controls.'
}finally{foreach($child in $children){if(-not $child.HasExited){Stop-Process -Id $child.Id -ErrorAction SilentlyContinue}}}
