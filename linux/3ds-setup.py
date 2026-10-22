#!/usr/bin/env python3
"""3ds-setup (Linux): install Azahar (3DS emulator) and keep your saves backed up.

Commands:
  install   download/update Azahar, pick a backup folder, start the background watcher
  watch     background loop: while Azahar runs, back up saves and auto save-state
  backup    back up right now
  restore   list save snapshots, or restore one (optionally just one game)
  status    show where everything is
"""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

APP = "3ds-setup"
RELEASE_API = "https://api.github.com/repos/azahar-emu/azahar/releases/latest"
HOME = Path.home()
XDG_DATA = Path(os.environ.get("XDG_DATA_HOME") or HOME / ".local/share")
XDG_CONFIG = Path(os.environ.get("XDG_CONFIG_HOME") or HOME / ".config")
INSTALL_DIR = XDG_DATA / APP
CONFIG_PATH = XDG_CONFIG / APP / "config.json"
LOG_PATH = INSTALL_DIR / "3ds-setup.log"
LOCK_PATH = INSTALL_DIR / "watch.lock"
APPIMAGE = INSTALL_DIR / "azahar.AppImage"
VERSION_FILE = INSTALL_DIR / "azahar-version.txt"
# Used when the backup folder is missing (e.g. external drive unplugged); moved over once it's back.
FALLBACK_DIR = INSTALL_DIR / "pending-backups"

