@echo off
setlocal
set SCRIPT_DIR=%~dp0
powershell -ExecutionPolicy Bypass -NoLogo -NoProfile -File "%SCRIPT_DIR%start_service.ps1"
endlocal
