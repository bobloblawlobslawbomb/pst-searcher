$ErrorActionPreference = "SilentlyContinue"
function Line($k,$v){ "{0,-22} {1}" -f $k, $v }

"===== HOST ====="
$os = Get-CimInstance Win32_OperatingSystem
Line "OS" ("{0} build {1}" -f $os.Caption, $os.BuildNumber)
Line "CPU" ((Get-CimInstance Win32_Processor).Name)
Line "RAM_GB" ([math]::Round($os.TotalVisibleMemorySize/1MB,1))
Line "FreeRAM_GB" ([math]::Round($os.FreePhysicalMemory/1MB,1))
Line "Arch" $env:PROCESSOR_ARCHITECTURE
Get-PSDrive -PSProvider FileSystem | Where-Object {$_.Free -ne $null} | ForEach-Object {
  Line ("drive_" + $_.Name) ("{0} GB free of {1} GB" -f [math]::Round($_.Free/1GB,1), [math]::Round(($_.Free+$_.Used)/1GB,1))
}

"`n===== RUNTIMES ====="
Line "powershell" $PSVersionTable.PSVersion
foreach ($c in "python","python3","py","pip","node","npm","dotnet","cargo","rustc","git","winget","code") {
  $cmd = Get-Command $c -ErrorAction SilentlyContinue
  if ($cmd) {
    $ver = ""
    try { $ver = (& $c --version 2>&1 | Select-Object -First 1) } catch {}
    Line $c ("{0} | {1}" -f $cmd.Source, $ver)
  } else { Line $c "NOT FOUND" }
}

"`n===== .NET ====="
$ndp = Get-ChildItem "HKLM:\SOFTWARE\Microsoft\NET Framework Setup\NDP\v4\Full" -ErrorAction SilentlyContinue
if ($ndp) { Line ".NET Framework" ((Get-ItemProperty $ndp.PSPath).Version + "  release=" + (Get-ItemProperty $ndp.PSPath).Release) }
$d = Get-Command dotnet -ErrorAction SilentlyContinue
if ($d) { "  -- dotnet --list-runtimes --"; (& dotnet --list-runtimes) | ForEach-Object { "     $_" } }
else { Line "dotnet SDK/runtime" "NOT INSTALLED (framework-dependent .NET apps won't run)" }

"`n===== WEBVIEW2 (needed by Tauri / pywebview) ====="
$wv = Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
if (-not $wv) { $wv = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue }
if ($wv) { Line "WebView2" ("v" + $wv.pv) } else { Line "WebView2" "NOT FOUND" }
$edge = Get-Command msedge -ErrorAction SilentlyContinue
$edgePath = "not found"
if ($edge) { $edgePath = $edge.Source }
else {
  $ef = Get-Item "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" -ErrorAction SilentlyContinue
  if ($ef) { $edgePath = $ef.FullName }
}
Line "Edge" $edgePath

"`n===== Python detail (for a PyInstaller build) ====="
$py = Get-Command python -ErrorAction SilentlyContinue
if ($py) {
  (& python -c "import sys,platform;print('  exe:',sys.executable);print('  ver:',platform.python_version());print('  arch:',platform.machine())")
  (& python -c "import venv,ensurepip,sqlite3;c=sqlite3.connect(':memory:');print('  sqlite:',sqlite3.sqlite_version);print('  fts5:',bool(c.execute(\"select 1 from pragma_compile_options where compile_options like '%FTS5%'\").fetchone()))" 2>&1 | ForEach-Object { "  $_" })
} else { "  python not on PATH" }
$py_launcher = Get-Command py -ErrorAction SilentlyContinue
if ($py_launcher) { "  -- py -0p --"; (& py -0p) | ForEach-Object { "     $_" } }

"`n===== MSVC runtime (pypff / SQLite native deps) ====="
$vc = Get-ChildItem "HKLM:\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" -ErrorAction SilentlyContinue
if ($vc) { Line "VC++ x64 runtime" (Get-ItemProperty $vc.PSPath).Version } else { Line "VC++ x64 runtime" "not detected in that key" }
