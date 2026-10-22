#!/bin/sh
# Install Azahar (3DS) and turn on automatic save backups.
#   ./install.sh                         asks where backups go
#   ./install.sh --backup-dir /media/me/USB/EmuBackups
exec python3 "$(dirname "$0")/emusetup.py" install "$@"
