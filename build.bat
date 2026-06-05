@echo off
setlocal
cd /d "%~dp0"

echo ==========================================
echo   Clipsta Desktop - Windows Build
echo ==========================================
echo.

:: Check node
where node >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo ERROR: Node.js not found.
    echo Download from https://nodejs.org ^(v22+ recommended^)
    pause & exit /b 1
)
for /f "tokens=*" %%v in ('node --version') do echo Node: %%v

:: Check npm
where npm >nul 2>&1
if %ERRORLEVEL% NEQ 0 ( echo ERROR: npm not found & pause & exit /b 1 )

echo.
echo [1/3] Installing dependencies...
call npm install
if %ERRORLEVEL% NEQ 0 ( echo INSTALL FAILED & pause & exit /b 1 )

echo.
echo [2/3] Building app...
call npm run build:win
if %ERRORLEVEL% NEQ 0 ( echo BUILD FAILED & pause & exit /b 1 )

echo.
echo [3/3] Done!
echo Installer: %~dp0release\Clipsta Setup*.exe
echo.

if exist "release\Clipsta Setup*.exe" (
    echo Opening release folder...
    explorer release
)

pause
endlocal
