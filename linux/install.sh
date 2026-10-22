#!/bin/sh
# Install Azahar (3DS) and turn on automatic save backups.
#   ./install.sh                         asks where backups go
#   ./install.sh --backup-dir /media/me/USB/3DS-Backups
exec python3 "$(dirname "$0")/3ds-setup.py" install "$@"
