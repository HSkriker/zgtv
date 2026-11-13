#!/usr/bin/env bash
# 行情数据同步：把通达信客户端（东方证券 DFZQ）的行情增量同步进 tv/data/ 快照。
#
# ⚠ 2026-11-06 起**默认不再走这条路**：run.sh 默认直读通达信目录（零拷贝，启动 0.5 秒就绪）。
# 原因：DFZQ 每天盘后重写全部 9682 个 .lc1（每个是 173 个交易日的滚动窗口，每天整体前移
# 7680 字节），整文件 rsync 每次都要搬 11.5GB、跨两块 Windows 盘约 2.4 分钟且全程无输出，
# 看着像卡死。直读模式见 run.sh 的「行情数据源」段。
#
# 本脚本现在只在两种情况下用：
#   ① 要做本地稳定快照（离线机 / 怕盘中写盘撞上）：TV_SYNC=copy ./run.sh
#   ② 手动强制对平：`tools/sync-data.sh`（TDX_VIPDOC / TDX_GBBQ 可指其它通达信目录）
#
# 同步内容：vipdoc 日线(.day) + 1分钟线(.lc1) + gbbq 除权原始文件。
# xdxr 缓存（data/tdx_xdxr/）不用同步：tv 进程读 gbbq 后会自行落盘重建，
# 怀疑陈旧时把 data/tdx_xdxr 整目录删掉即可强制重建。
#
# 行业目录/热度/缠论扫描三个缓存 tv 会自行本地重算：
#   catalog —— 本脚本末尾检测分类文件变化后重生成；
#   热度/扫描 —— tv 服务启动时检测过期自动重算（/api/heat-rebuild、/api/scan-rebuild 可手动触发）。
# 唯一不可再生的是 index_constituents_cache.json（成分股目录源头在东财，半年调仓，离线冻结）。
set -euo pipefail
cd "$(dirname "$0")/.."

# 同步标记记的是**开始**时刻（再留 2 秒余量），不是结束时刻：
# rsync 跑的同时 DFZQ 可能还在写文件（实测 .day 15:57、.lc1 16:03 分批落盘），
# 同步期间写入的文件 mtime 会晚于开始时刻 ⇒ 下次启动仍判「有新数据」再对平一次。
# 用结束时刻打标记则这些文件永久落在标记之前，半截同步再也补不回来（2026-11-03 事故）。
START=$(date +%s)

# 行情源只认显式指定，不猜任何机器上的客户端目录（run.sh 调用时一定会传 TDX_VIPDOC/TDX_GBBQ）。
SRC_VIPDOC="${TDX_VIPDOC:-${SRC_VIPDOC:-}}"
SRC_GBBQ="${TDX_GBBQ:-${SRC_GBBQ:-}}"

if [ -z "$SRC_VIPDOC" ] || [ -z "$SRC_GBBQ" ]; then
  cat >&2 <<'MSG'
[sync] 未指定行情源。请用环境变量给出通达信目录：
[sync]   TDX_VIPDOC="<通达信安装目录>/vipdoc" TDX_GBBQ="<通达信安装目录>/T0002/hq_cache/gbbq" bash tools/sync-data.sh
[sync] （用 ./run.sh 或 TV_SYNC=copy ./run.sh 时会自动传入，无需手写）
MSG
  exit 1
fi

for p in "$SRC_VIPDOC" "$SRC_GBBQ"; do
  if [ ! -e "$p" ]; then
    echo "[sync] 找不到 $p（用 SRC_VIPDOC / SRC_GBBQ 环境变量覆盖）" >&2
    exit 1
  fi
done

for m in sh sz bj; do
  rsync -a "$SRC_VIPDOC/$m/lday/"    "data/vipdoc/$m/lday/"
  rsync -a "$SRC_VIPDOC/$m/minline/" "data/vipdoc/$m/minline/"
done
rsync -a "$SRC_GBBQ" "data/tdx/gbbq"
# 概念题材文件（block_gn.dat 与 gbbq 同目录）；旧客户端没有该文件时跳过，不影响其余同步
if [ -f "$(dirname "$SRC_GBBQ")/block_gn.dat" ]; then
  rsync -a "$(dirname "$SRC_GBBQ")/block_gn.dat" "data/tdx/block_gn.dat"
fi
# 行业分类文件有更新时重生成 catalog（只读通达信 hq_cache，秒级；
# tdxhy.cfg 无股票名，新成员名字回落为代码。catalog 变了 overview 指纹自动失效重算）
# 逻辑与 run.sh 共用 tools/build-catalog.sh，两处不分叉
TV_HQ_DIR="$(dirname "$SRC_GBBQ")" bash tools/build-catalog.sh
# .last-sync 放在最后：只标记完整走完的同步（此前放在 catalog 重生成之前，
# 重生成一失败脚本就退出、.last-sync 却已更新，旧 catalog 会被静默跳过重试）。
# 时间取 START-2（原因见文件头）：同步窗口内写入的文件下次仍会被判为新数据。
touch -d "@$((START - 2))" data/.last-sync

echo "[sync] 完成：$(find data/vipdoc -name '*.day' | wc -l) 个 .day，$(find data/vipdoc -name '*.lc1' | wc -l) 个 .lc1"
echo "[sync] 缓存指纹已随 mtime 更新，服务下次请求会自动重算受影响的 K 线/缠论缓存。"
