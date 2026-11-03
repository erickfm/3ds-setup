# 3DS Setup

Play 3DS games on your computer without ever losing a save. This installs [Azahar](https://azahar-emu.org) (the 3DS emulator that replaced Citra) and **3DS**, a little app that looks like the 3DS home screen: your games with their real icons, a big Play button, and every older save one click away. Your saves are backed up quietly while you play.

Works on **Windows, Mac, Linux and Steam Deck**.

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
4. That's it. The **3DS** app opens, and there's a **3DS** icon on your Desktop for next time.

### Mac

1. Open **Terminal**: press ⌘ + Space, type `Terminal`, and press Return.
2. Copy this line, paste it into Terminal, and press Return:
   ```
   curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
   ```
3. Answer the same two questions: where backups go, and controller or keyboard.
4. The **3DS** app opens. Next time, find it in your Applications folder.

The first time an autosave happens, macOS asks whether "python3" may control your computer. Allow it: click **OK**, and when one of the prompts sends you to **System Settings**, switch python3 on there. If you skip this, everything still works except the automatic save states.

### Steam Deck

You only need Desktop Mode once, to install. After that you play from Gaming Mode.

1. Switch to **Desktop Mode**: press the **STEAM** button → **Power** → **Switch to Desktop**.
2. Start the installer, whichever way is easier:
   - **If you have a web browser:** download **[3DS-Setup-SteamDeck.desktop](https://github.com/erickfm/3ds-setup/releases/latest/download/3DS-Setup-SteamDeck.desktop)**, open your **Downloads** folder and double-click it. If it asks whether to run it, say yes.
   - **Otherwise:** open **Konsole** from the app menu (bottom-left corner), press **STEAM + X** to bring up the keyboard, type this line and press Enter:
     ```
     curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
     ```
3. There are no questions. Steam closes for a moment while **3DS** is added to your library, then the 3DS app opens. Add your games there.
4. Double-click **Return to Gaming Mode** on the desktop. **3DS** is in your Library under **Non-Steam**, with its own artwork.

On the Deck the app fills the screen, with the selected game on the left, your games on the right and bigger text.

The Deck's controls are set up for you (see [Playing](#playing)), and saves back up automatically in Gaming Mode too.

### Linux

Same line as on Mac:

```
curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
```

## Playing

Open **3DS**.

- **Your games show up by themselves** from the `3DS Games` folder, Downloads, Desktop, a `Games` folder, EmuDeck's `Emulation/roms`, and (on Linux) memory cards and USB drives, such as a Steam Deck's microSD card.
- **Add games:** click **Add games** (or drag game files onto the window) to copy games into the `3DS Games` folder. Azahar needs decrypted game files (`.3ds`, `.cci`, `.cxi`, `.cia`, `.3dsx`, or the compressed `.zcci`/`.zcia`). Dump them from a 3DS you own ([how](https://azahar-emu.org/pages/faq/)). A `.cia` gets set up inside Azahar the first time you play it.
- **Play:** click a game, then **Play** (or double-click it). The game opens in Azahar; close that window to come back. Arrow keys and Enter work too.
- **With a controller** (Xbox, PlayStation, Steam Deck...): D-pad or left stick to choose, **A** to play, **Y** for the game's older saves, **X** for all saves, **Start** (☰) for settings, **B** to go back. Everything in the app works this way: past the edge of the games, the D-pad goes on to the buttons around them (Add games, Saves, Settings, Play, Older saves), and in settings and the other windows it moves between the choices and **A** picks one. (Add games and Change... open your computer's own file picker, which needs a mouse, the Deck's trackpad, or the touchscreen.)
- **Controller:** plug it in before you start a game. Buttons go by their letters: the controller's A is the 3DS A, B is B, and so on.
- **Steam Deck:** **A**, **B**, **X** and **Y** are the same buttons as on a 3DS. **L1/R1** are L and R, **L2/R2** are ZL and ZR, **☰** is Start and **⧉** is Select. The left stick is the Circle Pad, and you tap the touchscreen for the bottom screen.
- **Saves back up automatically** every few minutes while you play, and again when you stop. Each game shows when it was last saved and whether that save is backed up.
- **Box art** for each game is downloaded from [GameTDB](https://www.gametdb.com) the first time it's shown.
- **Game settings** in Settings (the gear): render resolution (how many times the 3DS's own 400×240 the game is drawn at; 3× by default), screen layout, swap screens, and how fast speed-up runs (1.5× by default). Everything else is in Azahar's own settings.
- **Keyboard keys in a game:** **Tab** speed up on/off, **F1** quick save, **F2** quick load, **F9** swap screens, **F10** screen layout, **F11** full screen, **F4** pause. On a Steam Deck, set a back button to one of these in Steam's controller settings (in Gaming Mode: 3DS → controller icon → Edit layout → Back grip buttons).
- **Updates:** 3DS checks for a new version each time it starts and shows **Update available** at the top when there is one. Click it (or Settings → Updates) and it updates itself, Azahar included, then starts again. Nothing else changes: your games, saves and settings stay as they are. (Installed before v1.6.0? Run the install once more to get this.)
- **Dark mode** follows your computer; pick Light or Dark in Settings (the gear) to override it.
- **Save states:** every 10 minutes your game also gets a save state. To jump back to the newest one, press **Ctrl+V** (**⌘V** on a Mac). You can change or turn this off in Settings (the gear).

## Getting a save back

In **3DS**, click **Saves** to see every game's saves (or pick a game and click **Older saves**). Versions are listed newest first, grouped by day. Pick the one you want to go back to. Before going back, it backs up the save you have now, so you can always undo it.

(Without the app: Start menu → 3DS (Azahar) → Restore 3DS saves on Windows, Applications → 3DS Setup → Restore 3DS Saves on a Mac, or Restore 3DS saves in your Linux app menu.)

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
| 3DS app | `%LOCALAPPDATA%\3ds-setup\3DS.exe` | `~/Applications/3DS.app` | `~/.local/share/3ds-setup/3ds-launcher` |
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

It adds the `3DS Games` folder to the game list, sets 3× render resolution and a 1.5× speed-up, and gives speed-up, quick save and quick load single keys (Tab, F1, F2; Load Amiibo moves to Ctrl+F2). Settings you've changed yourself are left alone. If you chose a controller and Azahar has no button mapping yet, it also sets up a controller profile that works with any controller, with each button matching its letter (A is A). An existing mapping is never touched.

### The 3DS app

[`launcher/`](launcher) is a [Tauri](https://tauri.app) app: Rust in `src/`, plain HTML/CSS/JS in `ui/`. It reads each game's name, icon and title ID straight from the file (cartridge, CIA, homebrew, and Azahar's compressed formats), starts games with `azahar <file>`, and reads the backup snapshots to list a game's older saves. Backing up and restoring go through the 3ds-setup script, so there's one copy of that logic. On Linux it needs WebKitGTK 4.1 (`libwebkit2gtk-4.1-0`), which most desktops already have; on Windows, Edge WebView2, which setup installs if it's missing.

```sh
cd launcher && npx @tauri-apps/cli@2.12.1 dev     # run it
THREEDS_LAUNCHER=path/to/build ./mac-linux/install.sh   # install a local build
```

### Command line

```sh
3ds-setup status | backup | restore [N --game TITLEID] | install | update | uninstall   # Mac/Linux (~/.local/bin)
powershell -File windows\3ds-setup.ps1 status|backup|restore|install|update|uninstall  # Windows
```

`update` is what the app's Update button runs, with the newest release's script: an install with no questions or dialogs that leaves the open app running (it restarts itself afterwards; on Windows the running `3DS.exe` is renamed aside so the new one can take its place).

### Development

Every push builds the 3DS app, then runs the installer, a backup/restore and an uninstall on real Windows (PowerShell 5.1), macOS and Linux machines ([`.github/workflows/test.yml`](.github/workflows/test.yml)). Pushing a `v*` tag publishes the release files built by `tools/build-release.py`. Game names come from [3dsdb](https://github.com/hax0kartik/3dsdb) via `tools/update-titles.py`.

</details>
