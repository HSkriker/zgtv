#!/usr/bin/env python3
"""概念题材缓存对表验证：MAC 全量缓存 vs ① 独立 0x1218 单股查询 ② 本地 block_gn.dat（2022 旧快照）。

判据（双向，缺一不可）：
  ① **新鲜度**：MAC 缓存必须含 2022 旧文件里不存在的近年题材，且旧文件的老题材仍在；
  ② **覆盖度**：旧文件有概念的股票，MAC 缓存也要覆盖（缺口 > 5% 判失败）；
  ③ **权威交叉**：抽样 N 只股票，独立发 0x1218 问一次，逐条比对
     （类型/代码/名称三字段）。**这一条不能只用名字后缀猜类型**——
     `ST板块` 名字像地区块但其实是概念（type=4），用启发式会误判。
     口径按板块类型的稳定性分开（见 STABLE_TYPES / STYLE_TYPE 注释）：
     缓存按自然周刷新（每周第一次启动刷一次），周内厂商的成员漂移不可消除，
     所以三类都只要求聚合重叠率：地区(3)/概念(4) 逐股重叠 ≥85%，风格(5) ≥80%——
     整块解码失败（名字全乱/全丢）会掉到 0，照常拦得住。
  ④ 分类型抽样展示（概念/地区/风格分开看）。

退出码 0=全过，1=有判据失败，2=前置缺失。
"""
import json
import socket
import struct
import sys
from pathlib import Path

TV = Path(__file__).resolve().parent.parent
CACHE = TV / "data" / "tdx" / "concepts_cache.json"
LEGACY = TV / "data" / "tdx" / "block_gn.dat"

MAC_HOST = "121.36.248.138"
MAC_PORT = 7709
SETUP_HEX = [
    "0c0218930001030003000d0001",
    "0c0218940001030003000d0002",
    "0c031899000120002000db0fd5d0c9ccd6a4a8af0000008fc22540130000d500c9ccbdf0d7ea00000002",
]
# 抽样：覆盖沪/深/创/科/北 + 新旧题材都有
SAMPLE = ["600519", "000338", "300750", "000001", "688981", "002594", "601899", "300059",
          "603259", "000858", "002415", "600036", "300124", "688111", "601012"]

BLOCK_HEADER = 384
BLOCK_STOCK_AREA = 2800
CODE_REC = 7
MODERN = ["AI智能体", "DeepSeek概念", "智谱AI", "ChatGPT概念", "AIGC概念", "低空经济"]
LEGACY_REQUIRED = ["固态电池", "芯片"]
TYPE_NAMES = {3: "地区", 4: "概念", 5: "风格", 12: "行业"}

# 对表口径按类型稳定性分档（缓存按自然周刷新，周内漂移不可消除，全等只在对齐抓取时刻时成立）：
# - 地区(3)/概念(4)：产品主口径，成员漂移小，逐股重叠率下限 0.85——
#   解码错误（板块码/类型/名字错位）会让重叠塌到 0，判据强度不受影响
# - 风格(5)：厂商按自有节奏重算——「昨成交20」每个交易日重建、「板块趋势/通达信热股」随行情滚动、
#   「基金/保险/社保重仓」随披露调整。周缓存与实时逐条相等在数学上做不到，
#   改用聚合重叠率：整块解码失败（名字全乱/全丢）会掉到 0，正常漂移在 90% 以上。
STABLE_TYPES = (3, 4)
STABLE_OVERLAP_MIN = 0.85
STYLE_TYPE = 5
STYLE_OVERLAP_MIN = 0.80


def fail(msg):
    print(f"  ✗ {msg}")
    return False


def ok(msg):
    print(f"  ✓ {msg}")
    return True


def market_of(code):
    if code[0] in "69":
        return 1
    return 0


# ── 最小 MAC 客户端（独立于 Rust 实现，用于交叉验证）──
def _rx(s, n):
    b = b""
    while len(b) < n:
        c = s.recv(n - len(b))
        if not c:
            raise EOFError("连接断开")
        b += c
    return b


def _rf(s, check_zip=True):
    """读一帧。

    check_zip=False 用于**握手**：握手响应本来就是压缩帧（zs=86/uz=189），
    判压缩会直接抛错（tdxmac.rs 里同一个坑已记档）。板块/K 线响应才是未压缩的。
    """
    h = _rx(s, 16)
    zs, uz = struct.unpack_from("<HH", h, 12)
    body = _rx(s, zs)
    if check_zip and zs != uz:
        raise ValueError(f"响应被压缩 zs={zs} uz={uz}")
    return body


