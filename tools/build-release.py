#!/usr/bin/env python3
"""Build the release files into dist/:
  3DS-Setup-Windows.bat   one double-clickable file (the PowerShell script is embedded in it)
  install.sh, 3ds-setup.py, titles.json   for the Mac/Linux one-line install
"""
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
MARKER = "###PS1###"

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


def main():
    shutil.rmtree(DIST, ignore_errors=True)
    DIST.mkdir()
    ps1 = (ROOT / "windows/3ds-setup.ps1").read_text(encoding="ascii")
    assert MARKER not in ps1
    bat = BAT + MARKER + "\n" + ps1
    (DIST / "3DS-Setup-Windows.bat").write_bytes(bat.replace("\r\n", "\n").replace("\n", "\r\n").encode("ascii"))
    for src in ("mac-linux/install.sh", "mac-linux/3ds-setup.py", "titles.json"):
        shutil.copy2(ROOT / src, DIST / Path(src).name)
    for f in sorted(DIST.iterdir()):
        print(f"{f.stat().st_size:>9,}  {f.name}")


if __name__ == "__main__":
    main()
