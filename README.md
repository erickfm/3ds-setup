# 3DS Setup

Play 3DS games on your computer without ever losing a save. This installs [Azahar](https://azahar-emu.org) (the 3DS emulator that replaced Citra), sets it up for you, and quietly backs up your saves while you play.

Works on **Windows, Mac and Linux**.

## Install

### Windows

1. Download **[3DS-Setup-Windows.bat](https://github.com/erickfm/3ds-setup/releases/latest/download/3DS-Setup-Windows.bat)**.
   - If your browser warns about the file, choose **Keep**. In Edge that's **⋯ → Keep → Show more → Keep anyway**.
2. Double-click the downloaded file.
   - If you see a blue "Windows protected your PC" box, click **More info → Run anyway**.
   - If Windows asks to install the "Microsoft Visual C++ Redistributable", click **Yes**. Azahar needs it.
3. Answer two quick questions:
   - **Where should backups go?** Pick an external drive or your OneDrive folder if you have one. Otherwise just click OK.
   - **Controller or keyboard?**
4. That's it. You'll have two new icons on your Desktop: **Azahar (3DS)** and **3DS Games**.

### Mac

1. Open **Terminal**: press ⌘ + Space, type `Terminal`, and press Return.
2. Copy this line, paste it into Terminal, and press Return:
   ```
   curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
   ```
3. Answer the same two questions: where backups go, and controller or keyboard.
4. **Azahar** is now in your Applications folder.

The first time an autosave happens, macOS asks whether "python3" may control your computer. Allow it: click **OK**, and when one of the prompts sends you to **System Settings**, switch python3 on there. If you skip this, everything still works except the automatic save states.

### Linux

Same line as on Mac:

```
curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
```

## Playing

- **Put your games in the `3DS Games` folder** (in Documents). Open Azahar and they're in the list; double-click one to play. Azahar needs decrypted game files (`.3ds`, `.cci`, `.cxi`, `.cia`, `.3dsx`, or the compressed `.zcci`/`.zcia`). Dump them from a 3DS you own ([how](https://azahar-emu.org/pages/faq/)).
- **Controller:** plug it in before you open Azahar. Buttons are laid out like a real 3DS: A is the right face button, B the bottom one.
- **Saves back up automatically** every few minutes while Azahar is open, and again when you close it.
- **Save states:** every 10 minutes your game also gets a save state. To jump back to the newest one, press **Ctrl+V** (**⌘V** on a Mac).

## Getting a save back

- **Windows:** Start menu → **3DS (Azahar)** → **Restore 3DS saves**
- **Mac:** Applications → **3DS Setup** → **Restore 3DS Saves**
- **Linux:** your app menu → **Restore 3DS saves**

Pick a time from the list, then pick the game. Before restoring, it backs up what you have now, so you can always undo a restore.

## Uninstalling

- **Windows:** Settings → Apps → **3DS Setup** → Uninstall (or Start menu → 3DS (Azahar) → Uninstall 3DS Setup)
- **Mac:** Applications → 3DS Setup → **Uninstall 3DS Setup**
- **Linux:** your app menu → **Uninstall 3DS Setup**

This removes the programs only. **Your saves and backups are never deleted.**

## Good to know

- **Normal in-game saves are the safest.** Save states are a handy extra, but they can stop working after an Azahar update. Keep saving in the game like you normally would.
- **Backing up to an external drive?** If it's unplugged, backups are kept on the computer and moved over automatically once it's back.
- **Autosaves rotate through save-state slots 1–10**, so after a while they replace states you saved by hand in those slots. Every one is still kept in your backups folder. The Quick Save slot is never touched.

<details>
<summary><b>Details for tinkerers</b></summary>

### Where things are

| | Windows | Mac | Linux |
|---|---|---|---|
| Azahar | `%LOCALAPPDATA%\Programs\Azahar` | `~/Applications/Azahar.app` | `~/.local/share/3ds-setup/azahar.AppImage` |
| Settings | `%APPDATA%\3ds-setup\config.json` | `~/Library/Application Support/3ds-setup/config.json` | `~/.config/3ds-setup/config.json` |
| Log | `%LOCALAPPDATA%\3ds-setup\3ds-setup.log` | `~/Library/Application Support/3ds-setup/3ds-setup.log` | `~/.local/share/3ds-setup/3ds-setup.log` |

Backups are written to `<backup folder>/<computer name>/`: `saves/*.zip` (in-game saves plus Azahar's settings, made only when something changed) and `states/<title ID>/*.cst`. Each snapshot holds every game's save data, extra data and system saves, but never the games themselves, so they stay small. Retention: the newest 200 snapshots plus the last one of every day forever, and 30 save states per game. The computer-name folder lets several PCs share one cloud folder.

### Settings

```json
{
  "backup_dir": "D:\\3DS-Backups",
  "games_dir": "C:\\Users\\you\\Documents\\3DS Games",
  "backup_every_minutes": 5,
  "autosave_state_every_minutes": 10,
  "autosave_hotkey": "ctrl+c",
  "keep_save_snapshots": 200,
  "keep_states_per_game": 30
}
```

Set `autosave_state_every_minutes` to `0` to turn autosaves off. `autosave_hotkey` must match Azahar's "Save to Oldest Non-Quicksave Slot" hotkey (Ctrl+C by default; on a Mac Qt maps Ctrl to ⌘). The watcher picks up changes within seconds.

### How autosave works

Azahar has no timer for save states, so the watcher presses that hotkey for you. It only does this when an Azahar window with a game loaded is in front (the title reads `Azahar <version> | <game>`). On Linux this needs X11 and `xdotool`.

### What setup changes in Azahar

It adds the `3DS Games` folder to the game list. If you chose a controller and Azahar has no button mapping yet, it also sets up a controller profile that works with any controller (Azahar's own Auto-Map layout). An existing mapping is never touched.

### Command line

```sh
3ds-setup status | backup | restore [N --game TITLEID] | install | uninstall      # Mac/Linux (~/.local/bin)
powershell -File windows\3ds-setup.ps1 status|backup|restore|install|uninstall     # Windows
```

### Development

Every push runs the installer, a backup/restore and an uninstall on real Windows (PowerShell 5.1), macOS and Linux machines ([`.github/workflows/test.yml`](.github/workflows/test.yml)). Pushing a `v*` tag publishes the release files built by `tools/build-release.py`. Game names come from [3dsdb](https://github.com/hax0kartik/3dsdb) via `tools/update-titles.py`.

</details>