DEFAULTS = {
    "backup_dir": str(HOME / "3DS-Backups"),
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
    if shutil.which("notify-send"):
        subprocess.run(["notify-send", "-a", "3DS backups", "3DS backups", msg],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def load_config():
    cfg = dict(DEFAULTS)
    if CONFIG_PATH.exists():
        cfg.update(json.loads(CONFIG_PATH.read_text()))
    return cfg


def save_config(cfg):
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(json.dumps(cfg, indent=2) + "\n")


def azahar_dirs():
    """(data_dir, config_dir) Azahar is using: Flatpak if present, otherwise the normal XDG dirs."""
    flatpak = HOME / ".var/app/org.azahar_emu.Azahar"
    if (flatpak / "data/azahar-emu").is_dir():
        return flatpak / "data/azahar-emu", flatpak / "config/azahar-emu"
    return XDG_DATA / "azahar-emu", XDG_CONFIG / "azahar-emu"


def azahar_running():
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


def try_autosave(cfg):
    """Press Azahar's save-state hotkey, but only while an Azahar game window is focused.
    Returns True if the key was sent."""
    global _autosave_unavailable_logged
    if os.environ.get("XDG_SESSION_TYPE") == "wayland" or not shutil.which("xdotool"):
        if not _autosave_unavailable_logged:
            log("auto save-state needs X11 + xdotool; skipping (save backups still run)")
            _autosave_unavailable_logged = True
        return False
    r = subprocess.run(["xdotool", "getactivewindow", "getwindowname"],
                       capture_output=True, text=True)
    title = r.stdout.strip()
    # Azahar's title is "Azahar <version> | <game>" only while a game is loaded.
    if not (title.startswith("Azahar ") and " | " in title):
        return False
    subprocess.run(["xdotool", "key", "--clearmodifiers", cfg["autosave_hotkey"]])
    log(f"auto save-state ({title})")
    return True


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
        print(f"NOTE: {cfg['backup_dir']} isn't available, backed up to {root} instead")
    if snap:
        print(f"done: {snap}")


def list_snapshots(cfg):
    snaps = []
    for d in {backup_root(cfg)[0] / "saves", FALLBACK_DIR / socket.gethostname() / "saves"}:
        snaps += d.glob("*.zip")
    return sorted(snaps, key=lambda p: p.name, reverse=True)


def cmd_restore(args):
    cfg = load_config()
    snaps = list_snapshots(cfg)
    if not snaps:
        sys.exit("no snapshots found")
    if args.snapshot is None:
        for i, s in enumerate(snaps[:30], 1):
            with zipfile.ZipFile(s) as z:
                titles = sorted({t for t in map(title_of, z.namelist()) if t})
            print(f"{i:3}  {s.stem}   games: {', '.join(titles) or '-'}")
        print("\nrestore with:  3ds-setup restore <number> [--game TITLEID]")
        return

    if args.snapshot.isdigit():
        if not 1 <= int(args.snapshot) <= len(snaps):
            sys.exit(f"pick a number from 1 to {len(snaps)} (run '3ds-setup restore' to list)")
        snap = snaps[int(args.snapshot) - 1]
    else:
        snap = Path(args.snapshot)
    if azahar_running():
        sys.exit("close Azahar first, then restore")
    game = args.game.upper() if args.game else None

    with zipfile.ZipFile(snap) as z:
        names = [n for n in z.namelist() if not n.startswith("config/") and not n.endswith("/")]
        if game:
            names = [n for n in names if (title_of(n) or "").endswith(game)]
            if not names:
                sys.exit(f"{snap.name} has no save for {game}")

        print(f"backing up current saves first (so this can be undone)...")
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
    print(f"restored {len(names)} files from {snap.name}")
    if before:
        print(f"your previous saves are in {before}")


def download_azahar(update=True):
    req = urllib.request.Request(RELEASE_API, headers={"User-Agent": APP})
    release = json.load(urllib.request.urlopen(req, timeout=30))
    tag = release["tag_name"]
    if APPIMAGE.exists() and VERSION_FILE.exists() and VERSION_FILE.read_text().strip() == tag:
        print(f"Azahar {tag} already installed")
        return tag
    if APPIMAGE.exists() and not update:
        return VERSION_FILE.read_text().strip() if VERSION_FILE.exists() else "?"
    asset = next(a for a in release["assets"] if a["name"] == "azahar.AppImage")
    print(f"downloading Azahar {tag} ({asset['size'] // 2**20} MB)...")
    tmp = APPIMAGE.with_name("azahar.AppImage.partial")
    INSTALL_DIR.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(asset["browser_download_url"], headers={"User-Agent": APP})
    with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
        shutil.copyfileobj(r, f)
    expected = (asset.get("digest") or "").removeprefix("sha256:")
    if expected:
        h = hashlib.sha256()
        with open(tmp, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        if h.hexdigest() != expected:
            tmp.unlink()
            sys.exit("download checksum mismatch, aborting")
    tmp.chmod(0o755)
    os.replace(tmp, APPIMAGE)
    VERSION_FILE.write_text(tag + "\n")
    return tag


def stop_watcher():
    try:
        pid = int(LOCK_PATH.read_text().strip())
        os.kill(pid, signal.SIGTERM)
        time.sleep(1)
    except (OSError, ValueError):
        pass


def cmd_install(args):
    cfg = load_config()
    if args.backup_dir:
        cfg["backup_dir"] = args.backup_dir
    elif not CONFIG_PATH.exists() and sys.stdin.isatty():
        print("Where should backups go? Pick an external drive or a synced folder (Dropbox etc).")
        answer = input(f"Backup folder [{cfg['backup_dir']}]: ").strip()
        if answer:
            cfg["backup_dir"] = answer
    cfg["backup_dir"] = str(Path(cfg["backup_dir"]).expanduser().resolve())
    Path(cfg["backup_dir"]).mkdir(parents=True, exist_ok=True)
    save_config(cfg)

    tag = download_azahar()

    me = INSTALL_DIR / "3ds-setup.py"
    if Path(__file__).resolve() != me.resolve():
        shutil.copy2(__file__, me)
    me.chmod(0o755)
    bin_link = HOME / ".local/bin/3ds-setup"
    bin_link.parent.mkdir(parents=True, exist_ok=True)
    if bin_link.is_symlink() or bin_link.exists():
        bin_link.unlink()
    bin_link.symlink_to(me)

    apps = XDG_DATA / "applications"
    apps.mkdir(parents=True, exist_ok=True)
    (apps / "azahar-3ds-setup.desktop").write_text(
        "[Desktop Entry]\nType=Application\nName=Azahar (3DS)\n"
        f"Exec={APPIMAGE} %f\nTerminal=false\nCategories=Game;Emulator;\n"
        "MimeType=application/x-ctr-3dsx;application/x-ctr-cci;application/x-ctr-cia;"
        "application/x-ctr-cxi;\n")
    autostart = XDG_CONFIG / "autostart"
    autostart.mkdir(parents=True, exist_ok=True)
    (autostart / "3ds-setup-watch.desktop").write_text(
        "[Desktop Entry]\nType=Application\nName=3DS save backups\n"
        f"Exec={sys.executable} {me} watch\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n")

    stop_watcher()
    subprocess.Popen([sys.executable, str(me), "watch"], start_new_session=True,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    print(f"\nAzahar {tag} installed: {APPIMAGE}  (menu: 'Azahar (3DS)')")
    print(f"Backups go to {cfg['backup_dir']}/{socket.gethostname()}/")
    print(f"Watcher running and set to start at login. Settings: {CONFIG_PATH}")
    if not shutil.which("xdotool"):
        print("Install xdotool for auto save-states:  sudo apt install xdotool")


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
    print(f"Azahar:        {version}  {APPIMAGE}")
    print(f"Azahar data:   {data_dir}")
    print(f"Backups:       {root}" + ("" if on_target else "   (backup folder missing! using local fallback)"))
    print(f"Snapshots:     {len(snaps)}" + (f", newest {snaps[0].stem}" if snaps else ""))
    print(f"Watcher:       {watcher}")
    print(f"Config:        {CONFIG_PATH}")
    print(f"Log:           {LOG_PATH}")


def main():
    p = argparse.ArgumentParser(prog=APP, description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("install")
    i.add_argument("--backup-dir")
    sub.add_parser("watch")
    sub.add_parser("backup")
    r = sub.add_parser("restore")
    r.add_argument("snapshot", nargs="?", help="number from the list, or a path to a .zip")
    r.add_argument("--game", help="only restore this title ID (full 16 hex or last 8)")
    sub.add_parser("status")
    args = p.parse_args()
    {"install": cmd_install, "watch": cmd_watch, "backup": cmd_backup,
     "restore": cmd_restore, "status": cmd_status}[args.cmd](args)


if __name__ == "__main__":
    main()
