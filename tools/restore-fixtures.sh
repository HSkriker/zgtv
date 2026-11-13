#!/usr/bin/env bash
# 从本机行情还原 eq/fixtures/**（夹具不入库，见 .gitignore）。
#
# 前置：先跑 tools/sync-data.sh 把通达信数据同步进 data/（全新 clone 没有 data/ 时本脚本会报错退出）。
#
# 只管 .day/.lc1/xdxr 三样。另外两样还原不了，仓库里没有它们的生成脚本：
#   eq/golden/**            —— bar 级金标，由录制机导出；缺 → bars_parity/candles_golden 自动 SKIP
#   crates/*/tests/fixtures/*.http —— 真实 HTTP 录播；缺 → gate1_fixtures 自动 SKIP
# 所以还原完仍然只有「数据在位、金标不在」的跳过态，这是预期，不是坏了。
#
# 默认拒绝覆盖已有夹具：现有夹具是 2026-10-26 那次金标录制的基准，
# 换一批数据进来会让 bars_parity 逐值对表变红。真要换基线用 --force。
set -euo pipefail
cd "$(dirname "$0")/.."

# 录制口径（与 eq/fixtures/manifest.json 一致；manifest 本身不还原，它记的是原始机器的 sha256/mtime）
DAY_CODES="sh600000 sh600030 sh600036 sh600519 sh601318 sh688981 sz000001 sz000002 sz000858 sz002594 sz300059 sz300750"
MIN_CODES="sh600036 sh600519 sz000001 sz000858 sz002594 sz300750"
XDXR_CODES="000001 000002 000858 002594 300059 300750 600000 600030 600036 600519 601318 688981"
LC1_KEEP=4800          # 分钟线只留最后 4800 根；.lc1 定长 32 字节/根
LC1_BYTES=$((LC1_KEEP * 32))

OUT="${FIXTURES_OUT:-eq/fixtures}"
# 行情源**只认显式指定**（不猜任何机器上的客户端目录）：
#   TDX_VIPDOC=... 或 SRC_VIPDOC=...（通达信 vipdoc）→ 都没给就回落仓库内 data/vipdoc 快照。
if [ -n "${TDX_VIPDOC:-}" ]; then
  SRC_VIPDOC="$TDX_VIPDOC"
elif [ -n "${SRC_VIPDOC:-}" ]; then
  :
else
  SRC_VIPDOC="data/vipdoc"
fi
# 除权事件缓存：默认仓库内 data/tdx_xdxr（tv 运行期自建），也可 SRC_XDXR=... 指别处。
SRC_XDXR="${SRC_XDXR:-data/tdx_xdxr}"

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

if [ -d "$OUT/tdx" ] && [ "$FORCE" = 0 ]; then
  echo "[fixtures] $OUT/tdx 已存在，拒绝覆盖（换基线请加 --force）" >&2
  exit 1
fi
if [ ! -d "$SRC_VIPDOC" ]; then
  echo "[fixtures] 找不到行情源 $SRC_VIPDOC —— 用 SRC_VIPDOC=... / TDX_VIPDOC=... 指定，或跑 tools/sync-data.sh 建快照" >&2
  exit 1
fi
if [ ! -d "$SRC_XDXR" ]; then
  echo "[fixtures] 找不到除权缓存 $SRC_XDXR —— 用 SRC_XDXR=... 指定（tv 跑过一次即会自建 data/tdx_xdxr）" >&2
  exit 1
fi

missing=0
copy_day() {  # $1=市场前缀代码  $2=目标子目录
  local mkt="${1:0:2}" code="${1:2:6}" src dst
  src="$SRC_VIPDOC/$mkt/lday/$1.day"
  dst="$OUT/$2/$mkt/lday/$1.day"
  if [ ! -f "$src" ]; then echo "[fixtures] 缺 $src" >&2; missing=$((missing+1)); return; fi
  mkdir -p "$(dirname "$dst")"
  cp -f "$src" "$dst"
}
copy_min() {
  local mkt="${1:0:2}" src dst
  src="$SRC_VIPDOC/$mkt/minline/$1.lc1"
  dst="$OUT/$2/$mkt/minline/$1.lc1"
  if [ ! -f "$src" ]; then echo "[fixtures] 缺 $src" >&2; missing=$((missing+1)); return; fi
  mkdir -p "$(dirname "$dst")"
  tail -c "$LC1_BYTES" "$src" > "$dst"     # 与录制同口径：只取尾部 LC1_KEEP 根
}
copy_xdxr() {
  local src dst
  src="$SRC_XDXR/$1.json"
  dst="$OUT/$2/xdxr/$1.json"
  if [ ! -f "$src" ]; then echo "[fixtures] 缺 $src" >&2; missing=$((missing+1)); return; fi
  mkdir -p "$(dirname "$dst")"
  cp -f "$src" "$dst"
}

# 两处布局都填：eq/fixtures/ 是历史布局，eq/fixtures/tdx/ 是 bars_parity 实际读取的那份
for sub in . tdx; do
  for c in $DAY_CODES; do copy_day "$c" "$sub"; done
  for c in $MIN_CODES; do copy_min "$c" "$sub"; done
  for c in $XDXR_CODES; do copy_xdxr "$c" "$sub"; done
done

if [ "$missing" -gt 0 ]; then
  echo "[fixtures] $missing 个源文件缺失，未还原完整" >&2
  exit 1
fi

echo "[fixtures] 已还原 $OUT：$(find "$OUT" -name '*.day' | wc -l) 个 .day、$(find "$OUT" -name '*.lc1' | wc -l) 个 .lc1、$(find "$OUT" -name '*.json' -path '*xdxr*' | wc -l) 个 xdxr"
echo "[fixtures] 注意：金标 eq/golden 与 .http 录播不在本脚本覆盖范围内，缺它们时相关测试会打印 SKIP"
