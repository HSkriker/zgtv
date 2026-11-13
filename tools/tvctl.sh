#!/usr/bin/env bash
# tv 启停控制：start | stop | status
set -uo pipefail
cd "$(dirname "$0")/.."

BIN_PID=target/run.pid
URL=http://127.0.0.1:8790/api/health

alive() { curl -sf -o /dev/null --max-time 2 "$URL"; }

case "${1:-}" in
  start)
    if alive; then
      pgrep -x tv > "$BIN_PID" 2>/dev/null || true
      echo "[tv] 已在运行：http://127.0.0.1:8790"
      exit 0
    fi
    if pgrep -f '[.]/run\.sh' >/dev/null; then
      echo "[tv] 正在启动中，请稍候再试"
      exit 1
    fi
    # target/ 被 gitignore：全新 clone 上先建目录，否则下面的重定向直接失败、后台任务秒死
    mkdir -p target
    setsid nohup ./run.sh > target/run.log 2>&1 < /dev/null &
    # 记下 setsid 会话首进程 pid：stop 时按进程组杀，能覆盖编译期的 bash/cargo
    echo $! > target/run.pid
    echo "[tv] 启动中（首次需要编译 release，约 1-3 分钟；之后通常 1 秒内就绪）…"
    rp=$(cat target/run.pid 2>/dev/null)
    for _ in $(seq 1 240); do
      sleep 1
      if alive; then
        pgrep -x tv > "$BIN_PID" 2>/dev/null || true
        echo "[tv] 已就绪：http://127.0.0.1:8790"
        exit 0
      fi
      # 快速失败：启动进程已退出而端口没起来（如编译报错），别干等满 240 秒
      # （run.sh 末尾 exec tv，该 pid 会从 bash 变成 tv 本体，存活即启动仍在进行）
      if [ -n "$rp" ] && ! kill -0 "$rp" 2>/dev/null; then
        echo "[tv] 启动进程已退出但服务未就绪，日志末几行："
        tail -n 8 target/run.log 2>/dev/null
        exit 1
      fi
    done
    echo "[tv] 启动超时，日志见 target/run.log"
    exit 1
    ;;
  stop)
    # 先按启动会话的进程组杀（覆盖编译期的 bash/cargo 与运行期的 tv），
    # 杀前校验 /proc cmdline 确属本仓 run.sh，防 pid 复用误杀无关进程
    if [ -f target/run.pid ]; then
      rp=$(cat target/run.pid 2>/dev/null)
      if [ -n "$rp" ] && tr '\0' ' ' < "/proc/$rp/cmdline" 2>/dev/null | grep -q 'run\.sh'; then
        if kill -TERM -- "-$rp" 2>/dev/null; then
          echo "[tv] 已向启动会话（进程组 -$rp）发送 TERM"
        elif kill -TERM "$rp" 2>/dev/null; then
          echo "[tv] 已停止启动进程 pid=$rp"
        fi
      fi
      # 用 unlink 不用 rm：这台机器上 rm 是 shim，会把文件搬进 H:\.Trash-1000
      # （每次启停丢一个 pid 文件进去，回收站已经堆了几十个）
      unlink target/run.pid 2>/dev/null || true
    fi
    if [ -f "$BIN_PID" ]; then
      p=$(head -1 "$BIN_PID" 2>/dev/null)
      if [ -n "$p" ] && tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | grep -q '/tv$'; then
        kill "$p" 2>/dev/null && echo "[tv] 已停止 pid=$p" || echo "[tv] pid $p 已不存在"
      fi
      unlink "$BIN_PID" 2>/dev/null || true
    fi
    if pgrep -x tv >/dev/null; then
      pkill -x tv
      echo "[tv] 已停止（按进程名）"
    fi
    for _ in $(seq 1 20); do
      if ! alive; then
        echo "[tv] 服务已停止"
        exit 0
      fi
      sleep 0.3
    done
    echo "[tv] 服务仍在响应，停止失败"
    exit 1
    ;;
  status)
    if alive; then
      echo "[tv] 运行中：http://127.0.0.1:8790 pid=$(pgrep -x tv | tr '\n' ' ')"
      exit 0
    fi
    echo "[tv] 未运行"
    exit 1
    ;;
  *)
    echo "用法: $0 start|stop|status"
    exit 2
    ;;
esac
