@echo off
setlocal

reg delete "HKCU\Software\Classes\cli-delegate" /f