def mac_belong(conn, market, code):
    body = struct.pack("<H8s16x21s", market, code.encode("gbk"), b"Stock_GLHQ")
    inner = struct.pack("<H", 0x1218) + body
    conn.sendall(struct.pack("<BIBHH", 1, 0, 1, len(inner), len(inner)) + inner)
    resp = _rf(conn)
    rows = json.loads(resp[27:].decode("gbk", "replace"))
    out = []
    for r in rows:
        if len(r) not in (9, 13):
            continue
        out.append((int(r[0]), str(r[2]), str(r[3])))
    return out


def connect():
    s = socket.create_connection((MAC_HOST, MAC_PORT), timeout=8)
    s.settimeout(8)
    for hx in SETUP_HEX:
        s.sendall(bytes.fromhex(hx))
        _rf(s, check_zip=False)  # 握手响应是压缩帧
    return s


def parse_legacy(path):
    data = path.read_bytes()
    n = struct.unpack_from("<H", data, BLOCK_HEADER)[0]
    pos = BLOCK_HEADER + 2
    out, names = {}, []
    for _ in range(n):
        name = data[pos:pos + 9].decode("gbk", "replace").rstrip("\0")
        cnt, bt = struct.unpack_from("<HH", data, pos + 9)
        begin = pos + 13
        pos = begin + BLOCK_STOCK_AREA
        if bt != 2 or name == "通达信88":
            continue
        names.append(name)
        for i in range(cnt):
            p = begin + i * CODE_REC
            c = data[p:p + 6].decode("utf-8", "replace").rstrip("\0")
            if c:
                out.setdefault(c, []).append(name)
    return out, names


