$ErrorActionPreference = "Continue"
$dir = "C:\Users\chris\psttest"
$log = Join-Path $dir "launch.log"
function Say($m) { $m | Out-File -FilePath $log -Encoding utf8 -Append }

"=== launch $(Get-Date -Format o) ===" | Out-File -FilePath $log -Encoding utf8
Say "session: $((Get-Process -Id $PID).SessionId)  user: $env:USERNAME  interactive: $([Environment]::UserInteractive)"

$exe = Join-Path $dir "gui.exe"
$db  = Join-Path $dir "gui-index.db"
Get-Process gui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2
Start-Process -FilePath $exe -ArgumentList @($db, "--query", "Alpha")
Start-Sleep -Seconds 12

$p = Get-Process gui -ErrorAction SilentlyContinue | Select-Object -First 1
if ($p) {
  Say ("RUNNING pid={0} session={1} hwnd={2} title='{3}' ws={4}MB" -f $p.Id, $p.SessionId, $p.MainWindowHandle, $p.MainWindowTitle, [math]::Round($p.WorkingSet64/1MB,1))
} else {
  Say "NOT RUNNING after 12s"
  Get-Process | Where-Object { $_.ProcessName -like "*gui*" } | ForEach-Object { Say "  candidate: $($_.ProcessName) $($_.Id)" }
}

# screenshot of the interactive desktop
try {
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type -AssemblyName System.Drawing
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.X, $b.Y, 0, 0, $b.Size)
  $png = Join-Path $dir "screen.png"
  $bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  Say ("screenshot saved {0}x{1} -> {2} bytes" -f $b.Width, $b.Height, (Get-Item $png).Length)
} catch {
  Say "screenshot failed: $($_.Exception.Message)"
}
Say "=== done ==="
