# pypinyin 数据文件（vendored，data-only）

这个目录只保留 [pypinyin](https://github.com/mozillazg/python-pinyin) 0.55.0 的
**两份词典数据**，Python 源码/包元数据/可执行文件已全部移除：

| 文件 | 内容 |
|---|---|
| `pinyin_dict.json` | 41,923 个字符 → 带声调读音（键是十进制码位字符串） |
| `phrases_dict.json` | 47,111 条词语 → 逐字读音（多音字消歧） |

## 为什么留着

`tv --gen-symbols` 要给 5,579 个股票名生成拼音首字母/全拼。pypinyin 的词语词典
是必须的：只按单字转换会让 108 个名字（1.94%）出错，例如
`平安银行 → pinganyinxing`、`中国长城 → zhongguozhangcheng`。

## 算法在哪里

Rust 重写在 `src/pinyin.rs`，逐条对齐 pypinyin 的
`simpleseg`（汉字段/非汉字段切分）+ `mmseg`（正向最大匹配，严格模式）+
`lazy_pinyin` 的 `NORMAL` / `FIRST_LETTER` 风格。**不运行任何 Python 代码**。

验收口径：`cargo test` 里的 parity 用例拿本地 `data/symbols.json`（旧版 pypinyin
生成的产物）做逐行比对；当前 5,579 行 `py` / `pyfull` 全部一致。

## 许可

MIT，版权归 pypinyin 作者（`Copyright (c) 2016 mozillazg, 闲耘 <hotoo.cn@gmail.com>`）。
许可正文见同目录 `LICENSE.txt`。词典数据的再分发遵循同一许可。
