#!/usr/bin/env python3
"""Build the release files into dist/:
  3DS-Setup-Windows.bat   one double-clickable file (the PowerShell script is embedded in it)
  install.sh, 3ds-setup.py, titles.json   for the Mac/Linux one-line install
  the 3DS app for each system, when --launchers points at the CI builds of it

  tools/build-release.py [--tag v1.2.3 --launchers DIR]

With --launchers, the setup scripts get this release's app downloads and checksums written into
them, so they install exactly the app files published next to them.
"""
import argparse
import hashlib
import json
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
MARKER = "###PS1###"
LAUNCHERS = {
    "windows": "3ds-launcher-windows-x64.exe",
    "mac": "3DS-macOS.zip",
    "linux": "3ds-launcher-linux-x86_64.tar.gz",
}

# The batch part copies everything after MARKER into %LOCALAPPDATA%\3ds-setup\3ds-setup.ps1 and
# runs it. cmd stops reading at "exit /b", so it never sees the PowerShell below the marker.
# The marker is split in two inside the command so the search finds the real one, not itself.
BAT = r"""@echo off
rem 3DS Setup for Windows: double-click to install the Azahar 3DS emulator with automatic save backups.
title 3DS Setup
set "SELF=%~f0"
powershell -NoProfile -ExecutionPolicy Bypass -Command "$d = Join-Path $env:LOCALAPPDATA '3ds-setup'; New-Item -ItemType Directory -Force -Path $d | Out-Null; $s = [IO.File]::ReadAllText($env:SELF); $m = '###' + 'PS1###'; $i = $s.LastIndexOf($m); if ($i -lt 0) { exit 1 }; [IO.File]::WriteAllText((Join-Path $d '3ds-setup.ps1'), $s.Substring($i + $m.Length).TrimStart(), (New-Object Text.UTF8Encoding($true)))"
if errorlevel 1 goto fail
powershell -NoProfile -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\3ds-setup\3ds-setup.ps1" install -Pause
exit /b
:fail
echo Something went wrong unpacking the installer. Please download it again.
pause
exit /b 1
"""


def fill_in(text, old, new):
    assert text.count(old) == 1, f"placeholder {old!r} not found exactly once"
    return text.replace(old, new)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default="")
    ap.add_argument("--launchers", type=Path, help="folder with the CI builds of the 3DS app")
    args = ap.parse_args()

    shutil.rmtree(DIST, ignore_errors=True)
    DIST.mkdir()
    ps1 = (ROOT / "windows/3ds-setup.ps1").read_text(encoding="ascii")
    py = (ROOT / "mac-linux/3ds-setup.py").read_text(encoding="utf-8")
    if args.launchers:
        assert args.tag, "--launchers needs --tag"
        found = {}
        for system, name in LAUNCHERS.items():
            shutil.copy2(args.launchers / name, DIST / name)
            found[system] = [name, hashlib.sha256((DIST / name).read_bytes()).hexdigest()]
        name, sha = found.pop("windows")
        ps1 = fill_in(ps1, "$ReleaseTag = ''", f"$ReleaseTag = '{args.tag}'")
        ps1 = fill_in(ps1, "$Launcher = @{}", f"$Launcher = @{{ name = '{name}'; sha256 = '{sha}' }}")
        py = fill_in(py, 'RELEASE_TAG = ""', f"RELEASE_TAG = {json.dumps(args.tag)}")
        py = fill_in(py, "LAUNCHER = {}", f"LAUNCHER = {json.dumps(found)}")
    assert MARKER not in ps1
    bat = BAT + MARKER + "\n" + ps1
    (DIST / "3DS-Setup-Windows.bat").write_bytes(bat.replace("\r\n", "\n").replace("\n", "\r\n").encode("ascii"))
    (DIST / "3ds-setup.py").write_text(py, encoding="utf-8")
    for src in ("mac-linux/install.sh", "titles.json"):
        shutil.copy2(ROOT / src, DIST / Path(src).name)
    for f in sorted(DIST.iterdir()):
        print(f"{f.stat().st_size:>9,}  {f.name}")


if __name__ == "__main__":
    main()
