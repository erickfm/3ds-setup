#!/bin/sh
# 3DS Setup for Mac and Linux: installs the Azahar 3DS emulator with automatic save backups.
#
#   Easiest (paste into Terminal):
#     curl -fsSL https://github.com/erickfm/3ds-setup/releases/latest/download/install.sh | sh
#   From a checkout:
#     ./mac-linux/install.sh [--backup-dir DIR] [--controller yes|no]
set -eu
RELEASE="https://github.com/erickfm/3ds-setup/releases/latest/download"

if [ "$(uname)" = Darwin ]; then
    BASE="$HOME/Library/Application Support/3ds-setup"
else
    BASE="${XDG_DATA_HOME:-$HOME/.local/share}/3ds-setup"
fi
mkdir -p "$BASE"

# The script: from this checkout if we're in one, otherwise from the latest release.
HERE=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ -n "$HERE" ] && [ -f "$HERE/3ds-setup.py" ]; then
    SCRIPT="$HERE/3ds-setup.py"
else
    echo "Downloading 3DS Setup..."
    curl -fsSL "$RELEASE/3ds-setup.py" -o "$BASE/3ds-setup.py.partial"
    mv "$BASE/3ds-setup.py.partial" "$BASE/3ds-setup.py"
    SCRIPT="$BASE/3ds-setup.py"
fi

if [ "$(uname)" = Darwin ]; then
    # Bring our own Python, so nobody gets the "install developer tools" pop-up.
    PY="$BASE/python/bin/python3"
    if [ ! -x "$PY" ]; then
        PBS="https://github.com/astral-sh/python-build-standalone/releases/download/20261001"
        case "$(uname -m)" in
            arm64)
                URL="$PBS/cpython-3.12.15%2B20261001-aarch64-apple-darwin-install_only_stripped.tar.gz"
                SUM=10cab8f6ed6202fdd81637aa6eda4af8d5b7eaa8fc42f9df3c6bea4923de0d93 ;;
            x86_64)
                URL="$PBS/cpython-3.12.15%2B20261001-x86_64-apple-darwin-install_only_stripped.tar.gz"
                SUM=d101ac54bc34afff54741406261325dc896b7b646a36a58fff4845ef0a00b2ce ;;
            *) echo "Sorry, this Mac isn't supported ($(uname -m))."; exit 1 ;;
        esac
        echo "Downloading Python (one time, about 25 MB)..."
        TMP=$(mktemp -d)
        curl -fsSL "$URL" -o "$TMP/python.tar.gz"
        if ! echo "$SUM  $TMP/python.tar.gz" | shasum -a 256 -c - >/dev/null 2>&1; then
            echo "The Python download was corrupted. Please try again."
            rm -rf "$TMP"; exit 1
        fi
        tar -xzf "$TMP/python.tar.gz" -C "$TMP"
        rm -rf "$BASE/python"
        mv "$TMP/python" "$BASE/python"
        rm -rf "$TMP"
    fi
else
    PY=$(command -v python3 || true)
    if [ -z "$PY" ]; then
        echo "Please install python3 first (for example: sudo apt install python3)"; exit 1
    fi
fi

# When piped from curl, stdin is the script itself; give the installer the keyboard back.
if (: </dev/tty) 2>/dev/null; then
    exec "$PY" "$SCRIPT" install "$@" </dev/tty
else
    exec "$PY" "$SCRIPT" install "$@"
fi
