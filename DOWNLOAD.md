# 源码下载清单（2026-10-30）

只下了官方**开源**部分。全部为本地离线使用，运行时不访问任何 CDN。

## 一、已下载

### 1. 运行库 `vendor/`

| 文件 | 大小 | 用途 |
|---|---:|---|
| `lightweight-charts.standalone.production.js` | 194 KB | 浏览器 `<script>` 直接加载，全局变量 `LightweightCharts` |
| `lightweight-charts.production.mjs` | 185 KB | 以后若加打包步骤用（ESM） |
| `typings.d.ts` | 153 KB | 类型提示 / IDE 补全 |
| `LICENSE` | 12 KB | Apache-2.0，**必须随代码保留** |
| `NOTICE` | 101 B | TradingView 署名，**必须随代码保留** |
| `lightweight-charts.package.json` | 1.4 KB | 版本元数据（5.2.1） |
| `lightweight-charts-5.2.1.tgz` | 603 KB | 原始 npm 包，留作溯源 |

来源：`https://registry.npmjs.org/lightweight-charts/-/lightweight-charts-5.2.1.tgz`

### 2. 官方参考源码 `reference/lwc-src/`（不进 git）

稀疏克隆，只取 3 个目录：

| 目录 | 内容 |
|---|---|
| `indicator-examples/` | 11 个官方指标实现（moving-average / momentum / average-price / percent-change / correlation / spread / ratio / sum / product / median-price / weighted-close） |
| `plugin-examples/` | 官方示例插件：**trend-line**、**rectangle-drawing-tool**、user-price-lines、user-price-alerts、partial-price-line、volume-profile、session-highlighting、bands-indicator、overlay-price-scale 等 29 个 |
| `packages/` | 13 个官方维护插件包源码（`@tradingview/lwc-plugin-*@1.0.0`），含 `vertical-line`、`image-watermark`、`pretty-histogram-series`、`rounded-candles-series` 等 |

来源：

```bash
git clone --depth 1 --branch plugins-2026-09-16 --filter=blob:none --sparse \
  https://github.com/tradingview/lightweight-charts.git lwc-src
cd lwc-src && git sparse-checkout set indicator-examples plugin-examples packages
```

- commit：`bf1ac13ac434d813b14096b558398aa615211923`
- 该 tag 同时是全部 `@tradingview/lwc-plugin-*@1.0.0` 的发布点，仓库版本 = **5.2.1**（与 vendor 完全同版本，无错配）
- 体积：4.7 MB

## 二、校验值（sha256）

```
e21cc5caa0226ef30bd8549c50b9ef926615f2a4ee6b4e486353477a55f598cf  lightweight-charts.standalone.production.js
faf0924968b1cbb9ea5bb6baa43a27ed6a0802341c07238b72ae869ffebab90c  lightweight-charts.production.mjs
e3d7cd2e718e92c4f61413f1b15aebb39f20c9e987a138b30e575c7843cc8c71  typings.d.ts
70c9d5382506dd184465425c08a99ad9bd6d9ac1313c252968ba0b585e5ef823  LICENSE
f76c6afab94884448f0426e30d6e9d555ca7247894cd3484e477d2f87513036e  NOTICE
64310292298df4dd527494865e5d91d447f8bb397b68de3912c2a6465025ee5e  lightweight-charts-5.2.1.tgz
c6535c33425f81748d8ccbeed8d98fe71dd4a17157d7c04561cac2a2a180abe1  lightweight-charts.package.json
```

## 三、明确没下（不是漏了）

| 仓库 | 原因 |
|---|---|
| `tradingview/charting_library` | 私有仓。官方 FAQ 明文不授权个人自用/研究；仅供公司做公开免费网站 |
| `tradingview/trading_platform` | 同上，且收费 |
| `charting-library-examples` / `charting-library-tutorial` | MIT，但缺 `charting_library` 无法运行；其 datafeed 接口与 Lightweight Charts 不通用 |
| `saveload_backend` | 无 LICENSE 文件（默认保留所有权利），不可抄；结构自己写 |
| `yahoo_datafeed` | 无 LICENSE，且是 Yahoo 演示 fork，与 A 股无关 |

## 四、刷新方式

```bash
# 升级库版本：改下面两处版本号，重下并更新本文件的校验值
cd vendor
curl -LO https://registry.npmjs.org/lightweight-charts/-/lightweight-charts-<VER>.tgz
# 参考源码
cd reference/lwc-src && git fetch --depth 1 origin tag <TAG> && git checkout <TAG>
```
