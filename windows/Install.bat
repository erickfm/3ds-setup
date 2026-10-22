@echo off
rem Double-click me to install Azahar (3DS) and turn on automatic save backups.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "3ds-setup.ps1" install -Pause
