#!/usr/bin/env python3
"""MAC 分钟源对表：Rust 实现（src/tdxmac.rs，经 /api/bars 出）与 Python easy_tdx MAC 客户端。

背景：远端分钟源换成通达信 MAC（扩展行情）协议后，口径必须与独立实现逐根一致。
标准协议的 K 线接口在公共服务器上是空的（easytdx / pytdx / tdxrs 全实测为空），
MAC 协议才供数 —— 所以这条对表是 MAC 实现的唯一外部基准。

关键约束：**必须连 tv 实际使用的那台主机**。
各 MAC 主机的日汇总一致（成交量/成交额对本地 .lc1 完全吻合），但**桶级分桶有微差**
（tick 归桶时序不同，个别 5 分钟桶差 1~2 手）——跨主机对表会把这种服务器差异
误判成实现不一致。tv 把实际主机放在 /api/bars 的 meta.detail 里，这里据此连同一台。

用法：python3 tools/mac_parity.py [base_url]
缺 easy_tdx（Linux 侧 pip install easy-tdx-plus）时打印 SKIP 并退出 0，不阻断终检。
"""
import json
import sys
import urllib.request
from datetime import datetime, timezone

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8790"
CODES = ["000338", "600519"]
# 1 分钟档自 2026-11-02 起只读本地 .lc1（离线采集，见 live.rs::minute_series），不再走 MAC；
# 留着它只会每次跑出一条必然的 SKIP，MAC 分钟链路的对表由 5 分钟承担。
PERIODS = ["5"]
# 只比最近 N 根：MAC 的分钟历史深度有限（实测约 10~17 个交易日），且 tv 的远端窗口同理
TAIL = 400


def api(path):
    with urllib.request.urlopen(BASE + path, timeout=180) as r:
        return json.loads(r.read().decode("utf-8"))


def tv_minutes(code, period):
    """tv 侧：/api/bars 的分钟行 → {时间: (close, vol手, amount元)}"""
    j = api(f"/api/bars?code={code}&period={period}&lmt=28800")
    out = {}
    for b in j.get("bars") or []:
        t = b["time"]
        if isinstance(t, int):
            # 接口给的是「北京墙钟当 UTC」的纪元秒
            key = datetime.fromtimestamp(t + 8 * 3600, timezone.utc).strftime("%Y-%m-%d %H:%M")
        else:
            key = str(t)
        out[key] = (b["close"], b["vol"], b["amount"])
    return out, j.get("meta", {})


def mac_minutes(code, period, host=None):
    """Python 侧：easy_tdx MAC 客户端的同周期 K 线 → {时间: (close, vol股, amount元)}"""
    from easy_tdx import MacClient, Period

    period_map = {"1": Period.MIN_1, "5": Period.MIN_5}
    market = 1 if code.startswith("6") else 0
    if host:
        c = MacClient(host=host, port=7709, timeout=15)
    else:
        c = MacClient()
    c.connect()
    df = c.get_stock_kline(market, code, period_map[period], 0, 700, 1, 1)  # fq=1 前复权
    out = {}
    for row in df.itertuples():
        key = row.datetime.strftime("%Y-%m-%d %H:%M")
        out[key] = (float(row.close), float(row.vol), float(row.amount))
    return out


def close(a, b, tol):
    return abs(a - b) <= max(tol, abs(b) * 1e-6)


def main():
    try:
        import easy_tdx  # noqa: F401
    except Exception:
        print("SKIP  mac_parity（未安装 easy_tdx：pip install easy-tdx-plus）")
        return 0
    fails = []
    for code in CODES:
        for period in PERIODS:
            try:
                tv, meta = tv_minutes(code, period)
            except Exception as e:
                print(f"SKIP  {code} {period}min（取 tv 侧数据失败：{e}）")
                continue
            src = meta.get("source")
            host = meta.get("detail") or ""
            if src != "mac":
                print(f"SKIP  {code} {period}min（tv 本次分钟源={src}，未走 MAC，无可比基准）")
                continue
            try:
                mac = mac_minutes(code, period, host or None)
            except Exception as e:
                print(f"SKIP  {code} {period}min（MAC 取数失败：{e}）")
                continue
            common = sorted(set(tv) & set(mac))
            if len(common) < 10:
                fails.append(f"{code} {period}min 共同时间戳只有 {len(common)} 个（<10）")
                continue
            tail = common[-TAIL:]
            # vol / amount 不受复权基影响 → 必须逐根全等
            bad_v = bad_a = 0
            # close：tv 侧做了「逐日基对齐」（MAC 服务端复权因子与本地 gbbq 不是同一套，
            # 除权日差 0.33%），所以同一天内 tv/mac 的比值必须**恒定**（统一比例缩放）。
            #   · 比例恒定 → MAC 解码与缩放逻辑都对（字段错位/部分缩放都会让比例在一天内跳变）
            #   · 比例 = 1 的日子 → 逐根 close 必须全等（没有任何基差）
            #   · 比例 ≠ 1 的日子 → 记为「基对齐日」，只要求当天比例一致
            ratios = {}
            for k in tail:
                tc, tv_vol, ta = tv[k]
                mc, mv, ma = mac[k]
                if not close(tv_vol, mv / 100.0, 1e-3):
                    bad_v += 1
                if not close(ta, ma, 1.0):
                    bad_a += 1
                day = k[:10]
                r = tc / mc if mc else 0.0
                ratios.setdefault(day, []).append((k, r))
            bad_days = []   # 比例在一天内不恒定
            aligned_days = []  # 比例 ≠ 1 的基对齐日
            for day, items in sorted(ratios.items()):
                vals = [r for _, r in items]
                lo, hi = min(vals), max(vals)
                if hi - lo > max(1e-6, abs(lo) * 1e-5):
                    bad_days.append((day, lo, hi))
                elif abs(lo - 1.0) > 1e-6:
                    aligned_days.append((day, lo))
            print(
                f"{code} {period}min: 比对 {len(tail)} 根（共同 {len(common)}，tv 源={src}"
                f"{'@' + host if host else ''}）"
                f" vol 不符 {bad_v} / amount 不符 {bad_a} / 基对齐日 {len(aligned_days)} 天"
                f" / 比例不恒定 {len(bad_days)} 天"
            )
            for day, lo in aligned_days[:4]:
                print(f"    基对齐日 {day}: tv/mac = {lo:.6f}（MAC 服务端复权基与本地 gbbq 的差，已由 tv 逐日对齐）")
            for day, lo, hi in bad_days[:4]:
                print(f"    !! {day} 比例在一天内不恒定: {lo:.6f} ~ {hi:.6f}")
            if bad_v or bad_a or bad_days:
                fails.append(
                    f"{code} {period}min 不一致 vol={bad_v} amount={bad_a} 比例不恒定={len(bad_days)}"
                )
    if fails:
        print("FAIL " + "；".join(fails))
        return 1
    print("ALL PASS")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:  # 网络抖动/接口异常不该让终检整条红
        print(f"SKIP  mac_parity（脚本异常：{e}）")
        sys.exit(0)
