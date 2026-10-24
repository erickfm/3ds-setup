#!/usr/bin/env python3
"""3ds-setup (Linux + macOS): install Azahar (3DS emulator) and keep your saves backed up.

Commands:
  install    download/update Azahar, pick a backup folder, start the background watcher
  watch      background loop: while Azahar runs, back up saves and auto save-state
  backup     back up right now
  restore    pick a backup to bring back (or: restore <number> [--game ID])
  status     show where everything is
  uninstall  remove Azahar and 3ds-setup (saves and backups are kept)
"""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
import plistlib
import re
import shutil
import platform
import signal
import socket
import ssl
import subprocess
import sys
import tarfile
import time
import urllib.request
import zipfile
from pathlib import Path

APP = "3ds-setup"
RELEASE_API = "https://api.github.com/repos/azahar-emu/azahar/releases/latest"
OUR_RELEASE = "https://github.com/erickfm/3ds-setup/releases/latest/download"
HOME = Path.home()
XDG_DATA = Path(os.environ.get("XDG_DATA_HOME") or HOME / ".local/share")
XDG_CONFIG = Path(os.environ.get("XDG_CONFIG_HOME") or HOME / ".config")
IS_MAC = sys.platform == "darwin"
if IS_MAC:
    INSTALL_DIR = HOME / "Library/Application Support" / APP
    CONFIG_PATH = INSTALL_DIR / "config.json"
else:
    INSTALL_DIR = XDG_DATA / APP
    CONFIG_PATH = XDG_CONFIG / APP / "config.json"
LOG_PATH = INSTALL_DIR / "3ds-setup.log"
LOCK_PATH = INSTALL_DIR / "watch.lock"
TITLES_PATH = INSTALL_DIR / "titles.json"
APPIMAGE = INSTALL_DIR / "azahar.AppImage"
MAC_APP = HOME / "Applications/Azahar.app"
MAC_HELPERS = HOME / "Applications/3DS Setup"
AZAHAR_PATH = MAC_APP if IS_MAC else APPIMAGE
LAUNCH_AGENT_LABEL = "io.github.erickfm.3ds-setup"
LAUNCH_AGENT = HOME / "Library/LaunchAgents" / f"{LAUNCH_AGENT_LABEL}.plist"
LINUX_APPS = XDG_DATA / "applications"
LINUX_AUTOSTART = XDG_CONFIG / "autostart" / "3ds-setup-watch.desktop"
BIN_LINK = HOME / ".local/bin/3ds-setup"
VERSION_FILE = INSTALL_DIR / "azahar-version.txt"
HEARTBEAT = INSTALL_DIR / "watch.heartbeat"  # touched every tick so the 3DS app can tell we're running
# The 3DS app (launcher/ in the repo). tools/build-release.py fills these in for each release.
RELEASE_TAG = ""
LAUNCHER = {}
LAUNCHER_BIN = INSTALL_DIR / "3ds-launcher"
LAUNCHER_ICON = INSTALL_DIR / "3ds.png"
MAC_LAUNCHER = HOME / "Applications/3DS.app"
LAUNCHER_ID = "io.github.erickfm.threeds"  # the app's identifier; its web view keeps data under it
# Used when the backup folder is missing (e.g. external drive unplugged); moved over once it's back.
FALLBACK_DIR = INSTALL_DIR / "pending-backups"

DEFAULTS = {
    "backup_dir": str(HOME / "3DS-Backups"),
    "games_dir": str((HOME / "Documents" if (HOME / "Documents").is_dir() else HOME) / "3DS Games"),
    "backup_every_minutes": 5,
    "autosave_state_every_minutes": 10,  # 0 turns auto save-states off
    "autosave_hotkey": "ctrl+c",  # Azahar default for "Save to Oldest Non-Quicksave Slot"
    "keep_save_snapshots": 200,  # plus the last snapshot of every day, kept forever
    "keep_states_per_game": 30,
}

TICK_SECONDS = 15


# ---------------------------------------------------------------- helpers

def log(msg):
    line = f"{dt.datetime.now():%Y-%m-%d %H:%M:%S}  {msg}"
    print(line, flush=True)
    try:
        LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
        with open(LOG_PATH, "a") as f:
            f.write(line + "\n")
    except OSError:
        pass


def notify(msg):
    if IS_MAC:
        osascript('display notification (item 1 of argv) with title "3DS backups"', msg)
    elif shutil.which("notify-send"):
        subprocess.run(["notify-send", "-a", "3DS backups", "3DS backups", msg],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def load_config():
    cfg = dict(DEFAULTS)
    if CONFIG_PATH.exists():
        cfg.update(json.loads(CONFIG_PATH.read_text()))
    return cfg


def save_config(cfg):
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    tmp = CONFIG_PATH.with_name(CONFIG_PATH.name + ".partial")
    tmp.write_text(json.dumps(cfg, indent=2) + "\n")
    os.replace(tmp, CONFIG_PATH)  # the 3DS app may be reading it right now


def azahar_dirs():
    """(data_dir, config_dir) Azahar is using: Flatpak if present, otherwise the normal XDG dirs."""
    if IS_MAC:
        # Same rule as Azahar: XDG dirs if an older setup created them, else Application Support.
        if (XDG_DATA / "azahar-emu").exists() or (XDG_CONFIG / "azahar-emu").exists():
            return XDG_DATA / "azahar-emu", XDG_CONFIG / "azahar-emu"
        data = HOME / "Library/Application Support/Azahar"
        return data, data / "config"
    flatpak = HOME / ".var/app/org.azahar_emu.Azahar"
    if (flatpak / "data/azahar-emu").is_dir():
        return flatpak / "data/azahar-emu", flatpak / "config/azahar-emu"
    return XDG_DATA / "azahar-emu", XDG_CONFIG / "azahar-emu"


def azahar_running():
    if IS_MAC:
        out = subprocess.run(["ps", "-axo", "comm="], capture_output=True, text=True).stdout
        return any(os.path.basename(line.strip()) == "azahar" for line in out.splitlines())
    # Match on the executable, not just the process name: inside the AppImage the
    # emulator runs as "AppRun.wrapped" with exe .../bin/azahar.
    for proc in Path("/proc").glob("[0-9]*"):
        try:
            names = {(proc / "comm").read_text().strip(), os.path.basename(os.readlink(proc / "exe"))}
        except OSError:
            continue
        if any(n.startswith("azahar") and not n.startswith("azahar-room") for n in names):
            return True
    return False


def write_atomic_copy(src, dest):
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + ".partial")
    shutil.copy2(src, tmp)
    os.replace(tmp, dest)


