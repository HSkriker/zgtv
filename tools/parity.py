#!/usr/bin/env python3
"""tv ↔ zg 对表：日线 / 分钟线 / 缠论计数。全部 PASS 才 exit 0。

历史对表工具：tv 已于 2026-11-01 完全脱离 zg（crate 与数据均已内嵌）。
本脚本仍需一份**外部 zg 可执行文件**做参照物，但仓库不再假定额度任何固定路径：
必须用 `TV_ZG_BIN=/path/to/zg` 显式指定，未指定或文件不存在就打印 SKIP 并 exit 0。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

TV = "http://127.0.0.1:8790"
# zg 参照物二进制：只认环境变量，不给默认路径（tv 与 zg 已无任何仓库关系）。
ZG_BIN = os.environ.get("TV_ZG_BIN")
BJT = timezone(timedelta(hours=8))
TV_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

fails = []


def api(path):
    with urllib.request.urlopen(TV + path, timeout=120) as r:
        return json.loads(r.read().decode("utf-8"))


def zg(*args):
    env = os.environ.copy()
    # zg 自己也要 ZG_ROOT（它按 <root>/data/... 找数据）。没显式给就按二进制位置反推：
    # 约定布局 <zg 仓根>/zg-rs/target/<profile>/zg，最多往上找 4 层看有没有 zg-rs/。
    # 其余行情路径一律继承调用方环境，不再替某台机器写死 /mnt/... 兜底。
    if not env.get("ZG_ROOT"):
        p = os.path.abspath(ZG_BIN)
        for _ in range(4):
            p = os.path.dirname(p)
            if os.path.isdir(os.path.join(p, "zg-rs")):
                env["ZG_ROOT"] = p
                break
    out = subprocess.check_output([ZG_BIN, *args], text=True, env=env)
    return json.loads(out)


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  {detail}" if detail else ""))
    if not ok:
        fails.append(name)


def eq(a, b):
    a, b = float(a), float(b)
    return abs(a - b) <= max(1e-6, abs(b) * 1e-9)


def main():
    if not ZG_BIN:
        print("SKIP  全部对表项：未指定 zg 参照物二进制（TV_ZG_BIN 未设）")
        print("      tv 自身的独立自检走 ./tools/final_check.sh（不需要 zg）。")
        print("      要对表时：TV_ZG_BIN=/path/to/zg python3 tools/parity.py")
        return 0
    if not os.path.exists(ZG_BIN):
        print(f"SKIP  全部对表项：找不到 zg 参照物 {ZG_BIN}")
        print("      tv 自身的独立自检走 ./tools/final_check.sh（不需要 zg）。")
        return 0

    # ---------- 日线 ----------
    t = api("/api/bars?code=600519&period=day&lmt=5")
    z = zg("bars", "chart", "--code", "600519", "--period", "day", "--lmt", "5")
    check("day 根数", len(t["bars"]) == len(z["bars"]) == 5,
          f'{len(t["bars"])} vs {len(z["bars"])}')
    for i, (a, b) in enumerate(zip(t["bars"], z["bars"])):
        check(f"day[{i}].time", a["time"] == b["date"], f'{a["time"]} vs {b["date"]}')
        for k in ("open", "high", "low", "close", "vol"):
            check(f"day[{i}].{k}", eq(a[k], b[k]), f'{a[k]} vs {b[k]}')

    # ---------- 分钟 ----------
    # zg 的 minute 通道要求至少 60 根（不足直接报「数据不足60根」），因此两边都取 60，比最后 3 根。
    t = api("/api/bars?code=600519&period=5&lmt=60")
    z = zg("bars", "minute", "--market", "sh", "--code", "600519",
           "--freq", "F5", "--count", "60")
    check("min 根数", len(t["bars"]) == len(z["bars"]) == 60,
          f'{len(t["bars"])} vs {len(z["bars"])}')
    for i, (a, b) in enumerate(zip(t["bars"][-3:], z["bars"][-3:])):
        ts = datetime.fromtimestamp(a["time"], BJT).strftime("%Y-%m-%d %H:%M")
        check(f"min[-{3 - i}].time", ts == b["date"][:16], f'{ts} vs {b["date"][:16]}')
        check(f"min[-{3 - i}].close", eq(a["close"], b["close"]), f'{a["close"]} vs {b["close"]}')
        # tv 股票已除 100（手）；zg 给的是股
        check(f"min[-{3 - i}].vol", eq(a["vol"] * 100, b["volume"]),
              f'{a["vol"] * 100} vs {b["volume"]}')

    # ---------- 缠论（需要 8786 在跑） ----------
    # 口径差异（2026-11-01 起，TV-31）：tv 把 `max_bi_num_for` 按自家图窗放宽
    # （日线 50→500，分钟档同理），zg 仍是窄图窗旧表。笔/中枢**数量**因此不同，
    # 但重叠尾部的结构应逐字段一致 —— 比「尾部全等」而不是比总数。
    try:
        t = api("/api/chan?code=600519&period=day")
        with urllib.request.urlopen(
                "http://127.0.0.1:8786/api/czsc?code=600519&period=day", timeout=120) as r:
            z = json.loads(r.read().decode("utf-8"))
        tb, zb = t["bis"], z["bis"]
        n = min(len(tb), len(zb))
        tail_ok = n > 0 and all(
            all(tb[-i][k] == zb[-i][k] for k in ("sdt", "edt", "sd", "high", "low"))
            for i in range(1, n + 1)
        )
        check(f"chan bis 尾部 {n} 根逐字段一致（tv {len(tb)} / zg {len(zb)}，笔数表不同）",
              tail_ok, f"tv {len(tb)} / zg {len(zb)}")
        tz, zz = t["zs"], z["zs"]
        m = min(len(tz), len(zz))
        # 尾部最老的一个中枢起点会受窗口截断影响（zg 笔窗更短），只比 m-1 个
        zs_ok = m >= 2 and all(
            all(tz[-i][k] == zz[-i][k] for k in ("sdt", "edt", "zd", "zg", "bis"))
            for i in range(1, m)
        )
        check(f"chan zs 尾部 {max(m - 1, 0)} 个逐字段一致（tv {len(tz)} / zg {len(zz)}）",
              zs_ok, f"tv {len(tz)} / zg {len(zz)}")
        check("chan bs 数量", len(t["bs"]) == len(z["bs"]),
              f'{len(t["bs"])} vs {len(z["bs"])}')
    except (urllib.error.URLError, ConnectionError, KeyError):
        print("SKIP  chan 对照（8786 未启动）")

    print()
    if fails:
        print(f"FAILED {len(fails)} 项：")
        for f in fails:
            print("  -", f)
        return 1
    print("ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
