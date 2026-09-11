@echo off
setlocal

if not exist "%~dp0cli-delegate.vbs" (
    echo Missing "%~dp0cli-delegate.vbs".
    exit /b 1
)

reg add "HKCU\Software\Classes\cli-delegate" /ve /t REG_SZ /d "URL:cli-delegate Protocol" /f || exit /b 1
reg add "HKCU\Software\Classes\cli-delegate" /v "URL Protocol" /t REG_SZ /d "" /f || exit /b 1
reg add "HKCU\Software\Classes\cli-delegate\shell\open\command" /ve /t REG_EXPAND_SZ /d "\"%SystemRoot%\System32\wscript.exe\" \"%~dp0cli-delegate.vbs\" \"%%1\"" /f || exit /b 1

echo cli-delegate protocol registered for:
echo   %~dp0cli-delegate.vbs
