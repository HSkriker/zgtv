# 第三方版权声明（THIRD-PARTY-NOTICES）

本文件只登记**随本仓库分发的第三方代码**，以及**被移植（port）的上游项目**。
按 crates.io 拉的纯依赖不在这里逐条列（见文末「依赖许可汇总」）。

---

## 1. 随仓库分发的第三方代码

### 1.1 TradingView Lightweight Charts™ 5.2.1 — Apache-2.0

- 位置：`vendor/lightweight-charts.standalone.production.js`、
  `vendor/lightweight-charts.production.mjs`、`vendor/typings.d.ts`、
  `vendor/lightweight-charts.package.json`、原始包 `vendor/lightweight-charts-5.2.1.tgz`
- 许可：Apache License 2.0；版权头见文件顶部注释与 `vendor/NOTICE`
  （`Copyright (с) 2025 TradingView, Inc. https://www.tradingview.com/`）
- **未修改**：`vendor/lightweight-charts.production.mjs` 的 sha256 与 `DOWNLOAD.md`
  记录的官方值逐字节一致；`.tgz` 是 npm 原始包。前端通过 `web/` 里的
  `<script>`/`import` 引用它，不重新打包、不上传 CDN。

### 1.2 pypinyin 0.55.0 — MIT（data-only）

- 位置：`modules/market/vendor/pypinyin/` —— **只保留两份词典数据**：
  `pinyin_dict.json`（41,923 字）与 `phrases_dict.json`（47,111 词）。
  Python 源码、`.pyi`、`bin/pypinyin.exe`、`pypinyin-0.55.0.dist-info/`
  均已从仓库移除（2026-11-06）。
- 许可正文：`modules/market/vendor/pypinyin/LICENSE.txt`
  （The MIT License (MIT)，`Copyright (c) 2016 mozillazg, 闲耘 <hotoo.cn@gmail.com>`，
  正文取自原 wheel 的 `dist-info/licenses/LICENSE.txt`）
- 用途：给 `tv --gen-symbols` 提供多音字消歧词典（股票名 → 拼音首字母/全拼）。
  转换算法已在 `src/pinyin.rs` 用 Rust 重写（见 §2.9），运行期**不执行任何 Python 代码**。

---

## 2. 被移植的上游项目

移植＝照着上游的算法/结构重写，**不是**逐字节复制源码；下列条目说明「哪些思路与
口径来自上游、哪些是本仓库自己改的」，方便回溯与差异审计。

### 2.1 smart-money-concepts（Hskriker fork 0.0.27）— MIT → `web/js/pa2.js`

- **上游链**（三级，PA/SMC 算法层的完整出处）：
  1. 原作 `smartmoneyconcepts` —— 作者 **Joshua Attridge**，
     仓库 <https://github.com/joshyattridge/smartmoneyconcepts>，MIT，
     LICENSE 写 `Copyright (c) 2020 NeuralNine`（NeuralNine 是同一作者的账号名，
     证据：本地 `setup.py` 的 `author="Joshua Attridge"` 与 `url=` 指向该仓库，
     与包内 `LICENSE` 同目录）。
  2. fork **HSkriker/smart-money-concepts** <https://github.com/HSkriker/smart-money-concepts>
     版本 `0.0.27`（`smc.py:53 __version__ = "0.0.27"`），修掉上游三处 numpy 负索引回绕。
  3. tv 的 JS 移植 `web/js/pa2.js`；中间还有一层 zg 侧的 `pa2-layer.js`
     （基于上游 **0.0.25**，`crates`/`web` 内已无该文件，只在注释里作对照）。
- 本地副本在 `target/smc-fork/`（构建产物目录，**不入库**），故 MIT 正文收在本文件 §4。
- 移植范围：`swing_highs_lows / bos_choch / fvg / ob / liquidity / retracements /
  previous_high_low` 的判定口径逐段对齐上游。
- 本仓库的偏离（有意为之，代码内已注释）：
  - `DEFAULT_SWING = 10`（上游默认 50 是外汇/分钟级口径，日线过稀）；
  - 新增 `MIN_GAP_ATR = 0.15 × ATR(60)` 窄 gap FVG 过滤（上游无此过滤）；
  - numpy 负索引回绕：上游 fork 已改为显式取 0，本移植沿用 fork 口径。
- 渲染层 `web/js/pa2-primitive.js` 的视觉口径对齐 zg `pa2-layer.js`，唯一有意分歧是
  填色按 A 股红涨绿跌习惯对调（2026-11-01 用户决定），算法不参与。

### 2.2 TradingView plugin-examples → `web/js/alerts-primitive.js`

- 上游：`tradingview/plugin-examples` 的 `user-price-alerts` 示例（Apache-2.0）。
- 移植的是「在 price scale 上挂一个可点的用户标记」这一 primitive 结构，
  纯前端实现，无后端。尺寸/命中常量在本文件顶部重写过。

