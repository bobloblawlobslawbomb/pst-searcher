$ErrorActionPreference = "Continue"
$dir = "C:\Users\chris\psttest"
$log = Join-Path $dir "close_test.log"
function Say($m) { $m | Out-File -FilePath $log -Encoding utf8 -Append }
"=== close test $(Get-Date -Format o) ===" | Out-File -FilePath $log -Encoding utf8

Get-Process gui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

$exe = Join-Path $dir "gui.exe"
$db  = Join-Path $dir "ui3.db"
Say "launching"
$p = Start-Process -FilePath $exe -PassThru -ArgumentList @($db, "--query", "settlement AND audit", "--select-first")
Start-Sleep -Seconds 14
$p.Refresh()
Say ("after launch: pid=" + $p.Id + " hwnd=" + $p.MainWindowHandle + " title='" + $p.MainWindowTitle + "'")
Say ("responding=" + $p.Responding + " ws_MB=" + [math]::Round($p.WorkingSet64/1MB,1))

# send WM_CLOSE, exactly what clicking the X does
$sent = $p.CloseMainWindow()
Say ("CloseMainWindow() returned: " + $sent)
Start-Sleep -Seconds 6

$alive = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
if ($alive) {
    $alive.Refresh()
    Say ("RESULT: STILL RUNNING after close - pid=" + $alive.Id + " hwnd=" + $alive.MainWindowHandle + " responding=" + $alive.Responding)
    Say "RESULT: fix NOT working - killing it now"
    Stop-Process -Id $alive.Id -Force
} else {
    Say "RESULT: process exited cleanly on window close - fix verified"
}
Say "=== done ==="
