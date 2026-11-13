#!/usr/bin/env bash
# tv 唯一启动入口。不要在别处再写一份环境变量。
set -euo pipefail
cd "$(dirname "$0")"

export ZG_ROOT="$PWD"
# 交易日历/盘中判定用本地时区（zg-core calendar 的 Local::now()）：换 UTC 环境会导致
# provisional 形态与指数新鲜度判定漂移，这里显式钉住北京时间
export TZ="${TZ:-Asia/Shanghai}"

# ── 先编译 ──
# 必须排在行业目录重生成之前：build-catalog.sh 用同一个 release 二进制的 `--build-catalog`
# 重生成行业目录；二进制还没编出来时它只能跳过（下次启动自动重试）。
# 内嵌四 crate 是编译输入，Cargo.lock 变更（升级依赖）同样要重编——漏了会拿旧二进制跑新代码。
#
# wsl.exe 非交互 shell（start-tv.bat → tvctl.sh）不读 .bashrc/.profile，cargo 不在 PATH
#（2026-11-06 实测 `cargo: command not found`，set -e 直接杀死启动），这里显式补。
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

# 新旧判据用编译成功后的 stamp 文件，不用二进制 mtime：cargo 空跑（无需重链）不会刷新
# target/release/tv 的 mtime，任何比二进制新的源文件（哪怕只是 tests/）都会让每次启动
# 永久重触发 cargo build——经 start-tv.bat 启动时就撞上上面的 PATH 坑直接起不来。
BIN=target/release/tv
STAMP=target/.build-stamp
need_build=0
if [ ! -x "$BIN" ] || [ ! -f "$STAMP" ]; then
  need_build=1
elif [ -n "$(find src crates Cargo.toml Cargo.lock -path '*/tests/*' -prune -o -newer "$STAMP" -print -quit 2>/dev/null)" ]; then
  need_build=1
fi
if [ "$need_build" = 1 ]; then
  echo "[tv] 编译 release（首次较慢，之后很快）…"
  if ! command -v cargo >/dev/null 2>&1; then
    echo "[tv] 错误：找不到 cargo，且 ~/.cargo/env 不存在——无法按需重编译。" >&2
    echo "[tv] 交互终端里手动 cargo build --release 一次，或修好 PATH 再启动。" >&2
    exit 1
  fi
  cargo build --release
  touch "$STAMP"
fi

# ── 行情数据源 ──
# 2026-11-06 起默认「直读通达信目录」，不再每次启动把 11.5GB 拷进 data/vipdoc。
#
# 背景：DFZQ 盘后重写全部 9682 个 .lc1。每个 .lc1 是 173 个交易日的滚动窗口
#（32 字节/分钟根，173×240×32 = 1,328,640 字节），新的一天进来就把最老的一天挤掉、
# 整体前移 7680 字节 —— 内容天天变，而 rsync 本地默认模式是整文件搬运，于是每次启动都要
# 搬完整的 11.5GB。跨两块 Windows 盘（drvfs）实测约 2.4 分钟，且**全程零输出**，
# 看着就像卡死（2026-11-06 这次启动就是这样被 Ctrl+C 掉的，rsync 报 SIGINT code 20）。
#
# 应用其实只读 vipdoc（crates/zg-core/src/paths.rs 的默认值就是 G:\...\vipdoc），
# 直读零拷贝：实测端口 0.5 秒就绪、数据末根当天。
#
# 三种模式：
#   · direct（默认）—— 直读通达信目录，不拷贝
#   · copy           —— 增量同步进本地快照后读快照（TV_SYNC=copy ./run.sh）
#                      用于离线机，或怕 DFZQ 盘中写盘时读到半个文件
#   · 显式 ZG_VIPDOC —— 调用方指定数据源，原样使用，不拷贝不改写
#
# 代价（direct）：不再有本地稳定快照；DFZQ 盘中写盘时读 .lc1 可能读到半个文件
#（拷贝模式同样会拷到半个文件，这个风险两模式一样存在，不是 direct 独有）。
TDX_VIPDOC="${TDX_VIPDOC:-/mnt/g/Program Files/DFZQ/vipdoc}"
TDX_GBBQ="${TDX_GBBQ:-/mnt/g/Program Files/DFZQ/T0002/hq_cache/gbbq}"
TDX_HQ_DIR="$(dirname "$TDX_GBBQ")"

