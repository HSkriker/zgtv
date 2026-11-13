@echo off
chcp 65001 >nul
setlocal
rem 如果默认发行版不是装着本项目的那个，取消下一行注释并改成发行版名
rem set DISTRO=-d Ubuntu

rem 脚本路径不写死：用 wslpath 把 bat 所在目录（%~dp0）换算成 WSL 里的路径，
rem 仓库放在哪个盘、哪个目录都不用改这里。
rem %~dp0 末尾带反斜杠，"X:\a\b\" 会把 wslpath 的右引号吃掉，先去掉。
set "ROOT_WIN=%~dp0"
set "ROOT_WIN=%ROOT_WIN:~0,-1%"
for /f "usebackq delims=" %%i in (`wsl.exe %DISTRO% wslpath -a "%ROOT_WIN%"`) do set "ROOT_WSL=%%i"
if not defined ROOT_WSL (
  echo [tv] 无法把项目目录换算成 WSL 路径，检查 WSL 是否可用
  goto fail
)

wsl.exe %DISTRO% bash "%ROOT_WSL%/tools/tvctl.sh" start
if errorlevel 1 goto fail
if not defined TV_NO_BROWSER start "" http://127.0.0.1:8790
goto end

:fail
echo [tv] 启动失败，日志：%~dp0target\run.log

:end
echo.
pause
