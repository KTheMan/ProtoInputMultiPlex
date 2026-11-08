@echo off
setlocal
cd /d "%~dp0"
if not exist "data" mkdir "data"
"%~dp0protoinputmultiplex.exe" mcp --runtime "%~dp0runtime" --preferences "%~dp0data\app-preferences.json" %*
exit /b %errorlevel%
