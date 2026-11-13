#!/usr/bin/env python3
"""门禁的源码文本视图：剥掉 Rust 注释、保留字符串字面量。

为什么门禁需要它：接线门问的是「代码里有没有这么调用」，而注释里出现
`kline_tencent_min(code`、`// 禁止 kline_em(` 只是文档描述。直接对原文做
find/grep 会让注释把门翻红（误红）或撑绿（误绿：注释提到某调用就当它接了）。
剥完注释再匹配，两个方向都只由真实代码决定。

剥不干净时后果是「找不到模式」⇒ 调用方一律显式报错，不会静默放行。
"""
import sys


def strip_comments(text: str) -> str:
    out = []
    i, n = 0, len(text)
    in_str = False
    block = 0
    while i < n:
        c = text[i]
        d = text[i + 1] if i + 1 < n else ""
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if block:
            if c == "/" and d == "*":
                block += 1
                i += 2
                continue
            if c == "*" and d == "/":
                block -= 1
                i += 2
                continue
            if c == "\n":
                out.append(c)  # 行号对齐，便于失败时按行定位
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if c == "/" and d == "/":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if c == "/" and d == "*":
            block = 1
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def code_only(paths) -> str:
    """按给定顺序拼接各文件的「仅代码」文本。"""
    return "".join(strip_comments(open(p, encoding="utf-8").read()) for p in paths)


def first_in_code(paths, pat):
    """返回 (文件, 偏移)；模式只存在于注释里时视为未命中（-1）。"""
    for p in paths:
        i = strip_comments(open(p, encoding="utf-8").read()).find(pat)
        if i >= 0:
            return p, i
    return None, -1


def _selftest() -> int:
    """剥注释器的正确性撑着整族门禁，所以把证据留在仓库里而不是滚动缓冲里。"""
    src = 'a("http://x"); // kline_tencent_min(code\n//! mac_daily_rows(&mkt, &cd, want)\n' \
          '/* 嵌套 /* 块 */ 里提 kline_em( */\nb("x /* y */ z");\nlet s = "含引号 \\" 与 // 串";\n'
    got = strip_comments(src)
    for gone in ("kline_tencent_min(code", "mac_daily_rows(&mkt", "kline_em(", "嵌套", "里提"):
        assert gone not in got, f"注释没剥干净: {gone} -> {got!r}"
    for kept in ("http://x", "/* y */", '含引号 \\" 与 // 串', "a(", "b(", "let s ="):
        assert kept in got, f"代码/字符串被误剥: {kept} -> {got!r}"
    assert got.count("\n") == src.count("\n"), "剥完行数变了，失败时无法按行定位"
    return 0


if __name__ == "__main__":
    args = sys.argv[1:]
    mode, files = (args[0], args[1:]) if args else ("", [])
    if mode == "selftest":
        sys.exit(_selftest())
    if mode == "code" and files:
        sys.stdout.write(code_only(files))
    elif mode == "fn" and files:
        # 名字存在性门：要求 `fn NAME(` 完整形态（改名成 NAME_x 不算还在），且只认代码
        import re

        ok = re.search(r"\bfn\s+%s\s*\(" % re.escape(files[0]), code_only(files[1:]))
        sys.exit(0 if ok else 1)
    else:
        print("用法: gate_scan.py selftest | code <文件...> | fn <名字> <文件...>", file=sys.stderr)
        sys.exit(2)