# 旧开关 TV_NO_AUTOSYNC=1 的等价物：只关拷贝，不关直读
SYNC_MODE="${TV_SYNC:-direct}"
if [ -n "${TV_NO_AUTOSYNC:-}" ]; then
  SYNC_MODE="direct"
fi

if [ -n "${ZG_VIPDOC:-}" ]; then
  # 显式给了 vipdoc 但没给 gbbq 时补默认值，否则 set -u 直接崩在 export（unbound variable）
  if [ -z "${ZG_GBBQ:-}" ]; then
    if [ -f "$TDX_GBBQ" ]; then ZG_GBBQ="$TDX_GBBQ"; else ZG_GBBQ="$PWD/data/tdx/gbbq"; fi
  fi
  SRC_DESC="显式指定 ZG_VIPDOC=$ZG_VIPDOC"
elif [ "$SYNC_MODE" = "copy" ]; then
  # 拷贝模式：把通达信增量同步进本地快照，然后读快照
  # 探测范围必须含 minline：DFZQ 约 15:57 写 .day、16:03 才写 .lc1，只查 lday 时
  # 「同步跑在两次写入之间」会漏掉分钟线，且 .lc1 比标记新之后再也没有人看它（实测
  # 2026-11-03 那次同步后 SH/BJ 分钟线永久停在前一交易日）。
  # 跳过条件：显式 export 了 ZG_VIPDOC（你已自选数据源）；或本机没装通达信。
  # 代价：无更新的晚上也要 stat 一遍（lday+minline 约 14 秒），换来分钟线不会静默滞后。
  if [ ! -d "$TDX_VIPDOC/sh/lday" ]; then
    echo "[tv] 拷贝模式但找不到通达信目录 $TDX_VIPDOC，回落本地快照" >&2
  else
    stale=""
    if [ ! -f data/.last-sync ]; then
      stale=1   # 快照在但没有同步标记：当作有更新，补一次增量对平
    else
      for m in sh sz bj; do
        if [ -n "$(find "$TDX_VIPDOC/$m/lday" -name '*.day' -newer data/.last-sync -print -quit 2>/dev/null)" ]; then
          stale=1; break
        fi
      done
      if [ -z "$stale" ]; then
        for m in sh sz bj; do
          if [ -n "$(find "$TDX_VIPDOC/$m/minline" -name '*.lc1' -newer data/.last-sync -print -quit 2>/dev/null)" ]; then
            stale=1; break
          fi
        done
      fi
      if [ -z "$stale" ] && { [ "$TDX_GBBQ" -nt data/.last-sync ] \
          || { [ -f "$TDX_HQ_DIR/block_gn.dat" ] && [ "$TDX_HQ_DIR/block_gn.dat" -nt data/.last-sync ]; } \
          || { [ -f "$TDX_HQ_DIR/tdxzs3.cfg" ] && [ "$TDX_HQ_DIR/tdxzs3.cfg" -nt data/.last-sync ]; } \
          || { [ -f "$TDX_HQ_DIR/tdxhy.cfg" ] && [ "$TDX_HQ_DIR/tdxhy.cfg" -nt data/.last-sync ]; }; }; then
        stale=1
      fi
    fi
    if [ -n "$stale" ]; then
      echo "[tv] 通达信目录有新数据，自动同步进 tv 快照…（TV_SYNC=direct 可关闭）"
      TDX_VIPDOC="$TDX_VIPDOC" TDX_GBBQ="$TDX_GBBQ" bash tools/sync-data.sh
    fi
  fi
  ZG_VIPDOC="$PWD/data/vipdoc"
  ZG_GBBQ="$PWD/data/tdx/gbbq"
  SRC_DESC="本地快照 data/vipdoc（TV_SYNC=copy）"
