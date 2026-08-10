@echo off
setlocal
pushd "%~dp0"

set "AFC_STEAM_APP_ID=480"
set "AFC_STEAM_DEV_SPACEWAR_480=1"

echo Starting Animal Fighter Club with Steam Spacewar App ID 480...
echo Keep the Steam desktop client running and signed in.
ffc-prototype.exe
set "AFC_EXIT_CODE=%ERRORLEVEL%"

if not "%AFC_EXIT_CODE%"=="0" (
  echo.
  echo Animal Fighter Club exited with code %AFC_EXIT_CODE%.
  pause
)

popd
exit /b %AFC_EXIT_CODE%
