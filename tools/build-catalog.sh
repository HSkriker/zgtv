#!/usr/bin/env bash
# 行业目录（modules/market/industry_catalog.json）重生成。
#
# 读通达信客户端的分类文件（hq_cache/tdxzs3.cfg + tdxhy.cfg），写 tv 的行业目录。
# 只读 hq_cache，秒级，不碰行情。分类文件没更新时直接跳过，不产生无谓 diff。
#
# 两个调用方共用这一份逻辑，避免分叉：
#   · run.sh          —— 直读模式（默认）下也要重生成，否则行业目录永远停在旧口径
#   · tools/sync-data.sh —— 拷贝模式（TV_SYNC=copy）末尾重生成
#
# 环境变量：
#   TV_HQ_DIR —— 通达信 hq_cache 目录。**没有默认值**：两个调用方都会显式传入
#                 （run.sh 传探测到的客户端目录，sync-data.sh 传 SRC_GBBQ 的同级目录）；
#                 手跑时不给就跳过重生成，不猜任何机器上的路径。
set -euo pipefail
cd "$(dirname "$0")/.."

HQ="${TV_HQ_DIR:-}"
CAT=modules/market/industry_catalog.json
BIN=target/release/tv

if [ -z "$HQ" ]; then
  echo "[catalog] 未指定 TV_HQ_DIR（通达信 hq_cache 目录），跳过行业目录重生成" >&2
  echo "[catalog] 手动跑时：TV_HQ_DIR=\"<通达信安装目录>/T0002/hq_cache\" bash tools/build-catalog.sh" >&2
  exit 0
fi

if [ ! -x "$BIN" ]; then
  # run.sh 把编译排在前面；手动跑本脚本时二进制可能还没编出来
  echo "[catalog] 警告：$BIN 不存在，跳过行业目录重生成（下次启动自动重试）"
  exit 0
fi
if [ ! -f "$HQ/tdxzs3.cfg" ] && [ ! -f "$HQ/tdxhy.cfg" ]; then
  echo "[catalog] 警告：$HQ 下没有分类文件，跳过行业目录重生成"
  exit 0
fi
if [ -f "$CAT" ] \
   && { [ ! "$HQ/tdxzs3.cfg" -nt "$CAT" ] && [ ! "$HQ/tdxhy.cfg" -nt "$CAT" ]; }; then
  echo "[catalog] 分类文件未更新，沿用现有行业目录"
  exit 0
fi

# 重生成失败不掩埋启动：降级沿用旧 catalog，下次启动自动重试
if "$BIN" --build-catalog --hq-cache "$HQ" --out "$CAT"; then
  echo "[catalog] 行业目录已重生成（通达信分类文件有更新）"
else
  echo "[catalog] 警告：行业目录重生成失败，沿用旧 catalog（下次启动重试）" >&2
fi
