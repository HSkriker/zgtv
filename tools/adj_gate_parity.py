#!/usr/bin/env python3
"""复权基兼容门（src/live.rs::remote_mergeable）的对拍脚本。

兼容门决定「远端日线能不能并进本地前复权序列」。它一旦放行错基源，
那 2900 根（约 11.6 年）的历史会被远端整体改写，日 K 图与分时图就此错基
（分时侧永远在本地 gbbq 基上）。所以这个门必须有实测依据，不能靠直觉。

本脚本直接取三类真源，按 Rust 侧同一套判据算一遍，给出「放行 / 拒收」的判决：

  local : 本地 gbbq 事件法前复权（tv /api/bars 的基准）
  tq    : TQ-Local `dividend_type=front`（设计上是逐位同基）
  em    : 东财 `fqt=1` 减法前复权（设计上是异基，必须被拒）

判据（与 Rust 侧一一对应）：
  dev     = |round2(remote.close) - round2(local.close)|
  allowed = max(0.1% × local.close, 0.011 元)      # 相对门限 + 1 分 1 毫绝对下限
  ratio   = dev / allowed
  两段采样：最旧 200 根重叠、最近 200 根重叠，各自取中位；任一段中位 > 1 即拒。

用法：
  python3 tools/adj_gate_parity.py 600346 000001 600519 601398
  python3 tools/adj_gate_parity.py --no-tq 600346     # 只对东财（不碰 TQ）

TQ 纪律：一次一只、count ≤ 2900、两次调用间隔 ≥ 2s（与 src/live.rs 的 TQ 闸门一致）。
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
import urllib.request

TQ_URL = "http://127.0.0.1:17709/"
TV_URL = "http://127.0.0.1:8790"
EM_URL = "https://push2his.eastmoney.com/api/qt/stock/kline/get"

TOL = 0.001          # REMOTE_ADJ_TOL：相对门限 0.1%
FLOOR = 0.011        # REMOTE_ROUND_FLOOR：绝对下限 1 分 1 毫
WINDOW = 200         # 每段采样根数


def round2(x: float) -> float:
    return round(x + 1e-12, 2)


def tq_daily(code: str, count: int = 2900) -> dict[str, float]:
    """TQ 前复权日线（本地直连 TQ 服务，走与 tv 相同的 JSON-RPC 形状）。"""
    sym = code + (".SH" if code[0] in "69" else ".SZ")
    body = json.dumps({
        "id": 1,
        "method": "get_market_data",
        "params": {"stock_list": [sym], "period": "1d", "count": count,
                   "dividend_type": "front"},
    }).encode()
    req = urllib.request.Request(TQ_URL, data=body,
                                 headers={"Content-Type": "application/json; charset=utf-8"})
    j = json.loads(urllib.request.urlopen(req, timeout=90).read().decode())
    val = ((j.get("result") or {}).get("Value") or {}).get(sym) or {}
    out: dict[str, float] = {}
    for d, c in zip(val.get("Date") or [], val.get("Close") or []):
        s = str(d)[:8]
        if len(s) == 8:
            out[f"{s[:4]}-{s[4:6]}-{s[6:8]}"] = float(c)
    return out


def em_daily(code: str, lmt: int = 2900) -> dict[str, float]:
    """东财 fqt=1（减法前复权）日线。"""
    secid = ("1." if code[0] in "69" else "0.") + code
    url = (f"{EM_URL}?secid={secid}&fields1=f1,f2,f3,f4,f5,f6"
           f"&fields2=f51,f52,f53,f54,f55,f56,f57,f58&klt=101&fqt=1"
           f"&beg=0&end=20500101&lmt={lmt}")
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0",
                                               "Referer": "https://quote.eastmoney.com/"})
    j = json.loads(urllib.request.urlopen(req, timeout=30).read().decode("utf-8", "ignore"))
    out: dict[str, float] = {}
    for line in (j.get("data") or {}).get("klines") or []:
        p = line.split(",")
        out[p[0]] = float(p[4])
    return out


def tv_local(code: str, lmt: int = 2900) -> dict[str, float]:
    """tv 的本地日线（qfq，3 位小数）。"""
    url = f"{TV_URL}/api/bars?code={code}&period=day&lmt={lmt}"
    j = json.loads(urllib.request.urlopen(url, timeout=300).read().decode())
    return {b["time"]: float(b["close"]) for b in j.get("bars") or []}


def gate(local: dict[str, float], remote: dict[str, float]) -> tuple[bool, list[str]]:
    """复刻 remote_mergeable 的判据：返回（是否放行, 每段的中位说明）。"""
    ov = [(d, local[d], remote[d]) for d in sorted(set(local) & set(remote))
          if local[d] > 0 and remote[d] > 0]
    if len(ov) < 5:
        return True, ["重叠不足 5 根 → 放行"]
    notes: list[str] = []
    ok = True
    for name, seg in (("最旧", ov[:WINDOW]), ("最近", ov[-WINDOW:])):
        if len(seg) < 5:
            continue
        ratios = []
        for _d, lc, rc in seg:
            dev = abs(round2(rc) - round2(lc))
            allowed = max(TOL * lc, FLOOR)
            ratios.append((dev / allowed, dev / lc * 100.0))
        med = statistics.median(r[0] for r in ratios)
        worst = max(ratios, key=lambda r: r[0])
        notes.append(f"{name} {len(seg)}根 中位占容差 {med:.3f} 倍（最差 {worst[1]:.3f}%）")
        if med > 1.0:
            ok = False
    return ok, notes


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("codes", nargs="+", help="6 位股票代码")
    ap.add_argument("--no-tq", action="store_true", help="不查 TQ（只对东财）")
    args = ap.parse_args()

    bad = 0
    for i, code in enumerate(args.codes):
        local = tv_local(code)
        print(f"== {code}  本地 {len(local)} 根")
        for label, fetch in (("TQ", None if args.no_tq else lambda: tq_daily(code)),
                             ("东财", lambda: em_daily(code))):
            if fetch is None:
                continue
            try:
                remote = fetch()
            except Exception as e:  # noqa: BLE001
                print(f"   {label}: 取数失败 {e}")
                bad += 1
                continue
            if i or label == "东财":
                time.sleep(2)   # TQ 间隔闸门（同 src/live.rs 的 CALL_GAP）
            ok, notes = gate(local, remote)
            verdict = "放行" if ok else "拒收"
            print(f"   {label}: {verdict}  " + " | ".join(notes))
            if label == "TQ" and not ok:
                bad += 1      # TQ 必须放行
            if label == "东财" and ok:
                bad += 1      # 东财必须拒收
    print(f"\n不合期望 {bad} 项")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
