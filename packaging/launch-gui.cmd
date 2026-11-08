@echo off
setlocal
cd /d "%~dp0"
if not exist "data" mkdir "data"
"%~dp0protoinputmultiplex.exe" gui --preferences "%~dp0data\app-preferences.json" %*
exit /b %errorlevel%