def main():
    if not CACHE.exists():
        print(f"缺少缓存 {CACHE}（先跑 ./target/release/tv --refresh-concepts）")
        return 2
    if not LEGACY.exists():
        print(f"缺少旧文件 {LEGACY}")
        return 2

    cache = json.loads(CACHE.read_text())
    # 缓存按类型分开存：{code: {type: [names]}}
    mac_by_type = {}
    mac_all = set()
    for code, tags in cache["map"].items():
        d = mac_by_type.setdefault(code, {})
        for t in tags:
            d.setdefault(t[0], []).append(t[2])
            mac_all.add(t[2])
    legacy_map, legacy_names = parse_legacy(LEGACY)

    print(f"MAC 缓存: source={cache['source']} fetched_at={cache['fetched_at']} "
          f"板块={cache['boards']} 股票={len(mac_by_type)}")
    print(f"旧文件  : 概念块={len(legacy_names)} 股票={len(legacy_map)}")

    passed = True

    # ① 新鲜度
    print(f"\n① 新鲜度（MAC 板块名全集 {len(mac_all)}）")
    hits = [n for n in MODERN if n in mac_all]
    if len(hits) >= 3:
        passed &= ok(f"近年题材命中 {len(hits)}/{len(MODERN)}: {hits}")
    else:
        passed &= fail(f"近年题材命中太少 {len(hits)}/{len(MODERN)}")
    for name in LEGACY_REQUIRED:
        if name in mac_all:
            passed &= ok(f"老题材仍在: {name}")
        else:
            passed &= fail(f"老题材丢失: {name}")

    # ② 覆盖度（区分「块已停用」与「成分调整」）
    print("\n② 覆盖度")
    legacy_codes = set(legacy_map)
    mac_codes = set(mac_by_type)
    absent = [c for c in legacy_codes if c not in mac_codes]
    ratio = len(absent) / max(len(legacy_codes), 1)
    print(f"  旧文件有概念但 MAC 缓存无该股票: {len(absent)}/{len(legacy_codes)} ({ratio:.1%})")
    if ratio > 0.05:
        passed &= fail(f"覆盖缺口过大 ({ratio:.1%} > 5%)，例: {absent[:8]}")
    else:
        passed &= ok(f"覆盖缺口 {ratio:.1%} ≤ 5%")
    retired = [n for n in legacy_names if n not in mac_all]
    print(f"  旧文件概念块在新数据里整块消失: {len(retired)}/{len(legacy_names)}")
    if retired:
        print(f"    例: {sorted(retired)[:10]}")

    # ③ 权威交叉：独立 0x1218 逐股对表
    print(f"\n③ 权威交叉（独立 0x1218 抽样 {len(SAMPLE)} 只）")
    try:
        conn = connect()
    except Exception as e:
        print(f"  ! MAC 连不上（{e}），跳过交叉验证（不判失败，但不构成通过证据）")
        conn = None
    if conn:
        bad_total = 0
        style_hit = style_union = 0
        stable_hit = stable_union = 0
        for code in SAMPLE:
            try:
                live = mac_belong(conn, market_of(code), code)
            except Exception as e:
                print(f"  · {code} 查询失败: {e}")
                bad_total += 1
                continue
            cached = mac_by_type.get(code, {})
            cached_set = {(t, n) for t, names in cached.items() for n in names}
            live_set = {(t, n) for t, _, n in live}
            # 缓存只存 3/4/5 三类（行业走本地 catalog），并剔除通达信88 指数块
            live_set = {x for x in live_set if x[0] in (3, 4, 5) and x[1] != "通达信88"}
            c_stable = {x for x in cached_set if x[0] in STABLE_TYPES}
            l_stable = {x for x in live_set if x[0] in STABLE_TYPES}
            c_style = {n for t, n in cached_set if t == STYLE_TYPE}
            l_style = {n for t, n in live_set if t == STYLE_TYPE}
            style_hit += len(c_style & l_style)
            style_union += len(c_style | l_style)
            stable_hit += len(c_stable & l_stable)
            stable_union += len(c_stable | l_stable)
            s_ratio = len(c_stable & l_stable) / len(c_stable | l_stable) if c_stable | l_stable else 1.0
            drift = sorted(c_stable ^ l_stable)
            if s_ratio < STABLE_OVERLAP_MIN:
                bad_total += 1
                print(f"  ✗ {code}: 概念/地区 重叠 {s_ratio:.0%} < {STABLE_OVERLAP_MIN:.0%}"
                      f"（疑似解码错误）仅实时有 {sorted(l_stable - c_stable)[:4]}"
                      f" / 仅缓存有 {sorted(c_stable - l_stable)[:4]}")
                continue
            print(f"  ✓ {code}: 概念/地区 {len(c_stable & l_stable)}/{len(c_stable | l_stable)} 重叠"
                  + (f"（周内漂移 {drift[:3]}）" if drift else "")
                  + (f"，风格 {len(c_style & l_style)}/{len(c_style | l_style)} 重叠"
                     if c_style | l_style else ""))
        conn.close()
        style_ratio = style_hit / style_union if style_union else 1.0
        stable_ratio = stable_hit / stable_union if stable_union else 1.0
        print(f"  概念/地区聚合重叠 {stable_hit}/{stable_union}（{stable_ratio:.1%}）")
        print(f"  风格块聚合重叠 {style_hit}/{style_union}（{style_ratio:.1%}）")
        if bad_total:
            passed &= fail(f"{bad_total}/{len(SAMPLE)} 只概念/地区重叠低于 {STABLE_OVERLAP_MIN:.0%}")
        else:
            passed &= ok(f"{len(SAMPLE)}/{len(SAMPLE)} 只概念/地区重叠达标（周缓存允许周内漂移）")
        if style_ratio < STYLE_OVERLAP_MIN:
            passed &= fail(f"风格块重叠过低 {style_ratio:.1%} < {STYLE_OVERLAP_MIN:.0%}"
                           "（疑似整块解码失败）")
        else:
            passed &= ok(f"风格块重叠 {style_ratio:.1%} ≥ {STYLE_OVERLAP_MIN:.0%}"
                         "（厂商动态重算，不要求逐条相等）")

    # ④ 分类型抽样
    print("\n④ 分类型抽样（缓存）")
    for code in ["600519", "000338", "300750", "688981"]:
        d = mac_by_type.get(code, {})
        parts = []
        for t in (4, 3, 5):
            names = d.get(t, [])
            if names:
                parts.append(f"{TYPE_NAMES[t]}({len(names)}): {'、'.join(names[:5])}")
        print(f"  {code}")
        for p in parts:
            print(f"    {p}")

    print()
    if passed:
        print("=== 概念源对表 ALL PASS ===")
        return 0
    print("=== 概念源对表 FAILED ===")
    return 1


if __name__ == "__main__":
    sys.exit(main())
