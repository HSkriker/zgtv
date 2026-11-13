# tv · 本地看图终端

TradingView 观感的 A 股看图工具。K 线 + 成交量 + 缠论，数据默认直读本机通达信文件；
盘中可走实时链路（任意实时请求最短间隔 300s），`TV_LIVE=0` 退回纯离线。

## 特性

- 10 档周期：日 / 周 / 月 / 季 / 年 + 1 / 5 / 15 / 30 / 60 分钟，日线 10 年深度
- 缠论：笔 / 中枢 / 买卖点，支持全市场买点选股；板块与指数同规则
- 指标：MA / EMA / BOLL / MACD / RSI（后端 ta-lib 计算，齿轮面板可调参）
- 61 个蜡烛形态标记（TA-Lib CDL 全量）、SMC/PA2 结构层、成交量分布（VP）
- 画线 7 种、价格预警、区间测量
- 行业热度 / 宽基指数 / 板块总览：128 个二级行业轮动图，单击看 K 线、双击摊成分股
- 前端零构建（原生 JS + 本地 lightweight-charts），不引 CDN / npm

## 跑起来

```bash
./run.sh          # 首次自动 cargo build --release，然后监听 8790
# 浏览器打开 http://127.0.0.1:8790
```

前置：Rust 工具链 + 通达信客户端。Windows 可双击 `start-tv.bat` / `stop-tv.bat`
（脚本路径由 bat 按自身所在目录换算，仓库放哪个盘都不用改）。

> **仓库不含行情与凭据**：`data/` 下的行情快照、`eq/fixtures`、`eq/golden`、`*.http`
> 录播响应一律不入库（清单见 `.gitignore`）。本机没有通达信时 `./run.sh` 会回落
> `data/vipdoc` 快照（先 `TV_SYNC=copy ./run.sh` 建一份）。API key 只从环境变量
> （`HITHINK_FINANCE_API_KEY` 等）或本机凭据文件读，代码与历史里都没有。

## 数据源

| 模式 | 命令 | 说明 |
|---|---|---|
| direct（默认） | `./run.sh` | 直读通达信目录，零拷贝 |
| copy | `TV_SYNC=copy ./run.sh` | 增量同步进 `data/vipdoc` 后读快照（离线机 / 怕盘中写盘撞半截文件） |
| 离线 | `TV_LIVE=0 ./run.sh` | 一个实时请求都不发 |

## 环境变量

`run.sh` 每次启动都会导出这几项，正常使用不用管；手跑二进制时才需要自己设：

| 变量 | 值 |
|---|---|
| `ZG_ROOT` | 仓库根目录 |
| `ZG_VIPDOC` | `<通达信安装目录>/vipdoc`（copy 模式：`data/vipdoc`）|
| `ZG_GBBQ` | `<通达信安装目录>/T0002/hq_cache/gbbq`（copy 模式：`data/tdx/gbbq`）|
| `ZG_XDXR_CACHE` | `data/tdx_xdxr` |

## 目录

```
src/        后端（Rust）：bars / board / chan / overview / scan / heat / drawings / cache / tags
            + 一次性任务：--gen-symbols（标的表）、--fetch-stocks（股票列表）、--build-catalog（行业目录）
crates/     内嵌的 zg-core / zg-chan / zg-market / zg-api（2026-11-01 整包原样拷贝）
web/        前端（零构建，原生 JS + 本地 lightweight-charts）
vendor/     lightweight-charts 5.2.1（本地文件，不用 CDN）
tools/      sync-data.sh（行情快照同步）、build-catalog.sh（行业目录重生成）、parity.py（对表）、
            final_check.sh（终检）、tvctl.sh（启停）
modules/    industry_catalog.json（行业目录）+ market/vendor（pypinyin 的词典 JSON，镜像 zg 布局）
data/       运行期数据（market/ 缓存、symbols.json、drawings.json）+ 可选的行情快照
            （vipdoc/tdx/tdx_xdxr，拷贝模式下才用到）—— 一律不入库，只留本机；
            clone 后 stocks.json / symbols.json 由 run.sh 自动 bootstrap（无网机器手动放 stocks.json）
target/     编译产物；验证脚本与对拍基准（smc-fork/ = PA2 算法源 smc.py + MIT LICENSE，只作基准不运行）
```

## 文档

| 文件 | 内容 |
|---|---|
| `docs/技术说明.md` | 功能口径、实时链路、实测证据与踩坑原因 |
| `docs/模块来源与使用说明.md` | 各层代码从哪来、启动链路、工具用法 |
| `DOWNLOAD.md` | 前端库版本与校验值 |
| `THIRD-PARTY-NOTICES.md` | 第三方许可汇总 |

## 自检

```bash
./tools/final_check.sh      # 终检（服务需在跑）
cargo test --workspace      # 单测：缺夹具 / 金标的用例自动 SKIP，不会红
python3 tools/parity.py     # 与外部 zg 对表（TV_ZG_BIN 未设则 SKIP）
```

## 许可

MIT（`LICENSE`）。第三方代码与移植上游署名见 `THIRD-PARTY-NOTICES.md`。

## 绝不做

- 不做 WebSocket / SSE（HTTP 轮询），不做 Tick / 五档 / Level-2
- 不引 CDN、不引 npm、不引 `charting_library`
- 构建与运行不依赖 `gs/zg`（四层 crate 已内嵌 `crates/`）
