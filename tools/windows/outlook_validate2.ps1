param([string]$Pst)
$ErrorActionPreference = "Continue"
$dir = "C:\Users\chris\psttest"
$tag = [System.IO.Path]::GetFileNameWithoutExtension($Pst)
$log = Join-Path $dir "ov_$tag.log"
function Say($m) { ((Get-Date -Format HH:mm:ss) + "  " + $m) | Out-File -FilePath $log -Encoding utf8 -Append }
"=== validate $Pst $(Get-Date -Format o) ===" | Out-File -FilePath $log -Encoding utf8

Say "creating Outlook COM object"
try { $ol = New-Object -ComObject Outlook.Application } catch { Say "COM create failed: $($_.Exception.Message)"; exit 1 }
Say "COM ok, version $($ol.Version)"

$ns = $ol.GetNamespace("MAPI")
Say "MAPI namespace ok, stores before: $($ns.Stores.Count)"

Say "calling AddStore (this is the moment of truth)"
$t0 = Get-Date
try {
    $ns.AddStore($Pst)
    Say ("AddStore returned after {0:N1}s" -f ((Get-Date) - $t0).TotalSeconds)
} catch {
    Say ("AddStore THREW after {0:N1}s: {1}: {2}" -f ((Get-Date) - $t0).TotalSeconds, $_.Exception.GetType().Name, $_.Exception.Message)
    Say ("  inner: " + $_.Exception.InnerException.Message)
    try { $ol.Quit() } catch {}
    exit 2
}

$target = $null
foreach ($s in $ns.Stores) { try { if ($s.FilePath -eq $Pst) { $target = $s } } catch {} }
if (-not $target) {
    Say "store not matched by FilePath; stores now:"
    foreach ($s in $ns.Stores) { try { Say ("   " + $s.DisplayName + " | " + $s.FilePath) } catch { Say ("   " + $s.DisplayName) } }
    try { $ol.Quit() } catch {}
    exit 3
}

Say ("opened: DisplayName='" + $target.DisplayName + "'")
$script:total = 0
$script:folders = 0
function Walk($folder, $prefix) {
    $script:total += $folder.Items.Count
    if ($folder.Items.Count -gt 0) { $script:folders += 1 }
    Say ("  " + $prefix + " items=" + $folder.Items.Count)
    foreach ($sub in $folder.Folders) { Walk $sub ($prefix + "/" + $sub.Name) }
}
Say "walking the store with Outlook"
Walk $target.GetRootFolder() ""
Say ("TOTAL items Outlook can read: " + $script:total)

$script:found = $null
function FirstItem($folder) {
    if ($script:found) { return }
    if ($folder.Items.Count -gt 0) { $script:found = $folder.Items.Item(1); return }
    foreach ($sub in $folder.Folders) { FirstItem $sub; if ($script:found) { return } }
}
FirstItem $target.GetRootFolder()
if ($script:found) {
    Say ("sample: subject='" + $script:found.Subject + "'")
    try { Say ("  from='" + $script:found.SenderName + "' to='" + $script:found.To + "' class=" + $script:found.MessageClass) } catch { Say "  (property read failed)" }
    try { Say ("  recipients=" + $script:found.Recipients.Count + " attachments=" + $script:found.Attachments.Count) } catch {}
}

$ns.RemoveStore($target.GetRootFolder())
Say "RemoveStore done"
try { $ol.Quit() } catch {}
Say "=== done ==="
