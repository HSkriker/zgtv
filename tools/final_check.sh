#!/usr/bin/env bash
# tv 终检：文档工单数 / 静态禁令 / 产物 / 接口 / ETag。
# 用法: ./tools/final_check.sh [--build]
set -uo pipefail
cd "$(dirname "$0")/.."

BASE="${BASE:-http://127.0.0.1:8790}"
fail=0
ok()   { printf 'PASS  %s\n' "$1"; }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

# 接线门禁扫描目标：功能层拆成「根文件 + 目录子模块」后，门禁查的是整个功能层
# （口径与断言不变，只是范围从单文件扩到模块全集；文件不存在则跳过该候选）。
_gather() { local a=(); for f in "$@"; do [ -f "$f" ] && a+=("$f"); done; printf '%s\n' "${a[@]}"; }
mapfile -t LIVE_SRC < <(_gather src/live.rs src/live/*.rs src/daymerge.rs)
mapfile -t OVV_SRC < <(_gather src/overview.rs src/overview/*.rs)
# 文件集必须完整：_gather 会静默跳过不存在的候选，而「禁止出现某字面量」这类反向门禁
# 少扫一个文件就等于少一个扫查面（改名/漏提交时会静默变绿）。数量在这里钉死。
if [ "${#LIVE_SRC[@]}" -ge 4 ] && [ "${#OVV_SRC[@]}" -ge 5 ]; then
  ok "功能层模块文件集完整（live ${#LIVE_SRC[@]} 个 / overview ${#OVV_SRC[@]} 个）"
else
  bad "模块文件缺失 ⇒ 反向门禁静默失扫：live=${LIVE_SRC[*]} overview=${OVV_SRC[*]}"
fi
# 「某某单测/函数还在」这类门统一走 has_fn：① 要求 `fn NAME(` 完整形态，改名成
# NAME_x 不再算通过（grep -qF 'fn NAME' 是前缀匹配，只有整段删掉才会红）；
# ② 只在剥掉注释的代码视图上查，注释里提一句函数名不算它还在。
has_fn() { local n=$1; shift; [ $# -gt 0 ] && python3 tools/gate_scan.py fn "$n" "$@"; }
# 接线门的代码视图：剥掉注释后再匹配，注释里的字面量既不能撑绿也不能翻红。
# 这个视图的正确性撑着下面一整族门，所以先跑它自带的自测（过剥/漏剥都会在这里翻红，
# 而不是让某条门静默失去匹配域）。
if _gs_err=$(python3 tools/gate_scan.py selftest 2>&1); then
  ok "剥注释器自测通过（链序门/名字门的匹配域可信）"
else
  bad "剥注释器自测失败 ⇒ 接线门的匹配域不可信：$(printf '%s\n' "$_gs_err" | tail -1)"
fi
LIVE_CODE="$(python3 tools/gate_scan.py code "${LIVE_SRC[@]}" 2>/dev/null)"

echo "== 1. 文档 =="
# 工单文档属开发过程记录，公开仓不携带（见 .gitignore）；不在位就跳过而不是变红
if [ -f docs/工单-初级工程师.md ]; then
  n=$(grep -c '^## TV-' docs/工单-初级工程师.md 2>/dev/null || true)
  n=${n:-0}
  if [ "$n" -eq 33 ]; then ok "工单数 = 33"; else bad "工单数 = $n（应为 33）"; fi
else
  skip "工单文档不在本仓（开发记录不公开），本门跳过"
fi
# 现状文档（README / 模块来源说明）里反引号点名的函数，必须在代码里真找得到。
# 依据：TV-32 工单曾写 `align_remote_minute`，函数拆分后没人回头改文档，读文档的人
# 去搜一个不存在的函数。工单正文是历史不改写，但「声称当前实现」的文档要对得上代码。
out=$(python3 - <<'PY'
import re, pathlib

CODE_EXT = {".rs", ".js", ".py", ".sh"}
blob = "\n".join(
    p.read_text(encoding="utf-8", errors="ignore")
    for root in ("src", "crates", "web/js", "tools")
    for p in pathlib.Path(root).rglob("*")
    if p.is_file() and p.suffix in CODE_EXT
)
missing = []
for doc in ("README.md", "docs/模块来源与使用说明.md"):
    text = pathlib.Path(doc).read_text(encoding="utf-8")
    for no, line in enumerate(text.splitlines(), 1):
        for m in re.finditer(r"`([A-Za-z_][A-Za-z0-9_]{4,})\(", line):
            name = m.group(1)
            if not re.search(r"\b%s\b" % re.escape(name), blob):
                missing.append(f"{doc}:{no} {name}()")
    # 已知失效名：反引号里不带括号的函数名（`align_remote_minute`）上面的规则看不见，
    # 而那正是 TV-32 文档留到今天的那个形态。改名/拆函数时把旧名追加到这里。
    for dead in ("align_remote_minute",):
        for no, line in enumerate(text.splitlines(), 1):
            if dead in line:
                missing.append(f"{doc}:{no} 已失效函数名 {dead}")
print("\n".join(missing))
PY
)
if [ -z "$out" ]; then
  ok "现状文档点名的函数在代码里都存在（含已失效名黑名单）"
else
  bad "文档引用了不存在的函数：$(printf '%s' "$out" | tr '\n' '；')"
fi

echo
echo "== 2. 静态禁令 =="
scan_web() {
  local name="$1"; shift
  local hit
  # --include 覆盖全部前端文本扩展名：漏一个（.mjs/.ts/...）就是禁令绕过口
  hit=$(grep -rInE --include='*.js' --include='*.mjs' --include='*.html' --include='*.css' "$@" web/ 2>/dev/null || true)
  if [ -n "$hit" ]; then
    bad "$name"
    printf '%s\n' "$hit" | head -5 | sed 's/^/       /'
  else
    ok "$name"
  fi
}

scan_web "无 charting_library" 'charting_library|TradingView\.Charting'
# 实时轮询只准用递归 setTimeout（页面隐藏时跳过取数）；setInterval 会在后台无限堆叠
scan_web "轮询用递归 setTimeout（无 setInterval / setImmediate）" 'setInterval|setImmediate'
scan_web "无 WebSocket / SSE（保持 HTTP 轮询）" 'EventSource|new WebSocket'

# 实时链路接线（方案 C，2026-11-02）：所有网络调用集中在 src/live（根文件 + 子模块）
for pat in 'startLivePoll' 'visibilityState' 'TV.api.quote('; do
  grep -qF "$pat" web/js/main.js && ok "main.js 接线 $pat" || bad "main.js 缺 $pat"
done
# 实时节拍：报价 / 状态 / 分钟线共用 300s 最短间隔闸，心跳 15s 只检查闸口、不发请求
grep -q 'const LIVE_MS = 300000;' web/js/main.js \
  && ok "实时最短间隔闸 LIVE_MS = 300s" || bad "实时最短间隔不是 300s（LIVE_MS 漂移）"
# 取模计数（liveTicks % 4）在换标的后要对齐到下一个边界，新股票会空等；一律用绝对时刻设闸
if grep -q 'liveTicks %' web/js/main.js; then bad "main.js 退回 tick 取模设闸（换标的会空等一个周期）"; else ok "节拍用绝对时刻设闸（无 tick 取模）"; fi
# 1 分钟档 = 离线采集（两端都要成立）：后端不接实时链，前端连报价都不发
grep -q 'period == "1"' "${LIVE_SRC[@]}" && ok "live.rs：1 分钟档不接实时分钟链（只读本地 .lc1）" \
  || bad "live.rs 的 1 分钟离线下闸丢失（会重新打实时源）"
grep -q "state.period === '1'" web/js/main.js && grep -q 'function localMinuteBadge' web/js/main.js \
  && ok "main.js：1 分钟档不发报价，徽标改报本地采集时刻" || bad "main.js 的 1 分钟报价豁免丢失"
# 非交易时段图可能来自远端（as_of 当天）而本地快照仍是 T-1，徽标写死 local_last 会误报
grep -q 'function idleBadge' web/js/main.js && grep -q 'idleBadge()' web/js/main.js \
  && ok "非交易时段徽标读 barsMeta 的 as_of/source" || bad "非交易时段徽标写死 local_last"
# source=local 时徽标要用**图表自己的** as_of：拿索引的 local_last 顶替会让
# 「个股 .day 停了但指数是新的」这种情况说谎（实测停牌/未下载个股）
grep -q '本地DFZQ 截至' web/js/main.js \
  && ok "本地徽标用图表 as_of（不拿索引日期顶替）" || bad "本地徽标未用图表 as_of"
# 远端为何被拒/并入了哪个源要能看见：meta.detail 进徽标 tooltip
grep -q 'barsMeta.detail' web/js/main.js && has_fn norm_date "${LIVE_SRC[@]}" \
  && ok "远端源状态可见（meta.detail → 徽标 tooltip）" || bad "meta.detail 未接前端"
# 复权基兼容门（remote_mergeable）三件事缺一不可：
#  ① 两段采样（最旧+最近）：前复权都锚在最新价，基差只随历史深度放大
#  ② 同精度比较（双侧 round2）+ 绝对容差下限：老历史 qfq 价被分红压到 1.5 元级时，
#     半分的舍入差就是 0.33%，纯相对判据会误杀同基源（实测 600346 vs TQ 0.1316%）
#  ③ 老历史错基必须拒（回归单测锁住）
grep -q 'REMOTE_ROUND_FLOOR' "${LIVE_SRC[@]}" && grep -q '"最旧"' "${LIVE_SRC[@]}" \
  && grep -q '"最近"' "${LIVE_SRC[@]}" \
  && ok "兼容门：两段采样 + 同精度/绝对下限容差" || bad "兼容门缺两段采样或同精度容差"
has_fn remote_mergeable_rejects_old_history_mismatch "${LIVE_SRC[@]}" \
  && has_fn remote_mergeable_accepts_rounding_noise_on_deflated_history "${LIVE_SRC[@]}" \
  && ok "兼容门回归单测齐（老历史错基必拒 / 舍入噪声不误杀）" || bad "兼容门回归单测缺失"
# 光有测试名不够：断言被反向（该拒写成放行）照样绿 ⇒ 连断言方向一起查
gate_body() { local f; for f in "${LIVE_SRC[@]}"; do sed -n "/fn $1()/,/^    }$/p" "$f"; done; }
if gate_body remote_mergeable_rejects_old_history_mismatch | grep -q 'assert!(!remote_mergeable' \
  && gate_body remote_mergeable_accepts_rounding_noise_on_deflated_history | grep -q 'assert!(remote_mergeable'; then
  ok "兼容门单测断言方向正确（拒/放行各就各位）"
else
  bad "兼容门单测断言方向反了（该拒的写成放行 / 该放行的写成拒）"
fi
# 反向断言：旧实现「只看最近 200 根」不得复活
if grep -q 'remote.iter().rev()' "${LIVE_SRC[@]}"; then
  bad "兼容门退回只看最近 200 根（老历史错基会漏放）"
else
  ok "兼容门未退回单段采样"
fi
[ -f tools/adj_gate_parity.py ] && ok "复权基对拍脚本在位（tools/adj_gate_parity.py）" \
  || bad "缺 tools/adj_gate_parity.py（门的实测依据无处可查）"
# 日线远端源**白名单**：只有 TQ 与 MAC（与本地 gbbq 同基）。公开源复权口径不同源，
# 留着只是白跑网络；拿异基源凑一张「看起来更新」的图比 stale 图更坑人。
for pat in 'kline_em(' 'kline_ths(' 'kline_datahub(' 'kline_tencent('; do
  # 用代码视图而不是原文：注释里写「禁止 kline_em(」是文档，不该把门翻红
  if printf '%s\n' "$LIVE_CODE" | grep -qF "$pat"; then
    bad "日线远端链仍接异基公开源 $pat（白名单只剩 TQ+MAC）"
  fi
done
grep -qF 'kline_tq_day(' <<<"$LIVE_CODE" && grep -qF 'mac_daily_rows(' <<<"$LIVE_CODE" \
  && ok "日线远端源白名单 = TQ + MAC" || bad "日线远端源白名单接线缺失"
# 本地已是最近交易日时不该问远端：纯本地最干净，也没有换基风险
grep -q 'local_fresh' "${LIVE_SRC[@]}" \
  && ok "本地已是最新交易日时不问远端（省网络 + 零换基风险）" \
  || bad "本地最新时仍去问远端（多余取数 + 换基风险）"
# 合并必须本地优先：远端只补本地没有的日期（远端覆盖 = 11.6 年历史整体换基）
has_fn merge_days_prefers_local_on_overlap "${LIVE_SRC[@]}" \
  && has_fn merge_days_normalizes_date_keys "${LIVE_SRC[@]}" \
  && ok "合并语义 = 本地优先（单测锁住：重叠日用本地、缺日由远端补）" \
  || bad "合并语义缺本地优先的回归单测"
for pat in '"/api/quote"' '"/api/live"' '"/api/market-status"'; do
  grep -qF "$pat" src/router.rs && ok "router.rs 接线 $pat" || bad "router.rs 缺 $pat"
done
grep -qF 'live::warmup' src/main.rs && ok "main.rs 接线 live::warmup" || bad "main.rs 缺 live::warmup"
# —— TV-31：bin 只做启动编排，业务模块注册在 lib（集成测试要能看见 router::dispatch）——
[ -f src/lib.rs ] && ok "src/lib.rs 存在（bin 只做启动编排）" || bad "缺 src/lib.rs"
[ -f tests/api_contract.rs ] && ok "路由契约测试存在" || bad "缺 tests/api_contract.rs"
grep -qF 'pub mod router;' src/lib.rs && ok "lib.rs 注册 pub mod router" || bad "lib.rs 缺 pub mod router"
for pat in 'TV_LIVE' 'TV_OFFLINE'; do
  grep -qF "$pat" "${LIVE_SRC[@]}" && ok "live.rs 接线 $pat" || bad "live.rs 缺 $pat"
done
# 函数名是外部调用点（live::enabled / day_series / minute_series）的接线锚 ⇒ 走 has_fn
for n in enabled ttl_bucket day_series minute_series; do
  has_fn "$n" "${LIVE_SRC[@]}" && ok "live.rs 接线 fn $n" || bad "live.rs 缺 fn $n"
done
# TQ 两个致命接口必须在拒发层拦死：src/ 不得出现调用字面量（注释不加引号，不会误报）
if grep -rInE '"get_market_snapshot"|"get_scjy_value"' src/ >/dev/null 2>&1; then
  bad "src/ 仍调用 TQ 致命接口（get_market_snapshot / get_scjy_value）"
else
  ok "src/ 未调用 TQ 致命接口"
fi

# 公网 URL 要排除 localhost，单独判（scan_web 不支持排除）
cdn=$(grep -rInE --include='*.html' --include='*.js' --include='*.css' 'https?://' web/ 2>/dev/null \
  | grep -vE '127\.0\.0\.1|localhost' || true)
if [ -n "$cdn" ]; then
  bad "存在公网 URL"
  printf '%s\n' "$cdn" | head -5 | sed 's/^/       /'
else
  ok "无公网 URL（localhost 除外）"
fi

# 脱离 zg（2026-11-01）：crate 内嵌 crates/、数据内嵌 data|modules，运行链路不再引用平级 zg 仓库
grep -q '\.\./zg' Cargo.toml && bad "Cargo.toml 仍指向 ../zg" || ok "Cargo.toml 无 ../zg 依赖"
zgr=$(grep -rIn 'zg_root' src/ 2>/dev/null || true)
if [ -n "$zgr" ]; then bad "src/ 仍有 zg_root 引用"; printf '%s\n' "$zgr" | head -3 | sed 's/^/       /'
else ok "src/ 无 zg_root 引用"; fi
# 行情源：2026-11-06 起默认直读通达信目录（不再每次启动把 11.5GB 拷进 data/vipdoc）
grep -q 'TDX_VIPDOC:-/mnt/g/Program Files/DFZQ/vipdoc' run.sh \
  && ok "run.sh 默认直读通达信目录（零拷贝启动）" \
  || bad "run.sh 默认行情源不是通达信目录"
# 拷贝模式仍要保留（离线机 / 怕盘中写盘撞上），且行业目录重生成在直读模式下也要接线
grep -q 'TV_SYNC' run.sh && grep -q 'build-catalog.sh' run.sh \
  && ok "run.sh 接线：TV_SYNC=copy 可切回快照模式 + 行业目录重生成" \
  || bad "run.sh 缺 TV_SYNC / build-catalog.sh 接线"
# 只查 lday 会漏掉分钟线：DFZQ 的 .lc1 比 .day 晚写约 6 分钟，拷贝模式跑在两次写入
# 之间时分钟线永久滞后（2026-11-03 事故：SH/BJ 停在前一交易日而 SZ 正常）
grep -q 'minline' run.sh && grep -qF "*.lc1" run.sh \
  && ok "run.sh 拷贝 gate 覆盖 minline（分钟线不会再被漏掉）" \
  || bad "run.sh 拷贝 gate 缺 minline（.lc1 会永久滞后）"
# 标记必须记「开始」而不是「结束」：否则同步期间写入的文件永远落在标记之前
grep -qF 'touch -d "@' tools/sync-data.sh && grep -q 'START=' tools/sync-data.sh \
  && ok "sync-data.sh 以同步开始时刻打标记（半截同步可自愈）" \
  || bad "sync-data.sh 仍用结束时刻打标记（半截同步永久隐形）"
grep -q 'pub fn rebuild' src/heat.rs && grep -q 'pub fn rebuild' src/scan.rs \
  && grep -q 'heat::stale' src/main.rs && grep -q 'scan::stale' src/main.rs \
  && ok "热度/扫描缓存本地重算已接线（启动自愈 + rebuild 端点）" \
  || bad "缺热度/扫描缓存本地重算接线"
# 行业目录重生成：逻辑抽到 tools/build-catalog.sh，run.sh（直读模式）与 sync-data.sh
# （copy 模式）共用同一份 —— 两处都要挂上，否则直读模式下行业目录永远停在旧口径
grep -q -- '--build-catalog' tools/build-catalog.sh \
  && grep -q 'build-catalog.sh' tools/sync-data.sh && grep -q 'build-catalog.sh' run.sh \
  && ok "行业目录重生成已接线（build-catalog.sh：run.sh + sync-data.sh 共用）" \
  || bad "行业目录重生成接线缺失（build-catalog.sh / sync-data.sh / run.sh）"

echo
echo "== 3. 产物 =="
[ -f data/symbols.json ] && ok "data/symbols.json" || bad "缺少 data/symbols.json（跑 target/release/tv --gen-symbols）"
if [ -f vendor/lightweight-charts.standalone.production.js ]; then
  sz=$(stat -c%s vendor/lightweight-charts.standalone.production.js)
  if [ "$sz" -gt 150000 ]; then ok "lightweight-charts 本地包 ${sz}B"; else bad "lightweight-charts 体积异常 ${sz}B"; fi
else
  bad "缺少 vendor/lightweight-charts.standalone.production.js"
fi

if [ "${1:-}" = "--build" ]; then
  echo
  echo "== 4. 编译 =="
  if out=$(cargo build --release 2>&1); then
    w=$(printf '%s\n' "$out" | grep -c '^warning' || true)
    if [ "$w" -eq 0 ]; then ok "cargo build --release 无 warning"; else bad "cargo 有 $w 条 warning"; fi
  else
    bad "cargo build --release 失败"
  fi
  # 季/年重采样是本轮新写的纯函数，单测覆盖桶标签与量额求和
  if tout=$(cargo test --release --offline 2>&1) && printf '%s' "$tout" | grep -q 'test result: ok'; then
    ok "cargo test（$(printf '%s' "$tout" | grep -oE '[0-9]+ passed' | head -1)）"
  else
    bad "cargo test 失败"
    printf '%s\n' "$tout" | tail -15 | sed 's/^/       /'
  fi
  # 成员 crate 测试（根包 cargo test 不会带它们）：zg-market 蜡烛金标 + zg-chan registry 对拍
  # + zg-core（含 max_bi_num 表快照）。必须同时要求「零 FAILED」——防止单套件失败被别的套件的 ok 行掩盖
  # ZG_ROOT 必须显式给：zg-chan 的 bi_window_causality 只重设 root/vipdoc/gbbq/xdxr 四项，
  # 没设 vendor_dir——不注入 ZG_ROOT 时它按「测试二进制所在目录」推成 target/release/deps/modules/，
  # 除权缓存一旦过期就会去那里找 vendor 并报「vendor 目录不存在」（crates/ 是逐字节拷贝，不能改测试）。
  if mout=$(ZG_ROOT="$PWD" cargo test --release --offline -p zg-core -p zg-market -p zg-chan 2>&1) \
     && ! printf '%s' "$mout" | grep -q 'FAILED' \
     && printf '%s' "$mout" | grep -q 'test result: ok'; then
    ok "成员 crate 测试（$(printf '%s' "$mout" | grep -oE '[0-9]+ passed' | awk '{s+=$1} END {print s+0}') passed）"
  else
    bad "成员 crate 测试失败"
    printf '%s\n' "$mout" | tail -15 | sed 's/^/       /'
  fi
else
  skip "cargo build（加 --build 才跑）"
fi

echo
echo "== 5. 接口 =="
if ! curl -s -m 3 -o /dev/null "$BASE/api/health"; then
  bad "服务未启动（$BASE）——先 ./run.sh"
  echo
  echo "失败 $fail 项"
  exit 1
fi

# 实时模式：/api/bars 走 TQ/腾讯前复权，与本地 gbbq 前复权在历史段有分位差，
# 三个「独立重算」对拍只在 TV_LIVE=0 纯本地模式下有意义（否则必然噪声失败）。
LIVE=$(curl -s -m 5 "$BASE/api/health" \
  | python3 -c "import sys,json;print('1' if json.load(sys.stdin).get('live') else '0')" 2>/dev/null || echo 0)

check_json() {
  local name="$1" url="$2" expr="$3"
  local body
  if ! body=$(curl -s -m 120 "$url"); then bad "$name（请求失败）"; return; fi
  if printf '%s' "$body" | python3 -c "import sys,json;d=json.load(sys.stdin);assert $expr" 2>/dev/null; then
    ok "$name"
  else
    bad "$name（响应不符）: $(printf '%s' "$body" | head -c 160)"
  fi
}

check_json "GET /api/health"   "$BASE/api/health"                     'd["ok"] is True'
check_json "GET /api/health 含实时三键" "$BASE/api/health" \
  'isinstance(d.get("live"),bool) and isinstance(d.get("session"),bool) and "data_as_of" in d'
check_json "GET /api/market-status" "$BASE/api/market-status" \
  'd["ok"] and isinstance(d["live"],bool) and isinstance(d["session"],bool) and isinstance(d["tq"],dict)'
check_json "GET /api/bars 含 meta 来源/日期/新鲜度" "$BASE/api/bars?code=600519&period=day&lmt=5" \
  'd["ok"] and isinstance(d.get("meta",{}).get("source"),str) and d["meta"].get("as_of") and isinstance(d["meta"].get("stale"),bool)'
# /api/live 缺参必须 400（实时模式）；离线模式先被开关拦成 503
c=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/live")
if [ "$c" = 400 ] || [ "$c" = 503 ]; then
  ok "GET /api/live 缺 codes → $c（实时 400 / 离线 503）"
else
  bad "GET /api/live 缺 codes 返回 $c（应为 400 或 503）"
fi
check_json "GET /api/symbols"  "$BASE/api/symbols"                    'len(d["rows"])>5000'
check_json "GET /api/bars 日线" "$BASE/api/bars?code=600519&period=day&lmt=5"      'd["ok"] and len(d["bars"])==5'
check_json "GET /api/bars 5分"  "$BASE/api/bars?code=600519&period=5&lmt=5"        'd["ok"] and len(d["bars"])==5'
check_json "GET /api/chan 日线" "$BASE/api/chan?code=600519&period=day"            'd["ok"] and isinstance(d["bis"],list)'
check_json "GET /api/scan"     "$BASE/api/scan?code=&freq=D&limit=5"  'd["ok"] and isinstance(d["rows"],list)'
check_json "GET /api/heat"     "$BASE/api/heat"                       'len(d.get("industries",[]))>=100 and len(d.get("stocks",{}))>=100'
# 热度新鲜度口径：上游 zg-market 的 payload.stale 是「内存快照超 180 秒未刷新」
# （crates/zg-market/src/industry.rs:395），而 tv 的 provider 离线 ⇒ 那个标志必然为 true。
# 继承它 = 面板常显「旧快照」（数据是当天的、标记是假的）。tv 侧必须按数据口径自己算。
if grep -q 'old_stale' src/heat.rs; then
  bad "heat 仍继承上游 180s TTL 的 stale（面板会常显旧快照）"
else
  ok "heat 新鲜度按数据口径自算（不继承上游 TTL 标志）"
fi
# API 口径：本地日线已到最近交易日时，/api/heat 的 stale 必须是 false
heat_st=$(curl -s -m 120 "$BASE/api/heat" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print(d.get('as_of', ''), 'true' if d.get('stale') else 'false')
" 2>/dev/null)
ref_day=$(curl -s -m 30 "$BASE/api/market-status" | python3 -c "
import sys, json
print(json.load(sys.stdin).get('reference', ''))
" 2>/dev/null)
h_asof=${heat_st%% *}; h_stale=${heat_st##* }
if [ -n "$h_asof" ] && [ -n "$ref_day" ] && [ "$h_asof" \> "$ref_day" -o "$h_asof" = "$ref_day" ]; then
  if [ "$h_stale" = "false" ]; then
    ok "热度 stale 与数据口径一致（as_of $h_asof ≥ 参考日 $ref_day → 不标陈旧）"
  else
    bad "热度 stale 与数据口径矛盾（as_of $h_asof ≥ 参考日 $ref_day，却标了陈旧）"
  fi
else
  skip "热度 stale 口径无法判定（as_of=$h_asof 参考日=$ref_day）"
fi
check_json "GET /api/board-kline 板块日线" "$BASE/api/board-kline?code=X2102&period=day&lmt=5" 'd["ok"] and len(d["bars"])==5 and d["board"]'
check_json "GET /api/board-kline 指数日线" "$BASE/api/board-kline?code=000300&period=day&lmt=5" 'd["ok"] and len(d["bars"])==5 and d["board"]'
check_json "GET /api/market-indices" "$BASE/api/market-indices" 'len(d["items"])==11'
check_json "GET /api/index-constituents" "$BASE/api/index-constituents?code=000300" 'len(d["rows"])>=300'
check_json "GET /api/drawings" "$BASE/api/drawings?code=600519&period=day" 'd["ok"] and isinstance(d["rows"],list)'

# —— 板块总览 / 板块缠论 / 蜡烛形态（本轮新增）——
# 口径：industry_catalog.json 的 128 个通达信研究行业二级 + 5587 只成员（含北交所），全量入表
check_json "GET /api/sector-overview 128 行业全量" "$BASE/api/sector-overview" \
  'd["ok"] and len(d["industries"])==128 and not any("no_data" in r for r in d["industries"]) and {r["code"]:r["name"] for r in d["industries"]}.get("881130")=="酿酒" and {r["code"]:r["name"] for r in d["industries"]}.get("881268")=="电网设备" and {r["code"]:r["name"] for r in d["industries"]}.get("881002")=="煤炭开采" and len([r for r in d["industries"] if r.get("mom_rank6") is not None and r.get("crowd_pct") is not None])>=120 and sum(r["breadth"]["n_total"] for r in d["industries"])==len(json.load(open("modules/market/industry_catalog.json"))["members"])'
check_json "GET /api/sector-overview v5 新字段（合成分/双窗RS/波动拥挤/轨迹）" "$BASE/api/sector-overview" \
  'all(r.get("mom_rank_c") is not None and r.get("rankc_d20") is not None and r.get("rs63") is not None and r.get("rs252") is not None and r.get("crowd_vol_pct") is not None and isinstance(r.get("traj_quad"), list) and len(r["traj_quad"])==20 and r.get("mom",{}).get("9M") is not None for r in d["industries"] if r.get("mom_rank6") is not None)'
check_json "GET /api/sector-overview v9 新字段（合成拥挤/IR/RS合成/RS90/label_comp）" "$BASE/api/sector-overview" \
  'sum(1 for r in d["industries"] if r.get("crowd_comp") is not None)>=120 and sum(1 for r in d["industries"] if r.get("mom_rank_ir") is not None)>=120 and sum(1 for r in d["industries"] if r.get("rs_comp") is not None)>=120 and all(isinstance(r.get("rs90"),bool) and isinstance(r.get("label_comp"),str) and all(k in r for k in ("crowd_comp","mom_rank_ir","rs_comp","label_comp")) and all(("cc" in e and "ir" in e) for e in (r.get("traj_quad") or []) if e) for r in d["industries"])'
check_json "GET /api/sector-overview v10 新字段（IR/RS252 的 20 日位移）" "$BASE/api/sector-overview" \
  'sum(1 for r in d["industries"] if r.get("mom_rank_ir_d20") is not None)>=120 and sum(1 for r in d["industries"] if r.get("rs252_d20") is not None)>=120 and all(("mom_rank_ir_d20" in r and "rs252_d20" in r) for r in d["industries"])'
# v11：陈旧原因/覆盖率必须真的发到线上（源码改了但进程没重启 = 界面拿不到，静态门禁查不出这种）
check_json "GET /api/sector-overview v11 新字段（陈旧原因/盘中/基准落后/当日覆盖率）" "$BASE/api/sector-overview" \
  'all(k in d for k in ("session","stale","stale_reason","bench_lag","coverage")) and isinstance(d["session"],bool) and isinstance(d["stale"],bool) and isinstance(d["stale_reason"],str) and isinstance(d["coverage"].get("as_of"),int) and d["coverage"].get("full",0)>=100 and not any(("crowd_mix" in r or "rank6_traj" in r or "rankc_traj" in r) for r in d["industries"])'
# v12：双窗成分股 RPS 广度。分母关系是口径门禁：北交所不进 RPS 截面 ⇒ n_rps20 ≤ n_quoted；
# 季窗多一道 64 根历史门禁 ⇒ n_rps63 ≤ n_rps20；占比要么 null（分母 0）要么 [0,100]
check_json "GET /api/sector-overview v12 新字段（成分股 RPS 广度 20/63 双窗）" "$BASE/api/sector-overview" \
  'all(all(k in r["breadth"] for k in ("rps20_pct","rps63_pct","n_rps20","n_rps63")) and r["breadth"]["n_rps20"]<=r["breadth"]["n_quoted"] and r["breadth"]["n_rps63"]<=r["breadth"]["n_rps20"] and all(r["breadth"][k] is None or 0<=r["breadth"][k]<=100 for k in ("rps20_pct","rps63_pct")) for r in d["industries"]) and sum(1 for r in d["industries"] if r["breadth"].get("rps63_pct") is not None)>=120'
# 板块总览 v5 接线门禁
for pat in 'mom_rank_c' 'rankc_d20' 'rs63' 'rs252' 'crowd_vol_pct' 'traj_quad'; do
  grep -qF "$pat" "${OVV_SRC[@]}" && ok "overview.rs 接线 $pat" || bad "overview.rs 缺 $pat"
done
# v14：缓存指纹双信号叠加——基准文件指纹（抓 TDX 原地追加）+ lday 目录 mtime（抓 rsync/拷贝改名落地）
grep -qF '"v14|' "${OVV_SRC[@]}" && ok "缓存 stamp v14（双信号叠加）" || bad "缓存 stamp 未升 v14"
grep -qF 'market_stamp' "${OVV_SRC[@]}" && ok "overview.rs 接线 market_stamp" || bad "overview.rs 缺 market_stamp"
grep -qF 'lday(' "${OVV_SRC[@]}" && ok "overview.rs 接线 lday 目录指纹" || bad "overview.rs 缺 lday 目录指纹"
# —— v12 RPS 广度接线门禁 ——
for pat in 'rps20_pct' 'rps63_pct' 'RPS_HI'; do
  grep -qF "$pat" "${OVV_SRC[@]}" && ok "overview.rs 接线 $pat" || bad "overview.rs 缺 $pat"
done
for n in rps_percentiles assign_rps_pcts; do
  has_fn "$n" "${OVV_SRC[@]}" && ok "overview.rs 接线 fn $n" || bad "overview.rs 缺 fn $n"
done
has_fn rps_breadth_excludes_bj_and_short_history "${OVV_SRC[@]}" \
  && ok "RPS 广度单测覆盖北交所剔除 + 次新股只进月窗" || bad "缺 RPS 广度单测"
grep -qF "case 'rps': return r.breadth && r.breadth.rps63_pct;" web/js/panels/overview.js \
  && grep -qF "th('rps', 'RPS63'" web/js/panels/overview.js \
  && grep -qF 'bcell(b.rps63_pct)' web/js/panels/overview.js \
  && ok "overview.js 接线 RPS63 列（排序 + 表头 + 单元格）" || bad "overview.js 的 RPS63 列接线不全"
grep -qF '成分RPS≥90 季' web/js/panels/overview.js \
  && ok "tooltip 双窗展示（季/月并排）" || bad "tooltip 缺成分 RPS 双窗"
# —— v11 审查修复门禁 ——
# 死字段：界面与回放脚本都不消费，却各自被旧门禁钉着（改了反而过不了检查）。
# 只钉「作为 JSON 键/JS 标识符出现」，注释里的变更记录不算命中。
! grep -qE '"(rank6_traj|rankc_traj|crowd_mix)"' "${OVV_SRC[@]}" \
  && ! grep -qE '\b(rank6_traj|rankc_traj|crowd_mix)\b' web/js/panels.js \
  && ! grep -qE '\b(rank6_traj|rankc_traj|crowd_mix)\b' web/js/panels/overview.js \
  && ok "死字段 rank6_traj/rankc_traj/crowd_mix 已从前后端移除" \
  || bad "仍残留无消费者的 payload 字段（占行字节、且被旧门禁钉死）"
# 重建串行化：128 行业全量重算约 50 秒，并发 ?force=1 会各算一遍
grep -qF 'let _hold = build_lock().lock_ok();' "${OVV_SRC[@]}" \
  && grep -qF 'BUILD_GEN' "${OVV_SRC[@]}" \
  && ok "总览重建单飞（同代次只算一次，后来者直接吃结果）" \
  || bad "总览重建无单飞（并发请求会并行重算 50 秒）"
# 落盘原子性：truncate+write 让并发读方拿到半截 JSON → parse 失败 → 再重算一次
grep -qF 'with_extension("json.tmp")' "${OVV_SRC[@]}" \
  && grep -qF 'std::fs::rename(&tmp, file)' "${OVV_SRC[@]}" \
  && ok "总览缓存先 .tmp 再 rename（读方不会看到半截文件）" \
  || bad "总览缓存直写正式文件（并发读会拿到截断 JSON）"
# 缓存写失败不该把已算好的数据报成 500
grep -qF 'if let Err(e) = write_cache(&dir, &file, &stamp, &payload)' "${OVV_SRC[@]}" \
  && ok "缓存落盘失败只记日志，仍返回算好的数据" || bad "落盘失败会把可用结果报成错误"
# c1 分母覆盖率：当日有 bar 的行业数远低于满覆盖日时，份额被系统性抬高
grep -qF 'COV_TOL_NUM' "${OVV_SRC[@]}" \
  && has_fn c1_share_requires_coverage "${OVV_SRC[@]}" \
  && ok "成交额占比带覆盖率门禁 + 单测" || bad "c1 分母无覆盖率门禁（少数行业缺 bar 会抬高所有人份额）"
# 回放 CSV 必须能复算外圈阈值，否则对账脚本无法独立验证 5/15、3/8
grep -qF 'label,label_comp,rank6_d20,rankc_d20,mom_rank_ir_d20,rs252_d20' "${OVV_SRC[@]}" \
  && ok "回放 CSV 输出四模式 *_d20 位移列（外圈阈值可独立复算）" \
  || bad "回放 CSV 缺 *_d20 列（外圈阈值无法复核）"
# 陈旧/覆盖率后端一直有算，界面必须说：否则基准落后一天 = 整片列空白且看着像坏了
grep -qF 'd.stale_reason' web/js/panels/overview.js \
  && grep -qF 'ov-stale' web/js/panels/overview.js && grep -qF '.ov-stale' web/css/app.css \
  && ok "状态行显示陈旧/盘中提示（含 stale_reason）" \
  || bad "状态行没渲染 stale/stale_reason（指标列整片空白无解释）"
grep -qF 'cov.as_of < cov.full' web/js/panels/overview.js \
  && ok "状态行显示当日有 bar 的行业覆盖率" || bad "状态行没显示 coverage（c1 口径缺透明说明）"
# 数据根目录：ZG_ROOT 指错了要能退回可执行文件布局，而不是静默找不到数据
has_fn has_layout src/paths.rs \
  && ok "ZG_ROOT 先校验 modules/+data/ 布局再用" || bad "ZG_ROOT 未校验布局（指错目录=界面空白且无提示）"
# —— v10：散点外圈统一为「纵轴指标的 20 日位移」，双窗不再无圈可画 ——
grep -qF "rs: { key: 'rs252_d20'" web/js/panels/overview.js \
  && ok "双窗模式外圈接 RS252 超额的 20 日位移" || bad "双窗模式外圈没接 rs252_d20"
grep -qF "ir: { key: 'mom_rank_ir_d20'" web/js/panels/overview.js \
  && ok "IR 模式外圈接 IR 排名的 20 日位移" || bad "IR 模式外圈没接 mom_rank_ir_d20"
grep -qF "comp: { key: 'rankc_d20'" web/js/panels/overview.js \
  && ok "合成模式外圈接 RkC 排名的 20 日位移" || bad "合成模式外圈没接 rankc_d20"
grep -qF "rot: { key: 'rank6_d20'" web/js/panels/overview.js \
  && ok "轮动模式外圈接 Rk6 排名的 20 日位移" || bad "轮动模式外圈没接 rank6_d20"
# 外圈规格必须四模式齐全、且不在规格里按模式排除任何一个。
# （原门禁是整仓 `! grep -qF "ovMode !== 'rs'"`：JS 里别处合法地使用这个表达式也会被判失败）
_ocs=$(sed -n '/const OV_ACCEL = {/,/^  };/p' web/js/panels/overview.js)
_NMODES=$(printf '%s\n' "$_ocs" | grep -cE "^ +[a-z]+: \\{ key: ")
[ "$_NMODES" -eq 4 ] \
  && ok "OV_ACCEL 四模式各有位移字段" || bad "OV_ACCEL 少模式（该模式散点没有外圈）"
! printf '%s\n' "$_ocs" | grep -q 'ovMode' \
  && ok "外圈规格不按模式排除（四模式各跟各的纵轴画）" || bad "外圈规格里仍按模式排除（双窗可能没圈）"
has_fn d20_is_t_minus_t20_and_missing_stays_none "${OVV_SRC[@]}" \
  && has_fn rs252_d20_sign_tracks_relative_outperformance "${OVV_SRC[@]}" \
  && ok "overview 单测覆盖 d20 口径与 RS252 位移方向" || bad "overview 缺 d20/RS252 单测"
grep -qF '"rs252_d20"' "${OVV_SRC[@]}" && grep -qF '"mom_rank_ir_d20"' "${OVV_SRC[@]}" \
  && ok "payload 输出 rs252_d20 / mom_rank_ir_d20" || bad "payload 缺新加速度字段"
# —— v8 审查修复：散点/轨迹横轴对旧缓存回退 + 加速度 ring 不拦点击 ——
grep -qF 'function ovCrowdX(r) { return r.crowd_comp != null ? r.crowd_comp : r.crowd_pct; }' web/js/panels/overview.js \
  && ok "散点横轴走 ovCrowdX（无 crowd_comp 的旧缓存回退单分量 crowd_pct）" \
  || bad "散点横轴没回退（新 JS 配未重启的旧进程 = rot/comp 整块空白）"
grep -qF 'const cx = e.cc != null ? e.cc : e.c;' web/js/panels/overview.js \
  && ok "轨迹横轴同样回退（cc→c）" || bad "轨迹横轴没回退（旧缓存下轨迹消失）"
# rs 分支必须在横轴回退取值之前：双窗模式两轴都不吃拥挤字段
_ocp=$(sed -n '/function ovCurPt/,/^  }/p' web/js/panels/overview.js)
_rs=$(printf '%s\n' "$_ocp" | grep -n "=== 'rs'" | head -1 | cut -d: -f1)
_cx=$(printf '%s\n' "$_ocp" | grep -n 'cx == null' | head -1 | cut -d: -f1)
[ -n "$_rs" ] && [ -n "$_cx" ] && [ "$_rs" -lt "$_cx" ] \
  && ok "rs 分支排在拥挤回退之前" || bad "rs 分支排在拥挤回退之后（无拥挤的双窗行会被误杀）"
grep -qF 'fill="none" pointer-events="none"' web/js/panels/overview.js \
  && ok "动量加速度 ring 不参与命中测试" \
  || bad "ring 描边带截胡主点事件（点边缘 ~2px 环带点不动）"
# 散点外圈配色口径：A股涨红跌绿 —— 走强=红圈、走弱=绿圈（引用 --up/--down，与表格 Δ20 列同色）
# 门禁只钉「映射关系」不钉变量名：接收方改名（d20→acc.v）不该让门禁误报
grep -qF "var(--up)' : 'var(--down)'" web/js/panels/overview.js \
  && ok "散点加速度环 红=走强/绿=走弱（A股口径，引用 --up/--down）" \
  || bad "散点加速度环配色不是 A股口径（应为 红=走强/绿=走弱）"
grep -qF '红走强/绿走弱' web/js/panels/overview.js \
  && ok "图例口径文案与外圈配色一致（红走强/绿走弱）" \
  || bad "图例口径文案仍是旧配色说法（绿强红弱）"
# 筛选态外圈可见度：钉「性质」而不是钉字面量。
# 结构必须是 (强?基准:弱基准) × (筛选?dim系数:1) 的单一乘数——旧写法把筛选态的
# stroke-opacity 顶成 1、只留 opacity 0.62，于是非目标弱环 0.62 > 目标弱环 0.5，
# 一筛选反而让不该显眼的环更显眼；同时 1px 细线低于 30% 在深底上等于隐形。
_opv=$(python3 - <<'PY' 2>&1
import re, sys
src = open('web/js/panels/overview.js').read()
m = re.search(r'function ovRingOpacity\(dim, strong\) \{\s*return \(strong \? ([\d.]+) : ([\d.]+)\)'
              r' \* \(dim \? ([\d.]+) : 1\);', src)
if not m:
    sys.exit('ovRingOpacity 不是 (强弱基准)×(dim 系数) 的单一乘数形态')
S, W, D = (float(x) for x in m.groups())
v = lambda dim, strong: (S if strong else W) * (D if dim else 1)
p = [v(0, 1), v(1, 1), v(0, 0), v(1, 0)]
if not all(a > b for a, b in zip(p, p[1:])):
    sys.exit('可见度不单调：' + ' < '.join('%.3f' % x for x in reversed(p)))
if p[3] < 0.30:
    sys.exit('筛选态弱环 %.2f 在深底上接近隐形' % p[3])
if p[2] > 0.60:
    sys.exit('未筛选弱环 %.2f 太抢，强弱分不出来' % p[2])
print('强未筛 %.2f > 强已筛 %.2f > 弱未筛 %.2f > 弱已筛 %.2f' % tuple(p))
PY
) && ok "外圈可见度单调（$_opv）" || bad "外圈可见度结构不对：$_opv"
grep -qF 'stroke-opacity="${ovRingOpacity(dim, acc.strong)}"' web/js/panels/overview.js \
  && ! grep -qF 'opRing' web/js/panels/overview.js \
  && ok "外圈统一走 ovRingOpacity（无独立 opRing 二次相乘）" \
  || bad "外圈仍用 opRing 分支（可见度可能与强弱脱钩）"
# 悬浮提示的单位不能走 signed()（自带 %）：排名位移不是百分比变化，会显示「-1.57%百分位点」
! grep -qF 'signed(acc.v)}${acc.unit}' web/js/panels/overview.js \
  && grep -qF '${av.toFixed(2)} ${acc.unit}' web/js/panels/overview.js \
  && ok "散点提示的 20 日位移单位不重复（百分位点/百分点）" \
  || bad "散点提示仍拼出「x%百分位点」（signed 自带 %）"
grep -qF "th('d20', 'Δ20', 'r', 'Rk6" web/js/panels/overview.js \
  && ok "表格 Δ20 标明固定 Rk6 口径（与外圈模式无关）" \
  || bad "表格 Δ20 未标明口径（IR/双窗外圈与表列会被读成同一个数）"
grep -qF '点击只亮金圈点、其余压暗保留位置参照' web/js/panels/overview.js \
  && ok "RS90 图例文案与实际行为一致（压暗而非移除）" \
  || bad "RS90 图例仍写「只留金圈点」，与压暗行为不符"
# 前端版本戳：钉在代码常量里、并与 index.html 的 ?v= 一致。
# 旧写法是从脚本 URL 反解 ?v= 再显示——URL 表示「请求了什么」不是「跑的是什么」：
# 浏览器命中旧缓存时 URL 仍是新的，徽标会显示一个根本没执行的版本，等于没有确认作用。
grep -qF "const TV_JS_BUILD = '" web/js/panels.js \
  && ! grep -qE 'const TV_JS_V[ (=]' web/js/panels.js \
  && ! grep -qE 'const TV_JS_V[ (=]' web/js/panels/overview.js \
  && ok "总览状态行的版本戳是代码常量（不从 URL 反解）" \
  || bad "版本戳仍从 URL 反解（旧缓存下会显示没跑过的版本）"
grep -qF 'js ${esc(TV_JS_BUILD)}' web/js/panels/overview.js \
  && ok "总览状态行显示的版本戳来自 panels.js 的代码常量" \
  || bad "总览状态行没显示代码常量版本戳（搬走后漏接线）"
_jsv=$(sed -n "s/^  const TV_JS_BUILD = '\([^']*\)';.*/\1/p" web/js/panels.js)
_htmlv=$(sed -n 's|.*panels\.js?v=\([^"&]*\)".*|\1|p' web/index.html | head -1)
[ -n "$_jsv" ] && [ "$_jsv" = "$_htmlv" ] \
  && ok "panels.js 版本常量与 index.html ?v= 一致（$_jsv）" \
  || bad "版本常量「$_jsv」≠ index.html ?v=「$_htmlv」（两者必须一起 bump）"
_htmlv2=$(sed -n 's|.*panels/overview\.js?v=\([^"&]*\)".*|\1|p' web/index.html | head -1)
[ -n "$_jsv" ] && [ "$_jsv" = "$_htmlv2" ] \
  && ok "overview.js 的 ?v= 与版本常量一致（$_jsv）" \
  || bad "overview.js ?v=「$_htmlv2」≠ 版本常量「$_jsv」（两个脚本必须一起 bump）"
grep -qF 'TV_JS_MISMATCH' web/js/panels.js \
  && ok "不一致时徽标显式报警（js↔v 不一致）" \
  || bad "URL 与代码戳不一致时界面毫无提示"
grep -qF 'const TV_JS_MISMATCH = P.build.mismatch;' web/js/panels/overview.js \
  && ! grep -qF 'currentScript' web/js/panels/overview.js \
  && ok "overview.js 的版本戳只从 panels.js 取别名（不自己反解 URL）" \
  || bad "overview.js 自己反解 URL 版本戳（会误报 js↔v 不一致）"
grep -qE '<script src="/js/panels\.js\?v=[A-Za-z0-9]+"></script>' web/index.html \
  && ok "index.html 给 panels.js 挂了 ?v= cache-busting" \
  || bad "index.html 没有 panels.js 的 ?v= 参数（改了 JS 可能仍跑旧缓存）"
grep -qE '<script src="/js/panels/overview\.js\?v=[A-Za-z0-9]+"></script>' web/index.html \
  && ok "index.html 给 panels/overview.js 挂了 ?v= cache-busting" \
  || bad "index.html 没有 panels/overview.js 的 ?v= 参数（改了 JS 可能仍跑旧缓存）"
# —— v9 修复回归门禁：当日数据门禁 / 筛选可取消 / 双窗象限 / 大窗金圈 / 旧缓存文案 / 显示精度 ——
grep -qF 'if s.c[k].is_none() {' "${OVV_SRC[@]}" \
  && ok "拥挤分量在当日无 bar 时整体置空（不会拿旧数据当今日）" \
  || bad "crowd 分量缺当日 bar 门禁（陈旧行业会输出 stale crowd_comp）"
grep -qF 'fn pct0(v: f64)' "${OVV_SRC[@]}" && grep -qF 'rs90_of' "${OVV_SRC[@]}" \
  && ok "label/rs90 按界面显示整数分位判定" \
  || bad "label/rs90 仍按原始小数判（显示 90 可能不亮星）"
grep -qF 'missing_current_bar_blocks_crowd_components' "${OVV_SRC[@]}" \
  && grep -qF 'labels_and_rs90_use_display_precision' "${OVV_SRC[@]}" \
  && ok "overview 单测覆盖缺当日 bar + 显示精度" || bad "overview 缺对应单测"
grep -qF "n > 0 || ovCat === k ? '' : ' disabled'" web/js/panels/overview.js \
  && ok "已激活的分类即使计数归零也可点击取消" || bad "分类筛选计数归零后卡死"
grep -qF "nRs > 0 || ovRs ? '' : ' disabled'" web/js/panels/overview.js \
  && ok "已激活的 RS90 即使计数归零也可点击取消" || bad "RS90 筛选计数归零后卡死"
grep -qF '右下=反转启动（短强长弱）；左上=回调蓄势（长强短弱）' web/js/panels/overview.js \
  && ok "双窗象限文案与轴方向一致" || bad "双窗右下/左上象限文案互换"
grep -qF '.ov-quad-big .ov-dot.rs90 { stroke-width: 1.5; }' web/css/app.css \
  && ok "大窗 RS90 金圈描边未被 .ov-quad-big .ov-dot 覆盖" \
  || bad "大窗 RS90 金圈被压成普通点粗细"
[ "$(grep -c 'ovCrowdLabel(' web/js/panels/overview.js)" -ge 3 ] \
  && ok "悬浮/行 title 的拥挤标签走 ovCrowdLabel 回退" \
  || bad "悬浮/行 title 仍直接读 crowd_comp（旧缓存显示 —）"
for pat in 'mom_rank_c' 'data-ovmode' 'ov-traj' 'polyline.ov-traj'; do
  grep -qF "$pat" web/js/panels/overview.js && ok "overview.js 接线 $pat" || bad "overview.js 缺 $pat"
done
# 散点放大窗 + 模式按钮悬浮简介（本轮新增）
for pat in 'ov-big' 'OV_MODE_TIP' 'ovModeBtns' 'renderOvBig' 'ovBigKey' 'IBD 权重 0.4×3M'; do
  grep -qF "$pat" web/js/panels/overview.js && ok "overview.js 接线 $pat" || bad "overview.js 缺 $pat"
done
grep -qF '.ov-big-mask' web/css/app.css && ok "app.css 散点放大窗样式" || bad "app.css 缺放大窗样式"
# —— 个股标签（L2 行业 + 概念题材）——
# 口径：L2=industry_catalog.json（X+4位 ref 前缀规则）；题材=MAC 在线源（TV-34），
# 地区/风格分字段（regions/styles），概念里不得混入地区块/风格块/通达信88。
# 断言用「包含 + 不包含」而不是全等：题材是**在线数据**，成分会随行情调整（信创 已不属于 000063）。
check_json "GET /api/stock-tags 茅台（L2 酿酒 + 概念 + 地区）" "$BASE/api/stock-tags?code=600519" \
  'd["ok"] and d["industry"]=={"code":"881130","name":"酿酒"} and "白酒概念" in d["concepts"] and d["regions"]==["贵州板块"] and not ({"贵州板块","基金重仓","通达信88"} & set(d["concepts"])) and d["concept_source"].startswith("mac@")'
check_json "GET /api/stock-tags 多概念股（000063 通信设备）" "$BASE/api/stock-tags?code=000063" \
  'd["ok"] and d["industry"]=={"code":"881338","name":"通信设备"} and {"5G概念","东数西算","AI智能体"} <= set(d["concepts"]) and "通达信88" not in d["concepts"] and len(d["concepts"])>=20'
check_json "GET /api/stock-tags 无归属代码（指数）两字段皆空" "$BASE/api/stock-tags?code=000300" \
  'd["ok"] and d["industry"] is None and d["concepts"]==[]'
for pat in 'block_gn.dat' 'BLOCK_HEADER: usize = 384' 'EXCLUDED_BLOCKS' 'encoding_rs::GBK'; do
  grep -qF "$pat" src/tags.rs && ok "tags.rs 接线 $pat" || bad "tags.rs 缺 $pat"
done
for pat in 'tags::payload' '"/api/stock-tags"'; do
  grep -qF "$pat" src/router.rs && ok "router.rs 接线 $pat" || bad "router.rs 缺 $pat"
done
grep -qF 'stockTags' web/js/main.js && grep -qF 'stockTags' web/js/api.js && ok "前端接线 stockTags" || bad "前端缺 stockTags"
grep -qF 'legend-tags' web/js/main.js && grep -qF 'id="legend-tags"' web/index.html && ok "前端接线 legend-tags" || bad "前端缺 legend-tags"
grep -qF 'legend-tags' web/css/app.css && ok "app.css 接线 legend-tags" || bad "app.css 缺 legend-tags"
grep -qF 'block_gn.dat' tools/sync-data.sh && ok "sync-data.sh 同步 block_gn.dat" || bad "sync-data.sh 缺 block_gn.dat 同步"
check_json "GET /api/chan board=1 板块笔" "$BASE/api/chan?code=881130&period=day&board=1" \
  'd["ok"] and isinstance(d["bis"],list) and len(d["bis"])>0'
check_json "board-kline 含 candle 三键" "$BASE/api/board-kline?code=881130&period=day&lmt=200" \
  'd.get("candle_engine")=="talib" and d.get("candle_pattern_set")=="all" and isinstance(d.get("candle_signals"),list)'
check_json "bars 含 candle_signals（600519 有信号）" "$BASE/api/bars?code=600519&period=day&lmt=200" \
  'd.get("candle_engine")=="talib" and isinstance(d.get("candle_signals"),list) and len(d["candle_signals"])>=1'
check_json "bars pset=rec27 收窄（与全量不同集）" "$BASE/api/bars?code=600519&period=day&lmt=200&pset=rec27" \
  'd.get("candle_pattern_set")=="rec27" and len(d["candle_signals"]) < 200'

echo
echo "== 5b. PA2（SMC 结构，纯前端算法）=="
for f in js/pa2.js js/pa2-primitive.js; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/$f")
  [ "$code" = 200 ] && ok "GET /$f" || bad "GET /$f → $code"
done
for pat in 'id="pa2-toggle"' '/js/pa2-primitive.js' '/js/pa2.js'; do
  grep -qF "$pat" web/index.html && ok "index.html 接线 $pat" || bad "index.html 缺 $pat"
done
if node -e '
  global.window = {};
  require("./web/js/pa2.js");
  const want = { "1":50, "5":50, "15":20, "30":20, "60":10, day:10, week:10, month:5, quarter:5, year:5 };
  const got = window.TV.pa2.SWING_DEFAULTS || {};
  process.exit(Object.keys(want).every(k => got[k] === want[k]) ? 0 : 1);
' 2>/dev/null; then
  ok "PA2 摆动长度按周期档默认（1/5m=50 … 年=5）"
else
  bad "PA2 摆动长度周期档默认表缺失/不符"
fi
if node -e '
  const path = "./web/js/pa2.js";
  function load(init) {
    const m = new Map(Object.entries(init || {}));
    global.window = {};
    global.localStorage = {
      getItem: k => (m.has(k) ? m.get(k) : null),
      setItem: (k, v) => m.set(k, String(v)),
    };
    delete require.cache[require.resolve(path)];
    require(path);
    return global.window.TV.pa2Layer;
  }
  function bars(n) {
    return Array.from({ length: n }, (_, i) => {
      const p = 10 + Math.sin(i / 2);
      return { time: String(i), open: p, high: p + 0.5, low: p - 0.5, close: p + 0.1, vol: 100 };
    });
  }
  let L = load();
  L.setData(bars(26), "year");
  if (L.debug().swingLength !== 5) process.exit(1);       // 26 根年线必须降到 5
  L.setSwing(10);
  if (L.debug().swingLength !== 5) process.exit(2);       // 3L=30 > 26，拒绝
  L.setData([], "day");
  L.setSwing(5);
  if (L.debug().swingLength !== 5 || L.debug().swingSource !== "custom") process.exit(3); // 空 bars 不静默拒绝
  L = load({ "tv.pa2.swing": "20" });
  L.setData(bars(26), "year");
  if (L.debug().swingLength !== 5) process.exit(4);       // 旧标量不再绑年线
  L.setData(bars(200), "day");
  if (L.debug().swingLength !== 20 || L.debug().swingSource !== "custom") process.exit(5); // 迁到日线
  process.exit(0);
' 2>/dev/null; then
  ok "PA2 摆动长度边界（3L 降档 / 空 bars / 旧标量迁日线）"
else
  bad "PA2 摆动长度边界检查失败"
fi
grep -q "swingOverrides" web/js/pa2.js && grep -q "swingSource" web/js/pa2.js \
  && grep -q "JSON.stringify(swingOverrides)" web/js/pa2.js \
  && ok "PA2 摆动长度每档独立记忆（JSON 持久化）" || bad "PA2 摆动长度未按档记忆"
oracle=$(ls target/pa2-oracle-*.json 2>/dev/null || true)
if [ -n "$oracle" ]; then
  npass=0; nbad=0
  for j in $oracle; do
    if out=$(node target/pa2-parity.mjs "$j" 2>&1) && ! printf '%s' "$out" | grep -q '^!! '; then
      npass=$((npass + 1))
    else
      nbad=$((nbad + 1)); bad "PA2 数值对拍不通过（$j）"
      printf '%s\n' "$out" | grep '^!!' | head -5 | sed 's/^/       /'
    fi
  done
  # 600070 / 600485 各含 63 / 83 根一字板，专测退化 K 线路径
  [ "$nbad" -eq 0 ] && ok "pa2.js ↔ fork smc.py 逐点全等（${npass} 只标的 × 23 列）"
  # 计数为 0 也算 FAIL：脚本异常退出但既无 !! 也无 OK 行时不能当「0 只标的」通过
  if out=$(node target/pa2-gapfilter-check.mjs 2>&1) && ! printf '%s' "$out" | grep -q '^!! ' \
     && [ "$(printf '%s' "$out" | grep -c '^OK ')" -gt 0 ]; then
    ok "FVG 窄 gap 过滤 ↔ 独立滑窗实现逐下标全等（$(printf '%s' "$out" | grep -c '^OK ') 只标的）"
  else
    bad "FVG 窄 gap 过滤校验不通过"
    printf '%s\n' "$out" | head -6 | sed 's/^/       /'
  fi
else
  skip "PA2 数值对拍 + 窄 gap 过滤校验（缺 target/pa2-oracle-*.json，用 target/pa2-oracle.py 生成）"
fi

echo
echo "== 5c. 周期深度（季/年重采样、分钟 120 交易日、季线缠论）=="
check_json "bars 季线全量（600519 = 102 季）" "$BASE/api/bars?code=600519&period=quarter&lmt=0" \
  'len(d["bars"])==102 and d["bars"][-1]["time"]>=d["bars"][0]["time"]'
check_json "bars 年线全量（600519 = 26 年）" "$BASE/api/bars?code=600519&period=year&lmt=0" \
  'len(d["bars"])==26'
check_json "chan 季线 freq=S 且出笔" "$BASE/api/chan?code=600519&period=quarter" \
  'd["ok"] and d["freq"]=="S" and len(d["bis"])>0'
check_json "board-kline 季线（881130 = 64 季）" "$BASE/api/board-kline?code=881130&period=quarter&lmt=0" \
  'd["ok"] and len(d["bars"])==64'
check_json "bars 1 分钟 ≥120 交易日 × 240（盘中末根为当日）" "$BASE/api/bars?code=600519&period=1&lmt=28800" \
  'len(d["bars"])==28800 and len({b["time"]//86400 for b in d["bars"]})>=119'

# 分钟线口径门（TV-32 回归）：远端分钟并入本地后必须
#   ① 每天不超过 240 根 —— 腾讯 m1 每天多一根 09:30 集合竞价伪 bar，须被归一掉；
#   ② 已收盘交易日的分钟末根与同日日线收盘偏差 ≤1% —— 分钟/日线必须同一复权基
#      （除权股在远端窗口内曾整体偏高 2.5%，接缝处出现假跳空）。
if out=$(python3 - "$BASE" <<'PY' 2>&1
import json, sys, urllib.request, datetime
base = sys.argv[1]
def api(p):
    with urllib.request.urlopen(base + p, timeout=180) as r:
        return json.loads(r.read().decode("utf-8"))
def dt(t):
    # 接口给的是「北京墙钟当 UTC」的纪元秒，+8h 还原成北京墙钟
    return datetime.datetime.fromtimestamp(t + 8 * 3600, datetime.UTC)
def day_of(t):
    return dt(t).strftime("%Y-%m-%d") if isinstance(t, int) else str(t)[:10]
problems = []
for code in ("600519", "000338"):
    try:
        m = api("/api/bars?code=%s&period=1&lmt=28800" % code)
        d = api("/api/bars?code=%s&period=day&lmt=60" % code)
    except Exception as e:
        print("ERR %s %s" % (code, e)); sys.exit(1)
    per_day = {}
    for b in m.get("bars") or []:
        per_day.setdefault(day_of(b["time"]), []).append(b)
    if not per_day:
        problems.append("%s 分钟线为空" % code); continue
    daily = {day_of(b["time"]): b for b in (d.get("bars") or [])}
    over = sorted(k for k, v in per_day.items() if len(v) > 240)
    if over:
        problems.append("%s 有 %d 天超过 240 根（集合竞价伪 bar 未归一：%s）"
                        % (code, len(over), ",".join(over[:3])))
    worst, worst_day = 0.0, ""
    for day, v in per_day.items():
        if dt(v[-1]["time"]).strftime("%H:%M") != "15:00":
            continue                      # 半截交易日（盘中）不比，末根不是收盘价
        dc = daily.get(day, {}).get("close")
        if not dc:
            continue
        r = v[-1]["close"] / dc - 1.0
        if abs(r) > abs(worst):
            worst, worst_day = r, day
    if abs(worst) > 0.01:
        problems.append("%s 分钟/日线最大偏差 %+.3f%%（%s）> 1%%" % (code, worst * 100, worst_day))
    print("%s: 天数=%d 单日最多=%d 根 分钟/日线最大偏差=%+.4f%% @%s"
          % (code, len(per_day), max(len(v) for v in per_day.values()), worst * 100, worst_day))
if problems:
    print("FAIL " + "；".join(problems)); sys.exit(1)
print("OK")
PY
); then
  ok "分钟线口径（≤240 根/日 + 已收盘日分钟/日线偏差 ≤1%）：$(printf '%s' "$out" | tr '\n' ' ')"
else
  bad "分钟线口径门不通过：$(printf '%s' "$out" | tr '\n' ' ')"
fi

# —— MAC 分钟源（通达信扩展行情协议，Rust 自实现）——
# 接线门禁：src/tdxmac.rs 存在 + live.rs 真的调它 + lib.rs 注册模块
[ -f src/tdxmac.rs ] && ok "src/tdxmac.rs 存在" || bad "缺 src/tdxmac.rs"
grep -qF 'pub mod tdxmac;' src/lib.rs && ok "lib.rs 注册 mod tdxmac" || bad "lib.rs 缺 mod tdxmac"
grep -qF 'tdxmac::MacConn' "${LIVE_SRC[@]}" && ok "live.rs 调用 tdxmac::MacConn" || bad "live.rs 未接 MAC 客户端"
grep -qF 'mac_minute_rows' "${LIVE_SRC[@]}" && ok "live.rs 有 mac_minute_rows" || bad "live.rs 缺 mac_minute_rows"
# 源链顺序门禁：MAC 必须在腾讯之前（腾讯已降为备选）
# 顺序只能在同一个文件里判：两个调用分处不同文件时，拼接后的先后由拼接顺序决定，
# 与真实降级链无关 ⇒ 必须先断言同文件，否则这条门会静默变成恒真。
python3 - <<'PY2'
import glob, sys

sys.path.insert(0, "tools")
from gate_scan import strip_comments
FILES = ["src/live.rs"] + sorted(glob.glob("src/live/*.rs"))
SRC = {f: strip_comments(open(f, encoding="utf-8").read()) for f in FILES}


def first(pat):
    for f in FILES:
        i = SRC[f].find(pat)
        if i >= 0:
            return f, i
    return None, -1


f_mac, i_mac = first("mac_minute_rows(&mkt")
f_tx, i_tx = first("kline_tencent_min(code")
if i_mac < 0 or i_tx < 0:
    print("!! 找不到分钟链的 MAC / 腾讯分支"); sys.exit(1)
if f_mac != f_tx:
    print(f"!! MAC({f_mac}) 与腾讯({f_tx}) 不在同一文件 —— 降级链被拆散，顺序无从判定"); sys.exit(1)
if i_mac > i_tx:
    print("!! MAC 未排在腾讯之前（源链顺序）"); sys.exit(1)
print("OK")
PY2
if [ $? -eq 0 ]; then ok "分钟源链顺序：MAC 在腾讯之前（腾讯降为备选）"; else bad "分钟源链顺序不对"; fi

# 日线源链顺序门禁：MAC 日线必须在「主链降级源」之前被尝试。
# get_kline 主链在 TQ 不可用时会静默降级到 em/tx/ths（与本地 gbbq 基不同源），
# 所以 MAC 必须插在主链结果被采纳之前 —— 顺序错了降级日线就会带着错基进来。
# 同分钟链：两个调用必须落在同一文件，否则先后由拼接顺序决定，门会静默恒真。
python3 - <<'PY2'
import glob, sys

sys.path.insert(0, "tools")
from gate_scan import strip_comments
FILES = ["src/live.rs"] + sorted(glob.glob("src/live/*.rs"))
SRC = {f: strip_comments(open(f, encoding="utf-8").read()) for f in FILES}
ALL = "".join(SRC[f] for f in FILES)


def first(pat):
    for f in FILES:
        i = SRC[f].find(pat)
        if i >= 0:
            return f, i
    return None, -1


f_tq, i_tq = first("kline_tq_day(code")
f_mac, i_mac = first("mac_daily_rows(&mkt, &cd, want)")
if i_tq < 0 or i_mac < 0:
    print("!! 找不到日线链的 TQ / MAC 分支"); sys.exit(1)
if f_tq != f_mac:
    print(f"!! TQ({f_tq}) 与 MAC({f_mac}) 不在同一文件 —— 白名单链被拆散，顺序无从判定"); sys.exit(1)
if not (i_tq < i_mac):
    print("!! 白名单顺序不对：应先 TQ（主力）再 MAC（同基替代）"); sys.exit(1)
# 旧版规则「MAC 要排在主链降级源之前」已随白名单作废：异基公开源整个移出日线链
if "let mut main: Option<(Vec<Agg>, String)>" in ALL:
    print("!! 日线链仍保留主链降级源分支（白名单应只剩 TQ+MAC）"); sys.exit(1)
print("OK")
PY2
if [ $? -eq 0 ]; then ok "日线源链顺序：白名单 TQ → MAC（异基降级源已移出）"; else bad "日线源链顺序不对"; fi

# 运行期门禁：日线源必须是与本地同基的源（tq / local / mac@*）——
# 出现 em/tx/ths 说明降级到了与本地 gbbq 基不同源的序列。
check_json "日线源为同基源（tq/local/mac）" "$BASE/api/bars?code=000338&period=day&lmt=60" \
  'd["ok"] and (d["meta"]["source"]=="tq" or d["meta"]["source"]=="local" or d["meta"]["source"].startswith("mac@"))'

# 运行期门禁：实时模式下分钟源必须是 MAC（或本地新鲜度命中），且 MAC 必须带出主机 IP。
# 探 5 分钟不探 1 分钟：1 分钟档自 2026-11-02 起只读本地 .lc1（离线采集），探它这条门永远只看到 local。
if [ "$LIVE" = "1" ]; then
  if out=$(curl -s -m 180 "$BASE/api/bars?code=600519&period=5&lmt=240" | python3 -c "
import sys, json
d = json.load(sys.stdin)
m = d.get('meta', {})
src, det = m.get('source', ''), m.get('detail', '')
assert src in ('mac', 'tq', 'local', 'tencent', 'tdx'), '未知分钟源 ' + src
if src == 'mac':
    assert det and all(p.isdigit() for p in det.split('.')), 'MAC 源未带出主机: ' + repr(det)
print(src + ('@' + det if det else ''))
" 2>&1); then
    ok "实时分钟源 = $out"
  else
    bad "实时分钟源异常：$out"
  fi
  # 独立实现对表：Rust MAC 输出 ↔ Python easy_tdx MAC 输出（同主机逐根）
  if out=$(python3 tools/mac_parity.py "$BASE" 2>&1); then
    if printf '%s' "$out" | grep -q '^ALL PASS$'; then
      ok "MAC 分钟源逐根对表（Rust ↔ easy_tdx，同主机）：$(printf '%s' "$out" | grep -c '比例不恒定 0 天') 组全等（vol/amount 逐根全等 + close 按日比例恒定）"
    elif printf '%s' "$out" | grep -q '^SKIP'; then
      skip "MAC 分钟源对表（$(printf '%s' "$out" | head -1)）"
    else
      bad "MAC 分钟源对表不通过"
      printf '%s\n' "$out" | tail -8 | sed 's/^/       /'
    fi
  else
    bad "MAC 分钟源对表脚本报错"
    printf '%s\n' "$out" | tail -8 | sed 's/^/       /'
  fi
else
  skip "实时分钟源与 MAC 对表（离线模式不接远端；TV_LIVE=1 重启后再跑）"
fi

# 年线只画 K 线：缠论必须明确拒算（HTTP 500 + 中文原因），不能静默给空结构
body=$(curl -s -m 60 "$BASE/api/chan?code=600519&period=year")
if printf '%s' "$body" | grep -q '年线'; then
  ok "chan 年线明确拒算并写明原因"
else
  bad "chan 年线未拒算: $(printf '%s' "$body" | head -c 160)"
fi

# 独立重算对拍：季/年 OHLCV 逐根全等 + 季线缠论不变量（Python 自己聚合，不走 Rust 代码）
if [ "$LIVE" = "1" ]; then
  skip "季/年重采样 + 季线缠论对拍（实时模式跳过；TV_LIVE=0 重启后再跑）"
elif out=$(python3 target/qy-parity.py 2>&1) && printf '%s' "$out" | grep -q '^ALL PASS$'; then
  ok "季/年重采样 + 季线缠论对拍（$(printf '%s' "$out" | grep -c '^OK ') 项）"
else
  bad "target/qy-parity.py 不通过"
  printf '%s\n' "$out" | grep -E '^!!|^FAIL' | head -6 | sed 's/^/       /'
fi

for pat in 'data-period="quarter"' 'data-period="year"'; do
  grep -qF "$pat" web/index.html && ok "index.html 接线 $pat" || bad "index.html 缺 $pat"
done
grep -q 'quarter: 0, year: 0' web/js/time.js && ok "time.js 季/年 = 有多少放多少" || bad "time.js 缺季/年全量口径"
# 各档打开的默认视图：单一机制（定屏上根数 + 右锚），不许再出现按周期分叉的窗口口径
grep -q 'function applyDefaultView' web/js/chart.js && grep -qF 'TV.chart.applyDefaultView(j.bars.length)' web/js/main.js \
  && ok "chart.js 提供统一默认视图，main.js 切档接线" || bad "默认视图未统一（chart.js/main.js 缺 applyDefaultView）"
# 口径断言（不钉死数字）：间距必须由「屏上根数」反推、根数取 min(数据长度, 常量)——
# 固定像素密度会让宽屏铺 600+ 根蜡烛，密到看不出形态（2026-11-03 换成数根数）
_ov=$(grep -oE 'const DEFAULT_VIEW_BARS = [0-9]+;' web/js/chart.js | grep -oE '[0-9]+')
[ -n "$_ov" ] && grep -qF 'Math.min(barCount, DEFAULT_VIEW_BARS)' web/js/chart.js \
  && ok "默认视图按屏上根数定窗口（最近 ${_ov} 根）" || bad "默认视图缺根数口径（DEFAULT_VIEW_BARS / Math.min(barCount, …)）"
if grep -q 'DEFAULT_SPACING' web/js/chart.js; then bad "chart.js 残留固定像素密度口径（DEFAULT_SPACING）"; else ok "chart.js 无固定像素密度残留"; fi
if grep -q 'DAY_DEFAULT_BARS' web/js/main.js; then bad "main.js 仍有日线专属窗口"; else ok "main.js 无日线专属窗口"; fi
if grep -q '^[[:space:]]*ts\.fitContent();' web/js/chart.js; then bad "chart.js 默认视图又用回 fitContent 调用（换档瞬间按合并索引空间算，实测月档铺出 393 根窗口装 303 根数据）"; else ok "chart.js 默认视图不走 fitContent 调用"; fi
[ "$(grep -c 'ts.scrollToRealTime();' web/js/chart.js)" = 1 ] \
  && ok "右边缘对齐走 scrollToRealTime（与数据长度无关，避开同帧按旧数据锚位）" \
  || bad "chart.js 右锚实现漂移（应恰 1 处 ts.scrollToRealTime();）"
# 缩放归键盘 ↑/↓，滚轮改成横向平移。三处必须同时成立：只关 handleScale.mouseWheel 会让
# 滚轮彻底失灵（LWC 的 deltaY 只喂缩放，没有「落回平移」这回事，实测 from 纹丝不动），
# 必须两个 mouseWheel 都关（LWC 才会摘掉自己的 wheel 监听）+ 自己 bindWheel 接上。
grep -q 'handleScroll: { mouseWheel: false }' web/js/chart.js && grep -q 'handleScale: { mouseWheel: false }' web/js/chart.js \
  && ok "chart.js 滚轮两个开关都关（LWC 不再处理 wheel）" || bad "chart.js 滚轮开关不齐（只关缩放＝滚轮彻底失灵）"
grep -q 'function bindWheel' web/js/chart.js && grep -q 'bindWheel(el);' web/js/chart.js \
  && ok "chart.js 自己接滚轮（普通滚轮平移、Shift+滚轮缩放）" || bad "chart.js 缺 bindWheel（没人 preventDefault，滚轮会去滚页面）"
grep -qF "ev.key === 'ArrowUp' || ev.key === '+'" web/js/main.js \
  && grep -qF "ev.key === 'ArrowDown' || ev.key === '-'" web/js/main.js \
  && ok "main.js ↑/↓ 与 +/- 同分支接缩放（搜索候选导航也用 ArrowUp，别拿它当命中）" || bad "main.js 缺 ↑/↓ 缩放绑定"
# Shift+滚轮 = 缩放周期长短（2026-11-02 用户改的：以前 Shift 是强制平移的退路）。
# 缩放口径必须只有一份：键盘 ↑/↓ 与 Shift+滚轮都走 chart.js 的 zoomBy。
grep -qF 'if (ev.shiftKey) {' web/js/chart.js \
  && grep -qF 'zoomBy(Math.min(2, Math.max(0.5, f)));' web/js/chart.js \
  && ok "Shift+滚轮 = 缩放周期长短（单独分支：向上放大、向下缩小）" \
  || bad "Shift+滚轮没接缩放（还在平移）"
grep -qF 'function zoomBy(factor)' web/js/chart.js && grep -qF 'TV.chart.zoomBy' web/js/main.js \
  && ok "键盘 ↑/↓ 与 Shift+滚轮共用一份缩放口径（chart.js 的 zoomBy）" \
  || bad "缩放口径分成两份（键盘与滚轮会不一致）"
! grep -qF 'if (!ev.shiftKey && TV.panels' web/js/chart.js \
  && ok "普通滚轮路径不再夹带 Shift 判断（Shift 已在上面单独 return）" \
  || bad "Shift 分支与平移路径混在一起（Shift 会既缩放又平移）"
# 滚轮换股：图上滚轮在侧栏有个股表时换股，无表回落平移（2026-11-02 用户定）
grep -qF 'TV.panels.wheelStep(px)' web/js/chart.js \
  && ok "chart.js 滚轮先问侧栏能否换股" || bad "chart.js 未接 wheelStep（滚轮换股没接线）"
grep -q 'function wheelStep' web/js/panels.js && grep -qF 'wheelStep, refreshActive' web/js/panels.js \
  && ok "panels.js 定义并导出 wheelStep/refreshActive" || bad "panels.js 的 wheelStep 未定义或未导出（内部函数不导出＝chart.js 调不到）"
grep -qF 'rows.length >= 2' web/js/panels.js \
  && ok "接管滚轮要求表里 ≥2 只（只剩本尊时不能把平移也吃掉）" || bad "panels.js 缺 ≥2 只的接管门槛"
grep -qF "'#panel-heat tr[data-stock]'" web/js/panels.js \
  && ! grep -qF "'#panel-heat tr[data-code]'" web/js/panels.js \
  && ! grep -qF "'#panel-heat tr[data-code]'" web/js/panels/overview.js \
  && ok "换股表只收成分股行 data-stock（总览表 data-code 是 881 板块码，收进来滚轮会去开板块）" \
  || bad "panels.js 的换股表口径漂移（板块总览的 881 码不得进翻动序列）"
grep -qF 'TV.panels.refreshActive();' web/js/main.js && grep -qF 'tr.row-on' web/css/app.css \
  && ok "换股后侧栏高亮补回（load 里 refreshActive + app.css .row-on）" || bad "当前标的的高亮链路缺失（滚轮靠它认锚、用户靠它看翻到哪）"
# 滚轮翻板块（2026-11-02 用户定）：停在行业热度 / 宽基指数 / 板块总览这一层时，
# 滚轮上下 = 翻上/下一块的 K 线（跟个股层一样）。三个页签各一张列表，都得接上。
grep -q 'function boardRows' web/js/panels.js \
  && grep -qF "take('button[data-industry]', 'industry')" web/js/panels.js \
  && grep -qF "take('button[data-index]', 'index')" web/js/panels.js \
  && grep -qF "take('.ov-table tbody tr[data-code]', 'code')" web/js/panels.js \
  && ok "板块层的三张列表都接上了（热度 / 指数 / 总览）" || bad "板块层列表有漏（某个页签的滚轮翻不了块）"
grep -qF 'if (hit.board) pickBoard(target.code, target.name);' web/js/panels.js \
  && ok "翻板块走 pickBoard（开 K 线，同码不重开整轮 load）" \
  || bad "翻板块没走 pickBoard（同码会白跑一整轮 load）"
grep -qF "if (st.kind === 'sector' && cur)" web/js/panels.js \
  && ok "板块层接管有前提：图上得是板块、且当前块在列表里（个股状态不被劫持）" \
  || bad "板块层接管条件缺失（个股状态也会被板块列表劫持）"
grep -qF '.heat-item.on { background: #2a2e39; }' web/css/app.css \
  && ok "当前块在列表里看得见（.heat-item.on 高亮）" || bad "app.css 缺 .heat-item.on（翻到哪块看不见）"
# 板块总览六类图例（2026-11-02 用户报「点了没反应、悬停也没说明」）：必须是可以点的 button、
# 悬停带阈值口径、点击把下面行业表筛成该类；散点仍喂全量（只压暗非该类，免得点凭空消失）。
grep -q 'function ovLegendHtml' web/js/panels/overview.js && grep -qF '<button type="button" class="ov-lg' web/js/panels/overview.js \
  && ok "六类图例是 button 且由生成器产出" || bad "图例退回 span（点不动，也没有 disabled 态）"
[ "$(grep -c 'ovLegendHtml(' web/js/panels/overview.js)" = 3 ] \
  && ok "侧栏与放大窗共用一份图例（定义 1 + 调用 2）" \
  || bad "图例出现第二份手写实现（两侧口径会分叉）"
grep -qF 'const shown = data.filter(r => (!ovCat || ovLabel(r) === ovCat) && (!ovRs || r.rs90));' web/js/panels/overview.js \
  && grep -qF 'shown.map(rowHtml)' web/js/panels/overview.js \
  && ok "点图例把行业表筛成该类（六类按 ovLabel=label_comp，并叠加 RS90 筛选）" || bad "图例点击没接列表筛选（用户报的原症状）"
grep -qF 'opacity="0.15"' web/js/panels/overview.js && ! grep -qF 'overviewQuad(shown' web/js/panels/overview.js \
  && ok "筛选只压暗散点非该类，不删点、不改坐标系" || bad "散点被喂了筛后的行（点会消失／量程漂移）"
grep -qF "ovCat = ovCat === k ? '' : k;" web/js/panels/overview.js \
  && grep -qF 'bindOvLegend(body);' web/js/panels/overview.js && grep -qF 'bindOvLegend(ovBigMask);' web/js/panels/overview.js \
  && ok "再点同一个取消 + 侧栏/大窗两处都接线" || bad "图例漏接一侧（放大窗点了没反应）"
[ "$(awk '/const OV_CAT_TIP = \{/,/^  \};/' web/js/panels/overview.js | grep -c "': '")" = 6 ] \
  && ok "六类都有悬停口径文案" || bad "OV_CAT_TIP 缺条目（悬停空白＝照样不知道判据）"
grep -qF '≥ 70' web/js/panels/overview.js && grep -qF '≤ 30' web/js/panels/overview.js \
  && grep -qF 'r >= 70.0' "${OVV_SRC[@]}" && grep -qF 'r <= 30.0' "${OVV_SRC[@]}" \
  && ok "图例阈值文案与后端 label_of 同源（Rk6 70/30 分位）" || bad "前端口径与后端分类阈值分叉"
# 三个横截面页签统一手势（2026-11-02 用户定）：单击只把主图切成板块/指数 K 线，双击才下钻成分股。
# 一次双击 = 两记 click + 一次 dblclick，openBoard 会跑整轮 load（bars+指标+缠论），
# 所以同码必须去重；大窗里的点单击即关窗，元素先消失 → dblclick 永远不发，靠 240ms 挂起让位。
grep -q 'function pickBoard' web/js/panels.js \
  && grep -qF "if (st && st.kind === 'sector' && st.code === code) return;" web/js/panels.js \
  && ok "pickBoard 定义且带同码去重（双击的第二记不重跑整轮 load）" \
  || bad "pickBoard 缺失或去重被删（双击会开两遍图）"
[ "$(grep -c 'bindBoardDbl(' web/js/panels.js web/js/panels/overview.js | awk -F: '{n+=$2} END{print n}')" = 5 ] \
  && ok "单击/双击绑定是一个公共件、三个页签四处复用（定义 1 + 调用 4，跨 panels.js/overview.js）" \
  || bad "bindBoardDbl 调用点数漂移（某个页签被手写回旧的单击下钻）"
grep -qF "el.addEventListener('dblclick', () => drill());" web/js/panels.js \
  && ok "下钻挂在 dblclick 上（不是 click）" || bad "双击下钻没接线"
grep -qF 'ovBigHold = setTimeout(' web/js/panels/overview.js \
  && [ "$(grep -c 'clearTimeout(ovBigHold);' web/js/panels/overview.js)" -ge 3 ] \
  && ok "大窗散点：单击挂起 240ms 等 dblclick，三条路径都先撤定时器" \
  || bad "大窗单击直接关窗（元素先消失，双击永远收不到）或定时器没撤（会补开一张图）"
grep -qF '· 单击看板块 K 线，双击展开成分股' web/js/panels.js \
  && grep -qF '单击看指数 K 线，双击展开成分股' web/js/panels.js \
  && ok "行业热度 / 宽基指数页签各自写明新手势（改了动作不说＝用户以为坏了）" \
  || bad "某个页签没有手势提示文案"
# 板块总览页签的常驻提示按用户要求删掉了（挤占 UI）：手势说明只留在散点的悬浮 tip 里。
! grep -qF '单击=板块K线 · 双击=成分股' web/js/panels/overview.js \
  && grep -qF '单击 = 板块 K 线 · 双击 = 成分股' web/js/panels/overview.js \
  && ok "板块总览不再挂常驻手势提示（用户：挤占 UI），说明仍在散点悬浮 tip 里" \
  || bad "板块总览的常驻手势提示被加回来（或散点 tip 里的手势说明丢了）"
grep -qF 'table.grid tr.click { cursor: pointer' web/css/app.css \
  && ok "总览表行有可点光标（tr.click）" || bad "app.css 缺 tr.click 的 pointer 光标（看不出能点）"
# 下钻的表体也并成一份（2026-11-02）：三条下钻各抄一张成分股表时，改一次列口径要改三处，
# 漏一处就出现「同一列两个含义」。表头标记只应出现一次，行内开股只应接一次。
[ "$(grep -o 'heat-stock-grid' web/js/panels.js | wc -l)" = 1 ] \
  && ok "下钻成分股表只有一份渲染（三个页签共用，不各抄一张）" \
  || bad "成分股表被抄成多份（改列口径会漏改另两处）"
sed -n '/function renderHeat(/,/^  }$/p' web/js/panels.js | grep -q 'renderDrill(' \
  && sed -n '/function renderIndexParent(/,/^  }$/p' web/js/panels.js | grep -q 'renderDrill(' \
  && sed -n '/function renderOvDrill(/,/^  }$/p' web/js/panels/overview.js | grep -q 'renderDrill(' \
  && ok "行业热度/宽基指数/板块总览三条下钻都走同一个 renderDrill" \
  || bad "某个页签的下钻没走共享渲染器"
[ "$(grep -o "tr.addEventListener('click', () => TV.app.open(tr.dataset.stock))" web/js/panels.js | wc -l)" = 1 ] \
  && ok "下钻表内点股票开股只接一次（在共享表里）" \
  || bad "行内开股被复制到多处"
grep -qF 'function memberPct(' web/js/panels.js \
  && ! grep -qF 'function sortedHeat(' web/js/panels.js \
  && ok "涨跌列口径一处归一（memberPct：热度 heat 与指数 pct 同为当日涨幅）" \
  || bad "热度与指数的涨跌字段没归一（同一列两套口径）"

# —— TV-36B：板块总览已抽到 panels/overview.js ——
grep -q 'function renderOverview' web/js/panels/overview.js \
  && ! grep -q 'function renderOverview' web/js/panels.js \
  && ok "板块总览已抽到 panels/overview.js" \
  || bad "总览代码仍在 panels.js 或未落地"
grep -q '/js/panels/overview.js?v=' web/index.html \
  && ok "index.html 已加载 panels/overview.js" || bad "index.html 缺 overview.js"

echo
echo "== 5d. 技术指标（后端 ta-lib，参数走 p=）=="
check_json "GET /api/ind MA 默认 5/10/20/40/60/120/250（对齐 zg SMA 族）" "$BASE/api/ind?code=600519&period=day&lmt=600&name=ma" \
  'd["ok"] and [s["key"] for s in d["series"]]==["ma5","ma10","ma20","ma40","ma60","ma120","ma250"] and all(len(s["values"])>300 for s in d["series"])'
check_json "GET /api/ind EMA 默认 13/21/70/84（对齐 zg EMA 族）" "$BASE/api/ind?code=600519&period=day&lmt=600&name=ema" \
  'd["ok"] and [s["key"] for s in d["series"]]==["ema13","ema21","ema70","ema84"] and all(len(s["values"])>500 for s in d["series"])'
check_json "GET /api/ind MACD 三线" "$BASE/api/ind?code=600519&period=day&lmt=600&name=macd" \
  'd["ok"] and [s["key"] for s in d["series"]]==["dif","dea","hist"]'
check_json "GET /api/ind MACD 口径声明" "$BASE/api/ind?code=600519&period=day&lmt=600&name=macd" \
  'd["ok"] and "2×(DIF-DEA)" in d["caliber"]'
check_json "GET /api/ind 1 分钟 28800 根" "$BASE/api/ind?code=600519&period=1&lmt=28800&name=ma&p=5" \
  'd["n"]==28800 and len(d["series"][0]["values"])==28796'
check_json "GET /api/ind 板块 881130" "$BASE/api/ind?code=881130&period=day&lmt=400&name=macd&board=1" \
  'd["ok"] and d["n"]==400 and len(d["series"])==3'
check_json "GET /api/ind BOLL 三轨" "$BASE/api/ind?code=600519&period=day&lmt=600&name=boll" \
  'd["ok"] and [s["key"] for s in d["series"]]==["boll-up","boll-mid","boll-low"]'
check_json "GET /api/ind 自定义参数 MA 9,21" "$BASE/api/ind?code=600519&period=day&lmt=600&name=ma&p=9,21" \
  'd["ok"] and [s["key"] for s in d["series"]]==["ma9","ma21"]'
check_json "GET /api/ind 自定义参数 BOLL 26,3" "$BASE/api/ind?code=600519&period=day&lmt=600&name=boll&p=26,3" \
  'd["ok"] and [s["key"] for s in d["series"]]==["boll-up","boll-mid","boll-low"]'
check_json "GET /api/ind 参数校验" "$BASE/api/ind?code=600519&name=nope" \
  'd.get("ok") is False and "不支持的指标" in d["error"] and "boll" in d["error"]'
check_json "GET /api/ind 已删指标 KDJ 拒绝" "$BASE/api/ind?code=600519&period=day&name=kdj" \
  'd.get("ok") is False and "不支持的指标" in d["error"]'
check_json "GET /api/ind 已删指标 买卖量拒绝" "$BASE/api/ind?code=600519&period=day&name=bsv" \
  'd.get("ok") is False and "不支持的指标" in d["error"]'
check_json "GET /api/ind MACD 参数不足报错（不再 500 panic）" "$BASE/api/ind?code=600519&period=day&lmt=600&name=macd&p=12,26" \
  'd.get("ok") is False and "3 个参数" in d["error"]'
check_json "GET /api/ind MACD 快线≥慢线报错" "$BASE/api/ind?code=600519&period=day&lmt=600&name=macd&p=30,12,9" \
  'd.get("ok") is False and "快线必须小于慢线" in d["error"]'
check_json "GET /api/ind BOLL 周期留空报错" "$BASE/api/ind?code=600519&period=day&lmt=600&name=boll&p=,2" \
  'd.get("ok") is False and "周期不能为空" in d["error"]'
check_json "GET /api/ind MA 中间参数留空不位移" "$BASE/api/ind?code=600519&period=day&lmt=600&name=ma&p=5,,20,60" \
  'd["ok"] and [s["key"] for s in d["series"]]==["ma5","ma20","ma60"]'

# 独立重算对拍：MA/MACD/RSI/BOLL 对 python TA-Lib
if [ "$LIVE" = "1" ]; then
  skip "指标数值对拍（实时模式跳过；TV_LIVE=0 重启后再跑）"
elif out=$(python3 target/ind-parity.py 2>&1) && printf '%s' "$out" | grep -q '^ALL PASS$'; then
  ok "指标数值对拍（$(printf '%s' "$out" | grep -c '^PASS ') 项）"
else
  bad "target/ind-parity.py 不通过"
  printf '%s\n' "$out" | grep -E '^FAIL|^  !!' | head -6 | sed 's/^/       /'
fi

# MACD 独立复算：按通达信 EMA 递归手算，验 DIF/DEA/柱(×2) 与恒等式（不依赖 TA-Lib）
if [ "$LIVE" = "1" ]; then
  skip "MACD 通达信口径复算（实时模式跳过；TV_LIVE=0 重启后再跑）"
elif out=$(python3 target/macd-check.py 2>&1) && printf '%s' "$out" | grep -q '^ALL PASS$'; then
  ok "MACD 通达信口径复算（$(printf '%s' "$out" | grep -c '^PASS ') 只标的）"
else
  bad "target/macd-check.py 不通过"
  printf '%s\n' "$out" | grep -E '^FAIL|^  !!' | head -4 | sed 's/^/       /'
fi

grep -q 'data-ind="ma"' web/index.html && ok "index.html 指标按钮接线" || bad "index.html 缺指标按钮"
grep -q 'data-ind="boll"' web/index.html && grep -q 'data-ind="macd"' web/index.html \
  && ok "index.html BOLL / MACD 按钮接线" || bad "index.html 缺 BOLL 或 MACD 按钮"
if grep -q 'data-ind="kdj"' web/index.html; then bad "index.html 仍有 KDJ 按钮"; else ok "KDJ 按钮已下线"; fi
if grep -q 'data-ind="bsv"' web/index.html; then bad "index.html 仍有买卖量按钮"; else ok "买卖量按钮已下线"; fi
if grep -q 'id="shot"' web/index.html; then bad "index.html 仍有截图按钮"; else ok "截图按钮已下线"; fi
if grep -q 'kdj\|bsv' web/js/ind.js; then bad "ind.js 仍引用已删指标"; else ok "ind.js 已无 KDJ/买卖量引用"; fi
grep -q '/js/band-primitive.js' web/index.html && ok "布林带填色原语已加载" || bad "index.html 未加载 band-primitive.js"
grep -q "tv.ind.on" web/js/ind.js && ok "ind.js 持久化键 tv.ind.on" || bad "ind.js 缺持久化键"
grep -q "TV.ind.resetScales" web/js/main.js && ok "双击价格轴覆盖指标系列" || bad "main.js 未接 resetScales"
grep -q 'id="legend-ind"' web/index.html && grep -q "renderLegend" web/js/ind.js \
  && ok "指标图例接线（legend-ind + renderLegend）" || bad "指标图例未接线"
grep -q "err.set(name" web/js/ind.js && grep -q "button.err" web/css/app.css \
  && ok "指标加载失败可见（按钮标红 + tooltip）" || bad "指标加载失败仍静默"
grep -q "cache.delete(name);   // 缓存是别的标的/周期" web/js/ind.js \
  && ok "开关指标时旧周期缓存会失效重拉" || bad "ind.js 未处理跨周期陈旧缓存"

# 指标参数面板：齿轮 → 浮层 → p= 透传 → 缓存键含参数
grep -q "ind-gear" web/js/ind.js && grep -q "ind-params" web/js/ind.js \
  && ok "ind.js 参数面板接线（齿轮 + 浮层）" || bad "ind.js 缺参数面板接线"
grep -q "tv.ind.params" web/js/ind.js && ok "ind.js 参数持久化键 tv.ind.params" || bad "ind.js 缺参数持久化键"
grep -q "&p=" web/js/api.js && ok "api.js 指标请求带 p= 参数" || bad "api.js 未透传指标参数"
grep -q "ind-params" web/css/app.css && ok "app.css 参数浮层样式" || bad "app.css 缺参数浮层样式"
grep -q "required: true" web/js/ind.js && ok "ind.js 必填参数校验（MACD/BOLL）" || bad "ind.js 缺必填参数校验"
grep -q "至少保留一条线" web/js/ind.js && ok "ind.js MA 留空语义" || bad "ind.js 缺 MA 留空校验"
grep -q "def: 250" web/js/ind.js && grep -q "5, 10, 20, 40, 60, 120, 250" src/ind.rs \
  && ok "MA 默认 7 条与 zg SMA 族对齐" || bad "MA 默认周期未对齐 zg（5/10/20/40/60/120/250）"
grep -q "def: 84" web/js/ind.js && grep -q "13, 21, 70, 84" src/ind.rs \
  && ok "EMA 默认 4 条与 zg EMA 族对齐" || bad "EMA 默认周期未对齐 zg（13/21/70/84）"
grep -q "const main = 0.74" web/js/chart.js && grep -q "(1 - main) / (1 + n)" web/js/chart.js \
  && ok "副图份额：主图恒 74%，成交量与副图均分剩余 26%" || bad "chart.js 副图份额逻辑不符合 74/均分 口径"
grep -q "ta-lib 0.8.3" src/ind.rs && grep -q "hist2" src/ind.rs \
  && ok "MACD 口径：ta-lib + 柱 2×(DIF-DEA)（tv 原始实现）" || bad "MACD 引擎口径不符合 tv 原始实现"
grep -q "function stampOf" web/js/ind.js && grep -q "paramString(name)" web/js/ind.js \
  && ok "ind.js stamp 含参数（切参数不串数据）" || bad "ind.js stamp 未含参数"
grep -q "tv.candles.set" web/js/candles.js && grep -q "candle-pop" web/js/candles.js \
  && grep -q "pset" web/js/api.js \
  && ok "形态浮层可切集合（全部 61 / rec27），pset 进请求与缓存键" || bad "形态浮层/集合选择器未接线"
grep -q "renderCandles" web/js/panels.js web/js/panels/overview.js && bad "侧栏仍残留 renderCandles（应已并入「形」浮层）" \
  || ok "侧栏已无蜡烛形态面板（并入「形」浮层）"
grep -q "vp-pop" web/js/volume-profile.js && grep -q "vp-gear" web/js/volume-profile.js \
  && ok "VP 浮层已接线（齿轮开层，装范围/自定义日期/状态行）" || bad "VP 浮层/齿轮未接线"
grep -q "section('vp'" web/js/volume-profile.js && bad "侧栏仍残留 VP 面板（应已并入 VP 浮层）" \
  || ok "侧栏已无成交量分布面板（并入 VP 浮层）"
grep -q "pa2-pop" web/js/pa2.js && grep -q "pa2-gear" web/js/pa2.js \
  && ok "PA2 浮层已接线（齿轮开层，装摆动长度/统计/BOS·CHoCH 表）" || bad "PA2 浮层/齿轮未接线"
grep -q "renderPa2" web/js/panels.js web/js/panels/overview.js && bad "侧栏仍残留 renderPa2（应已并入 SMC 浮层）" \
  || ok "侧栏已无 PA2 结构面板（并入 SMC 浮层）"

echo
echo "== 5e. 个股题材（MAC 在线源，本地块文件停在 2022）=="
# 源码门：题材主源必须是 MAC 缓存，旧 block_gn.dat 只能作回退
grep -q 'concepts::for_stock' src/tags.rs \
  && ok "tags.rs 主源走 concepts（MAC 缓存）" || bad "tags.rs 未走 MAC 概念缓存"
grep -q 'legacy_concepts_map' src/tags.rs \
  && ok "旧 block_gn.dat 仅作回退（函数名显式标 legacy）" || bad "旧概念路径未标 legacy"
has_fn belong_board src/tdxmac.rs && has_fn board_list src/tdxmac.rs \
  && has_fn board_members src/tdxmac.rs \
  && ok "MAC 板块三命令已实现" || bad "MAC 板块命令缺失"
grep -q 'board_protocol_code' src/tdxmac.rs \
  && ok "板块码换算已接线（880564 → 20564）" || bad "缺板块码换算（直传会静默返回 0 行）"
# 陈旧后门：旧文件若被当成主源，会静默给出 2022 口径
if grep -q 'EXCLUDED_BLOCKS.*通达信88' src/tags.rs && grep -q 'EXCLUDED_NAMES.*通达信88' src/concepts/cache.rs; then
  ok "通达信88 指数块在新旧两条路径都剔除"
else
  bad "通达信88 剔除口径不一致"
fi
# 解码守卫：批量路径必须有「不可信行比例」判据，否则解码错位只会写进半垃圾
grep -q 'valid_member' src/concepts/source.rs && grep -q 'rejected \* 20' src/concepts/source.rs \
  && ok "成分股解码守卫（不可信行 >5% 拒绝落盘）" || bad "缺成分股解码守卫"
# 退化缓存：单股按需写入后不能让全量刷新压后一整周
grep -q 's.boards == 0 ||' src/concepts/cache.rs \
  && ok "退化缓存（boards=0）判为过期" || bad "单股缓存会压制全量刷新"
# 离线开关必须管到「单股按需查询」这条旁路，否则 TV_LIVE=0 仍会静默出网
grep -q 'if !crate::live::enabled()' src/concepts/source.rs \
  && ok "概念单股查询受 TV_LIVE 开关约束" || bad "离线模式仍会联网查题材"
# 概念/地区/风格统一按自然周刷新：每周第一次启动刷一次（判据 = 根 ts 早于本周一 00:00），
# 周内读路径不出网。旧的 30 分钟风格快周期必须整体移除——留着等于每周口径下仍逐股出网。
has_fn fresh_this_week src/concepts/cache.rs \
  && ok "缓存按自然周判新（每周第一次启动刷一次）" || bad "缺每周刷新判据"
grep -qF 's.boards == 0 || !fresh_this_week(s.ts)' src/concepts/cache.rs \
  && ok "stale() 走周判据" || bad "过期判据未接周口径"
! grep -q 'STYLE_TTL_SECS\|refresh_styles_if_stale\|merge_styles' src/concepts.rs src/concepts/*.rs \
  && ok "30 分钟风格快周期已移除（命中缓存整周不出网）" || bad "仍残留逐股风格快刷新"
# 出网入口有三条：单股查询、`--refresh-concepts`、`/api/concepts-rebuild`。只挡第一条
# 等于开关仍可被一次 CLI 或页面按钮绕过（50 秒全量出网）。两条刷新入口必须都挡。
[ "$(grep -c 'live_allowed()?;' src/concepts/source.rs)" = 2 ] \
  && ok "两条全量刷新入口（CLI + 端点）都受离线开关约束" \
  || bad "离线开关可被 --refresh-concepts 或 /api/concepts-rebuild 绕过"

st=$(curl -s -m 5 "$BASE/api/concepts-stats")
src=$(printf '%s' "$st" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("source",""))' 2>/dev/null)
blocks=$(printf '%s' "$st" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("boards",0))' 2>/dev/null)
stocks=$(printf '%s' "$st" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("stocks",0))' 2>/dev/null)
stale=$(printf '%s' "$st" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(str(d.get("stale")).lower())' 2>/dev/null)
if [ -z "$stocks" ] || [ "$stocks" = "0" ]; then
  bad "概念缓存缺失或为空（先跑 tv --refresh-concepts）"
else
  ok "概念缓存：$blocks 个板块 / $stocks 只股票 / 来源 $src"
  case "$src" in
    mac@*) ok "题材来自 MAC 在线源" ;;
    *)     bad "题材来源不是 MAC（$src）" ;;
  esac
  [ "$stale" = "true" ] && bad "概念缓存已过期（stale=true：本周还没刷过）" || ok "概念缓存新鲜（本周内）"
fi

# 近年题材必须在：这是「本地快照停在 2022」这件事的正向判据
if python3 - "$BASE" <<'PY' 2>&1
import json, sys, urllib.request
base = sys.argv[1]
def tags(code):
    with urllib.request.urlopen(f"{base}/api/stock-tags?code={code}", timeout=8) as r:
        return json.loads(r.read())
t = tags("000338")
c = t.get("concepts") or []
need = ["AI智能体", "DeepSeek概念", "固态电池"]
missing = [n for n in need if n not in c]
assert not missing, f"概念缺 {missing}（实际 {c}）"
# 反向：地区/风格/指数块不得混进概念
bad = [n for n in c if n in ("山东板块", "基金重仓", "通达信88")]
assert not bad, f"概念里混入非题材块 {bad}"
assert (t.get("regions") or []) == ["山东板块"], f"地区字段异常 {t.get('regions')}"
assert t.get("concept_source", "").startswith("mac@"), f"来源不是 MAC {t.get('concept_source')}"
print(f"000338 概念 {len(c)} 个，地区 {t['regions']}，来源 {t['concept_source']}")
PY
then
  ok "近年题材在列（AI智能体/DeepSeek概念）+ 概念不混地区/风格"
else
  bad "概念内容不含近年题材或混入非题材块"
fi

# —— TV-36A：concepts 已拆成 cache/source 两文件 ——
[ -f src/concepts/cache.rs ] && [ -f src/concepts/source.rs ] \
  && ok "concepts 已拆为 cache/source 两文件" || bad "concepts 拆分缺失"
[ "$(wc -l < src/concepts.rs)" -le 60 ] \
  && ok "src/concepts.rs 只剩模块声明与 re-export" || bad "src/concepts.rs 仍然过大（>60 行）"

# —— 复查补丁（TV-34.1）：缓存写回路径的不变量 ——
has_fn cache_key src/concepts/cache.rs && grep -q 'cache_key(code.trim())' src/concepts/source.rs \
  && ok "缓存键归一化（sh600519/600519.SH 走同一键）" || bad "缓存键未归一化（带前后缀的请求必然 miss 并污染文件）"
grep -q 's.boards, s.ts, s.quality' src/concepts/source.rs \
  && ok "单股补漏沿用旧根 ts（不把下一次周刷整整数地推迟）" || bad "补漏写回会覆盖根 ts"
grep -q 'drop(guard)' src/concepts/source.rs \
  && { grep -q 'write_locked' src/concepts/source.rs || grep -q 'write_locked' src/concepts/cache.rs; } \
  && ok "补漏的读-改-写在 FILE_LOCK 内（并发不丢更新）" || bad "补漏合并不在锁内（并发写会互相覆盖）"
has_fn negative_put src/concepts/cache.rs && grep -q 'negative_put' src/concepts/source.rs \
  && ok "非个股代码走内存负缓存（不进文件、不每次出网）" || bad "指数/板块码每次都重连 MAC"
grep -q 'take_live_conn' src/concepts/source.rs \
  && ok "单股实时查询复用 MAC 连接" || bad "每次补漏都新建 TCP 会话"
grep -q 'with_retry(' src/concepts/source.rs \
  && ok "板块页抓取有整页重试（一次抖动不废掉 50 秒全量刷新）" || bad "全量刷新无重试"
grep -q '!is_excluded(&b.name)' src/concepts/source.rs \
  && ok "通达信88 在两条写盘路径都剔（批量 retain + 单股 live_belong）" || bad "单股补漏会写回通达信88，与批量口径不一致"
grep -q 'Builder::new' src/concepts/source.rs \
  && ok "后台刷新用 Builder 接住建线程失败（标志位不会永久卡住）" || bad "thread::spawn 失败会永久占用 REBUILDING"
grep -q 'REBUILD_ATTEMPT_AT' src/concepts/source.rs \
  && ok "补漏触发的全量刷新带退避（MAC 半故障不打满线程池）" || bad "补漏路径无退避"
# 解码质量必须随文件落盘：否则「这份缓存是怎么建出来的」第二天无从查证
grep -q '"quality".into()' src/concepts/cache.rs \
  && ok "解码质量落盘（rows/rejected/empty_boards）" || bad "质量指标只存在于刷新返回值，无法事后审计"

# 数据层门禁：直接查缓存文件的形态（源码 grep 抓不到的回归，文件里抓得到）
if python3 - <<'PY' 2>&1
import json, collections
p = 'data/tdx/concepts_cache.json'
d = json.load(open(p))
m = d['map']
bad_keys = [k for k in m if len(k) != 6 or not k.isdigit()]
assert not bad_keys, f"非规范缓存键 {len(bad_keys)} 个（前 5: {bad_keys[:5]}）"
stamps = [t[3] for v in m.values() for t in v if len(t) > 3]
assert stamps, "条目没有抓取时刻（第 4 位缺失）"
top, cnt = collections.Counter(round(s, 3) for s in stamps).most_common(1)[0]
# 绝大多数条目共享同一抓取时刻 = 全量刷新的签名，且那个时刻就是根 ts。
# 若单股补漏重新覆盖根 ts，根 ts 会变成唯一的新值、与这个众数对不上。
assert cnt / len(stamps) > 0.5, f"条目时间戳过于分散（众数只占 {cnt}/{len(stamps)}）"
assert abs(top - d['ts']) < 2, f"根 ts {d['ts']} 与条目时间戳众数 {top} 不一致（补漏覆盖了全量刷新时刻）"
q = d.get('quality') or {}
assert q.get('rows', 0) > 0, "缓存没有解码质量指标"
rej = q.get('rejected', 0)
assert rej * 20 <= q['rows'] + rej, f"落盘缓存的不可信行占比超 5%（{rej}/{q['rows']}）"
print(f"  缓存 {len(m)} 只 / 条目时间戳众数占比 {cnt}/{len(stamps)} / 质量 {q}")
PY
then
  ok "缓存文件形态（键归一 + 根 ts=全量时刻 + 质量达标）"
else
  bad "缓存文件形态不符（键污染 / 根 ts 被补漏覆盖 / 质量缺失）"
fi

# stats 必须把质量透出来（门禁和运维看得到，不用读文件）
SQ=$(curl -s -m 5 "$BASE/api/concepts-stats" | python3 -c '
import json,sys
d=json.load(sys.stdin)
q=d.get("quality") or {}
print("bad" if not q or q.get("rejected_pct", 99) > 5 else "ok", q.get("rejected_pct", "-"))' 2>/dev/null)
case "$SQ" in
  ok*) ok "concepts-stats 透出解码质量（不可信行 ${SQ#ok }%）" ;;
  *)   bad "concepts-stats 缺 quality 或不可信行超 5%" ;;
esac

# 独立对表：全量缓存 ↔ 独立 0x1218 单股查询（批量路径解码错了这条必挂）
if [ "$LIVE" = "1" ]; then
  if out=$(python3 tools/concepts_parity.py 2>&1); then
    if printf '%s' "$out" | grep -q 'ALL PASS'; then
      ok "概念源对表（$(printf '%s' "$out" | grep -cE '✓ [0-9]{6}: .*重叠') 只概念/地区重叠达标 + 风格重叠 + 新鲜度/覆盖度）"
    else
      skip "概念源对表（$(printf '%s' "$out" | grep -m1 '跳过' || echo '未达判据')）"
    fi
  else
    bad "概念源对表不通过"
    printf '%s\n' "$out" | grep -E '^  ✗' | head -5 | sed 's/^/       /'
  fi
else
  skip "概念源对表（离线模式不连 MAC）"
fi

echo
echo "== 6. ETag =="
U="$BASE/api/bars?code=600519&period=day&lmt=5"
ET=$(curl -s -D- -o /dev/null "$U" | awk -F': ' 'tolower($1)=="etag"{print $2}' | tr -d '\r')
if [ -z "$ET" ]; then
  bad "响应没有 ETag"
else
  ok "ETag = $ET"
  c=$(curl -s -o /dev/null -w '%{http_code}' -H "If-None-Match: $ET" "$U")
  [ "$c" = 304 ] && ok "条件请求 304" || bad "条件请求返回 $c（应为 304）"
  c2=$(curl -s -o /dev/null -w '%{http_code}' -H "If-None-Match: $ET" \
       "$BASE/api/bars?code=600519&period=day&lmt=6")
  [ "$c2" = 200 ] && ok "lmt 变化不复用旧 304" || bad "lmt 变化返回 $c2（必须 200）"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "ALL PASS"
  exit 0
fi
echo "失败 $fail 项"
exit 1
