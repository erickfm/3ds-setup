# 3ds-setup

One-step 3DS setup: installs [Azahar](https://azahar-emu.org) (the emulator that replaced Citra), plus a background watcher that makes sure you never lose a save.

- **Downloads and installs Azahar** (latest release from its GitHub, checksum-verified). Re-run it any time to update.
- **Backs up your in-game saves** every 5 minutes while Azahar is open, plus when it starts and closes. Each backup is a dated `.zip` snapshot, made only when something actually changed.
- **Auto save-states** every 10 minutes while you're in a game, and every save state gets archived too.
- **Restore** any snapshot, for every game or just one. Before restoring, it backs up your current saves, so a restore can always be undone.
- **External drive unplugged?** Backups go to a local holding folder and move over automatically once the drive is back. You get a notification when that happens.

## Windows

1. Download this repo (green **Code** button → **Download ZIP**) and unzip it.
2. Open the `windows` folder and double-click **`Install.bat`**.
   - If Windows says "Windows protected your PC", click **More info → Run anyway**.
   - A folder picker asks where backups should go. Pick an external drive or your OneDrive/Dropbox folder.
   - If Windows asks to install the "Microsoft Visual C++ Redistributable", say yes. Azahar needs it.
3. Done. You'll find these in the Start menu under **3DS (Azahar)**:

   | Shortcut | What it does |
   |---|---|
   | Azahar (3DS) | Opens the emulator (also on the Desktop) |
   | Back up 3DS saves now | Takes an extra backup right away |
   | Restore 3DS saves | Shows your backups and asks which one to bring back |
   | Open backups folder | Opens the folder in Explorer |
   | 3DS backup status | Shows whether everything's working |
   | Update Azahar | Installs the newest Azahar |

The backup watcher starts with Windows on its own. You don't need to do anything.

## Linux

```sh
./linux/install.sh                                   # asks where backups go
./linux/install.sh --backup-dir /media/$USER/MyDrive/EmuBackups
```

This adds **Azahar (3DS)** to your app menu and an `emusetup` command (in `~/.local/bin`). The watcher also starts on login.

```sh
emusetup status          # is the watcher running, where do backups go
emusetup backup          # back up now
emusetup restore         # list snapshots
emusetup restore 3 --game 00055D00   # restore snapshot #3, only that game
emusetup install         # update Azahar
```

For auto save-states you need X11 and `xdotool`. On Wayland, save backups still work, but auto save-states are skipped.

## How the auto save-state works

Azahar has no built-in timer for save states. Every 10 minutes the watcher presses Azahar's **"Save to Oldest Slot"** hotkey (`Ctrl+C` by default) for you, but only when an Azahar window with a game loaded is the active window. If you're tabbed out, it waits until you're back.

- To **load the latest autosave**, press `Ctrl+V` ("Load from Newest Slot"), or use *Emulation → Load State*.
- Autosaves rotate through slots 1–10, so they'll eventually write over states you saved by hand in those slots. Every state still ends up in the backup folder (the last 30 per game), so nothing is really gone. Slot 0 (Quick Save) is never touched.
- If you've changed that hotkey in Azahar, set `autosave_hotkey` in the config to match.

**In-game saves are your real safety net. Save states are a bonus.** A save state is a snapshot of the whole emulator, so it can stop loading after an Azahar update, and it can also capture a glitched moment. Normal in-game saves keep working across versions and on other devices. Keep saving in-game, and use the snapshots here to undo a bad save.

## Where things go

```
<backup folder>/<computer name>/
  saves/2026-10-22_19-30-05.zip     in-game saves + Azahar settings
  states/<TITLE ID>/<date>_slot03.cst   archived save states
```

The computer-name subfolder means two PCs can share one cloud folder without overwriting each other's backups.

What's in a save snapshot: each game's save data (`sdmc/.../title/*/*/data`), extra data (`sdmc/.../extdata`), system saves (`nand/data`), and Azahar's config. Game files are never copied, so snapshots stay small.

Retention: the newest 200 snapshots, **plus the last snapshot of every day, kept forever**. For save states, the last 30 per game.

To use a backed-up save state: copy it into Azahar's `states` folder and rename it to `<TITLE ID>.<slot>.cst`, e.g. `0004000000055D00.05.cst`.

## Settings

The config file lives at `%APPDATA%\emusetup\config.json` on Windows and `~/.config/emusetup/config.json` on Linux. The watcher picks up changes without a restart.

```json
{
  "backup_dir": "D:\\EmuBackups",
  "backup_every_minutes": 5,
  "autosave_state_every_minutes": 10,
  "autosave_hotkey": "ctrl+c",
  "keep_save_snapshots": 200,
  "keep_states_per_game": 30
}
```

Set `autosave_state_every_minutes` to `0` to turn auto save-states off.

## Games

Azahar doesn't come with games. Dump the ones you own from your own 3DS (see the [Azahar FAQ](https://azahar-emu.org/pages/faq/)). Azahar needs decrypted dumps (`.3ds`/`.cci`/`.cxi`/`.cia`/`.3dsx`, or its compressed versions like `.zcci`/`.zcia`).
