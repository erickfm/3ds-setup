#!/bin/sh
# macOS: double-click me in Finder to install Azahar (3DS) and turn on automatic save backups.
cd "$(dirname "$0")" && sh ./install.sh
echo
read -r -p "Press Return to close this window. " _
