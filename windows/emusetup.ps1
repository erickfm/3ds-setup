<#
emusetup (Windows): install Azahar (3DS emulator) and keep your saves backed up.

  install   download/update Azahar, pick a backup folder, start the background watcher
  watch     background loop: while Azahar runs, back up saves and auto save-state
  backup    back up right now
  restore   pick a save snapshot to restore (optionally just one game)
  status    show where everything is

Works on the PowerShell 5.1 that ships with Windows; nothing else to install.
#>
param(
    [Parameter(Position = 0)][string]$Command = 'status',
    [Parameter(Position = 1)][string]$Snapshot,
    [string]$Game,
    [string]$BackupDir,
    [switch]$Pause
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Invoke-WebRequest is very slow with the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

$App = 'emusetup'
$ReleaseApi = 'https://api.github.com/repos/azahar-emu/azahar/releases/latest'
$InstallDir = "$env:LOCALAPPDATA\$App"
$AzaharDir = "$env:LOCALAPPDATA\Programs\Azahar"
$AzaharExe = "$AzaharDir\azahar.exe"
$VersionFile = "$AzaharDir\emusetup-version.txt"
$ConfigPath = "$env:APPDATA\$App\config.json"
$LogPath = "$InstallDir\emusetup.log"
# Used when the backup folder is missing (e.g. external drive unplugged); moved over once it's back.
$FallbackDir = "$InstallDir\pending-backups"
$Host_ = $env:COMPUTERNAME
$TickSeconds = 15

$Defaults = [ordered]@{
    backup_dir                   = "$env:USERPROFILE\EmuBackups"
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
    $mutex = New-Object Threading.Mutex($false, 'Local\emusetup-watch')
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

function Invoke-Backup {
    $cfg = Get-Config
    Move-Fallback $cfg
    $snap = Backup-Saves $cfg -Force
    Backup-States $cfg
    $root = Get-BackupRoot $cfg
    if (-not $root.OnTarget) { Write-Host "NOTE: $($cfg.backup_dir) isn't available, backed up to $($root.Path) instead" -ForegroundColor Yellow }
    if ($snap) { Write-Host "Done! Saved to $snap" -ForegroundColor Green }
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
    if (-not $snaps.Count) { Write-Host 'No snapshots found.'; return }
    $pick = $Snapshot
    if (-not $pick) {
        $i = 1
        foreach ($s in $snaps | Select-Object -First 30) {
            $zip = [IO.Compression.ZipFile]::OpenRead($s.FullName)
            try { $titles = @($zip.Entries | ForEach-Object { Get-TitleOf $_.FullName } | Where-Object { $_ } | Sort-Object -Unique) } finally { $zip.Dispose() }
            Write-Host ('{0,3}  {1}   games: {2}' -f $i, $s.BaseName, ($titles -join ', '))
            $i++
        }
        $pick = Read-Host "`nWhich snapshot number? (blank = cancel)"
        if (-not $pick) { return }
        if (-not $Game) { $Game = Read-Host 'Only one game? Type its ID from the list (blank = restore all games)' }
    }
    if ($pick -match '^\d+$') {
        if ([int]$pick -lt 1 -or [int]$pick -gt $snaps.Count) { throw "pick a number from 1 to $($snaps.Count)" }
        $snap = $snaps[[int]$pick - 1].FullName
    } else {
        $snap = $pick
    }
    if (Test-AzaharRunning) { throw 'Close Azahar first, then restore.' }

    $root = Get-AzaharUserDir
    $zip = [IO.Compression.ZipFile]::OpenRead($snap)
    try {
        $entries = @($zip.Entries | Where-Object { -not $_.FullName.StartsWith('config/') -and -not $_.FullName.EndsWith('/') })
        if ($Game) {
            $g = $Game.ToUpper()
            $entries = @($entries | Where-Object { $t = Get-TitleOf $_.FullName; $t -and $t.EndsWith($g) })
            if (-not $entries.Count) { throw "$(Split-Path $snap -Leaf) has no save for $g" }
        }
        Write-Host 'Backing up current saves first (so this can be undone)...'
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
    Write-Host "Restored $($entries.Count) files from $(Split-Path $snap -Leaf)" -ForegroundColor Green
    if ($before) { Write-Host "Your previous saves are in $before" }
}

function Install-Azahar {
    $release = Invoke-RestMethod -Uri $ReleaseApi -Headers @{ 'User-Agent' = $App }
    $tag = $release.tag_name
    if ((Test-Path -LiteralPath $AzaharExe) -and (Test-Path -LiteralPath $VersionFile) -and (Get-Content -LiteralPath $VersionFile -Raw).Trim() -eq $tag) {
        Write-Host "Azahar $tag already installed"
        return $tag
    }
    if (Test-AzaharRunning) { throw 'Close Azahar first so it can be updated.' }
    $asset = $release.assets | Where-Object { $_.name -like 'azahar-windows-msvc-*.zip' } | Select-Object -First 1
    $tmp = Join-Path $env:TEMP "$App-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        Write-Host "Downloading Azahar $tag ($([int]($asset.size / 1MB)) MB)..."
        $zipPath = Join-Path $tmp 'azahar.zip'
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -UseBasicParsing -Headers @{ 'User-Agent' = $App }
        if ($asset.digest) {
            $expected = $asset.digest -replace '^sha256:', ''
            if ((Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash -ne $expected) { throw 'download checksum mismatch, aborting' }
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

function Select-BackupFolder($default) {
    Write-Host "`nChoose where backups go: an external drive or a synced folder (OneDrive, Dropbox...)."
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $dlg = New-Object System.Windows.Forms.FolderBrowserDialog
        $dlg.Description = 'Where should 3DS save backups go? (external drive or OneDrive/Dropbox folder)'
        $dlg.ShowNewFolderButton = $true
        $owner = New-Object System.Windows.Forms.Form -Property @{ TopMost = $true }
        if ($dlg.ShowDialog($owner) -eq 'OK') { return (Join-Path $dlg.SelectedPath 'EmuBackups') }
    } catch {}
    $answer = Read-Host "Backup folder [$default]"
    if ($answer) { return $answer }
    return $default
}

function New-Shortcut($path, $target, $arguments, $workdir, [int]$window = 1) {
    New-Item -ItemType Directory -Force -Path (Split-Path $path) | Out-Null
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
    $s.TargetPath = $target
    if ($arguments) { $s.Arguments = $arguments }
    if ($workdir) { $s.WorkingDirectory = $workdir }
    $s.WindowStyle = $window
    $s.Save()
}

function Stop-Watcher {
    Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" |
        Where-Object { $_.ProcessId -ne $PID -and $_.CommandLine -like '*emusetup.ps1*watch*' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

function Invoke-Install {
    $cfg = Get-Config
    if ($BackupDir) { $cfg.backup_dir = $BackupDir }
    elseif (-not (Test-Path -LiteralPath $ConfigPath)) { $cfg.backup_dir = Select-BackupFolder $cfg.backup_dir }
    New-Item -ItemType Directory -Force -Path $cfg.backup_dir | Out-Null
    $cfg.backup_dir = (Resolve-Path -LiteralPath $cfg.backup_dir).Path
    Save-Config $cfg

    Install-VcRuntime
    $tag = Install-Azahar

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $me = Join-Path $InstallDir 'emusetup.ps1'
    if ($PSCommandPath -ne $me) { Copy-Item -LiteralPath $PSCommandPath -Destination $me -Force }
    Unblock-File -LiteralPath $me

    $ps = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $run = "-NoProfile -ExecutionPolicy Bypass -File `"$me`""
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) '3DS (Azahar)'
    New-Shortcut (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Azahar (3DS).lnk') $AzaharExe '' $AzaharDir
    New-Shortcut "$menu\Azahar (3DS).lnk" $AzaharExe '' $AzaharDir
    New-Shortcut "$menu\Back up 3DS saves now.lnk" $ps "$run backup -Pause"
    New-Shortcut "$menu\Restore 3DS saves.lnk" $ps "$run restore -Pause"
    New-Shortcut "$menu\3DS backup status.lnk" $ps "$run status -Pause"
    New-Shortcut "$menu\Update Azahar.lnk" $ps "$run install -Pause"
    New-Shortcut "$menu\Open backups folder.lnk" "$env:SystemRoot\explorer.exe" "`"$($cfg.backup_dir)`""
    New-Shortcut (Join-Path ([Environment]::GetFolderPath('Startup')) '3DS save backups.lnk') $ps "-WindowStyle Hidden $run watch" '' 7

    Stop-Watcher
    Start-Process -FilePath $ps -ArgumentList "-WindowStyle Hidden $run watch" -WindowStyle Hidden

    Write-Host ''
    Write-Host "All set! Azahar $tag is installed (Desktop + Start menu: 'Azahar (3DS)')." -ForegroundColor Green
    Write-Host "Backups go to $($cfg.backup_dir)\$Host_"
    Write-Host 'The backup watcher is running now and starts with Windows.'
    Write-Host "Settings: $ConfigPath"
}

function Show-Status {
    $cfg = Get-Config
    $root = Get-BackupRoot $cfg
    $snaps = Get-Snapshots $cfg
    $watcher = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like '*emusetup.ps1*watch*' })
    $version = if (Test-Path -LiteralPath $VersionFile) { (Get-Content -LiteralPath $VersionFile -Raw).Trim() } else { 'not installed' }
    Write-Host "Azahar:       $version  $AzaharExe"
    Write-Host "Azahar data:  $(Get-AzaharUserDir)"
    $warn = if ($root.OnTarget) { '' } else { '   (backup folder missing! using local fallback)' }
    Write-Host "Backups:      $($root.Path)$warn"
    $newest = if ($snaps.Count) { ", newest $($snaps[0].BaseName)" } else { '' }
    Write-Host "Snapshots:    $($snaps.Count)$newest"
    Write-Host "Watcher:      $(if ($watcher.Count) { 'running' } else { 'NOT running (sign out and back in, or run Update Azahar)' })"
    Write-Host "Config:       $ConfigPath"
    Write-Host "Log:          $LogPath"
}

try {
    switch ($Command) {
        'install' { Invoke-Install }
        'watch'   { Invoke-Watch }
        'backup'  { Invoke-Backup }
        'restore' { Invoke-Restore }
        'status'  { Show-Status }
        default   { Write-Host 'usage: emusetup.ps1 install|watch|backup|restore|status' }
    }
} catch {
    Write-Host "ERROR: $_" -ForegroundColor Red
    if ($Pause) { Read-Host 'Press Enter to close' | Out-Null }
    exit 1
}
if ($Pause) { Read-Host 'Press Enter to close' | Out-Null }
