$ErrorActionPreference = "Continue"
$dir = "C:\Users\chris\psttest"
$portable = Join-Path $dir "portable"
$log = Join-Path $dir "portable.log"
function Say($m) { $m | Out-File -FilePath $log -Encoding utf8 -Append }
"=== standalone test $(Get-Date -Format o) ===" | Out-File -FilePath $log -Encoding utf8

# folder containing ONLY the exe: if it starts, nothing local is needed
Remove-Item $portable -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $portable | Out-Null
Copy-Item (Join-Path $dir "gui.exe") $portable
Say "--- contents of the isolated folder before launch ---"
Get-ChildItem $portable | ForEach-Object { Say ("  " + $_.Name + "  " + $_.Length + " bytes") }

$exe = Join-Path $portable "gui.exe"
Say "launching from the isolated folder (no --db argument, so it uses its own folder)"
$p = Start-Process -FilePath $exe -WorkingDirectory $portable -PassThru
Start-Sleep -Seconds 16
$p.Refresh()
Say ("after launch: pid=" + $p.Id + " hwnd=" + $p.MainWindowHandle + " title='" + $p.MainWindowTitle + "' responding=" + $p.Responding)

Say "--- files in the isolated folder after launch ---"
Get-ChildItem $portable | ForEach-Object { Say ("  " + $_.Name + "  " + $_.Length + " bytes") }

$sent = $p.CloseMainWindow()
Say ("CloseMainWindow() returned: $sent")
Start-Sleep -Seconds 6
$alive = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
if ($alive) {
    Say "RESULT: still running after close"
    Stop-Process -Id $alive.Id -Force
} else {
    Say "RESULT: exited cleanly on close"
}

# failure mode when the folder is not writable: point --db at a path that cannot exist
Say "--- unwritable index path (failure mode) ---"
$bad = Join-Path $portable "nodir\sub\index.db"
$err = Join-Path $dir "badpath.err.txt"
$pb = Start-Process -FilePath $exe -ArgumentList @("--db", $bad, "--counts") -PassThru -Wait `
      -RedirectStandardOutput (Join-Path $dir "badpath.out.txt") -RedirectStandardError $err
Say ("exited with code " + $pb.ExitCode)
foreach ($f in @((Join-Path $dir "badpath.out.txt"), $err)) {
    if (Test-Path $f) { Say ("--- " + (Split-Path $f -Leaf) + " ---"); Get-Content $f | Select-Object -First 6 | ForEach-Object { Say ("  " + $_) } }
}
Say "=== done ==="