elif [ -d "$TDX_VIPDOC/sh/lday" ] && [ -f "$TDX_GBBQ" ]; then
  # 直读通达信目录：零拷贝，启动不再等同步
  ZG_VIPDOC="$TDX_VIPDOC"
  ZG_GBBQ="$TDX_GBBQ"
  SRC_DESC="直读通达信目录 $TDX_VIPDOC"
else
  # 本机没装通达信（或路径不对）：回落本地快照。快照可能不是最新 —— 要说清楚，
  # 「陈旧」和「缺失」一样致命，不能静默让用户以为看到的是当天数据。
  ZG_VIPDOC="$PWD/data/vipdoc"
  ZG_GBBQ="$PWD/data/tdx/gbbq"
  SRC_DESC="本地快照 data/vipdoc（本机无通达信目录）"
  if [ ! -d "$ZG_VIPDOC" ]; then
    echo "[tv] 警告：找不到通达信目录 $TDX_VIPDOC，且本地快照 data/vipdoc 也不存在" >&2
    echo "[tv] 本机没有可用行情源：用 ZG_VIPDOC=/path/to/vipdoc 指到别的通达信目录，" >&2
    echo "[tv] 或跑 tools/sync-data.sh 建一份快照（副本模式见 TV_SYNC=copy）" >&2
  else
    echo "[tv] 警告：找不到通达信目录 $TDX_VIPDOC，回落本地快照（数据可能不是最新）" >&2
  fi
fi
export ZG_VIPDOC ZG_GBBQ

export ZG_XDXR_CACHE="${ZG_XDXR_CACHE:-$PWD/data/tdx_xdxr}"

for p in "$ZG_VIPDOC" "$ZG_GBBQ" "$ZG_XDXR_CACHE"; do
  if [ ! -e "$p" ]; then
    echo "[tv] 找不到路径: $p" >&2
    echo "[tv] 用环境变量覆盖，例如 ZG_VIPDOC=\"/mnt/x/vipdoc\" ./run.sh" >&2
    echo "[tv] 没有本地快照时：TV_SYNC=copy ./run.sh 或 tools/sync-data.sh 建一份" >&2
    exit 1
  fi
done

# 数据新鲜度只给人看（应用自己还判末根日期并标 stale）：stat 一个文件，毫秒级
FRESH="$ZG_VIPDOC/sh/lday/sh000001.day"
if [ -f "$FRESH" ]; then
  echo "[tv] 行情源: $SRC_DESC，末根日线 $(date -d "@$(stat -c %Y "$FRESH")" '+%Y-%m-%d %H:%M')"
else
  echo "[tv] 行情源: $SRC_DESC（末根日线文件缺失，新鲜度交给应用判定）"
fi

# ── 行业目录 ──
# 分类文件（tdxzs3/tdxhy.cfg）更新时重生成 industry_catalog.json（只读 hq_cache，秒级）。
# 直读模式下没有 sync-data.sh 兜底，这一步必须在这里做，否则行业目录会永远停在旧口径。
TV_HQ_DIR="$TDX_HQ_DIR" bash tools/build-catalog.sh

# ── 标的表（两个 JSON 都不进 git，新 clone 必须能自举）──
if [ ! -f data/market/stocks.json ]; then
  echo "[tv] data/market/stocks.json 不存在，联网拉取全市场股票列表…"
  if ! "$BIN" --fetch-stocks; then
    echo "[tv] 拉取失败：无网机器请手动放一份 data/market/stocks.json" >&2
    echo "[tv] 格式：[{\"code\":\"000001\",\"name\":\"平安银行\"}, …]" >&2
    [ -f data/symbols.json ] || exit 1
  fi
fi
if [ ! -f data/symbols.json ] \
   || { [ -f data/market/stocks.json ] && [ data/market/stocks.json -nt data/symbols.json ]; }; then
  echo "[tv] 生成 data/symbols.json（market + 拼音首字母/全拼）…"
  "$BIN" --gen-symbols
fi

echo "[tv] 启动: http://127.0.0.1:8790"
exec "$BIN"
