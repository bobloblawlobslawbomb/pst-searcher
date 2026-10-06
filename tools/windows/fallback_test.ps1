$ErrorActionPreference = "Continue"
$dir = "C:\Users\chris\psttest"
$ft = Join-Path $dir "fallbacktest"
$log = Join-Path $dir "fallback.log"
function Say($m) { $m | Out-File -FilePath $log -Encoding utf8 -Append }
"=== fallback test $(Get-Date -Format o) ===" | Out-File -FilePath $log -Encoding utf8

$local = Join-Path $env:LOCALAPPDATA "PstSearcher"
Say "clearing a previous fallback index if present: $local"
Remove-Item $local -Recurse -Force -ErrorAction SilentlyContinue

# Simulate "the program folder is not writable" without touching ACLs: create a DIRECTORY
# where the default database file would go. SQLite cannot open it, so the fallback must kick in.
Remove-Item $ft -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $ft | Out-Null
Copy-Item (Join-Path $dir "pst-searcher.exe") $ft
New-Item -ItemType Directory -Force -Path (Join-Path $ft "pst-index.db") | Out-Null
Say "--- simulated non-writable program folder ---"
Get-ChildItem $ft | ForEach-Object { Say ("  " + $_.Name + $(if ($_.PSIsContainer) { "  <DIR>" } else { "  " + $_.Length + " bytes" })) }

$exe = Join-Path $ft "pst-searcher.exe"
Say "launching from that folder with no --db"
$p = Start-Process -FilePath $exe -WorkingDirectory $ft -PassThru
Start-Sleep -Seconds 16
$p.Refresh()
Say ("after launch: pid=" + $p.Id + " hwnd=" + $p.MainWindowHandle + " title='" + $p.MainWindowTitle + "' responding=" + $p.Responding)

Say "--- did it fall back to %LOCALAPPDATA%\PstSearcher? ---"
if (Test-Path (Join-Path $local "pst-index.db")) {
    Say ("  YES: " + (Join-Path $local "pst-index.db") + "  " + (Get-Item (Join-Path $local "pst-index.db")).Length + " bytes")
} else {
    Say "  NO fallback index found"
}
Say "--- any console window? (a Windows-subsystem build should have none) ---"
$conhost = Get-Process conhost -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -gt (Get-Date).AddMinutes(-1) }
if ($conhost) { Say ("  conhost started recently: " + ($conhost.Id -join ",")) } else { Say "  no new console host process" }

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.X, $b.Y, 0, 0, $b.Size)
$png = Join-Path $dir "fallback.png"
$bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Say ("screenshot -> " + (Get-Item $png).Length + " bytes")

$p.CloseMainWindow() | Out-Null
Start-Sleep -Seconds 5
if (Get-Process -Id $p.Id -ErrorAction SilentlyContinue) { Say "RESULT: still running after close"; Stop-Process -Id $p.Id -Force } else { Say "RESULT: exited cleanly on close" }
Say "=== done ==="
