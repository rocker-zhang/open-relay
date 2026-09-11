@echo off
rem ===========================================================================
rem  Oly notification hook (cmd edition): renders event data into a markdown
rem  sticker via rusticker.
rem
rem  Reads OLY_EVENT_* environment variables, builds a markdown document,
rem  writes a unique temp .md file, launches rusticker with the absolute path,
rem  then deletes the temp file after a short delay.
rem
rem  Designed for Oly's notification_hook config. Uses env vars exclusively to
rem  avoid placeholder-quoting fragility on Windows. Plain cmd.exe startup is
rem  much faster than pwsh.
rem ===========================================================================

setlocal EnableDelayedExpansion

rem UTF-8 output so non-ASCII event text survives the round trip into the .md
chcp 65001 >nul

rem -- paths ------------------------------------------------------------------
set "RUSTICKER=rusticker.exe"
where rusticker.exe >nul 2>nul
if errorlevel 1 set "RUSTICKER=rusticker"
if not defined TEMP set "TEMP=%~dp0"
set "OUTFILE=%TEMP%\oly-notify-%RANDOM%%RANDOM%%RANDOM%.md"

rem -- values (with defaults) -------------------------------------------------
set "KIND=!OLY_EVENT_KIND!"
if "!KIND!"=="" set "KIND=event"
set "TITLE=!OLY_EVENT_TITLE!"
if "!TITLE!"=="" set "TITLE=(no title)"
set "NOW=%DATE% %TIME%"

rem -- header -----------------------------------------------------------------
> "%OUTFILE%" (
  echo(## !TITLE!
)

rem -- parse session ids early (needed for the description link) --------------
set "SIDTAGS="
set "SIDCOUNT=0"
set "ONLY_SID="
set "SIDS=!OLY_EVENT_SESSION_IDS:,= !"
set "SIDS=!SIDS:;= !"
for %%s in (!SIDS!) do (
  set "SIDTAGS=!SIDTAGS!`%%s` "
  set "ONLY_SID=%%s"
  set /a SIDCOUNT+=1
)
if not "!SIDCOUNT!"=="1" set "ONLY_SID="

rem -- description (blockquote) -----------------------------------------------
if not "!OLY_EVENT_DESCRIPTION!"=="" (
  >>"%OUTFILE%" echo(
  >>"%OUTFILE%" echo(!OLY_EVENT_DESCRIPTION!
)

rem -- send-enter link (only when exactly one session id) ---------------------
if not "!ONLY_SID!"=="" (
  >>"%OUTFILE%" echo(
  >>"%OUTFILE%" echo([Attach](cli-delegate://oly?args="attach%%20!ONLY_SID!^&showWindow=true^) [Send Enter](cli-delegate://oly?args="send%%20!ONLY_SID!%%20key:enter"^) [Check in browser](http://localhost:15443!OLY_EVENT_NAVIGATION_URL!^&node=!OLY_EVENT_NODE!^)
)

rem -- body (fenced code block) -----------------------------------------------
if not "!OLY_EVENT_BODY!"=="" (
  >>"%OUTFILE%" echo(
  >>"%OUTFILE%" echo(```text
  >>"%OUTFILE%" echo(!OLY_EVENT_BODY!
  >>"%OUTFILE%" echo(```
)

rem -- details (node, session ids, trigger) -----------------------------------
set "HAS_DETAILS=0"
if not "!OLY_EVENT_NODE!"==""           set "HAS_DETAILS=1"
if not "!SIDTAGS!"==""                  set "HAS_DETAILS=1"
if not "!OLY_EVENT_TRIGGER_RULE!"==""   set "HAS_DETAILS=1"
if not "!OLY_EVENT_TRIGGER_DETAIL!"=="" set "HAS_DETAILS=1"

if "!HAS_DETAILS!"=="1" (
  >>"%OUTFILE%" echo(
)
if not "!OLY_EVENT_NODE!"=="" (
  >>"%OUTFILE%" echo(- **Node**  `!OLY_EVENT_NODE!`
)
if not "!SIDTAGS!"=="" (
  >>"%OUTFILE%" echo(- **Session IDs**  !SIDTAGS!
)
if not "!OLY_EVENT_TRIGGER_RULE!"=="" (
  >>"%OUTFILE%" echo(- **Trigger Rule**  !OLY_EVENT_TRIGGER_RULE!
)
if not "!OLY_EVENT_TRIGGER_DETAIL!"=="" (
  >>"%OUTFILE%" echo(- **Trigger Detail**  !OLY_EVENT_TRIGGER_DETAIL!
)

rem -- footer -----------------------------------------------------------------
>> "%OUTFILE%" (
  echo(
  echo(^> !NOW!
)


rem -- launch rusticker --------------------------------------------------------
"%RUSTICKER%" view --color yellow --width 400 --height 500 --flash "%OUTFILE%" >nul 2>&1

rem -- delete temp file (delay to let rusticker load it) -----------------------
ping -n 2 127.0.0.1 >nul
del /f /q "%OUTFILE%" >nul 2>&1

endlocal
exit /b 0
