<#
3ds-setup (Windows): install Azahar (3DS emulator) and keep your saves backed up.

  install    download/update Azahar, pick a backup folder, start the background watcher
  watch      background loop: while Azahar runs, back up saves and auto save-state
  backup     back up right now
  restore    pick a backup to bring back (or: restore <number> -Game <ID>)
  status     show where everything is
  uninstall  remove Azahar and 3ds-setup (saves and backups are kept)

Works on the PowerShell 5.1 that ships with Windows; nothing else to install.
#>
param(
    [Parameter(Position = 0)][string]$Command = 'status',
    [Parameter(Position = 1)][string]$Snapshot,
    [string]$Game,
    [string]$BackupDir,
    [ValidateSet('', 'yes', 'no')][string]$Controller = '',
    [switch]$Quiet,  # no dialogs (for scripts/CI)
    [switch]$Yes,    # uninstall without asking
    [switch]$Pause
)
if ($env:CI) { $Quiet = $true }
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Invoke-WebRequest is very slow with the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

$App = '3ds-setup'
$ReleaseApi = 'https://api.github.com/repos/azahar-emu/azahar/releases/latest'
$OurRelease = 'https://github.com/erickfm/3ds-setup/releases/latest/download'
$InstallDir = "$env:LOCALAPPDATA\$App"
$AzaharDir = "$env:LOCALAPPDATA\Programs\Azahar"
$AzaharExe = "$AzaharDir\azahar.exe"
$VersionFile = "$AzaharDir\3ds-setup-version.txt"
$ConfigPath = "$env:APPDATA\$App\config.json"
$LogPath = "$InstallDir\3ds-setup.log"
$TitlesPath = "$InstallDir\titles.json"
$UninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\3ds-setup'
# Used when the backup folder is missing (e.g. external drive unplugged); moved over once it's back.
$FallbackDir = "$InstallDir\pending-backups"
$Host_ = $env:COMPUTERNAME
$TickSeconds = 15

function Get-KnownFolder($name, $fallback) {
    $path = [Environment]::GetFolderPath($name)
    if ($path) { return $path }
    return (Join-Path $env:USERPROFILE $fallback)
}

$Defaults = [ordered]@{
    backup_dir                   = "$env:USERPROFILE\3DS-Backups"
    games_dir                    = (Join-Path (Get-KnownFolder 'MyDocuments' 'Documents') '3DS Games')
    backup_every_minutes         = 5
    autosave_state_every_minutes = 10       # 0 turns auto save-states off
    autosave_hotkey              = 'ctrl+c' # Azahar default for "Save to Oldest Non-Quicksave Slot"
    keep_save_snapshots          = 200      # plus the last snapshot of every day, kept forever
    keep_states_per_game         = 30
}

Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class EmuWin {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    public static string ForegroundTitle() {
        var sb = new StringBuilder(512);
        GetWindowText(GetForegroundWindow(), sb, 512);
        return sb.ToString();
    }
}
'@

# ---------------------------------------------------------------- helpers

function Log($msg) {
    $line = '{0}  {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
    Write-Host $line
    try {
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        Add-Content -LiteralPath $LogPath -Value $line
    } catch {}
}

$script:TrayIcon = $null
function Notify($msg) {
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        if (-not $script:TrayIcon) {
            $script:TrayIcon = New-Object System.Windows.Forms.NotifyIcon
            $script:TrayIcon.Icon = [System.Drawing.SystemIcons]::Warning
            $script:TrayIcon.Text = '3DS backups'
        }
        $script:TrayIcon.Visible = $true
        $script:TrayIcon.ShowBalloonTip(15000, '3DS backups', $msg, 'Warning')
    } catch {}
}

function Get-Config {
    $cfg = [ordered]@{}
    foreach ($k in $Defaults.Keys) { $cfg[$k] = $Defaults[$k] }
    if (Test-Path -LiteralPath $ConfigPath) {
        $saved = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
        foreach ($p in $saved.PSObject.Properties) { $cfg[$p.Name] = $p.Value }
    }
    return $cfg
}

function Save-Config($cfg) {
    New-Item -ItemType Directory -Force -Path (Split-Path $ConfigPath) | Out-Null
    $cfg | ConvertTo-Json | Set-Content -LiteralPath $ConfigPath -Encoding UTF8
}

function Get-AzaharUserDir {
    # Same rule as Azahar itself: a 'user' folder next to the exe means portable mode.
    if (Test-Path -LiteralPath "$AzaharDir\user") { return "$AzaharDir\user" }
    return "$env:APPDATA\Azahar"
}

function Test-AzaharRunning {
    return [bool](Get-Process -Name azahar -ErrorAction SilentlyContinue)
}