def fetch(url, timeout=60):
    # The standalone Python we bring on macOS may not find the system's root certificates.
    ctx = None
    if IS_MAC and Path("/etc/ssl/cert.pem").exists():
        ctx = ssl.create_default_context(cafile="/etc/ssl/cert.pem")
    req = urllib.request.Request(url, headers={"User-Agent": APP})
    return urllib.request.urlopen(req, timeout=timeout, context=ctx)


def download(url, dest, sha256=None):
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + ".partial")
    with fetch(url) as r, open(tmp, "wb") as f:
        shutil.copyfileobj(r, f)
    if sha256:
        h = hashlib.sha256()
        with open(tmp, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        if h.hexdigest() != sha256:
            tmp.unlink()
            raise RuntimeError(f"download of {url} was corrupted (checksum mismatch)")
    os.replace(tmp, dest)


# ---------------------------------------------------------------- dialogs
# macOS gets real dialog windows; Linux uses the terminal.

QUIET = False  # --quiet: never show dialogs (used by CI)


def osascript(script, *args):
    """Run AppleScript with args passed as argv (never pasted into the script)."""
    lines = ["on run argv"] + script.splitlines() + ["end run"]
    cmd = ["osascript"]
    for line in lines:
        cmd += ["-e", line]
    return subprocess.run(cmd + list(args), capture_output=True, text=True)


def message(text):
    if QUIET:
        print(text)
    elif IS_MAC:
        osascript('display dialog (item 1 of argv) buttons {"OK"} default button 1 '
                  'with title "3DS Setup"', text)
    else:
        print("\n" + text)


def ask_yes_no(question, yes="Yes", no="No"):
    if IS_MAC:
        r = osascript('return button returned of (display dialog (item 1 of argv) '
                      'buttons {(item 3 of argv), (item 2 of argv)} default button 2 '
                      'with title "3DS Setup")', question, yes, no)
        return r.stdout.strip() == yes
    answer = input(f"\n{question} [{yes[0].lower()}/{no[0].lower()}]: ").strip().lower()
    return answer.startswith(yes[0].lower())


def pick(prompt, options):
    """Let the user pick one of options. Returns its index, or None if cancelled."""
    labels = [f"{i}. {o}" for i, o in enumerate(options, 1)]  # numbered, so labels are unique
    if IS_MAC:
        r = osascript('set r to choose from list (items 2 thru -1 of argv) with prompt (item 1 of argv) '
                      'with title "3DS Setup"\nif r is false then return ""\nreturn item 1 of r',
                      prompt, *labels)
        choice = r.stdout.strip()
        return labels.index(choice) if choice in labels else None
    print(f"\n{prompt}")
    for label in labels:
        print(f"  {label}")
    answer = input("Number (blank = cancel): ").strip()
    if answer.isdigit() and 1 <= int(answer) <= len(options):
        return int(answer) - 1
    return None


def open_folder(path):
    if not QUIET:
        subprocess.run(["open" if IS_MAC else "xdg-open", str(path)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# ---------------------------------------------------------------- what counts as a save

def is_save_path(rel):
    """rel: posix path relative to Azahar's data dir. True for in-game save data only
    (not game content, which can be gigabytes)."""
    parts = rel.split("/")
    if parts[0] == "nand":
        return len(parts) > 2 and parts[1] == "data"
    if parts[:2] == ["sdmc", "Nintendo 3DS"] and len(parts) > 5:
        # sdmc/Nintendo 3DS/<id0>/<id1>/title/<hi>/<lo>/data/...
        if parts[4] == "extdata":
            return True
        if parts[4] == "title":
            return len(parts) > 8 and parts[7] == "data"
    return False


def save_container(rel):
    """The folder a restore should wipe before extracting rel, so a game's save is restored whole."""
    parts = rel.split("/")
    if parts[0] == "sdmc" and parts[4] == "title":
        return "/".join(parts[:8])  # .../title/<hi>/<lo>/data
    if parts[0] == "sdmc" and parts[4] == "extdata":
        return "/".join(parts[:7])  # .../extdata/<hi>/<lo>
    return None


def collect_save_files():
    data_dir, config_dir = azahar_dirs()
    files = []
    for root, dirs, names in os.walk(data_dir):
        rel_root = Path(root).relative_to(data_dir).as_posix()
        if rel_root == ".":
            dirs[:] = [d for d in dirs if d in ("sdmc", "nand")]
            continue
        for n in names:
            rel = f"{rel_root}/{n}"
            if is_save_path(rel):
                files.append((rel, Path(root) / n))
    for root, _, names in os.walk(config_dir):
        for n in names:
            p = Path(root) / n
            files.append(("config/" + p.relative_to(config_dir).as_posix(), p))
    return sorted(files)


def title_of(rel):
    """Title ID (16 hex) a save file belongs to, or None."""
    parts = rel.split("/")
    if parts[0] == "sdmc" and parts[4] in ("title", "extdata") and len(parts) > 6:
        return (parts[5] + parts[6]).upper()
    return None


def game_keys(names):
    """Map each save file in a snapshot to the game it belongs to. A game's extra data
    (extdata) is filed under the game itself, so restoring a game restores both."""
    titles = {title_of(n) for n in names if title_of(n) and n.split("/")[4] == "title"}
    by_unique_id = {(int(t[8:], 16) >> 8) & 0xFFFFF: t for t in titles}
    keys = {}
    for n in names:
        t = title_of(n)
        if t and n.split("/")[4] == "extdata":
            t = by_unique_id.get(int(t[8:], 16), t)
        keys[n] = t
    return keys


def load_titles():
    try:
        return json.loads(TITLES_PATH.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def game_name(tid, titles):
    if tid in titles:
        return titles[tid]
    if tid.startswith("00000000"):
        return f"Extra data {tid[8:]}"
    return f"Unknown game ({tid})"


def when_lower(text):
    """'Today 7:30 PM' -> 'today 7:30 PM' (mid-sentence), leaving weekdays alone."""
    return text[0].lower() + text[1:] if text.startswith(("Today", "Yesterday")) else f"on {text}"


def friendly_time(snapshot):
    """'Today 7:30 PM' / 'Yesterday 9:05 AM' / 'Thu Oct 23, 7:30 PM' from a snapshot's name."""
    when = dt.datetime.strptime(snapshot.stem[:19], "%Y-%m-%d_%H-%M-%S")
    clock = when.strftime("%I:%M %p").lstrip("0")
    days = (dt.date.today() - when.date()).days
    if days == 0:
        return f"Today {clock}"
    if days == 1:
        return f"Yesterday {clock}"
    year = f", {when.year}" if when.year != dt.date.today().year else ""
    return f"{when:%a %b} {when.day}{year}, {clock}"


# ---------------------------------------------------------------- backups

def backup_root(cfg):
    base = Path(cfg["backup_dir"]).expanduser()
    # Never create the base folder here: if an external drive is unplugged we'd
    # silently write into the empty mount point instead.
    if base.is_dir():
        return base / socket.gethostname(), True
    return FALLBACK_DIR / socket.gethostname(), False


def flush_fallback(cfg):
    base = Path(cfg["backup_dir"]).expanduser()
    if not FALLBACK_DIR.is_dir() or not base.is_dir():
        return
    shutil.copytree(FALLBACK_DIR, base, dirs_exist_ok=True)
    shutil.rmtree(FALLBACK_DIR)
    log(f"moved pending backups into {base}")


def backup_saves(cfg, force=False, prune=True):
    files = collect_save_files()
    if not any(not rel.startswith("config/") for rel, _ in files):
        log("no save data yet, nothing to back up")
        return None
    h = hashlib.sha256()
    for rel, path in files:
        h.update(rel.encode() + b"\0" + path.read_bytes() + b"\0")
    digest = h.hexdigest()

    root, _ = backup_root(cfg)
    dest = root / "saves"
    dest.mkdir(parents=True, exist_ok=True)
    last_hash = dest / "last-hash.txt"
    if not force and last_hash.exists() and last_hash.read_text().strip() == digest:
        return None

    stamp = f"{dt.datetime.now():%Y-%m-%d_%H-%M-%S}"
    final, n = dest / f"{stamp}.zip", 2
    while final.exists():  # two snapshots in the same second must not overwrite each other
        final, n = dest / f"{stamp}_{n}.zip", n + 1
    tmp = final.with_name(final.name + ".partial")
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as z:
        for rel, path in files:
            z.write(path, rel)
    with zipfile.ZipFile(tmp) as z:
        bad = z.testzip()
        if bad:
            raise RuntimeError(f"snapshot failed verification at {bad}")
    os.replace(tmp, final)
    last_hash.write_text(digest + "\n")
    log(f"saved snapshot {final}")

    if prune:
        prune_snapshots(dest, cfg["keep_save_snapshots"])
    return final


def prune_snapshots(dest, keep):
    """Keep the newest `keep` snapshots, plus the last one of every day forever
    (saves are tiny, and corruption can go unnoticed for weeks)."""
    snaps = sorted(dest.glob("*.zip"))
    last_of_day = {s.name[:10]: s for s in snaps}  # names start with YYYY-MM-DD
    for old in snaps[:-keep]:
        if last_of_day[old.name[:10]] != old:
            old.unlink()


def backup_states(cfg):
    data_dir, _ = azahar_dirs()
    states = data_dir / "states"
    if not states.is_dir():
        return
    root, _ = backup_root(cfg)
    dest = root / "states"
    index_path = dest / "index.json"
    index = json.loads(index_path.read_text()) if index_path.exists() else {}
    copied = set()
    for f in sorted(states.glob("*.cst")):
        st = f.stat()
        if time.time() - st.st_mtime < 5:
            continue  # Azahar may still be writing it; next tick gets it
        key = [st.st_size, st.st_mtime_ns]
        if index.get(f.name) == key:
            continue
        title, slot = f.name.split(".")[0], f.name.split(".")[-2]
        out = dest / title / f"{dt.datetime.fromtimestamp(st.st_mtime):%Y-%m-%d_%H-%M-%S}_slot{slot}.cst"
        write_atomic_copy(f, out)
        index[f.name] = key
        copied.add(title)
        log(f"saved state {out}")
    for title in copied:
        versions = sorted((dest / title).glob("*.cst"))
        for old in versions[:-cfg["keep_states_per_game"]]:
            old.unlink()
    if copied:
        index_path.write_text(json.dumps(index, indent=1))


_warned_fallback = False


def backup_all(cfg, reason):
    global _warned_fallback
    try:
        flush_fallback(cfg)
        _, on_target = backup_root(cfg)
        if not on_target and not _warned_fallback:
            notify(f"Backup folder {cfg['backup_dir']} isn't available (drive unplugged?). "
                   f"Backing up locally until it's back.")
            log(f"backup folder missing, using {FALLBACK_DIR}")
            _warned_fallback = True
        elif on_target:
            _warned_fallback = False
        backup_saves(cfg)
        backup_states(cfg)
    except Exception as e:  # keep the watcher alive no matter what
        log(f"BACKUP FAILED ({reason}): {e!r}")
        notify(f"3DS backup failed: {e}")


# ---------------------------------------------------------------- auto save-state

_autosave_unavailable_logged = False


def autosave_unavailable(why):
    global _autosave_unavailable_logged
    if not _autosave_unavailable_logged:
        log(f"auto save-state unavailable: {why}; skipping (save backups still run)")
        _autosave_unavailable_logged = True
    return None


def focused_title():
    """Title of the focused window, or None if we can't find out on this system."""
    if IS_MAC:
        r = osascript(MAC_FRONT_WINDOW)
        if r.returncode != 0:
            return autosave_unavailable(
                "allow Accessibility for python3 in System Settings > Privacy & Security")
        return r.stdout.strip()
    if os.environ.get("XDG_SESSION_TYPE") == "wayland" or not shutil.which("xdotool"):
        return autosave_unavailable("needs X11 + xdotool")
    r = subprocess.run(["xdotool", "getactivewindow", "getwindowname"],
                       capture_output=True, text=True)
    return r.stdout.strip()


MAC_FRONT_WINDOW = """tell application "System Events"
    set p to first application process whose frontmost is true
    if (count of windows of p) is 0 then return ""
    return name of front window of p
end tell"""

# Qt shortcuts on macOS: "Ctrl" is the Command key and "Meta" is Control.
MAC_MODIFIERS = {"ctrl": "command down", "control": "command down", "meta": "control down",
                 "shift": "shift down", "alt": "option down"}
MAC_FKEYS = {"f1": 122, "f2": 120, "f3": 99, "f4": 118, "f5": 96, "f6": 97, "f7": 98,
             "f8": 100, "f9": 101, "f10": 109, "f11": 103, "f12": 111}


def press_hotkey(hotkey):
    if not IS_MAC:
        subprocess.run(["xdotool", "key", "--clearmodifiers", hotkey])
        return
    *mods, key = hotkey.lower().split("+")
    if key in MAC_FKEYS:
        action = f"key code {MAC_FKEYS[key]}"
    elif re.fullmatch(r"[a-z0-9]", key):
        action = f'keystroke "{key}"'
    else:
        raise ValueError(f"can't understand hotkey {hotkey!r}")
    using = ", ".join(MAC_MODIFIERS[m] for m in mods)
    script = f'tell application "System Events" to {action}' + (f" using {{{using}}}" if using else "")
    subprocess.run(["osascript", "-e", script], check=True)


def try_autosave(cfg):
    """Press Azahar's save-state hotkey, but only while an Azahar game window is focused.
    Returns True if the key was sent."""
    title = focused_title()
    # Azahar's title is "Azahar <version> | <game>" only while a game is loaded.
    if not (title and title.startswith("Azahar ") and " | " in title):
        return False
    press_hotkey(cfg["autosave_hotkey"])
    log(f"auto save-state ({title})")
    return True


# ---------------------------------------------------------------- Azahar settings (qt-config.ini)

def ini_quote(value):
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def ini_section(lines, section):
    """(start, end) line range of [section]'s body, or None."""
    start = None
    for i, line in enumerate(lines):
        if line.strip() == f"[{section}]":
            start = i + 1
        elif start is not None and line.startswith("["):
            return start, i
    return (start, len(lines)) if start is not None else None


def ini_get(lines, section, key):
    rng = ini_section(lines, section)
    if rng:
        for line in lines[rng[0]:rng[1]]:
            if line.startswith(key + "="):
                return line[len(key) + 1:].strip()
    return None


def ini_set(lines, section, items):
    rng = ini_section(lines, section)
    if rng is None:
        lines += ["", f"[{section}]"]
        rng = (len(lines), len(lines))
    start, end = rng
    for key, value in items.items():
        for i in range(start, end):
            if lines[i].startswith(key + "=") or lines[i].startswith(key + "\\default="):
                lines[i] = None  # drop the old value and any "use default" flag
        while end > start and lines[end - 1] is not None and not lines[end - 1].strip():
            end -= 1  # insert before the blank line that ends the section
        lines.insert(end, f"{key}={value}")
        end += 1
    lines[:] = [ln for ln in lines if ln is not None]


# Azahar's own controller Auto-Map: buttons by position, like a real 3DS (SDL button numbers).
CONTROLLER_BUTTONS = {
    "button_a": (1, "B"), "button_b": (0, "A"), "button_x": (3, "Y"), "button_y": (2, "X"),
    "button_up": (11, "D-Pad Up"), "button_down": (12, "D-Pad Down"),
    "button_left": (13, "D-Pad Left"), "button_right": (14, "D-Pad Right"),
    "button_l": (9, "LB"), "button_r": (10, "RB"), "button_start": (6, "Start"),
    "button_select": (4, "Back"), "button_home": (5, "Guide"),
}


def controller_profile():
    # maptype:all = whichever controller is plugged in, no device ID needed.
    common = "api:controller,engine:sdl,guid:0,maptype:all,port:0"
    p = {"profiles\\1\\name": "Default", "profiles\\1\\input_maptype": "0"}
    for key, (button, name) in CONTROLLER_BUTTONS.items():
        p[f"profiles\\1\\{key}"] = ini_quote(f"{common},button:{button},name:{name}")
    for key, axis, name in (("button_zl", 4, "Left Trigger"), ("button_zr", 5, "Right Trigger")):
        p[f"profiles\\1\\{key}"] = ini_quote(
            f"{common},axis:{axis},direction:+,name:{name},threshold:0.500000")
    p["profiles\\1\\circle_pad"] = ini_quote(
        f"{common},axis_x:0,axis_y:1,name_x:Left Stick X,name_y:Left Stick Y")
    p["profiles\\1\\c_stick"] = ini_quote(
        f"{common},axis_x:2,axis_y:3,name_x:Right Stick X,name_y:Right Stick Y")
    p["profiles\\size"] = "1"
    p["profile"] = "0"
    return p


def setup_azahar_config(cfg, use_controller):
    """Point Azahar's game list at the games folder, and (fresh setups only) map a controller.
    Returns a list of what changed."""
    _, config_dir = azahar_dirs()
    ini = config_dir / "qt-config.ini"
    lines = ini.read_text(encoding="utf-8").splitlines() if ini.exists() else []
    done = []

    games = Path(cfg["games_dir"]).as_posix()
    size = int(ini_get(lines, "UI", "Paths\\gamedirs\\size") or 0)
    existing = [ini_get(lines, "UI", f"Paths\\gamedirs\\{i}\\path") for i in range(1, size + 1)]
    if games not in [(e or "").strip('"') for e in existing]:
        items = {}
        if size == 0:  # Azahar only adds these when the list is empty; keep them
            for i, special in ((1, "INSTALLED"), (2, "SYSTEM")):
                items[f"Paths\\gamedirs\\{i}\\path"] = special
                items[f"Paths\\gamedirs\\{i}\\deep_scan"] = "false"
                items[f"Paths\\gamedirs\\{i}\\expanded"] = "true"
            size = 2
        size += 1
        items[f"Paths\\gamedirs\\{size}\\path"] = ini_quote(games)
        items[f"Paths\\gamedirs\\{size}\\deep_scan"] = "true"
        items[f"Paths\\gamedirs\\{size}\\expanded"] = "true"
        items["Paths\\gamedirs\\size"] = str(size)
        ini_set(lines, "UI", items)
        done.append("games folder")

    # Never replace a mapping someone already set up.
    if use_controller and ini_get(lines, "Controls", "profiles\\size") is None:
        ini_set(lines, "Controls", controller_profile())
        done.append("controller")

    if done:
        config_dir.mkdir(parents=True, exist_ok=True)
        tmp = ini.with_name(ini.name + ".partial")
        tmp.write_text("\n".join(lines).strip("\n") + "\n", encoding="utf-8")
        os.replace(tmp, ini)
    return done


# ---------------------------------------------------------------- commands

def cmd_watch(args):
    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    lock = open(LOCK_PATH, "a+")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print("watcher already running")
        return
    lock.seek(0)
    lock.truncate()
    lock.write(str(os.getpid()))
    lock.flush()

    log("watcher started")
    was_running = False
    last_backup = last_autosave = time.monotonic()
    while True:
        try:
            HEARTBEAT.touch()
        except OSError:
            pass
        cfg = load_config()  # pick up edits without a restart
        running = azahar_running()
        now = time.monotonic()
        if running and not was_running:
            log("Azahar started")
            backup_all(cfg, "start")
            last_backup = last_autosave = now
        elif running:
            every = cfg["autosave_state_every_minutes"]
            # If the game isn't focused this keeps retrying each tick, so the
            # save happens as soon as you're back in the game.
            if every and now - last_autosave >= every * 60 and try_autosave(cfg):
                last_autosave = now
                # back up ~20s later, once the state file is written
                last_backup = now - cfg["backup_every_minutes"] * 60 + 20
            if now - last_backup >= cfg["backup_every_minutes"] * 60:
                backup_all(cfg, "periodic")
                last_backup = now
        elif was_running:
            log("Azahar closed")
            time.sleep(3)
            backup_all(cfg, "exit")
        was_running = running
        time.sleep(TICK_SECONDS)


def cmd_backup(args):
    cfg = load_config()
    flush_fallback(cfg)
    snap = backup_saves(cfg, force=True)
    backup_states(cfg)
    root, on_target = backup_root(cfg)
    if not on_target:
        message(f"Your backup folder ({cfg['backup_dir']}) isn't available, so this backup "
                f"was kept on this computer for now. It moves over once the folder is back.")
    elif snap:
        message("Backed up! Your saves are safe.")
    else:
        message("There are no saves to back up yet. Play a game and save first.")


def list_snapshots(cfg):
    snaps = []
    for d in {backup_root(cfg)[0] / "saves", FALLBACK_DIR / socket.gethostname() / "saves"}:
        snaps += d.glob("*.zip")
    return sorted(snaps, key=lambda p: p.name, reverse=True)


def cmd_restore(args):
    cfg = load_config()
    snaps = list_snapshots(cfg)
    if not snaps:
        return message("There are no backups yet.")
    titles = load_titles()

    def games_in(snap):
        with zipfile.ZipFile(snap) as z:
            keys = game_keys([n for n in z.namelist() if not n.startswith("config/")])
        return sorted({k for k in keys.values() if k}, key=lambda t: game_name(t, titles))

    interactive = args.snapshot is None
    if interactive:
        shown = snaps[:50]
        labels = []
        for s in shown:
            names = [game_name(t, titles) for t in games_in(s)]
            labels.append(f"{friendly_time(s)}  -  {', '.join(names) or 'system data'}")
        i = pick("Which backup do you want to go back to? (newest first)", labels)
        if i is None:
            return
        snap = shown[i]
        games = games_in(snap)
        game = None
        if len(games) > 1:
            j = pick("Restore which game?", ["All games"] + [game_name(t, titles) for t in games])
            if j is None:
                return
            game = games[j - 1] if j else None
        elif games:
            game = games[0]
        what = game_name(game, titles) if game else "all your games"
        if not ask_yes_no(f"Put {what} back to how it was {when_lower(friendly_time(snap))}?\n\n"
                          f"Your current save is backed up first, so you can undo this.",
                          "Restore", "Cancel"):
            return
    else:
        if args.snapshot.isdigit():
            if not 1 <= int(args.snapshot) <= len(snaps):
                sys.exit(f"pick a number from 1 to {len(snaps)} (run '3ds-setup restore' to list)")
            snap = snaps[int(args.snapshot) - 1]
        else:
            snap = Path(args.snapshot)
        game = args.game.upper() if args.game else None

    if azahar_running():
        message("Please close Azahar first, then try again.")
        sys.exit(3)

    with zipfile.ZipFile(snap) as z:
        names = [n for n in z.namelist() if not n.startswith("config/") and not n.endswith("/")]
        if game:
            keys = game_keys(names)
            names = [n for n in names if (keys[n] or "").endswith(game)]
            if not names:
                sys.exit(f"{snap.name} has no save for {game}")

        before = backup_saves(cfg, force=True, prune=False)  # don't prune what we're restoring
        data_dir, _ = azahar_dirs()
        for container in {c for c in map(save_container, names) if c}:
            shutil.rmtree(data_dir / container, ignore_errors=True)
        for n in names:
            target = (data_dir / n).resolve()
            if not target.is_relative_to(data_dir.resolve()):
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            with z.open(n) as src, open(target, "wb") as dst:
                shutil.copyfileobj(src, dst)
    log(f"restored {len(names)} files from {snap.name}" + (f" (game {game})" if game else ""))
    undo = f"\n\nChanged your mind? Restore the backup from {friendly_time(before)}." if before else ""
    message(f"Done! Restored from {friendly_time(snap)}.{undo}")


def download_azahar(update=True):
    with fetch(RELEASE_API, timeout=30) as r:
        release = json.load(r)
    tag = release["tag_name"]
    if AZAHAR_PATH.exists() and VERSION_FILE.exists() and VERSION_FILE.read_text().strip() == tag:
        print(f"Azahar {tag} already installed")
        return tag
    if AZAHAR_PATH.exists() and not update:
        return VERSION_FILE.read_text().strip() if VERSION_FILE.exists() else "?"
    if IS_MAC:
        if azahar_running():
            sys.exit("Please close Azahar first so it can be updated.")
        asset = next(a for a in release["assets"]
                     if a["name"].startswith("azahar-macos-universal-") and a["name"].endswith(".zip"))
    else:
        asset = next(a for a in release["assets"] if a["name"] == "azahar.AppImage")
    print(f"Downloading Azahar {tag} ({asset['size'] // 2**20} MB)...")
    tmp = INSTALL_DIR / asset["name"]
    download(asset["browser_download_url"], tmp, (asset.get("digest") or "").replace("sha256:", "", 1))
    if IS_MAC:
        install_mac_app(tmp)
    else:
        tmp.chmod(0o755)
        os.replace(tmp, APPIMAGE)
    VERSION_FILE.write_text(tag + "\n")
    return tag


def install_mac_app(zip_path, dest=MAC_APP, keep_zip=False):
    extract = INSTALL_DIR / "extract"
    shutil.rmtree(extract, ignore_errors=True)
    # ditto, not zipfile: it keeps the executable bits and the app's code signature intact.
    subprocess.run(["ditto", "-x", "-k", str(zip_path), str(extract)], check=True)
    app = next(extract.glob(f"**/{dest.name}"))
    dest.parent.mkdir(exist_ok=True)
    if dest.exists():
        shutil.rmtree(dest)
    shutil.move(str(app), str(dest))
    subprocess.run(["xattr", "-dr", "com.apple.quarantine", str(dest)], stderr=subprocess.DEVNULL)
    shutil.rmtree(extract)
    if not keep_zip:
        zip_path.unlink()


def launcher_download():
    """The 3DS app for this computer: a local build (THREEDS_LAUNCHER, for testing) or this
    release's download. None if there's no build for this computer."""
    if os.environ.get("THREEDS_LAUNCHER"):
        return Path(os.environ["THREEDS_LAUNCHER"])
    key = "mac" if IS_MAC else "linux" if platform.machine() in ("x86_64", "amd64") else None
    if key not in LAUNCHER:
        return None
    name, sha256 = LAUNCHER[key]
    dest = INSTALL_DIR / name
    print("Downloading the 3DS app...")
    download(f"https://github.com/erickfm/3ds-setup/releases/download/{RELEASE_TAG}/{name}", dest, sha256)
    return dest


def install_launcher():
    """Install the 3DS app. Returns False (and setup carries on without it) if it can't run here."""
    try:
        src = launcher_download()
    except Exception as e:
        log(f"couldn't download the 3DS app: {e!r}")
        return False
    if not src:
        return False
    local = "THREEDS_LAUNCHER" in os.environ
    if IS_MAC:
        install_mac_app(src, MAC_LAUNCHER, keep_zip=local)
        return True
    with tarfile.open(src) as tar:
        for name, dest in (("3ds-launcher", LAUNCHER_BIN), ("3ds.png", LAUNCHER_ICON)):
            tmp = dest.with_name(dest.name + ".partial")
            with tar.extractfile(name) as f, open(tmp, "wb") as out:
                shutil.copyfileobj(f, out)
            os.replace(tmp, dest)
    if not local:
        src.unlink()
    LAUNCHER_BIN.chmod(0o755)
    missing = subprocess.run(["ldd", str(LAUNCHER_BIN)], capture_output=True, text=True).stdout
    missing = [line.split()[0] for line in missing.splitlines() if "not found" in line]
    if missing:
        print(f"\nThe 3DS app needs some system libraries this computer doesn't have ({', '.join(missing)}).\n"
              "On Ubuntu/Debian: sudo apt install libwebkit2gtk-4.1-0   (then run the install again)")
        LAUNCHER_BIN.unlink()
        return False
    return True


def open_launcher():
    if QUIET:
        return
    if IS_MAC:
        subprocess.run(["open", str(MAC_LAUNCHER)], stderr=subprocess.DEVNULL)
    else:
        subprocess.Popen([str(LAUNCHER_BIN)], start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def stop_launcher():
    if IS_MAC:
        subprocess.run(["pkill", "-f", str(MAC_LAUNCHER / "Contents/MacOS")], stderr=subprocess.DEVNULL)
        return
    for proc in Path("/proc").glob("[0-9]*"):
        try:
            if os.readlink(proc / "exe") == str(LAUNCHER_BIN):
                os.kill(int(proc.name), signal.SIGTERM)
        except (OSError, ValueError):
            pass


def choose_backup_dir(default):
    if IS_MAC:
        r = osascript('return POSIX path of (choose folder with prompt (item 1 of argv))',
                      "Where should your 3DS save backups go?\n"
                      "An external drive, or a folder that syncs (iCloud Drive, Dropbox), is safest.")
        if r.returncode == 0 and r.stdout.strip():
            return str(Path(r.stdout.strip()) / "3DS-Backups")
        return default
    print("\nWhere should backups go? Pick an external drive or a synced folder (Dropbox etc).")
    answer = input(f"Backup folder [{default}]: ").strip()
    return answer or default


def install_helpers(me, launcher):
    """Double-clickable shortcuts for back up / restore / uninstall (and the 3DS app on Linux)."""
    py = sys.executable
    if IS_MAC:
        MAC_HELPERS.mkdir(parents=True, exist_ok=True)
        for name, cmd in (("Back Up 3DS Saves", "backup"), ("Restore 3DS Saves", "restore"),
                          ("Uninstall 3DS Setup", "uninstall")):
            app = MAC_HELPERS / f"{name}.app"
            shutil.rmtree(app, ignore_errors=True)
            script = (f'do shell script quoted form of "{applescript_str(py)}" & " " & '
                      f'quoted form of "{applescript_str(str(me))}" & " {cmd}"')
            subprocess.run(["osacompile", "-o", str(app), "-e", script], check=True)
        return
    LINUX_APPS.mkdir(parents=True, exist_ok=True)
    entries = {
        "azahar-3ds-setup.desktop": ("Azahar (3DS)", f'"{APPIMAGE}" %f', False),
        "3ds-setup-backup.desktop": ("Back up 3DS saves", f'"{py}" "{me}" --pause backup', True),
        "3ds-setup-restore.desktop": ("Restore 3DS saves", f'"{py}" "{me}" --pause restore', True),
        "3ds-setup-uninstall.desktop": ("Uninstall 3DS Setup", f'"{py}" "{me}" --pause uninstall', True),
    }
    if launcher:
        entries["3ds-setup-launcher.desktop"] = ("3DS", f'"{LAUNCHER_BIN}"', False)
    else:
        (LINUX_APPS / "3ds-setup-launcher.desktop").unlink(missing_ok=True)
    for fname, (name, exec_, terminal) in entries.items():
        extra = ("MimeType=application/x-ctr-3dsx;application/x-ctr-cci;application/x-ctr-cia;"
                 "application/x-ctr-cxi;\n") if fname.startswith("azahar") else ""
        if fname == "3ds-setup-launcher.desktop":
            extra = f"Icon={LAUNCHER_ICON}\nComment=Play your 3DS games\nStartupWMClass=3ds-launcher\n"
        (LINUX_APPS / fname).write_text(
            f"[Desktop Entry]\nType=Application\nName={name}\nExec={exec_}\n"
            f"Terminal={'true' if terminal else 'false'}\nCategories=Game;Emulator;\n{extra}")


def applescript_str(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


def start_watcher_at_login(me):
    if IS_MAC:
        LAUNCH_AGENT.parent.mkdir(parents=True, exist_ok=True)
        with open(LAUNCH_AGENT, "wb") as f:
            plistlib.dump({"Label": LAUNCH_AGENT_LABEL,
                           "ProgramArguments": [sys.executable, str(me), "watch"],
                           "RunAtLoad": True,
                           "StandardErrorPath": str(INSTALL_DIR / "watch-errors.log")}, f)
        domain = f"gui/{os.getuid()}"
        subprocess.run(["launchctl", "bootout", f"{domain}/{LAUNCH_AGENT_LABEL}"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        stop_watcher()
        subprocess.run(["launchctl", "bootstrap", domain, str(LAUNCH_AGENT)], check=True)
        return
    LINUX_AUTOSTART.parent.mkdir(parents=True, exist_ok=True)
    LINUX_AUTOSTART.write_text(
        "[Desktop Entry]\nType=Application\nName=3DS save backups\n"
        f'Exec="{sys.executable}" "{me}" watch\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n')
    stop_watcher()
    subprocess.Popen([sys.executable, str(me), "watch"], start_new_session=True,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def stop_watcher():
    try:
        pid = int(LOCK_PATH.read_text().strip())
        os.kill(pid, signal.SIGTERM)
        time.sleep(1)
    except (OSError, ValueError):
        pass


def install_titles():
    here = Path(__file__).resolve().parent
    for local in (here / "titles.json", here.parent / "titles.json"):
        if local.exists() and local.resolve() != TITLES_PATH.resolve():
            shutil.copy2(local, TITLES_PATH)
            return
    try:
        download(f"{OUR_RELEASE}/titles.json", TITLES_PATH)
    except Exception as e:  # names are a nicety; never fail the install over them
        log(f"couldn't download game names: {e!r}")


def cmd_install(args):
    cfg = load_config()
    first_time = not CONFIG_PATH.exists()
    interactive = not QUIET and sys.stdin.isatty()
    if args.backup_dir:
        cfg["backup_dir"] = args.backup_dir
    elif first_time and interactive:
        cfg["backup_dir"] = choose_backup_dir(cfg["backup_dir"])
    cfg["backup_dir"] = str(Path(cfg["backup_dir"]).expanduser().resolve())
    Path(cfg["backup_dir"]).mkdir(parents=True, exist_ok=True)

    if args.controller:
        cfg["controller"] = args.controller == "yes"
    elif "controller" not in cfg:
        cfg["controller"] = interactive and ask_yes_no(
            "Will you play with a game controller (Xbox, PlayStation, Switch Pro...)?\n\n"
            "Pick No to play with the keyboard.", "Controller", "Keyboard")
    save_config(cfg)

    tag = download_azahar()

    me = INSTALL_DIR / "3ds-setup.py"
    if Path(__file__).resolve() != me.resolve():
        shutil.copy2(__file__, me)
    me.chmod(0o755)
    install_titles()
    BIN_LINK.parent.mkdir(parents=True, exist_ok=True)
    if BIN_LINK.is_symlink() or BIN_LINK.exists():
        BIN_LINK.unlink()
    BIN_LINK.symlink_to(me)

    Path(cfg["games_dir"]).mkdir(parents=True, exist_ok=True)
    if azahar_running():
        print("(Azahar is open, so its settings were left alone. Re-run install after closing it.)")
    else:
        setup_azahar_config(cfg, cfg["controller"])
    launcher = install_launcher()
    install_helpers(me, launcher)
    start_watcher_at_login(me)

    log(f"installed Azahar {tag}; backups -> {cfg['backup_dir']}" + ("; 3DS app" if launcher else ""))
    if launcher:
        where = "in your Applications folder" if IS_MAC else "in your app menu"
        message(f"All set! The 3DS app opens next. Add your games there and press Play.\n\n"
                f"You'll find it {where} as '3DS'.\n\n"
                f"Your saves back up automatically to:\n{cfg['backup_dir']}")
        open_launcher()
    else:
        where = "in your Applications folder" if IS_MAC else "in your app menu as 'Azahar (3DS)'"
        message(f"All set! Azahar is {where}.\n\n"
                f"Put your 3DS games in the '3DS Games' folder (it opens next), "
                f"and they'll show up in Azahar.\n\n"
                f"Your saves back up automatically to:\n{cfg['backup_dir']}")
        open_folder(cfg["games_dir"])
    if IS_MAC:
        print("For auto save-states, macOS will ask to let python3 control your computer\n"
              "(Accessibility). Allow it in System Settings > Privacy & Security > Accessibility.")
    elif not shutil.which("xdotool"):
        print("Install xdotool for auto save-states:  sudo apt install xdotool")


def cmd_uninstall(args):
    cfg = load_config()
    data_dir, _ = azahar_dirs()
    if not args.yes and not ask_yes_no(
            "Remove Azahar and 3DS Setup from this computer?\n\n"
            "Your saves and backups are NOT deleted.", "Remove", "Cancel"):
        return
    if azahar_running():
        return message("Please close Azahar first, then try again.")
    if IS_MAC:
        subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{LAUNCH_AGENT_LABEL}"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    stop_watcher()
    stop_launcher()
    for p in (LAUNCH_AGENT, LINUX_AUTOSTART, BIN_LINK, *LINUX_APPS.glob("*3ds-setup*.desktop")):
        if p.is_symlink() or p.exists():
            p.unlink()
    if IS_MAC:
        lib = HOME / "Library"
        app_dirs = [MAC_APP, MAC_HELPERS, MAC_LAUNCHER, lib / "Application Support" / LAUNCHER_ID,
                    lib / "Caches" / LAUNCHER_ID, lib / "WebKit" / LAUNCHER_ID]
    else:
        app_dirs = [XDG_DATA / LAUNCHER_ID, HOME / ".cache" / LAUNCHER_ID]
    for d in app_dirs:
        if d.exists():
            shutil.rmtree(d)
    kept = []
    if INSTALL_DIR.exists():
        for p in INSTALL_DIR.iterdir():
            if p == FALLBACK_DIR:
                kept.append(p)  # backups that never made it to the backup folder
            elif p.is_dir():
                shutil.rmtree(p)
            else:
                p.unlink()
    if not IS_MAC and CONFIG_PATH.exists():
        CONFIG_PATH.unlink()
        try:
            CONFIG_PATH.parent.rmdir()
        except OSError:
            pass
    note = f"\nBackups not yet copied to your backup folder: {kept[0]}" if kept else ""
    message(f"Removed. Your saves are still in:\n{data_dir}\n\nYour backups are in:\n"
            f"{cfg['backup_dir']}{note}")


def cmd_status(args):
    cfg = load_config()
    data_dir, config_dir = azahar_dirs()
    root, on_target = backup_root(cfg)
    snaps = list_snapshots(cfg)
    try:
        pid = int(LOCK_PATH.read_text().strip())
        os.kill(pid, 0)
        watcher = f"running (pid {pid})"
    except (OSError, ValueError):
        watcher = "NOT running  (start it: 3ds-setup watch &)"
    version = VERSION_FILE.read_text().strip() if VERSION_FILE.exists() else "not installed"
    print(f"Azahar:        {version}  {AZAHAR_PATH}")
    print(f"Azahar data:   {data_dir}")
    print(f"Games folder:  {cfg['games_dir']}")
    print(f"Backups:       {root}" + ("" if on_target else "   (backup folder missing! using local fallback)"))
    print(f"Snapshots:     {len(snaps)}" + (f", newest {friendly_time(snaps[0])}" if snaps else ""))
    print(f"Watcher:       {watcher}")
    print(f"Config:        {CONFIG_PATH}")
    print(f"Log:           {LOG_PATH}")


def main():
    global QUIET
    p = argparse.ArgumentParser(prog=APP, description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--quiet", action="store_true", help="no dialogs (for scripts/CI)")
    p.add_argument("--pause", action="store_true", help="wait for Enter before exiting")
    sub = p.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("install")
    i.add_argument("--backup-dir")
    i.add_argument("--controller", choices=["yes", "no"], help="set Azahar up for a game controller")
    sub.add_parser("watch")
    sub.add_parser("backup")
    r = sub.add_parser("restore")
    r.add_argument("snapshot", nargs="?", help="number (newest = 1), or a path to a .zip")
    r.add_argument("--game", help="only restore this title ID (full 16 hex or last 8)")
    sub.add_parser("status")
    u = sub.add_parser("uninstall")
    u.add_argument("--yes", action="store_true", help="don't ask for confirmation")
    argv = sys.argv[1:]
    flags = [a for a in argv if a in ("--quiet", "--pause")]  # allowed before or after the command
    args = p.parse_args(flags + [a for a in argv if a not in flags])
    QUIET = args.quiet or bool(os.environ.get("CI"))
    commands = {"install": cmd_install, "watch": cmd_watch, "backup": cmd_backup,
                "restore": cmd_restore, "status": cmd_status, "uninstall": cmd_uninstall}
    try:
        commands[args.cmd](args)
    except KeyboardInterrupt:
        sys.exit(1)
    except Exception as e:
        if args.cmd == "watch":
            raise
        log(f"ERROR in {args.cmd}: {e!r}")
        message(f"Something went wrong: {e}\n\nDetails are in {LOG_PATH}")
        sys.exit(1)
    finally:
        if args.pause and not QUIET:  # menu shortcuts open a terminal that would otherwise vanish
            input("\nPress Enter to close. ")


if __name__ == "__main__":
    main()
