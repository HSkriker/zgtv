@echo off
chcp 65001 >nul
setlocal
rem 如果默认发行版不是装着本项目的那个，取消下一行注释并改成发行版名
rem set DISTRO=-d Ubuntu

rem 与 start-tv.bat 同口径：路径由 bat 所在目录换算，不写死。
set "ROOT_WIN=%~dp0"
set "ROOT_WIN=%ROOT_WIN:~0,-1%"
for /f "usebackq delims=" %%i in (`wsl.exe %DISTRO% wslpath -a "%ROOT_WIN%"`) do set "ROOT_WSL=%%i"
if not defined ROOT_WSL (
  echo [tv] 无法把项目目录换算成 WSL 路径，检查 WSL 是否可用
  goto end
)

wsl.exe %DISTRO% bash "%ROOT_WSL%/tools/tvctl.sh" stop
if errorlevel 1 (
  echo [tv] 停止失败，手动检查：wsl.exe pgrep -a -x tv
) else (
  echo [tv] 完成
)

:end
echo.
pause
