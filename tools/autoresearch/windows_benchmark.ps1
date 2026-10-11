[CmdletBinding()]
param(
 [ValidateSet('All','Build','Measure','Settings','Verify')][string]$Mode='All',
 [Parameter(Mandatory=$true)][string]$Root,
 [Parameter(Mandatory=$true)][string]$TaskRoot,
 [ValidateRange(1,3600)][int]$CpuSeconds=20,
 [ValidateRange(1,3600)][int]$DeliverySeconds=8,
 [ValidateRange(0,3600)][int]$Warmup=5,
 [ValidateRange(1,50)][int]$Repeats=3
)
$ErrorActionPreference='Stop'; Set-StrictMode -Version Latest
$Root=[IO.Path]::GetFullPath($Root); $TaskRoot=[IO.Path]::GetFullPath($TaskRoot)
if ($TaskRoot -ne 'V:\agent-workspaces\vosulay-autoresearch-20261007') { throw 'TaskRoot must be the authorized V: task workspace' }
$base='V:\agent-workspaces\vosulay-cef-native-20261004-resume\native-final-verification-20261005'
$state=Join-Path $TaskRoot 'windows-benchmark'; $runtime=Join-Path $state 'runtime'; $temp=Join-Path $state 'temp'
 $ref=Join-Path $runtime 'reference'
 $sessionId=[Guid]::NewGuid().ToString('N')
 $runRoot=Join-Path $state "results-$sessionId"