### 2.3 TradingView plugin-examples → `web/js/band-primitive.js`

- 上游：同上仓库的 `bands-indicator` 示例（Apache-2.0）。
- 只移植「上下轨之间铺一层半透明多边形」的渲染思路。带宽仍由两条 `LineSeries`
  绘制，因此不需要上游的 `autoscaleInfo` 兜底。

### 2.4 zg 四层 crate → `crates/zg-{core,chan,market,api}`

- 来源：内嵌的 zg 项目（2026-11-01 整包 `cp -a` 拷入，包名未改）。这些 crate 的
  模块头自述为 Python 实现（`zgcore` / `czsc_bridge` / `zgapp` / `market` /
  `monitor_app`）的 Rust 端口，其中缠论层再上游为 `czsc`。
- 详见 `docs/模块来源与使用说明.md` 第一部分。

### 2.5 easy-tdx-plus（MIT）→ `crates/zg-core/src/gbbq_keys.bin`

- 4176 字节的 XOR 表，**逐字节等于**本机 `easy_tdx/offline/gbbq.py::_HEX_KEY`
  （实测：Python 侧解 hex 后与 `.bin` 全等，2026-11-03）。
- 许可链：easy-tdx-plus 为 MIT（`Copyright (c) 2025-present easy-tdx contributors`）；
  其 `NOTICE` 自陈「The gbbq XOR decryption key is directly ported from pytdx」
  （rainx/pytdx，pytdx 自身许可未在本机取证，引用时以其仓库声明为准）。
- **这不是凭据**：它是通达信 `gbbq`（送转/分红除权）文件的公开解码表，任何人可从上游
  源码得到；改动它会改变解码结果。这是仓库里唯一入库的二进制数据文件。
- 注：`crates/zg-core/src/gbbq.rs:6` 原写「上游 MIT/Apache 双许可」，Apache 部分
  无据（easy-tdx-plus 只有 MIT），已按实测更正。

### 2.6 czsc（缠论判据的权威上游）— MIT

- **上游仓库**：<https://github.com/waditu/czsc>（Python 本体 + 仓内 Rust 端口同一个仓库）。
  本机走的六个 crate 的 `Cargo.toml` 里 `repository` 字段全部指向该地址（实测
  `~/.cargo/registry/src/*/czsc-{core,signals,ta,utils,derive,signal-macros}-1.0.2/Cargo.toml`），
  `license = "MIT"`，版本 `1.0.2`。
- **tv 用到它的两条路径**：
  1. `crates/zg-chan` 依赖 `czsc-signals`/`czsc-utils` =1.0.2 —— 分型/笔/中枢的判据
     完全来自上游，crate 内 `lib.rs:7-9` 明写「**不得自建判据、不得"优化"上游语义；
     所有口径以 Python oracle 为准**」；
  2. `src/chan.rs` 直调 `czsc_core` 只为季线 `Freq::S`（`zg-chan` 的 freq 表只到
     D/W/M/F*），版本用 `czsc-core = "=1.0.2"` 钉死（`Cargo.toml:29-30`）。
- **Python oracle**：对拍基准来自 Python 包 `czsc` 1.0.2 本体（`eq/golden` 的
  `signals_prod102.json` / `parity_params.json` 即由它导出，只含信号名与参数、不含行情）。
- 传递依赖 `polars 0.52.0` 是 `czsc-core` 带进来的，不是 tv 的直接依赖。
- 上游 LICENSE 文本未随 crates.io 包分发（包内无 `LICENSE` 文件），本条目按 manifest 的
  `license = "MIT"` 声明署名；需要正文时见上游仓库。

### 2.7 TA-Lib（技术指标与蜡烛形态引擎）— BSD-3-Clause

- **上游**：<https://github.com/TA-Lib/ta-lib>，`ta-lib` 0.8.3 是 C 版 TA-Lib 的
  **官方 Rust 端口**（manifest 描述：verified against the C reference），
  `license = "BSD-3-Clause"`，版权行 `Copyright (c) 1999-2026, Mario Fortier`
  （正文取自本机 registry 缓存内包自带的 LICENSE）。
- **tv 用法**：`src/ind.rs` 的 MA/EMA/BOLL/MACD/RSI 与 `crates/zg-market/src/candles.rs`
  的蜡烛形态（`ta-lib` 与 `ta-lib-dispatch` 0.8.3，`build = false`，纯 Rust、不链接 C 库）。
- **本仓库的偏离**：MACD 柱**乘 2** 是 tv 为对齐通达信/同花顺/东财口径自行加的，
  与 TA-Lib 默认输出不同（上游柱 = DIF − DEA，tv 柱 = 2×(DIF − DEA)）。
- 注意与 Python 侧 `TA-Lib 0.8.x` 的已知破坏性变更无关：本项目用 Rust crate，
  BBANDS 周期与 MA 类型都显式传参，不吃默认值。