function Open-Shared($path) {
    # FileShare.ReadWrite so we can still read a save Azahar has open.
    return New-Object IO.FileStream($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
}

function Copy-Atomic($src, $dest) {
    New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
    $tmp = "$dest.partial"
    Copy-Item -LiteralPath $src -Destination $tmp -Force
    Move-Item -LiteralPath $tmp -Destination $dest -Force
}

# ---------------------------------------------------------------- what counts as a save

function Test-SavePath($rel) {
    # rel: path relative to Azahar's user dir, with '/'. True for in-game save data and
    # config only (not game content, which can be gigabytes).
    $p = $rel.Split('/')
    if ($p[0] -eq 'config') { return $true }
    if ($p[0] -eq 'nand') { return ($p.Count -gt 2 -and $p[1] -eq 'data') }
    if ($p.Count -gt 5 -and $p[0] -eq 'sdmc' -and $p[1] -eq 'Nintendo 3DS') {
        # sdmc/Nintendo 3DS/<id0>/<id1>/title/<hi>/<lo>/data/...
        if ($p[4] -eq 'extdata') { return $true }
        if ($p[4] -eq 'title') { return ($p.Count -gt 8 -and $p[7] -eq 'data') }
    }
    return $false
}

function Get-SaveContainer($rel) {
    # The folder a restore should wipe before extracting rel, so a game's save is restored whole.
    $p = $rel.Split('/')
    if ($p[0] -eq 'sdmc' -and $p.Count -gt 8 -and $p[4] -eq 'title') { return ($p[0..7] -join '/') }
    if ($p[0] -eq 'sdmc' -and $p.Count -gt 7 -and $p[4] -eq 'extdata') { return ($p[0..6] -join '/') }
    return $null
}

function Get-TitleOf($rel) {
    $p = $rel.Split('/')
    if ($p[0] -eq 'sdmc' -and $p.Count -gt 6 -and ($p[4] -eq 'title' -or $p[4] -eq 'extdata')) {
        return ($p[5] + $p[6]).ToUpper()
    }
    return $null
}

function Get-SaveFiles {
    $root = Get-AzaharUserDir
    $files = @()
    foreach ($top in 'sdmc', 'nand', 'config') {
        $dir = Join-Path $root $top
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        foreach ($f in Get-ChildItem -LiteralPath $dir -Recurse -File -Force) {
            $rel = $f.FullName.Substring($root.Length).TrimStart('\').Replace('\', '/')
            if (Test-SavePath $rel) { $files += [pscustomobject]@{ Rel = $rel; Path = $f.FullName } }
        }
    }
    return @($files | Sort-Object Rel)
}

# ---------------------------------------------------------------- backups

function Get-BackupRoot($cfg) {
    # Never create the base folder here: if an external drive is unplugged we'd
    # write to the wrong place instead of noticing.
    if (Test-Path -LiteralPath $cfg.backup_dir -PathType Container) {
        return @{ Path = (Join-Path $cfg.backup_dir $Host_); OnTarget = $true }
    }
    return @{ Path = (Join-Path $FallbackDir $Host_); OnTarget = $false }
}

function Move-Fallback($cfg) {
    if (-not (Test-Path -LiteralPath $FallbackDir) -or -not (Test-Path -LiteralPath $cfg.backup_dir -PathType Container)) { return }
    foreach ($f in Get-ChildItem -LiteralPath $FallbackDir -Recurse -File) {
        Copy-Atomic $f.FullName (Join-Path $cfg.backup_dir $f.FullName.Substring($FallbackDir.Length).TrimStart('\'))
    }
    Remove-Item -LiteralPath $FallbackDir -Recurse -Force
    Log "moved pending backups into $($cfg.backup_dir)"
}

function Backup-Saves($cfg, [switch]$Force, [switch]$NoPrune) {
    $files = Get-SaveFiles
    if (-not ($files | Where-Object { -not $_.Rel.StartsWith('config/') })) {
        Log 'no save data yet, nothing to back up'
        return $null
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    $sep = [byte[]]@(0)
    $buf = New-Object byte[] 1048576
    foreach ($f in $files) {
        $name = [Text.Encoding]::UTF8.GetBytes($f.Rel)
        [void]$sha.TransformBlock($name, 0, $name.Length, $null, 0)
        [void]$sha.TransformBlock($sep, 0, 1, $null, 0)
        $s = Open-Shared $f.Path
        try {
            while (($n = $s.Read($buf, 0, $buf.Length)) -gt 0) { [void]$sha.TransformBlock($buf, 0, $n, $null, 0) }
        } finally { $s.Dispose() }
        [void]$sha.TransformBlock($sep, 0, 1, $null, 0)
    }
    [void]$sha.TransformFinalBlock($sep, 0, 0)
    $digest = -join ($sha.Hash | ForEach-Object { $_.ToString('x2') })

    $dest = Join-Path (Get-BackupRoot $cfg).Path 'saves'
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    $lastHash = Join-Path $dest 'last-hash.txt'
    if (-not $Force -and (Test-Path -LiteralPath $lastHash) -and (Get-Content -LiteralPath $lastHash -Raw).Trim() -eq $digest) {
        return $null
    }

    $stamp = Get-Date -Format 'yyyy-MM-dd_HH-mm-ss'
    $final = Join-Path $dest "$stamp.zip"
    $n = 2
    while (Test-Path -LiteralPath $final) {  # two snapshots in the same second must not overwrite each other
        $final = Join-Path $dest "${stamp}_$n.zip"; $n++
    }
    $tmp = "$final.partial"
    $zip = [IO.Compression.ZipFile]::Open($tmp, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($f in $files) {
            $entry = $zip.CreateEntry($f.Rel, [IO.Compression.CompressionLevel]::Optimal)
            $out = $entry.Open(); $in = Open-Shared $f.Path
            try { $in.CopyTo($out) } finally { $in.Dispose(); $out.Dispose() }
        }
    } finally { $zip.Dispose() }
    # Verify: every entry must read back fully.
    $zip = [IO.Compression.ZipFile]::OpenRead($tmp)
    try {
        foreach ($e in $zip.Entries) { $s = $e.Open(); try { $s.CopyTo([IO.Stream]::Null) } finally { $s.Dispose() } }
    } finally { $zip.Dispose() }
    Move-Item -LiteralPath $tmp -Destination $final
    Set-Content -LiteralPath $lastHash -Value $digest
    Log "saved snapshot $final"

    if (-not $NoPrune) { Remove-OldSnapshots $dest $cfg.keep_save_snapshots }
    return $final
}

function Remove-OldSnapshots($dest, $keep) {
    # Keep the newest $keep snapshots, plus the last one of every day forever
    # (saves are tiny, and corruption can go unnoticed for weeks).
    $snaps = @(Get-ChildItem -LiteralPath $dest -Filter '*.zip' | Sort-Object Name)
    $lastOfDay = @{}
    foreach ($s in $snaps) { $lastOfDay[$s.Name.Substring(0, 10)] = $s.Name }  # names start with yyyy-MM-dd
    for ($i = 0; $i -lt $snaps.Count - $keep; $i++) {
        if ($lastOfDay[$snaps[$i].Name.Substring(0, 10)] -ne $snaps[$i].Name) { Remove-Item -LiteralPath $snaps[$i].FullName -Force }
    }
}

function Backup-States($cfg) {
    $states = Join-Path (Get-AzaharUserDir) 'states'
    if (-not (Test-Path -LiteralPath $states)) { return }
    $dest = Join-Path (Get-BackupRoot $cfg).Path 'states'
    $indexPath = Join-Path $dest 'index.json'
    $index = @{}
    if (Test-Path -LiteralPath $indexPath) {
        (Get-Content -LiteralPath $indexPath -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $index[$_.Name] = $_.Value }
    }
    $copied = @{}
    foreach ($f in Get-ChildItem -LiteralPath $states -Filter '*.cst' | Sort-Object Name) {
        if (((Get-Date) - $f.LastWriteTime).TotalSeconds -lt 5) { continue }  # may still be writing
        $key = "$($f.Length)|$($f.LastWriteTimeUtc.Ticks)"
        if ($index[$f.Name] -eq $key) { continue }
        $parts = $f.Name.Split('.')
        $title = $parts[0]; $slot = $parts[$parts.Count - 2]
        $out = Join-Path (Join-Path $dest $title) ('{0}_slot{1}.cst' -f $f.LastWriteTime.ToString('yyyy-MM-dd_HH-mm-ss'), $slot)
        Copy-Atomic $f.FullName $out
        $index[$f.Name] = $key
        $copied[$title] = $true
        Log "saved state $out"
    }
    foreach ($title in $copied.Keys) {
        $versions = @(Get-ChildItem -LiteralPath (Join-Path $dest $title) -Filter '*.cst' | Sort-Object Name)
        if ($versions.Count -gt $cfg.keep_states_per_game) {
            $versions[0..($versions.Count - $cfg.keep_states_per_game - 1)] | Remove-Item -Force
        }
    }
    if ($copied.Count) { $index | ConvertTo-Json | Set-Content -LiteralPath $indexPath -Encoding UTF8 }
}

$script:WarnedFallback = $false
function Backup-All($cfg, $reason) {
    try {
        Move-Fallback $cfg
        $root = Get-BackupRoot $cfg
        if (-not $root.OnTarget -and -not $script:WarnedFallback) {
            Notify "Backup folder $($cfg.backup_dir) isn't available (drive unplugged?). Backing up locally until it's back."
            Log "backup folder missing, using $FallbackDir"
            $script:WarnedFallback = $true
        } elseif ($root.OnTarget) {
            $script:WarnedFallback = $false
        }
        [void](Backup-Saves $cfg)
        Backup-States $cfg
    } catch {
        # keep the watcher alive no matter what
        Log "BACKUP FAILED ($reason): $_"
        Notify "3DS backup failed: $_"
    }
}

# ---------------------------------------------------------------- auto save-state

function ConvertTo-VirtualKeys($hotkey) {
    $map = @{ ctrl = 0x11; control = 0x11; shift = 0x10; alt = 0x12 }
    $keys = @()
    foreach ($k in $hotkey.ToLower().Split('+')) {
        if ($map.ContainsKey($k)) { $keys += $map[$k] }
        elseif ($k -match '^f([0-9]{1,2})$') { $keys += 0x6F + [int]$Matches[1] }
        elseif ($k.Length -eq 1) { $keys += [int][char]$k.ToUpper() }
        else { throw "can't understand hotkey part '$k'" }
    }
    return $keys
}

function Invoke-Autosave($cfg) {
    # Press Azahar's save-state hotkey, but only while an Azahar game window is focused.
    $title = [EmuWin]::ForegroundTitle()
    # Azahar's title is "Azahar <version> | <game>" only while a game is loaded.
    if (-not ($title.StartsWith('Azahar ') -and $title.Contains(' | '))) { return $false }
    $keys = ConvertTo-VirtualKeys $cfg.autosave_hotkey
    foreach ($k in $keys) { [EmuWin]::keybd_event([byte]$k, 0, 0, [UIntPtr]::Zero) }
    [array]::Reverse($keys)
    foreach ($k in $keys) { [EmuWin]::keybd_event([byte]$k, 0, 2, [UIntPtr]::Zero) }
    Log "auto save-state ($title)"
    return $true
}

# ---------------------------------------------------------------- commands

function Invoke-Watch {
    $mutex = New-Object Threading.Mutex($false, 'Local\3ds-setup-watch')
    if (-not $mutex.WaitOne(0)) { Write-Host 'watcher already running'; return }
    Log 'watcher started'
    $wasRunning = $false
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $lastBackup = 0; $lastAutosave = 0
    while ($true) {
        $cfg = Get-Config  # pick up edits without a restart
        $running = Test-AzaharRunning
        $now = $clock.Elapsed.TotalSeconds
        if ($running -and -not $wasRunning) {
            Log 'Azahar started'
            Backup-All $cfg 'start'
            $lastBackup = $now; $lastAutosave = $now
        } elseif ($running) {
            $every = [double]$cfg.autosave_state_every_minutes
            # If the game isn't focused this keeps retrying each tick, so the
            # save happens as soon as you're back in the game.
            if ($every -gt 0 -and $now - $lastAutosave -ge $every * 60) {
                try {
                    if (Invoke-Autosave $cfg) {
                        $lastAutosave = $now
                        # back up ~20s later, once the state file is written
                        $lastBackup = $now - [double]$cfg.backup_every_minutes * 60 + 20
                    }
                } catch { Log "auto save-state failed: $_" }
            }
            if ($now - $lastBackup -ge [double]$cfg.backup_every_minutes * 60) {
                Backup-All $cfg 'periodic'
                $lastBackup = $now
            }
        } elseif ($wasRunning) {
            Log 'Azahar closed'
            Start-Sleep -Seconds 3
            Backup-All $cfg 'exit'
        }
        $wasRunning = $running
        Start-Sleep -Seconds $TickSeconds
    }
}

# ---------------------------------------------------------------- dialogs
# Everything a normal person sees is a regular Windows dialog, not the console.

function Get-TopWindow {
    Add-Type -AssemblyName System.Windows.Forms
    return (New-Object System.Windows.Forms.Form -Property @{ TopMost = $true })
}

function Show-Message($text, [switch]$Warning) {
    if ($Quiet) { Write-Host $text; return }
    Add-Type -AssemblyName System.Windows.Forms
    $icon = if ($Warning) { 'Warning' } else { 'Information' }
    [void][System.Windows.Forms.MessageBox]::Show((Get-TopWindow), $text, '3DS Setup', 'OK', $icon)
}

function Confirm-Choice($question) {
    Add-Type -AssemblyName System.Windows.Forms
    return [System.Windows.Forms.MessageBox]::Show((Get-TopWindow), $question, '3DS Setup', 'YesNo', 'Question') -eq 'Yes'
}

function Select-Item($title, $items) {
    # A clickable list; falls back to typing a number if Out-GridView isn't available.
    try {
        return $items | Out-GridView -Title $title -OutputMode Single
    } catch {
        Write-Host "`n$title"
        $i = 1
        foreach ($item in $items) { Write-Host ("  {0}. {1}" -f $i, (($item.PSObject.Properties | ForEach-Object { $_.Value }) -join '  ')); $i++ }
        $n = Read-Host 'Number (blank = cancel)'
        if ($n -match '^\d+$' -and [int]$n -ge 1 -and [int]$n -le @($items).Count) { return @($items)[[int]$n - 1] }
        return $null
    }
}

# ---------------------------------------------------------------- game names and dates

$script:Titles = $null
function Get-GameName($tid) {
    if ($null -eq $script:Titles) {
        $script:Titles = @{}
        try {
            (Get-Content -LiteralPath $TitlesPath -Raw -Encoding UTF8 | ConvertFrom-Json).PSObject.Properties |
                ForEach-Object { $script:Titles[$_.Name] = $_.Value }
        } catch {}
    }
    if ($script:Titles.ContainsKey($tid)) { return $script:Titles[$tid] }
    if ($tid.StartsWith('00000000')) { return "Extra data $($tid.Substring(8))" }
    return "Unknown game ($tid)"
}

function Get-GameKeys($names) {
    # Map each save file to its game; a game's extra data (extdata) is filed under the game itself.
    $byUniqueId = @{}
    foreach ($n in $names) {
        $t = Get-TitleOf $n
        if ($t -and $n.Split('/')[4] -eq 'title') { $byUniqueId[([Convert]::ToInt64($t.Substring(8), 16) -shr 8) -band 0xFFFFF] = $t }
    }
    $keys = @{}
    foreach ($n in $names) {
        $t = Get-TitleOf $n
        if ($t -and $n.Split('/')[4] -eq 'extdata') {
            $uid = [Convert]::ToInt64($t.Substring(8), 16)
            if ($byUniqueId.ContainsKey($uid)) { $t = $byUniqueId[$uid] }
        }
        $keys[$n] = $t
    }
    return $keys
}

function Get-SnapshotGames($path) {
    $zip = [IO.Compression.ZipFile]::OpenRead($path)
    try { $names = @($zip.Entries | ForEach-Object { $_.FullName } | Where-Object { -not $_.StartsWith('config/') }) } finally { $zip.Dispose() }
    $keys = Get-GameKeys $names
    return @($keys.Values | Where-Object { $_ } | Sort-Object -Unique | Sort-Object { Get-GameName $_ })
}

function Format-When($snapshot) {
    # 'Today 7:30 PM' / 'Yesterday 9:05 AM' / 'Thu Oct 23, 7:30 PM'
    $name = Split-Path $snapshot -Leaf
    $when = [datetime]::ParseExact($name.Substring(0, 19), 'yyyy-MM-dd_HH-mm-ss', [Globalization.CultureInfo]::InvariantCulture)
    $clock = $when.ToShortTimeString()
    $days = ((Get-Date).Date - $when.Date).Days
    if ($days -eq 0) { return "Today $clock" }
    if ($days -eq 1) { return "Yesterday $clock" }
    $year = if ($when.Year -ne (Get-Date).Year) { ", $($when.Year)" } else { '' }
    return "$($when.ToString('ddd MMM d'))$year, $clock"
}

# ---------------------------------------------------------------- Azahar settings (qt-config.ini)

function ConvertTo-IniString($value) { return '"' + $value.Replace('\', '\\').Replace('"', '\"') + '"' }

function Get-IniRange($lines, $section) {
    $start = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i].Trim() -eq "[$section]") { $start = $i + 1 }
        elseif ($start -ge 0 -and $lines[$i].StartsWith('[')) { return @($start, $i) }
    }
    if ($start -ge 0) { return @($start, $lines.Count) }
    return $null
}

function Get-IniValue($lines, $section, $key) {
    $r = Get-IniRange $lines $section
    if ($r) { for ($i = $r[0]; $i -lt $r[1]; $i++) { if ($lines[$i].StartsWith("$key=")) { return $lines[$i].Substring($key.Length + 1).Trim() } } }
    return $null
}

function Set-IniValues([System.Collections.Generic.List[string]]$lines, $section, $items) {
    $r = Get-IniRange $lines $section
    if (-not $r) { $lines.Add(''); $lines.Add("[$section]"); $r = @($lines.Count, $lines.Count) }
    $start = $r[0]; $end = $r[1]
    foreach ($key in $items.Keys) {
        # drop the old value and any "use default" flag
        for ($i = $end - 1; $i -ge $start; $i--) {
            if ($lines[$i].StartsWith("$key=") -or $lines[$i].StartsWith("$key\default=")) { $lines.RemoveAt($i); $end-- }
        }
        while ($end -gt $start -and -not $lines[$end - 1].Trim()) { $end-- }  # before the blank line ending the section
        $lines.Insert($end, "$key=$($items[$key])"); $end++
    }
}

# Azahar's own controller Auto-Map: buttons by position, like a real 3DS (SDL button numbers).
$ControllerButtons = [ordered]@{
    button_a = @(1, 'B'); button_b = @(0, 'A'); button_x = @(3, 'Y'); button_y = @(2, 'X')
    button_up = @(11, 'D-Pad Up'); button_down = @(12, 'D-Pad Down'); button_left = @(13, 'D-Pad Left'); button_right = @(14, 'D-Pad Right')
    button_l = @(9, 'LB'); button_r = @(10, 'RB'); button_start = @(6, 'Start'); button_select = @(4, 'Back'); button_home = @(5, 'Guide')
}

function Get-ControllerProfile {
    # maptype:all = whichever controller is plugged in, no device ID needed.
    $common = 'api:controller,engine:sdl,guid:0,maptype:all,port:0'
    $p = [ordered]@{ 'profiles\1\name' = 'Default'; 'profiles\1\input_maptype' = '0' }
    foreach ($k in $ControllerButtons.Keys) {
        $p["profiles\1\$k"] = ConvertTo-IniString "$common,button:$($ControllerButtons[$k][0]),name:$($ControllerButtons[$k][1])"
    }
    $p['profiles\1\button_zl'] = ConvertTo-IniString "$common,axis:4,direction:+,name:Left Trigger,threshold:0.500000"
    $p['profiles\1\button_zr'] = ConvertTo-IniString "$common,axis:5,direction:+,name:Right Trigger,threshold:0.500000"
    $p['profiles\1\circle_pad'] = ConvertTo-IniString "$common,axis_x:0,axis_y:1,name_x:Left Stick X,name_y:Left Stick Y"
    $p['profiles\1\c_stick'] = ConvertTo-IniString "$common,axis_x:2,axis_y:3,name_x:Right Stick X,name_y:Right Stick Y"
    $p['profiles\size'] = '1'
    $p['profile'] = '0'
    return $p
}

function Set-AzaharConfig($cfg, [bool]$useController) {
    # Point Azahar's game list at the games folder, and (fresh setups only) map a controller.
    $ini = Join-Path (Get-AzaharUserDir) 'config\qt-config.ini'
    $lines = New-Object 'System.Collections.Generic.List[string]'
    if (Test-Path -LiteralPath $ini) { foreach ($l in [IO.File]::ReadAllLines($ini)) { $lines.Add($l) } }
    $changed = $false

    $games = $cfg.games_dir.Replace('\', '/')
    $size = [int](Get-IniValue $lines 'UI' 'Paths\gamedirs\size')
    $existing = @(for ($i = 1; $i -le $size; $i++) { "$(Get-IniValue $lines 'UI' "Paths\gamedirs\$i\path")".Trim('"') })
    if ($existing -notcontains $games) {
        $items = [ordered]@{}
        if ($size -eq 0) {  # Azahar only adds these when the list is empty; keep them
            $i = 1
            foreach ($special in 'INSTALLED', 'SYSTEM') {
                $items["Paths\gamedirs\$i\path"] = $special; $items["Paths\gamedirs\$i\deep_scan"] = 'false'; $items["Paths\gamedirs\$i\expanded"] = 'true'; $i++
            }
            $size = 2
        }
        $size++
        $items["Paths\gamedirs\$size\path"] = ConvertTo-IniString $games
        $items["Paths\gamedirs\$size\deep_scan"] = 'true'
        $items["Paths\gamedirs\$size\expanded"] = 'true'
        $items['Paths\gamedirs\size'] = "$size"
        Set-IniValues $lines 'UI' $items
        $changed = $true
    }
    # Never replace a mapping someone already set up.
    if ($useController -and $null -eq (Get-IniValue $lines 'Controls' 'profiles\size')) {
        Set-IniValues $lines 'Controls' (Get-ControllerProfile)
        $changed = $true
    }
    if ($changed) {
        New-Item -ItemType Directory -Force -Path (Split-Path $ini) | Out-Null
        [IO.File]::WriteAllText("$ini.partial", (($lines -join "`n").Trim("`n") + "`n"), (New-Object Text.UTF8Encoding($false)))
        Move-Item -LiteralPath "$ini.partial" -Destination $ini -Force
    }
}

# ---------------------------------------------------------------- commands

function Invoke-Backup {
    $cfg = Get-Config
    Move-Fallback $cfg
    $snap = Backup-Saves $cfg -Force
    Backup-States $cfg
    $root = Get-BackupRoot $cfg
    if (-not $root.OnTarget) {
        Show-Message "Your backup folder ($($cfg.backup_dir)) isn't available, so this backup was kept on this computer for now. It moves over once the folder is back." -Warning
    } elseif ($snap) {
        Show-Message 'Backed up! Your saves are safe.'
    } else {
        Show-Message 'There are no saves to back up yet. Play a game and save first.'
    }
}

function Get-Snapshots($cfg) {
    $dirs = @((Join-Path (Get-BackupRoot $cfg).Path 'saves'), (Join-Path (Join-Path $FallbackDir $Host_) 'saves')) | Select-Object -Unique
    $snaps = @()
    foreach ($d in $dirs) { if (Test-Path -LiteralPath $d) { $snaps += Get-ChildItem -LiteralPath $d -Filter '*.zip' } }
    return @($snaps | Sort-Object Name -Descending)
}

function Invoke-Restore {
    $cfg = Get-Config
    $snaps = Get-Snapshots $cfg
    if (-not $snaps.Count) { Show-Message 'There are no backups yet.'; return }
    $gameId = if ($Game) { $Game.ToUpper() } else { $null }

    if (-not $Snapshot) {
        $rows = @()
        $i = 0
        foreach ($s in $snaps | Select-Object -First 50) {
            $names = @(Get-SnapshotGames $s.FullName | ForEach-Object { Get-GameName $_ })
            $rows += [pscustomobject]@{ Backup = (Format-When $s.Name); Games = $(if ($names) { $names -join ', ' } else { 'system data' }); '#' = $i }
            $i++
        }
        $row = Select-Item 'Which backup do you want to go back to? (newest first) - click one, then OK' $rows
        if (-not $row) { return }
        $snap = $snaps[$row.'#'].FullName
        $games = @(Get-SnapshotGames $snap)
        if ($games.Count -gt 1) {
            $choices = @([pscustomobject]@{ Game = 'All games'; '#' = -1 })
            for ($j = 0; $j -lt $games.Count; $j++) { $choices += [pscustomobject]@{ Game = (Get-GameName $games[$j]); '#' = $j } }
            $c = Select-Item 'Restore which game? - click one, then OK' $choices
            if (-not $c) { return }
            if ($c.'#' -ge 0) { $gameId = $games[$c.'#'] }
        } elseif ($games.Count -eq 1) {
            $gameId = $games[0]
        }
        $what = if ($gameId) { Get-GameName $gameId } else { 'all your games' }
        $when = Format-When $snap
        if ($when -notmatch '^(Today|Yesterday)') { $when = "on $when" } else { $when = $when.Substring(0, 1).ToLower() + $when.Substring(1) }
        if (-not (Confirm-Choice "Put $what back to how it was $when?`n`nYour current save is backed up first, so you can undo this.")) { return }
    } elseif ($Snapshot -match '^\d+$') {
        if ([int]$Snapshot -lt 1 -or [int]$Snapshot -gt $snaps.Count) { throw "pick a number from 1 to $($snaps.Count)" }
        $snap = $snaps[[int]$Snapshot - 1].FullName
    } else {
        $snap = $Snapshot
    }
    if (Test-AzaharRunning) { Show-Message 'Please close Azahar first, then try again.' -Warning; return }

    $root = Get-AzaharUserDir
    $zip = [IO.Compression.ZipFile]::OpenRead($snap)
    try {
        $entries = @($zip.Entries | Where-Object { -not $_.FullName.StartsWith('config/') -and -not $_.FullName.EndsWith('/') })
        if ($gameId) {
            $keys = Get-GameKeys @($entries | ForEach-Object { $_.FullName })
            $entries = @($entries | Where-Object { $k = $keys[$_.FullName]; $k -and $k.EndsWith($gameId) })
            if (-not $entries.Count) { throw "$(Split-Path $snap -Leaf) has no save for $gameId" }
        }
        $before = Backup-Saves $cfg -Force -NoPrune  # don't prune what we're restoring

        $containers = @($entries | ForEach-Object { Get-SaveContainer $_.FullName } | Where-Object { $_ } | Sort-Object -Unique)
        foreach ($c in $containers) {
            $dir = Join-Path $root $c.Replace('/', '\')
            if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
        }
        $rootFull = [IO.Path]::GetFullPath($root).TrimEnd('\') + '\'
        foreach ($e in $entries) {
            $target = [IO.Path]::GetFullPath((Join-Path $root $e.FullName.Replace('/', '\')))
            if (-not $target.StartsWith($rootFull, [StringComparison]::OrdinalIgnoreCase)) { continue }
            New-Item -ItemType Directory -Force -Path (Split-Path $target) | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($e, $target, $true)
        }
    } finally { $zip.Dispose() }
    Log "restored $($entries.Count) files from $(Split-Path $snap -Leaf)$(if ($gameId) { " (game $gameId)" })"
    $undo = if ($before) { "`n`nChanged your mind? Restore the backup from $(Format-When $before)." } else { '' }
    Show-Message "Done! Restored from $(Format-When $snap).$undo"
}

function Install-Azahar {
    $release = Invoke-RestMethod -Uri $ReleaseApi -Headers @{ 'User-Agent' = $App }
    $tag = $release.tag_name
    if ((Test-Path -LiteralPath $AzaharExe) -and (Test-Path -LiteralPath $VersionFile) -and (Get-Content -LiteralPath $VersionFile -Raw).Trim() -eq $tag) {
        Write-Host "Azahar $tag already installed"
        return $tag
    }
    if (Test-AzaharRunning) { throw 'Please close Azahar first so it can be updated.' }
    $asset = $release.assets | Where-Object { $_.name -like 'azahar-windows-msvc-*.zip' } | Select-Object -First 1
    $tmp = Join-Path $env:TEMP "$App-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        Write-Host "Downloading Azahar $tag ($([int]($asset.size / 1MB)) MB)..."
        $zipPath = Join-Path $tmp 'azahar.zip'
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -UseBasicParsing -Headers @{ 'User-Agent' = $App }
        if ($asset.digest) {
            $expected = $asset.digest -replace '^sha256:', ''
            if ((Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash -ne $expected) { throw 'The Azahar download was corrupted. Please try again.' }
        }
        $extracted = Join-Path $tmp 'x'
        [IO.Compression.ZipFile]::ExtractToDirectory($zipPath, $extracted)
        $exe = Get-ChildItem -LiteralPath $extracted -Recurse -Filter 'azahar.exe' | Select-Object -First 1
        if (-not $exe) { throw 'azahar.exe not found in download' }
        New-Item -ItemType Directory -Force -Path $AzaharDir | Out-Null
        # Replace the old program files, but never touch a portable 'user' folder.
        Get-ChildItem -LiteralPath $AzaharDir | Where-Object { $_.Name -ne 'user' } | Remove-Item -Recurse -Force
        Copy-Item -Path "$($exe.DirectoryName)\*" -Destination $AzaharDir -Recurse -Force
        Set-Content -LiteralPath $VersionFile -Value $tag
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    return $tag
}

function Install-VcRuntime {
    $sys = "$env:SystemRoot\System32"
    if ((Test-Path "$sys\vcruntime140_1.dll") -and (Test-Path "$sys\msvcp140.dll")) { return }
    Write-Host 'Installing the Microsoft Visual C++ runtime Azahar needs (Windows will ask for permission)...'
    $exe = Join-Path $env:TEMP 'vc_redist.x64.exe'
    Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $exe -UseBasicParsing
    Start-Process -FilePath $exe -ArgumentList '/install', '/passive', '/norestart' -Wait
    Remove-Item -LiteralPath $exe -Force -ErrorAction SilentlyContinue
}

function Install-Titles {
    $here = Split-Path $PSCommandPath
    foreach ($local in (Join-Path $here 'titles.json'), (Join-Path (Split-Path $here) 'titles.json')) {
        if ((Test-Path -LiteralPath $local) -and ([IO.Path]::GetFullPath($local) -ne [IO.Path]::GetFullPath($TitlesPath))) {
            Copy-Item -LiteralPath $local -Destination $TitlesPath -Force; return
        }
    }
    try {
        Invoke-WebRequest -Uri "$OurRelease/titles.json" -OutFile "$TitlesPath.partial" -UseBasicParsing -Headers @{ 'User-Agent' = $App }
        Move-Item -LiteralPath "$TitlesPath.partial" -Destination $TitlesPath -Force
    } catch { Log "couldn't download game names: $_" }  # names are a nicety; never fail the install
}

function Select-BackupFolder($default) {
    Write-Host "`nChoose where backups go: an external drive or a synced folder (OneDrive, Dropbox...)."
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $dlg = New-Object System.Windows.Forms.FolderBrowserDialog
        $dlg.Description = "Where should your 3DS save backups go?`nAn external drive, or a folder that syncs (OneDrive, Dropbox), is safest."
        $dlg.ShowNewFolderButton = $true
        if ($dlg.ShowDialog((Get-TopWindow)) -eq 'OK') { return (Join-Path $dlg.SelectedPath '3DS-Backups') }
    } catch {}
    return $default
}

function New-Shortcut($path, $target, $arguments, $workdir, [int]$window = 1, $icon) {
    New-Item -ItemType Directory -Force -Path (Split-Path $path) | Out-Null
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
    $s.TargetPath = $target
    if ($arguments) { $s.Arguments = $arguments }
    if ($workdir) { $s.WorkingDirectory = $workdir }
    if ($icon) { $s.IconLocation = $icon }
    $s.WindowStyle = $window
    $s.Save()
}

function Stop-Watcher {
    Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" |
        Where-Object { $_.ProcessId -ne $PID -and $_.CommandLine -like '*3ds-setup.ps1*watch*' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

$StartMenu = Join-Path (Get-KnownFolder 'Programs' 'AppData\Roaming\Microsoft\Windows\Start Menu\Programs') '3DS (Azahar)'
$Desktop = Get-KnownFolder 'Desktop' 'Desktop'
$StartupLink = Join-Path (Get-KnownFolder 'Startup' 'AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup') '3DS save backups.lnk'

function Invoke-Install {
    $cfg = Get-Config
    $firstTime = -not (Test-Path -LiteralPath $ConfigPath)
    if ($BackupDir) { $cfg.backup_dir = $BackupDir }
    elseif ($firstTime -and -not $Quiet) { $cfg.backup_dir = Select-BackupFolder $cfg.backup_dir }
    New-Item -ItemType Directory -Force -Path $cfg.backup_dir | Out-Null
    $cfg.backup_dir = (Resolve-Path -LiteralPath $cfg.backup_dir).Path
    if ($Controller) { $cfg.controller = ($Controller -eq 'yes') }
    elseif (-not $cfg.Contains('controller')) {
        $cfg.controller = (-not $Quiet) -and (Confirm-Choice "Will you play with a game controller (Xbox, PlayStation, Switch Pro...)?`n`nYes = controller`nNo = keyboard")
    }
    Save-Config $cfg

    Install-VcRuntime
    $tag = Install-Azahar

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $me = Join-Path $InstallDir '3ds-setup.ps1'
    if ($PSCommandPath -ne $me) { Copy-Item -LiteralPath $PSCommandPath -Destination $me -Force }
    Unblock-File -LiteralPath $me
    Install-Titles

    New-Item -ItemType Directory -Force -Path $cfg.games_dir | Out-Null
    if (Test-AzaharRunning) { Write-Host '(Azahar is open, so its settings were left alone. Run Update Azahar after closing it.)' }
    else { Set-AzaharConfig $cfg ([bool]$cfg.controller) }

    $ps = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $run = "-NoProfile -ExecutionPolicy Bypass -File `"$me`""
    $hidden = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$me`""
    $explorer = "$env:SystemRoot\explorer.exe"
    $folderIcon = "$env:SystemRoot\System32\shell32.dll,3"
    New-Shortcut (Join-Path $Desktop 'Azahar (3DS).lnk') $AzaharExe '' $AzaharDir
    New-Shortcut (Join-Path $Desktop '3DS Games.lnk') $explorer "`"$($cfg.games_dir)`"" '' 1 $folderIcon
    New-Shortcut "$StartMenu\Azahar (3DS).lnk" $AzaharExe '' $AzaharDir
    New-Shortcut "$StartMenu\3DS Games folder.lnk" $explorer "`"$($cfg.games_dir)`"" '' 1 $folderIcon
    New-Shortcut "$StartMenu\Back up 3DS saves now.lnk" $ps "$hidden backup" '' 7 $AzaharExe
    New-Shortcut "$StartMenu\Restore 3DS saves.lnk" $ps "$hidden restore" '' 7 $AzaharExe
    New-Shortcut "$StartMenu\Open backups folder.lnk" $explorer "`"$($cfg.backup_dir)`"" '' 1 $folderIcon
    New-Shortcut "$StartMenu\Update Azahar.lnk" $ps "$run install -Pause" '' 1 $AzaharExe
    New-Shortcut "$StartMenu\Uninstall 3DS Setup.lnk" $ps "$hidden uninstall" '' 7
    New-Shortcut $StartupLink $ps "$hidden watch" '' 7

    # Show up in Settings > Apps like a normal program, with a working Uninstall button.
    New-Item -Path $UninstallKey -Force | Out-Null
    $props = @{
        DisplayName = '3DS Setup (Azahar emulator + save backups)'; DisplayVersion = $tag; Publisher = '3ds-setup'
        DisplayIcon = $AzaharExe; InstallLocation = $AzaharDir; UninstallString = "`"$ps`" $hidden uninstall"
    }
    foreach ($k in $props.Keys) { Set-ItemProperty -Path $UninstallKey -Name $k -Value $props[$k] }
    foreach ($k in 'NoModify', 'NoRepair') { New-ItemProperty -Path $UninstallKey -Name $k -Value 1 -PropertyType DWord -Force | Out-Null }

    Stop-Watcher
    Start-Process -FilePath $ps -ArgumentList "$hidden watch" -WindowStyle Hidden

    Log "installed Azahar $tag; backups -> $($cfg.backup_dir)"
    Show-Message "All set! Open Azahar from the 'Azahar (3DS)' icon on your Desktop.`n`nPut your 3DS games in the '3DS Games' folder (it opens next, and there's a shortcut on your Desktop). They'll show up in Azahar.`n`nYour saves back up automatically to:`n$($cfg.backup_dir)"
    if (-not $Quiet) { Start-Process -FilePath $explorer -ArgumentList "`"$($cfg.games_dir)`"" }
}

function Invoke-Uninstall {
    $cfg = Get-Config
    if (-not $Yes -and -not (Confirm-Choice "Remove Azahar and 3DS Setup from this computer?`n`nYour saves and backups are NOT deleted.")) { return }
    if (Test-AzaharRunning) { Show-Message 'Please close Azahar first, then try again.' -Warning; return }
    $saves = Get-AzaharUserDir
    Stop-Watcher
    foreach ($p in $StartupLink, (Join-Path $Desktop 'Azahar (3DS).lnk'), (Join-Path $Desktop '3DS Games.lnk'), $StartMenu, $UninstallKey) {
        if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
    }
    # Program files go; a portable 'user' folder (which holds saves) stays.
    if (Test-Path -LiteralPath $AzaharDir) {
        Get-ChildItem -LiteralPath $AzaharDir | Where-Object { $_.Name -ne 'user' } | Remove-Item -Recurse -Force
        if (-not (Get-ChildItem -LiteralPath $AzaharDir)) { Remove-Item -LiteralPath $AzaharDir -Force }
    }
    $kept = $null
    if (Test-Path -LiteralPath $InstallDir) {
        Get-ChildItem -LiteralPath $InstallDir | Where-Object { $_.FullName -ne $FallbackDir } | Remove-Item -Recurse -Force
        if (Test-Path -LiteralPath $FallbackDir) { $kept = $FallbackDir } else { Remove-Item -LiteralPath $InstallDir -Force }
    }
    $cfgDir = Split-Path $ConfigPath
    if (Test-Path -LiteralPath $cfgDir) { Remove-Item -LiteralPath $cfgDir -Recurse -Force }
    $note = if ($kept) { "`n`nBackups not yet copied to your backup folder: $kept" } else { '' }
    Show-Message "Removed. Your saves are still in:`n$saves`n`nYour backups are in:`n$($cfg.backup_dir)$note"
}

function Show-Status {
    $cfg = Get-Config
    $root = Get-BackupRoot $cfg
    $snaps = Get-Snapshots $cfg
    $watcher = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like '*3ds-setup.ps1*watch*' })
    $version = if (Test-Path -LiteralPath $VersionFile) { (Get-Content -LiteralPath $VersionFile -Raw).Trim() } else { 'not installed' }
    Write-Host "Azahar:       $version  $AzaharExe"
    Write-Host "Azahar data:  $(Get-AzaharUserDir)"
    Write-Host "Games folder: $($cfg.games_dir)"
    $warn = if ($root.OnTarget) { '' } else { '   (backup folder missing! using local fallback)' }
    Write-Host "Backups:      $($root.Path)$warn"
    $newest = if ($snaps.Count) { ", newest $(Format-When $snaps[0].Name)" } else { '' }
    Write-Host "Snapshots:    $($snaps.Count)$newest"
    Write-Host "Watcher:      $(if ($watcher.Count) { 'running' } else { 'NOT running (sign out and back in, or run Update Azahar)' })"
    Write-Host "Config:       $ConfigPath"
    Write-Host "Log:          $LogPath"
}

try {
    switch ($Command) {
        'install'   { Invoke-Install }
        'watch'     { Invoke-Watch }
        'backup'    { Invoke-Backup }
        'restore'   { Invoke-Restore }
        'status'    { Show-Status }
        'uninstall' { Invoke-Uninstall }
        default     { Write-Host 'usage: 3ds-setup.ps1 install|watch|backup|restore|status|uninstall' }
    }
} catch {
    Log "ERROR in ${Command}: $_"
    if ($Command -ne 'watch') { Show-Message "Something went wrong: $_`n`nDetails are in $LogPath" -Warning }
    if ($Pause -and -not $Quiet) { Read-Host 'Press Enter to close' | Out-Null }
    exit 1
}
if ($Pause -and -not $Quiet) { Read-Host 'Press Enter to close' | Out-Null }
