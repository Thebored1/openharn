@echo off
REM Easy runner: starts MiniCPM (if needed) + opens the openharn REPL.
REM Usage:  openharn [target-directory]
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0openharn.ps1" %*