### 2.8 其它算法级参考

- `web/js/volume-profile.js`：成交分布口径参考 zg 与 MT5 的 Volume Profile 指标。- `src/tdxmac.rs`：MAC（扩展行情）二进制协议。`tdxmac.rs:7` 自陈「协议要点
  **字节级复现自 easy-tdx-plus 的 MAC 实现**，并手工按字节对真实服务器验证过」；
  主机表 `MAC_HOSTS`（`tdxmac.rs:46-53`）取自 easy-tdx-plus 的同名常量，三条握手命令
  `SETUP_HEX` 的文件注释记为来自 pytdx 的 setup1/2/3。许可链见 §2.5
  （easy-tdx-plus 为 MIT，其 NOTICE 又自陈移植自 pytdx / xmtdx）。
  定位是「按公开协议重写 + 自行对服务器验证」，不是复制上游源文件，但也不声称零上游。
- `crates/zg-core/src/gbbq.rs` 的偏移表（`0x44`/`0x448`/`0x48`/`0x848`/`0xC48`）
  逐行复刻 easy-tdx-plus `offline/gbbq.py` 的 `read_gbbq`（同一 MIT 上游，见 §2.5）。

### 2.9 pypinyin 的拼音算法 — MIT → `src/pinyin.rs`

- 上游：pypinyin 0.55.0（Python，MIT）。tv 侧只保留它的两份词典 JSON（§1.2），
  算法按以下三个模块逐条重写为 Rust：
  `pypinyin/seg/simpleseg.py`（按 RE_HANS 切汉字段/非汉字段）、
  `pypinyin/seg/mmseg.py`（正向最大匹配 + 严格模式：匹配不到词就单字）、
  `pypinyin/converter.py` + `style/others.py`（`lazy_pinyin` 的 NORMAL / FIRST_LETTER）。
- 复刻范围：`heteronym=False`、`errors='default'`、`strict=True`；含 pypinyin
  的 `PHONETIC_SYMBOL_DICT` 声调剥离表（含 `m̄`/`ḿ`/`ê̄` 等双码位符号）与
  `RE_HANS` 的全部 Unicode 区段。
- 验收：旧 `data/symbols.json`（pypinyin 生成）5,579 行 `py`/`pyfull` 逐字节一致；
  新输出与原 Python 脚本**整文件字节相同**。`cargo test` 里的 parity 用例持续看守。

---

## 3. 依赖许可汇总

`cargo tree --workspace -e normal -f '{p}|{l}'` 聚合结果：全部依赖的许可表达式里
**没有 GPL / LGPL / AGPL / MPL**（即无 copyleft 传染），因此本仓库整体按 **MIT**
（见根 `LICENSE`）分发与这些许可互不冲突；Apache-2.0 / BSD-3 / CDLA 组件的原文与
NOTICE 随 `vendor/`、registry 缓存保留。主要直接依赖：

| crate | 版本 | 许可 | 用途 |
| --- | --- | --- | --- |
| `czsc-core` | =1.0.2 | MIT | 季线缠论（`Freq::S`，zg-chan 的表只到 D/W/M/F*）；上游 <https://github.com/waditu/czsc>，见 §2.6 |
| `czsc-signals` / `czsc-ta` / `czsc-utils` / `czsc-derive` / `czsc-signal-macros` | 1.0.2 | MIT | 同一上游仓库，随 `czsc-core` 传递 |
| `ta-lib` / `ta-lib-dispatch` | 0.8.3 | BSD-3-Clause（© 1999-2026 Mario Fortier） | 蜡烛形态与技术指标引擎，官方 Rust 端口，见 §2.7 |
| `serde_json` | 1 | MIT OR Apache-2.0 | JSON（`preserve_order`） |
| `chrono` | 0.4 | MIT OR Apache-2.0 | 日期 |
| `encoding_rs` | 0.8 | MIT OR Apache-2.0 | `block_gn.dat` 的 GBK 板块名 |
| `webpki-roots` | 0.26.11 / 1.0.11 | CDLA-Permissive-2.0 | HTTPS 根证书表 |

> 注意：上表的许可取自各 crate manifest 的 **license 表达式**，未逐份比对 LICENSE
> 正文（表达式与正文不一致的情况在上游很少见，但这里如实说明校验方式）。
> 传递依赖里的 `r-efi` 为 `MIT OR Apache-2.0 OR CC0-1.0` 三重许可（UEFI 变量常量），
> `icu_*_data` 为 Unicode-3.0（字体/数据许可，非 copyleft）。

---

## 4. MIT License（上游 smart-money-concepts 正文）

```
MIT License

Copyright (c) 2020 NeuralNine

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Apache-2.0 全文见 `vendor/LICENSE`；BSD-3-Clause / CDLA-Permissive-2.0 等依赖正文
由 Cargo.lock 对应的 registry 缓存提供，构建时 `cargo vendor`/离线 registry 即可取。