New-Item -ItemType Directory -Force -Path $state,$runtime,$temp | Out-Null
 $env:TEMP=$temp; $env:TMP=$temp; $env:CARGO_TARGET_DIR=Join-Path $TaskRoot 'target'
 $pinDoc=Get-Content -Raw -LiteralPath (Join-Path $Root 'tools\cef\pins.json')|ConvertFrom-Json
 $winPin=$pinDoc.platforms.'windows-x86_64'
 $cef=Join-Path $base ('sdk\'+$winPin.cef_path_subdir)
 $env:CEF_PATH=$cef
 $env:CARGO_HOME=Join-Path $TaskRoot 'cargo-home'
 $cargoSeed=Join-Path $base 'cargo-home'
 if(-not (Test-Path -LiteralPath $env:CARGO_HOME)){if(-not(Test-Path -LiteralPath $cargoSeed)){throw "Pinned guest Cargo cache is absent: $cargoSeed"};New-Item -ItemType Directory -Force -Path $env:CARGO_HOME|Out-Null;& robocopy $cargoSeed $env:CARGO_HOME /E /COPY:DAT /R:0 /W:0 /NFL /NDL /NJH /NJS /NP;if($LASTEXITCODE -ge 8){throw "Cargo cache seeding failed: $LASTEXITCODE"}}
 $env:PATH='C:\Program Files\Rust\1.96.1\bin;'+$env:PATH
 $vcvars='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
 if(-not(Test-Path -LiteralPath $vcvars)){throw "VS BuildTools vcvars64 missing: $vcvars"}
 $envLines=& $env:ComSpec /d /s /c "`"$vcvars`" >nul && set"
 foreach($line in $envLines){if($line -match '^([^=]+)=(.*)$'){[Environment]::SetEnvironmentVariable($Matches[1],$Matches[2],'Process')}}
$env:PATH=$runtime+';'+$ref+';'+$env:PATH
if ($Mode -in @('All','Build')) {
 if (-not (Test-Path (Join-Path $cef 'include\cef_version.h'))) { throw "Pinned Windows CEF SDK not staged at $cef" }
 $receiptPath=Join-Path $cef '.vosulay-cef-verified.json'
 if (-not (Test-Path -LiteralPath $receiptPath)) { throw "Pinned SDK verification receipt missing under $cef" }
 $receipt=Get-Content -Raw -LiteralPath $receiptPath|ConvertFrom-Json
 if($receipt.platform -ne 'windows-x86_64' -or $receipt.cef_version -ne $pinDoc.cef_version -or $receipt.archive_sha256 -ne $winPin.sha256){throw 'CEF receipt does not match pinned Windows CEF'}
 foreach($entry in $receipt.runtime_sha256.PSObject.Properties){$file=Join-Path $cef ($entry.Name -replace '/','\');if(-not(Test-Path -LiteralPath $file)-or (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant() -ne $entry.Value){throw "CEF runtime digest mismatch: $($entry.Name)"}}
 if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) { throw 'MSVC x64 compiler unavailable after vcvars64' }
 if (-not (Get-Command cargo.exe -ErrorAction SilentlyContinue)) { throw 'Rust 1.96.1 toolchain unavailable' }
 Push-Location $Root
 try {
  $env:CEF_PATH=$cef
  & cargo build --offline --release --locked -p vosulay --lib --target x86_64-pc-windows-msvc; if($LASTEXITCODE){throw "Vosulay Windows host build failed: $LASTEXITCODE"}
  $product=Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc\release\vosulay.dll'
  if(-not(Test-Path -LiteralPath $product)){throw "Vosulay product cdylib missing: $product"}
  Copy-Item -LiteralPath $product (Join-Path $runtime 'vosulay.dll') -Force
  & cargo build --offline --release --locked -p vosulay-browser --example autoresearch_browser_windows; if($LASTEXITCODE){throw "CEF reference build failed: $LASTEXITCODE"}
  $dest=$runtime
  $release=Join-Path $cef 'Release'; $resources=Join-Path $cef 'Resources'
  Copy-Item -LiteralPath (Join-Path $release 'bootstrap.exe') (Join-Path $runtime 'vosulay.exe') -Force
  $ref=Join-Path $runtime 'reference'; New-Item -ItemType Directory -Force -Path $ref | Out-Null
  $client=Join-Path $env:CARGO_TARGET_DIR 'release\examples\autoresearch_browser_windows.dll'
  if(-not (Test-Path -LiteralPath $client)){throw "CEF reference cdylib not produced: $client"}
  Copy-Item -LiteralPath (Join-Path $release 'bootstrap.exe') (Join-Path $ref 'autoresearch_browser_windows.exe') -Force
  Copy-Item -LiteralPath $client (Join-Path $ref 'autoresearch_browser_windows.dll') -Force
  foreach($directory in @($runtime,$ref)){
   Remove-Item -LiteralPath (Join-Path $directory 'Resources') -Recurse -Force -ErrorAction SilentlyContinue
   foreach($f in $winPin.package_release_files){Copy-Item -LiteralPath (Join-Path $release $f) -Destination $directory -Force}
   foreach($f in $winPin.package_resources){Copy-Item -LiteralPath (Join-Path $resources $f) -Destination $directory -Recurse -Force}
   foreach($f in @('LICENSE.txt','CREDITS.html')){Copy-Item -LiteralPath (Join-Path $cef $f) -Destination $directory -Force}
  }
  $injectionDir=Join-Path (Split-Path $base -Parent) 'runtime-acceptance-final-20261005'
  # DLLs are built from source (vosulay-cef build-asdf), so no fixed digest exists; the directory supplies a prior package's pair.
  foreach($dll in $pinDoc.asdf_overlay.dlls){$source=Join-Path $injectionDir $dll.name;if(-not (Test-Path -LiteralPath $source -PathType Leaf)){throw "Injection DLL missing: $source"};Copy-Item -LiteralPath $source (Join-Path $runtime $dll.name) -Force}
  $objDir=Join-Path $state 'obj';New-Item -ItemType Directory -Force -Path $objDir|Out-Null
  $game=Join-Path $Root 'tools\autoresearch\windows_game.cpp';$gameExe=Join-Path $runtime 'windows_game.exe'
  & cl.exe /nologo /O2 /W4 /WX /EHsc /std:c++20 "/Fo$objDir\\" "/Fd$(Join-Path $state 'windows_game.pdb')" $game "/Fe:$gameExe" /link /SUBSYSTEM:WINDOWS /PDB:"$(Join-Path $state 'windows_game-link.pdb')" d3d11.lib dxgi.lib user32.lib;if($LASTEXITCODE){throw "DX11 game build failed: $LASTEXITCODE"}
  $src=Join-Path $Root 'tools\autoresearch\windows_native.cpp';$exe=Join-Path $runtime 'windows_native.exe'
  & cl.exe /nologo /O2 /W4 /WX /EHsc /std:c++20 "/Fo$objDir\\" "/Fd$(Join-Path $state 'windows_native.pdb')" $src "/Fe:$exe" /link /SUBSYSTEM:CONSOLE /PDB:"$(Join-Path $state 'windows_native-link.pdb')" d3d11.lib dxgi.lib user32.lib;if($LASTEXITCODE){throw "Native helper build failed: $LASTEXITCODE"}
  [IO.File]::WriteAllText((Join-Path $state 'build.receipt'),("CEF_SHA256={0}`nCEF_CHROMIUM=152.0.7977.134`nVOSULAY_SHA256={1}`nBROWSER_DLL_SHA256={2}`nNATIVE_SHA256={3}`nGAME_SHA256={4}`n" -f (Get-FileHash -Algorithm SHA256 (Join-Path $dest 'libcef.dll')).Hash,(Get-FileHash -Algorithm SHA256 (Join-Path $runtime 'vosulay.dll')).Hash,(Get-FileHash -Algorithm SHA256 (Join-Path $ref 'autoresearch_browser_windows.dll')).Hash,(Get-FileHash -Algorithm SHA256 $exe).Hash,(Get-FileHash -Algorithm SHA256 $gameExe).Hash))
 } finally { Pop-Location }
}
if ($Mode -eq 'Verify') {
 Push-Location $Root
 try {
  & cargo test --workspace --all-targets --all-features --locked --offline --target x86_64-pc-windows-msvc
  if($LASTEXITCODE){throw "Native workspace tests failed: $LASTEXITCODE"}
  & cargo clippy --workspace --all-targets --all-features --locked --offline --target x86_64-pc-windows-msvc -- -D warnings
  if($LASTEXITCODE){throw "Native workspace Clippy failed: $LASTEXITCODE"}
  & cargo fmt --all --check
  if($LASTEXITCODE){throw "Native workspace formatting failed: $LASTEXITCODE"}
 } finally { Pop-Location }
}
if ($Mode -in @('All','Measure','Settings')) {
 if ((Get-Process -Id $PID).SessionId -eq 0) { throw 'Measure requires the interactive guest console; launch this exact script with guest Cua' }
 if(-not(Test-Path -LiteralPath (Join-Path $state 'build.receipt'))){throw 'Build mode must complete before Measure'}
 $actors=[System.Collections.Generic.List[System.Diagnostics.Process]]::new();$job=$null;$log=Join-Path $TaskRoot 'measurement.log';$receiptOut=Join-Path $TaskRoot 'measurement-result.json';$reportPath=$runRoot;$reportFile=Join-Path $runRoot 'report.json';$runs=[System.Collections.Generic.List[object]]::new();$capFile=Join-Path $state 'capabilities.jsonl';$ready=Join-Path $state 'server.ready'
 New-Item -ItemType Directory -Force -Path $runRoot|Out-Null
 function StopOwnedTree([int]$rootId){
  $all=Get-CimInstance Win32_Process;$byId=@{};foreach($proc in $all){$byId[[int]$proc.ProcessId]=$proc}
  $owned=[Collections.Generic.List[int]]::new();$stack=[Collections.Generic.Stack[int]]::new();$stack.Push($rootId)
  while($stack.Count){
   $pidToCheck=$stack.Pop()
   if($byId.ContainsKey($pidToCheck)){
    $proc=$byId[$pidToCheck]
    if(-not$proc.ExecutablePath -or -not($proc.ExecutablePath.StartsWith($runtime+'\',[StringComparison]::OrdinalIgnoreCase) -or $proc.ExecutablePath.StartsWith($ref+'\',[StringComparison]::OrdinalIgnoreCase))){continue}
    $owned.Add($pidToCheck)
   }
   foreach($child in $all){if([int]$child.ParentProcessId -eq $pidToCheck){$stack.Push([int]$child.ProcessId)}}
  }
  for($i=$owned.Count-1;$i -ge 0;$i--){Stop-Process -Id $owned[$i] -Force -ErrorAction SilentlyContinue}
 }
 $exitCode=0
 try {
  Remove-Item -LiteralPath $capFile,$ready -Force -ErrorAction SilentlyContinue
  $job=Start-Job -ArgumentList $Root,$capFile,$ready -ScriptBlock {
   param($root,$caps,$readyFile)
   $ErrorActionPreference='Stop'
   $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,24050);$listener.Start()
   [IO.File]::WriteAllText($readyFile,'ready')
   $page=[IO.File]::ReadAllBytes((Join-Path $root 'tools\autoresearch\counter.html'))
   $profileBytes=[Text.Encoding]::UTF8.GetBytes('<!doctype html><title>Vosulay autoresearch browser</title><style>html,body{margin:0;width:640px;height:480px;overflow:hidden;background:#091f38}iframe{position:absolute;left:64px;top:64px;width:480px;height:160px;border:0}</style><iframe src="/counter/index.html"></iframe>')
   $stop=$false
   try {
    while(-not$stop){
     $client=$listener.AcceptTcpClient()
     try {
      $stream=$client.GetStream();$stream.ReadTimeout=2000
      $buffer=[byte[]]::new(16384);$count=0;$line=''
      while($count -lt $buffer.Length -and -not$line.Contains("`r`n")){
       $received=$stream.Read($buffer,$count,$buffer.Length-$count)
       if($received -eq 0){break}
       $count+=$received;$line=[Text.Encoding]::ASCII.GetString($buffer,0,$count)
      }
      if(-not$line.Contains("`r`n")){continue}
      $request=$line.Split("`r`n")[0].Split(' ')
      $uri=[Uri]::new([Uri]::new('http://127.0.0.1:24050/'),$request[1]);$path=$uri.AbsolutePath
      [IO.File]::AppendAllText($caps+'.requests.log',$path+[Environment]::NewLine)
      $body=$null;$type='text/plain';$status='200 OK'
      if($path -eq '/counter/index.html'){$body=$page;$type='text/html'}
      elseif($path -eq '/profile.html'){$body=$profileBytes;$type='text/html'}
      elseif($path -eq '/capabilities'){
       $data=[Uri]::UnescapeDataString($uri.Query.TrimStart('?').Substring(5).Replace('+',' '))
       if(-not$data){$status='400 Bad Request'}
       else{[IO.File]::AppendAllText($caps,$data+[Environment]::NewLine);$body=[Text.Encoding]::UTF8.GetBytes('ok')}
      }
      elseif($path.StartsWith('/api/counters/search/')){$body=[Text.Encoding]::UTF8.GetBytes('<!doctype html><div class="no-results">No installed counters</div>');$type='text/html'}
      elseif($path -eq '/__autoresearch_stop__'){$stop=$true;$body=[Text.Encoding]::UTF8.GetBytes('ok')}
      else{$status='404 Not Found'}
      if($null -eq $body){$body=[Text.Encoding]::UTF8.GetBytes('not found')}
      $header=[Text.Encoding]::ASCII.GetBytes("HTTP/1.1 $status`r`nContent-Type: $type`r`nContent-Length: $($body.Length)`r`nCache-Control: no-store`r`nConnection: close`r`n`r`n")
      $stream.Write($header,0,$header.Length);$stream.Write($body,0,$body.Length);$stream.Flush()
     } catch [IO.IOException] {
      # Chromium may preconnect and close a socket without sending a request.
     } finally {$client.Close()}
    }
   } finally {$listener.Stop()}
  }
  $deadline=[DateTime]::UtcNow.AddSeconds(15);while(-not(Test-Path -LiteralPath $ready)){if($job.State -eq 'Failed'){throw 'Workload HTTP server failed to start'};if([DateTime]::UtcNow -gt $deadline){throw 'Workload HTTP server did not become ready'};Start-Sleep -Milliseconds 50}
  if($Mode -eq 'Settings'){
   $settingsDir=Join-Path $runRoot 'settings';New-Item -ItemType Directory -Force -Path $settingsDir|Out-Null;$stopFile=Join-Path $settingsDir 'stop'
   Remove-Item -LiteralPath $stopFile -Force -ErrorAction SilentlyContinue
   $config=Join-Path $settingsDir 'config.json';$cfg=@{version=1;tosu_migration_completed=$true;active_profile='benchmark';profiles=@(@{id='benchmark';name='Benchmark';overlays=@(@{counter_id='benchmark';name='Benchmark';url='http://127.0.0.1:24050/counter/index.html';left=64;top=64;width=480;height=160;scale=1;opacity=1;rotation=0;enabled=$true;z_index=1})});settings=@{remote_listen_host='127.0.0.1';remote_listen_port=25050}}|ConvertTo-Json -Depth 12;[IO.File]::WriteAllText($config,$cfg,[Text.UTF8Encoding]::new($false))
   $game=Start-Process -FilePath (Join-Path $runtime 'windows_game.exe') -PassThru -WorkingDirectory $runtime;$actors.Add($game)
   $game.Refresh();$until=[DateTime]::UtcNow.AddSeconds(15);while($game.MainWindowHandle -eq 0 -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100;$game.Refresh()};if($game.MainWindowHandle -eq 0){throw 'Settings game window readiness timeout'}
   $cmdArgs=@('--cef-dir',"`"$runtime`"",'--config',"`"$config`"",'--vosu-url','http://127.0.0.1:24050/','--pid',[string]$game.Id,'--process-name','vosulay-unused-benchmark-name');$hostProc=Start-Process -FilePath (Join-Path $runtime 'vosulay.exe') -ArgumentList $cmdArgs -PassThru -WorkingDirectory $runtime;$actors.Add($hostProc)
   $session=@{pid_host=$hostProc.Id;pid_game=$game.Id;game_window=[long]$game.MainWindowHandle;stop_file=$stopFile;config=$config};$session|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $settingsDir 'session.json') -Encoding utf8
   Write-Output "SETTINGS_READY game_pid=$($game.Id) game_hwnd=$($game.MainWindowHandle) host_pid=$($hostProc.Id) stop_file=$stopFile"
   while(-not(Test-Path -LiteralPath $stopFile)){$game.Refresh();$hostProc.Refresh();if($game.HasExited -or $hostProc.HasExited){throw 'Settings actor exited unexpectedly'};Start-Sleep -Milliseconds 250}
   StopOwnedTree $hostProc.Id;StopOwnedTree $game.Id
  } else {
  $orders=[System.Collections.Generic.List[object]]::new();for($r=0;$r -lt $Repeats;$r++){foreach($name in @('cef','browser')){$orders.Add([pscustomobject]@{repeat=$r;name=$name})}}
  $rng=[Random]::new(211);for($i=$orders.Count-1;$i -gt 0;$i--){$j=$rng.Next($i+1);$tmp=$orders[$i];$orders[$i]=$orders[$j];$orders[$j]=$tmp}
  foreach($case in $orders){
   $out=Join-Path $runRoot "$($case.repeat)-$($case.name)";New-Item -ItemType Directory -Path $out,(Join-Path $out 'cache')|Out-Null
   $config=Join-Path $out 'config.json';$cfg=@{version=1;tosu_migration_completed=$true;active_profile='benchmark';profiles=@(@{id='benchmark';name='Benchmark';overlays=@(@{counter_id='benchmark';name='Benchmark';url='http://127.0.0.1:24050/counter/index.html';left=64;top=64;width=480;height=160;scale=1;opacity=1;rotation=0;enabled=$true;z_index=1})});settings=@{remote_listen_host='127.0.0.1';remote_listen_port=25050}}|ConvertTo-Json -Depth 12
   [IO.File]::WriteAllText($config,$cfg,[Text.UTF8Encoding]::new($false))
   $game=Start-Process -FilePath (Join-Path $runtime 'windows_game.exe') -PassThru -WorkingDirectory $runtime;$actors.Add($game)
   $game.Refresh();$until=[DateTime]::UtcNow.AddSeconds(15);while($game.MainWindowHandle -eq 0 -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100;$game.Refresh()};if($game.MainWindowHandle -eq 0){throw 'D3D11 game window readiness timeout'};$gameHwnd=$game.MainWindowHandle
   Remove-Item -LiteralPath $capFile -Force -ErrorAction SilentlyContinue
   if($case.name -eq 'cef'){
    $cmdArgs=@('--cef-dir',"`"$runtime`"",'--config',"`"$config`"",'--vosu-url','http://127.0.0.1:24050/','--pid',[string]$game.Id,'--process-name','vosulay-unused-benchmark-name','--benchmark-uncapped')
    $hostProc=Start-Process -FilePath (Join-Path $runtime 'vosulay.exe') -ArgumentList $cmdArgs -PassThru -WorkingDirectory $runtime -RedirectStandardOutput (Join-Path $out 'host.log') -RedirectStandardError (Join-Path $out 'host.stderr.log')
    $targetHwnd=$gameHwnd
   } else {
    $cmdArgs=@("--cef-dir=$ref","--cache=$(Join-Path $out 'browser-cache')",'--url=http://127.0.0.1:24050/profile.html','--force-device-scale-factor=1')
    $hostProc=Start-Process -FilePath (Join-Path $ref 'autoresearch_browser_windows.exe') -ArgumentList $cmdArgs -PassThru -WorkingDirectory $ref -RedirectStandardOutput (Join-Path $out 'host.log') -RedirectStandardError (Join-Path $out 'host.stderr.log')
    $targetHwnd=0
   }
   $hostRoot=$hostProc.Id
   $actors.Add($hostProc);$until=[DateTime]::UtcNow.AddSeconds(30)
   do {
    $hostProc.Refresh()
    if($case.name -eq 'browser' -and $hostProc.MainWindowHandle -ne 0){$targetHwnd=$hostProc.MainWindowHandle}
    $caps=@()
    if(Test-Path -LiteralPath $capFile){$caps=@(Get-Content -LiteralPath $capFile|ForEach-Object{try{$_|ConvertFrom-Json}catch{}})}
    if($hostProc.HasExited){throw "$($case.name) host exited during readiness; inspect $out"}
    if([DateTime]::UtcNow -gt $until){throw "$($case.name) browser readiness timeout; inspect $out"}
    Start-Sleep -Milliseconds 100
   } while($targetHwnd -eq 0 -or $caps.Count -eq 0)
   $cap=$caps[-1];if($cap.userAgent -notmatch 'Chrome/152\.' -or $cap.webglRenderer -match 'SwiftShader|llvmpipe|software|unavailable'){throw "Pinned hardware Chromium mismatch: $($cap|ConvertTo-Json -Compress)"}
   if($case.name -eq 'browser'){
    $version=Get-Content -LiteralPath (Join-Path $out 'browser-cache\chromium-version.txt') -Raw
    if($version -ne '152.0.7977.134'){throw "Reference Chromium version mismatch: $version"}
   }
   Start-Sleep -Seconds $Warmup
   $cim=Get-CimInstance Win32_Process;$children=@{};foreach($p in $cim){if(-not$children.ContainsKey([int]$p.ParentProcessId)){$children[[int]$p.ParentProcessId]=[Collections.Generic.List[int]]::new()};$children[[int]$p.ParentProcessId].Add([int]$p.ProcessId)}
   function TreeCpu([int]$root,[hashtable]$children){$ids=[Collections.Generic.HashSet[int]]::new();$stack=[Collections.Generic.Stack[int]]::new();$stack.Push($root);while($stack.Count){$id=$stack.Pop();if($ids.Add($id)-and $children.ContainsKey($id)){foreach($child in $children[$id]){$stack.Push($child)}}};$sum=0.0;foreach($id in $ids){try{$sum+=(Get-Process -Id $id -ErrorAction Stop).TotalProcessorTime.TotalSeconds}catch{}};return $sum}
   $baseHost=TreeCpu $hostRoot $children;$baseGame=TreeCpu $game.Id $children;$sw=[Diagnostics.Stopwatch]::StartNew();$cpuSamples=[Collections.Generic.List[object]]::new();$oldH=$baseHost;$oldG=$baseGame;$dh=0.0;$dg=0.0
   # A double first argument avoids PowerShell selecting Max(int,int) and rounding CPU deltas.
   while($sw.Elapsed.TotalSeconds -lt $CpuSeconds){Start-Sleep -Milliseconds 200;if($hostProc.HasExited -or $game.HasExited){throw 'Measured actor exited during CPU sample'};$cim=Get-CimInstance Win32_Process;$children=@{};foreach($p in $cim){if(-not$children.ContainsKey([int]$p.ParentProcessId)){$children[[int]$p.ParentProcessId]=[Collections.Generic.List[int]]::new()};$children[[int]$p.ParentProcessId].Add([int]$p.ProcessId)};$nowH=TreeCpu $hostRoot $children;$nowG=TreeCpu $game.Id $children;$dh+=[Math]::Max(0.0,$nowH-$oldH);$dg+=[Math]::Max(0.0,$nowG-$oldG);$oldH=$nowH;$oldG=$nowG;$cpuSamples.Add([pscustomobject]@{elapsed=$sw.Elapsed.TotalSeconds;host_tree_cpu_seconds=$nowH-$baseHost;game_tree_cpu_seconds=$nowG-$baseGame})}
   $cpuElapsed=$sw.Elapsed.TotalSeconds;$cpuPct=$dh/$cpuElapsed*100;$gamePct=$dg/$cpuElapsed*100
   $csv=Join-Path $out 'visible-transitions.csv';$obslog=Join-Path $out 'observer.log'
   & (Join-Path $runtime 'windows_native.exe') observe ([string]$targetHwnd) ([string]$DeliverySeconds) $csv | Set-Content -LiteralPath $obslog -Encoding utf8
   if($LASTEXITCODE -ne 0){throw "DXGI marker observer failed ($LASTEXITCODE); inspect $obslog"}
   $obsline=Get-Content -Raw $obslog;$parsed=[regex]::Match($obsline,'observer_frames=(\d+) valid=(\d+) transitions=(\d+) skipped_ids=(\d+) elapsed=([\d.]+) observer_hz=([\d.]+) visible_updates_per_second=([\d.]+) max_gap_ms=([\d.]+) display=(\d+)x(\d+)');if(-not$parsed.Success){throw "Missing native observer evidence: $obsline"}
   $valid=[int64]$parsed.Groups[2].Value;$updates=[double]$parsed.Groups[7].Value;if($valid -lt 2 -or $updates -le 0){throw 'No checksum-valid changing post-composition pixels'}
   $visibleRows=@(Import-Csv -LiteralPath $csv);$skipped=[uint64]0;$previous=$null
   foreach($row in $visibleRows){
    if($row.valid -ne '1'){continue}
    $id=[uint64]$row.frame_id
    if($null -ne $previous){
     $delta=($id+4294967296-$previous)%4294967296
     if($delta -ge 2147483648){throw 'Raw displayed marker moved backwards'}
     if($delta -gt 1){$skipped+=$delta-1}
    }
    $previous=$id
   }
   if($skipped -ne [uint64]$parsed.Groups[4].Value){throw 'Native and raw marker evidence disagree about skipped frame IDs'}
   $runs.Add([pscustomobject]@{runtime=$case.name;repeat=$case.repeat;host_cpu_percent=$cpuPct;game_cpu_percent=$gamePct;cpu_elapsed=$cpuElapsed;delivery_elapsed=[double]$parsed.Groups[5].Value;updates_per_second=$updates;observer_hz=[double]$parsed.Groups[6].Value;max_gap_ms=[double]$parsed.Groups[8].Value;capture_resolution=@([int]$parsed.Groups[9].Value,[int]$parsed.Groups[10].Value);invalid_samples=([int64]$parsed.Groups[1].Value-$valid);skipped_ids=$skipped;cpu_samples=@($cpuSamples);capabilities=$cap;csv=$csv;marker_bmp="$csv.marker.bmp";observer_log=$obslog;command=($cmdArgs -join ' ')})
   $runs|ConvertTo-Json -Depth 20|Set-Content -LiteralPath (Join-Path $runRoot 'samples.json') -Encoding utf8
   StopOwnedTree $hostProc.Id;StopOwnedTree $game.Id
  }
  function Median([double[]]$v){$s=@($v|Sort-Object);$middle=[int][Math]::Floor($s.Count/2.0);if($s.Count%2){return $s[$middle]};return ($s[$middle-1]+$s[$middle])/2.0}
  $summary=@{}
  foreach($name in @('cef','browser')){
   $rows=@($runs|Where-Object runtime -eq $name)
   $cost=@($rows|ForEach-Object{$_.host_cpu_percent*10000/$_.updates_per_second})
   $summary[$name]=@{cpu_percent=(Median @($rows.host_cpu_percent));game_cpu_percent=(Median @($rows.game_cpu_percent));updates_per_second=(Median @($rows.updates_per_second));observer_hz=(Median @($rows.observer_hz));cpu_us_per_visible_update=(Median $cost)}
  }
  $metrics=@{cpu_us_per_visible_update=$summary.cef.cpu_us_per_visible_update;render_cost_ratio=$summary.cef.cpu_us_per_visible_update/$summary.browser.cpu_us_per_visible_update;visible_update_ratio=$summary.cef.updates_per_second/$summary.browser.updates_per_second}
  foreach($name in @('cef','browser')){
   foreach($key in @('cpu_percent','game_cpu_percent','updates_per_second','observer_hz')){$metrics["${name}_$key"]=$summary[$name][$key]}
  }
  foreach($key in $metrics.Keys){$value=[double]$metrics[$key];if([double]::IsNaN($value) -or [double]::IsInfinity($value) -or $value -le 0){throw "Nonfinite or empty benchmark metric: $key=$value"}}
  $counter=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\counter.html');$gameHash=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\windows_game.cpp');$observerHash=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\windows_native.cpp');$frozenGameHash=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\game.c');$frozenObserverHash=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\observe.c');$pixelsHash=Get-FileHash -Algorithm SHA256 (Join-Path $Root 'tools\autoresearch\pixels.py')
  Add-Type -AssemblyName System.Windows.Forms
  $gpu=$runs[0].capabilities.webglRenderer;$display=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $report=@{platform='windows-dxgi-desktop-duplication';cef_version=$pinDoc.cef_version;chromium_version='152.0.7977.134';cef_archive_sha256=$winPin.sha256;reference='windowed Alloy using pinned CEF bootstrap/client DLL';seed=211;resolution=@(640,480);counter_geometry=@(64,64,480,160);display=@($display.Width,$display.Height);capture_resolution=$runs[0].capture_resolution;gpu=$gpu;workload_sha256=$counter.Hash.ToLowerInvariant();frozen_game_source_sha256=$frozenGameHash.Hash.ToLowerInvariant();frozen_observer_source_sha256=$frozenObserverHash.Hash.ToLowerInvariant();pixel_decoder_sha256=$pixelsHash.Hash.ToLowerInvariant();windows_game_source_sha256=$gameHash.Hash.ToLowerInvariant();windows_observer_source_sha256=$observerHash.Hash.ToLowerInvariant();cpu_seconds=$CpuSeconds;delivery_seconds=$DeliverySeconds;warmup_seconds=$Warmup;runs=$runs;summary=$summary;metrics=$metrics;limitations=@('DXGI Desktop Duplication samples DWM-composed desktop output, not physical scanout or input latency.','Observed changing checksum-valid IDs are a lower bound; display/compositor and observer cadence limit visible update throughput.','CPU process-tree and pixel-delivery intervals are separate; observer activity is excluded from CPU attribution.','DXGI stages only the 48x8 marker ROI; BMP ROI and each sample CSV are retained.')}
  $report|ConvertTo-Json -Depth 20|Set-Content -LiteralPath $reportFile -Encoding utf8
  $output=[Collections.Generic.List[string]]::new();$output.Add('ASI platform=windows-dxgi-desktop-duplication');$output.Add("ASI cef_version=$($pinDoc.cef_version)");$output.Add('ASI chromium_version=152.0.7977.134');$output.Add("ASI gpu=$gpu");$output.Add("ASI display=$($display.Width)x$($display.Height)");$output.Add("ASI workload_sha256=$($counter.Hash.ToLowerInvariant())");$output.Add("ASI frozen_game_sha256=$($frozenGameHash.Hash.ToLowerInvariant())");$output.Add("ASI frozen_observer_sha256=$($frozenObserverHash.Hash.ToLowerInvariant())");$output.Add("ASI pixel_decoder_sha256=$($pixelsHash.Hash.ToLowerInvariant())");$output.Add("ASI windows_game_sha256=$($gameHash.Hash.ToLowerInvariant())");$output.Add("ASI windows_observer_sha256=$($observerHash.Hash.ToLowerInvariant())");$output.Add('ASI capture=DXGI_DWM_composed_lower_bound');foreach($key in $metrics.Keys){$output.Add(('METRIC {0}={1:F6}' -f $key,[double]$metrics[$key]))};$output.Add("REPORT $reportPath");$output|Set-Content -LiteralPath $log -Encoding utf8;Get-Content $log
 }
} catch {$exitCode=1;[IO.File]::AppendAllText($log,($_|Out-String)+[Environment]::NewLine);throw}
finally{
  foreach($p in $actors){try{StopOwnedTree $p.Id}catch{}}
  if($job){
   if($job.State -eq 'Running'){
    try{$wake=[Net.WebRequest]::Create('http://127.0.0.1:24050/__autoresearch_stop__');$wake.Timeout=5000;$wake.GetResponse().Dispose()}catch{}
   }
   Receive-Job $job -ErrorAction Continue | Out-String | ForEach-Object{[IO.File]::AppendAllText($log,$_)}
   Stop-Job $job -ErrorAction SilentlyContinue;Remove-Job $job -Force -ErrorAction SilentlyContinue
  }
  foreach($caseDir in Get-ChildItem -LiteralPath $runRoot -Directory){
   foreach($cacheName in @('cef-cache','browser-cache')){
    $cefLog=Join-Path $caseDir.FullName "$cacheName\cef.log"
    if(Test-Path -LiteralPath $cefLog){Copy-Item -LiteralPath $cefLog -Destination (Join-Path $caseDir.FullName 'cef.log') -Force}
   }
  }
  & tar.exe -czf "$runRoot.tar.gz" --exclude cache --exclude cef-cache --exclude browser-cache -C $runRoot .
  if($LASTEXITCODE){$exitCode=1;[IO.File]::AppendAllText($log,"Evidence archive failed: $LASTEXITCODE`n")}
  $result=@{exit_code=$exitCode;report_path=$reportPath;log_path=$log};$result|ConvertTo-Json|Set-Content -LiteralPath $receiptOut -Encoding utf8
 }
}
