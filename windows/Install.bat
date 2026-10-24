@echo off
rem Double-click me to install Azahar (3DS) and turn on automatic save backups.
cd /d "%~dp0"
if not exist "3ds-setup.ps1" (
    echo Please unzip the download first: right-click the zip file, choose "Extract All",
    echo then double-click Install.bat in the extracted folder.
    pause
    exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "3ds-setup.ps1" install -Pause
