#!/bin/sh
# Install Azahar (3DS) and turn on automatic save backups. Linux and macOS.
#   ./install.sh                         asks where backups go
#   ./install.sh --backup-dir /media/me/USB/3DS-Backups
if ! python3 -c 'import sys' >/dev/null 2>&1; then
    echo "python3 isn't ready yet. On a Mac, a window should pop up offering to install"
    echo "the Command Line Tools: click Install, wait for it to finish, then run this again."
    exit 1
fi
exec python3 "$(dirname "$0")/3ds-setup.py" install "$@"
