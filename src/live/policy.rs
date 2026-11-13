//! 降级链与合并语义：本地优先 → 远端白名单（日线 TQ → MAC），复权基兼容门，
//! 盘中/收盘新鲜度与分钟链 cutoff。头文件里的每条口径注释都随代码原样迁移，勿改。

use super::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};
use zg_core::sync::MutexExt;
use zg_market::kbar::KBar;
use crate::bars::resample_qy;
use crate::daymerge::{merge_days, norm_date, MergePolicy};
use super::source_local::{local_day_series, local_min_to_agg};
use super::source_mac::{align_mac_to_local_base, mac_daily_rows, mac_minute_rows};

fn trim_rows(mut rows: Vec<Agg>, count: usize) -> Vec<Agg> {
    if count > 0 && rows.len() > count {
        rows = rows[rows.len() - count..].to_vec();
    }
    rows
}

fn kbars_to_agg(bars: &[KBar]) -> Vec<Agg> {
    bars.iter()
        .map(|b| Agg {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol,
            amount: b.amount,
        })
        .collect()
}

/// 远端日线（只取日线，周/月在本地合并后统一重采样，避免同周双标签）。
///
/// **白名单：TQ → MAC**（两个与本地 gbbq 同基的源）。公开源（同花顺/腾讯/东财/datahub）
/// 已从日线链移除 —— 它们的复权口径与本地不同源，留着只是白跑网络；
/// 与其拿异基源凑一张「看起来更新」的图，不如回落本地并如实标 stale。
/// 兼容门（`remote_mergeable`）保留为最后一道防线。
/// `want` 是日线根数，TQ 侧 count=want+40，调用方须保证 ≤2900（tq.rs 的 MAX_COUNT=3000）。
fn remote_daily_once(code: &str, want: usize, reference: &str, local: &[Agg])
    -> Result<(Vec<Agg>, String), String>
{
    let want = want.clamp(60, 2900);
    let mut errs: Vec<String> = Vec::new();
    // 统一的接受判据（新鲜度 + 与本地复权基兼容）；TQ 与 MAC 都已是 Agg 序列
    let accept_agg = |rows: &[Agg], src: &str| -> bool {
        if rows.is_empty() {
            return false;
        }
        let last = rows.last().map(|b| b.date.as_str()).unwrap_or("");
        if date_lt(last, reference) {
            return false;
        }
        remote_mergeable(local, rows, src)
    };
    let (mkt, cd) = match zg_core::reader::resolve(code, None) {
        Ok(v) => v,
        Err(e) => return Err(format!("解析 {code} 失败: {e}")),
    };

    // ── 白名单：只有 TQ 与 MAC 两个**同基**源能进图 ──
    //
    // 公开源（同花顺 / 腾讯 / 东财 / datahub）各有各的复权口径，不是本地 gbbq 基：
    // - 东财 `fqt=1` 是**减法**前复权，实测 7/7 只股票都被兼容门拒（最旧段占容差 54~890 倍）；
    // - 同花顺 / 腾讯实测中位差 0.04%~0.54%（同花顺本机无 API key，永远跳过）；
    // - datahub 未实测。
    // 留着它们只是白跑一次网络：与其拿异基源凑一张「看起来更新」的图（历史被整体换基、
    // 日 K 与分时图对不上），不如回落到本地并如实标 stale。兼容门仍然保留，
    // 它是「万一白名单里的源也错基」的最后一道防线。
    match zg_market::kline::kline_tq_day(code, "day", want, None) {
        Ok((_, bars)) if !bars.is_empty() => {
            let rows = kbars_to_agg(&bars);
            if accept_agg(&rows, "tq") {
                return Ok((rows, "tq".to_string()));
            }
            warn_once(
                &format!("daily-tq-{code}-{want}"),
                &format!("[live] {code} 日线 TQ：末根陈旧或复权不兼容"),
            );
            errs.push("TQ: 末根陈旧或复权不兼容".into());
        }
        Ok(_) => errs.push("TQ: 返回空".into()),
        Err(e) => errs.push(format!("TQ: {e}")),
    }
    // MAC 日线（不复权 + 本地事件法复权 → 与本地基同源）：TQ 护栏限流/熔断时的同基替代源。
    match mac_daily_rows(&mkt, &cd, want) {
        Ok((rows, host)) if accept_agg(&rows, "mac") => {
            return Ok((rows, format!("mac@{host}")));
        }
        Ok(_) => {
            warn_once(
                &format!("daily-mac-{code}-{want}"),
                &format!("[live] {code} 日线 MAC：末根陈旧或复权不兼容"),
            );
            errs.push("MAC: 末根陈旧或复权不兼容".into())
        }
        Err(e) => {
            warn_once(
                &format!("daily-mac-err-{code}-{want}"),
                &format!("[live] {code} 日线 MAC 取数失败：{e}"),
            );
            errs.push(format!("MAC: {e}"))
        }
    }
    Err(format!("远端日线全部失败或口径不兼容（{}）", errs.join("；")))
}

/// 远端日线：链全失败时若 TQ 在线则等一个护栏窗口（2s）重试一次。
/// 触发场景多为连续切周期把 TQ 的「2s / 3 连发」配额用光，不是真故障；
/// 退化成本地 stale 之前值得等 2s，避免一次误切周期让整张图变旧。
fn remote_daily(code: &str, want: usize, reference: &str, local: &[Agg])
    -> Result<(Vec<Agg>, String), String>
{
    match remote_daily_once(code, want, reference, local) {
        Ok(v) => Ok(v),
        Err(e) => {
            if !zg_market::tq::available() {
                return Err(e);
            }
            std::thread::sleep(std::time::Duration::from_millis(2100));
            remote_daily_once(code, want, reference, local)
                .map_err(|e2| format!("{e}；2s 后重试仍失败（{e2}）"))
        }
    }
}

/// 本地 + 远端日线合并：**本地优先**，远端只补本地**没有**的日期。
///
/// 为什么不是「远端覆盖本地」：远端一次能给 2900 根（≈11.6 年），让它覆盖就等于
/// 把整段历史的复权基换成远端那套 —— 日 K 图与分时图（永远在本地 gbbq 基上）就此错基。
/// 本地优先后，远端只剩「本地末根之后的那几根」会进来；那几根锚在最新价
/// （前复权以末根为锚），与本地同基，接缝天然连续。
/// 远端日线与本地复权基的重叠中位偏差上限。
///
/// **为什么必须比「明显错基」更严**：本地日线/分钟用通达信 gbbq 事件法（乘法前复权），
/// TQ 的 `dividend_type=front` 与它**逐位同基**（实测 250 个交易日偏差 0.0000%）；
/// 其余公开源是另一套复权口径，实测中位偏差 000338 0.53%、600519 0.25%、
/// 000001 0.09%、300750 0.04%（东财 fqt=1 还是**减法**前复权，高价股老历史会出负价）。
/// 0.5% 的基差会让「日 K 图」与「分时图」对不上（同一天两根柱子差半个点），
/// 而 TV 的分钟侧（本地 .lc1 事件法 / MAC 服务端前复权）都在 gbbq 基上 —— 基不齐必须拒。
/// 阈值取 0.1%：挡掉实测到的 0.25%/0.53% 基差，同时放过基差 <0.1% 的源。
/// 拒掉后回落到本地日线并标 `stale`（显式陈旧好过静默错基）。
const REMOTE_ADJ_TOL: f64 = 0.001;

/// 绝对容差下限（1 分 1 毫）。
///
/// 远端多为 2 位小数（MAC 显式 `round2`，TQ/东财源本身就是 2 位），本地 qfq 是 3 位
/// （`chart_bars` 的 `py_round(3)`）。两边各舍一次最多差 1 分；老历史被分红压到 1.5 元级时
/// 这半分钱的相对差就是 0.34%——**高于 0.1% 的相对门限**，纯相对判据会把同基源误杀
/// （实测 600346 vs TQ：未对齐精度时最旧 200 根中位 0.1316%，对齐后 0.0000%）。
/// 绝对下限让「低于一个价格最小变动单位」的基差直接放过：那个量级本来就测不出来。
const REMOTE_ROUND_FLOOR: f64 = 0.011;

/// 远端序列能否并进本地前复权序列。
///
/// 东财 `fqt=1` 是**减法**前复权，长历史高分红股会算出负价（实测 600519 2014 年
/// close=-155）；这种序列与本地 gbbq 乘法复权不同基，混进来会出现 -162 这类负
/// low 和整段错位。规则：任一价格 ≤0 直接拒；重叠段偏差超限也拒。
///
/// **采样要两头都看**：前复权一律以最新价为锚（`apply_qfq` 断言「末根不动」），
/// 所以任意两个复权基在「当下」必然重合，基差只随历史深度放大
/// （实测 600346：东财 vs 本地在最近 200 根中位差 1.61%，在 2013-2015 那段是 228%）。
/// 只看最近 200 根 → 老历史错基漏放；只看最旧 200 根 → 接缝处的新错基漏放。
/// ⇒ 取**最旧 200 根 + 最近 200 根**两段，**任一段**中位超限即拒。
/// 两段各自判定（不是合并成一个中位）：合并会把「局部错基」稀释掉
/// （300 根里错 100 根时，合并中位仍是 0 → 漏放）。
///
/// **比较必须同精度**：两边都 `round2` 到分再比（远端本来就是 2 位小数）。
/// 少这一步，老历史被分红压低的价格会把半分钱舍入放大成 0.3%+ 的假基差。
///
/// 判据用「占容差的比例」而不是纯相对偏差：容差 = max(0.1% × 价格, 1 分 1 毫)，
/// 中位比例 > 1 才拒。绝对下限让仙股（<1 元）不至于连半分钱的噪声都容不下。
pub(crate) fn remote_mergeable(local: &[Agg], remote: &[Agg], source: &str) -> bool {
    if remote
        .iter()
        .any(|b| b.open <= 0.0 || b.high <= 0.0 || b.low <= 0.0 || b.close <= 0.0)
    {
        eprintln!("[live] {source} 日线含非正价格（复权口径不兼容），丢弃远端");
        return false;
    }
    if local.is_empty() {
        return true;
    }
    let closes: HashMap<String, f64> = local
        .iter()
        .map(|l| (norm_date(&l.date), l.close))
        .collect();
    // 重叠段（按远端顺序＝按时间）
    let mut ov: Vec<(&Agg, f64)> = Vec::new();
    for b in remote.iter() {
        if let Some(&c) = closes.get(&norm_date(&b.date)) {
            if c > 0.0 {
                ov.push((b, c));
            }
        }
    }
    if ov.len() < 5 {
        // 重叠太少（新股/短历史）：无从判基，按旧行为放行
        return true;
    }
    let n = ov.len();
    let oldest = &ov[..n.min(200)];
    let newest = &ov[n.saturating_sub(200)..];
    for (seg, name) in [(oldest, "最旧"), (newest, "最近")] {
        if seg.len() < 5 {
            continue;
        }
        // 占容差的比例：容差 = max(相对门限, 绝对下限)
        let mut ratios: Vec<(f64, String, f64)> = seg
            .iter()
            .map(|(b, c)| {
                let dev = (round2(b.close) - round2(*c)).abs();
                let allowed = (REMOTE_ADJ_TOL * c).max(REMOTE_ROUND_FLOOR);
                (dev / allowed, norm_date(&b.date), dev / *c * 100.0)
            })
            .collect();
        let mut vals: Vec<f64> = ratios.iter().map(|r| r.0).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = vals[vals.len() / 2];
        if med > 1.0 {
            // 只报中位数时无法定位是哪一段错基，把最差三根连日期一起打出来。
            ratios.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let top: Vec<String> = ratios
                .iter()
                .take(3)
                .map(|(r, dt, dev)| format!("{dt} 偏差{dev:.3}%（占容差{:.1}倍）", r))
                .collect();
            eprintln!(
                "[live] {source} 日线与本地复权基不符（{name} {} 根重叠中位占容差 {:.2} 倍；最差 {}），丢弃远端",
                seg.len(),
                med,
                top.join(" / ")
            );
            return false;
        }
    }
    true
}

/// 日线合成周/月（桶标签与 zg `synth_wm_from_daily` 同口径）。
fn resample_dwm(rows: &[Agg], period: &str) -> Vec<Agg> {
    let kbars: Vec<KBar> = rows
        .iter()
        .map(|b| KBar {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol,
            amount: b.amount,
        })
        .collect();
    kbars_to_agg(&zg_market::market_data::synth_wm_from_daily(&kbars, period))
}

/// 日 / 周 / 月 / 季 / 年。
///
/// 开启实时：本地全历史 + 远端最近 2900 根日线按日期合并（远端覆盖同日），
/// 周/月/季/年都在合并后的日线上重采样 —— 既保住 10 年+ 历史深度，又拿到当日 bar。
/// 远端末根仍 < 参照日时整体标 `stale`（页面据此变黄）。
pub fn day_series(code: &str, period: &str, lmt: usize) -> Result<Series, String> {
    let p = period.trim().to_lowercase();
    if !enabled() {
        return local_day_series(code, &p, lmt, false);
    }
    if is_qy(&p) {
        let s = day_series(code, "day", 0)?;
        let mut rows = resample_qy(&to_row7(&s.rows), &p);
        if lmt > 0 && rows.len() > lmt {
            rows = rows[rows.len() - lmt..].to_vec();
        }
        let as_of = rows.last().map(|b| b.date.clone()).unwrap_or_default();
        let stale = date_lt(&as_of, &reference_day());
        return Ok(Series { rows, source: s.source, detail: String::new(), as_of, stale });
    }

    let reference = reference_day();
    // 本地全历史（读盘失败不算错：新股/北交所可能只有远端）
    let local = local_day_series(code, "day", 0, false)
        .map(|s| s.rows)
        .unwrap_or_default();
    let want = if p == "day" && lmt > 0 { lmt } else { 2900 };
    // 远端日线失败原因要可见：降级到 em 等备选源时，复权基可能与本地不同
    // （实测 000338 lmt=60 落到东财，日线与分钟差 0.85%），静默吞掉错误会让人误判成数据问题。
    //
    // **本地已是最新交易日 → 根本不问远端**：本地 .day 完整且与分时图同基，纯本地最干净，
    // 也没有换基风险。只有本地落后（盘中本地只有 T-1）才需要远端补缺口。
    let local_last = local.last().map(|b| b.date.as_str()).unwrap_or("");
    let local_fresh = !local_last.is_empty() && !date_lt(local_last, &reference);
    let mut remote_err = String::new();
    let (remote_rows, remote_src) = if local_fresh {
        (Vec::new(), String::new())
    } else {
        match remote_daily(code, want, &reference, &local) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[live] {code} 日线远端源全部失败：{e}");
                remote_err = e.clone();
                (Vec::new(), String::new())
            }
        }
    };
    let remote_last = remote_rows
        .last()
        .map(|b| b.date.clone())
        .unwrap_or_default();

    let daily = merge_days(local, remote_rows, MergePolicy::LocalWins);
    if daily.is_empty() {
        return Err("日线全部失败（本地与远端均无数据）".to_string());
    }
    let as_of = daily.last().map(|b| b.date.clone()).unwrap_or_default();
    // 末根来自远端时标远端来源（日期格式先归一，否则 '20261104' != '2026-11-04'）
    let mut source = if !remote_src.is_empty() && norm_date(&as_of) == norm_date(&remote_last) {
        remote_src.clone()
    } else {
        "local".to_string()
    };
    if source.is_empty() {
        source = "local".to_string();
    }
    // detail 写进 API 的 meta：前端徽标 tooltip 能看到「为什么没拿到远端」。
    // 以前只进 stderr（且 warn_once 去重），页面上一片空白，用户只能猜。
    let detail = if !remote_err.is_empty() {
        let e: String = remote_err.chars().take(200).collect();
        format!("远端源不可用：{e}")
    } else if !remote_src.is_empty() && remote_src != "local" && source == "local" {
        // 远端取到了但没补上缺口（它的新 bar 全是零成交占位柱，或不比本地新）
        format!("远端 {remote_src} 未补上缺口（末根仍为本地）")
    } else {
        String::new()
    };
    let stale = date_lt(&as_of, &reference);
    let mut rows = if p == "day" {
        daily
    } else {
        resample_dwm(&daily, &p)
    };
    rows = trim_rows(rows, lmt);
    Ok(Series { rows, source, detail, as_of, stale })
}

/// 远端分钟归一到本地口径（腾讯分钟与本地 `.lc1` 有两处硬差异，不归一就会串味）：
///
/// 1. **时间轴**：本地 `.lc1` 是通达信口径（桶结束时刻，09:31–11:30 / 13:01–15:00，
///    单日 240 根）；腾讯 m1 每天多一根 `09:30` 集合竞价快照（O=H=L=C）。直接按时间串
///    合并会凭空多出一根伪 bar，破坏「每天 240 根」的轴约定 → 这里丢掉它。
/// 2. **复权基**：腾讯分钟是未复权价，本地分钟是前复权价（`apply_minute_factors`）。
///    除权股会把两者差出一个除权因子（实测 000338 在 2026-10-08..10-22 差 2.5%），
///    于是分时图与日 K 图对不上、接缝处出现假跳空 → 这里用**同一套除权事件**
///    （zg-core `adjust::adjust_bars`，前复权锚在末根=最新价）把远端换算到本地复权基；
///    事件前的收盘价从本地日线原始价取（`context_bars`），与本地分钟的因子口径一致。
///
/// TQ / MAC 两个源的服务端本身就是前复权（`dividend_type=front` / MAC 的 `fq=1`），
/// 只需做时间轴归一，不要再叠一次前复权（会重复复权）。
fn normalize_axis(remote: &[KBar]) -> Vec<KBar> {
    // 集合竞价行（本地轴从 09:31 起，没有 09:30）。
    // 通达信把集合竞价量记在首根 09:31 上，腾讯单列一行 —— 丢掉这行的同时
    // 要把它的成交量并进下一根，否则当日分钟量会比日线少一个竞价量
    // （实测 000338 2026-10-08 少 7908 手 = 0.72%）。
    let mut out: Vec<KBar> = Vec::with_capacity(remote.len());
    let mut pending_vol = 0.0f64;
    for k in remote {
        if k.date.ends_with(" 09:30") {
            pending_vol += k.vol;
            continue;
        }
        let mut k = k.clone();
        k.vol += pending_vol;
        pending_vol = 0.0;
        out.push(k);
    }
    out
}

/// 未复权的远端分钟 → 本地前复权基（同一套除权事件）。
fn qfq_align(remote: &[KBar], mkt: &str, cd: &str) -> Vec<Agg> {
    let bars = normalize_axis(remote);
    if bars.is_empty() {
        return Vec::new();
    }
    let mut bars: Vec<zg_core::Bar> = bars
        .iter()
        .map(|k| zg_core::Bar {
            date: k.date.clone(),
            open: k.open,
            high: k.high,
            low: k.low,
            close: k.close,
            // 远端 vol 已是「手」（腾讯/TQ/tdx 同口径），复权不涉及量，原样带过
            volume: k.vol,
            amount: k.amount,
        })
        .collect();
    // 前复权对齐（锚点 = 末根 = 最新价，与本地分钟同一套事件）
    let anchor: String = bars
        .last()
        .map(|b| b.date.chars().take(10).collect())
        .unwrap_or_default();
    let evs = zg_core::events::events_of(mkt, cd, Some(&anchor)).unwrap_or_default();
    if !evs.is_empty() {
        let ctx = zg_core::api::read_day_bars(mkt, cd).unwrap_or_default();
        if let Err(e) =
            zg_core::adjust::adjust_bars(&mut bars, Some(&evs), zg_core::adjust::FQ_QFQ, Some(&ctx))
        {
            eprintln!("[live] 远端分钟前复权失败（{e}），按未复权合并");
        }
    }
    bars.into_iter()
        .map(|b| Agg {
            date: b.date,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.volume,
            amount: b.amount,
        })
        .collect()
}

/// 每个交易日的**收盘根**（15:00）收盘价。
///
/// 只认 15:00 收盘根：半截交易日（盘中）的末根不是收盘价，拿来对表会误判。
fn day_close_map(rows: &[Agg]) -> HashMap<String, f64> {
    let mut m: HashMap<String, f64> = HashMap::new();
    for b in rows {
        if b.date.ends_with(" 15:00") {
            m.insert(b.date.chars().take(10).collect(), b.close);
        }
    }
    m
}

/// 远端分钟能否并进本地分钟序列（复权口径兼容门，与日线 `remote_mergeable` 同思路）。
///
/// 比「两侧都有 15:00 收盘根」的交易日收盘价：重叠段中位偏差 >2% → 整段丢弃远端，
/// 退回本地（并标 stale），绝不让两个口径的价混在一根 K 线上。
fn minute_mergeable(local: &[Agg], remote: &[Agg]) -> bool {
    if remote
        .iter()
        .any(|b| b.open <= 0.0 || b.high <= 0.0 || b.low <= 0.0 || b.close <= 0.0)
    {
        eprintln!("[live] 远端分钟含非正价格，丢弃远端");
        return false;
    }
    let local_close = day_close_map(local);
    let remote_close = day_close_map(remote);
    let mut diffs: Vec<f64> = Vec::new();
    for (d, rc) in &remote_close {
        if let Some(lc) = local_close.get(d) {
            if *lc > 0.0 {
                diffs.push((rc / lc - 1.0).abs());
            }
        }
    }
    if diffs.len() >= 3 {
        diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = diffs[diffs.len() / 2];
        if med > 0.02 {
            eprintln!(
                "[live] 远端分钟与本地复权基不符（重叠段中位偏差 {:.2}%），丢弃远端",
                med * 100.0
            );
            return false;
        }
    }
    true
}

/// 同一 key 的告警只打一次（避免轮询把日志刷爆）。
fn warn_once(key: &str, msg: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock_ok();
    if seen.insert(key.to_string()) {
        eprintln!("{msg}");
    }
}

/// 远端分钟源（腾讯）不提供成交额：远端独立日（今天，本地没有当日数据）的分钟成交额是 0。
/// 这类「部分失效」不能静默 —— 每个 (代码, 日期) 只提示一次。
fn warn_minute_amount_missing(code: &str, day: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock_ok();
    if seen.insert(format!("{code} {day}")) {
        eprintln!(
            "[live] {code} {day} 分钟成交额为 0：远端分钟源不提供成交额（腾讯），本地也无当日数据"
        );
    }
}

/// 远端覆盖同日；远端缺成交额（腾讯分钟没有该字段，恒 0）时沿用本地同分钟的成交额，
/// 不把 0 当成真实成交额并进去。
fn merge_minute(local: Vec<Agg>, remote: Vec<Agg>) -> Vec<Agg> {
    let mut m: BTreeMap<String, Agg> = BTreeMap::new();
    for b in local {
        m.insert(b.date.clone(), b);
    }
    for b in remote {
        let mut b = b;
        if b.amount <= 0.0 {
            if let Some(l) = m.get(&b.date) {
                if l.amount > 0.0 {
                    b.amount = l.amount;
                }
            }
        }
        m.insert(b.date.clone(), b);
    }
    m.into_values().collect()
}

fn minute_cutoff() -> String {
    (chrono::Local::now() - chrono::Duration::minutes(15))
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// 分钟线：本地（15 分钟新鲜度门禁）→ TQ → MAC → 腾讯 → tdx-master。
///
/// 远端只给最近若干根，因此与本地历史按时间合并；这样既保留 TV 的 120 日深度，
/// 又能补上当天。指数分钟不接远端（TQ/腾讯的指数分钟单位与个股不一致）。
///
/// **1 分钟档例外：只读本地 `.lc1`（离线采集），本地不新鲜就标 `stale` 交出去。**
/// 通达信客户端盘中本来就在连续写 `.lc1`，再叠一条远端链只是多打一次源；
/// 而且 1 分钟是唯一「远端与本地按根对齐会看出接缝」的档，离线口径反而干净
/// （2026-11-02 用户定：1 分钟的报价采用离线采集）。
pub fn minute_series(code: &str, period: &str, count: usize) -> Result<Series, String> {
    let (mkt, cd) = zg_core::reader::resolve(code, None).map_err(|e| e.to_string())?;
    let is_stock = !zg_core::reader::is_index(&mkt, &cd);
    let freq = format!("F{period}");
    let need = if count > 0 { count.max(60) } else { 0 };
    let reference = reference_day();

    let local = zg_core::api::minute_bars(&mkt, &cd, &freq, need).ok();
    let local_rows = local.as_ref().map(|(b, _)| local_min_to_agg(b, is_stock));
    let local_last = local.as_ref().map(|(_, m)| m.last.clone()).unwrap_or_default();
    let local_fresh = if local_last.is_empty() {
        false
    } else if market_open_now() {
        local_last.as_str() >= minute_cutoff().as_str()
    } else {
        local_last.get(..10).unwrap_or("") >= reference.as_str()
    };
    if local_fresh {
        let rows = trim_rows(local_rows.clone().unwrap_or_default(), count);
        if !rows.is_empty() {
            return Ok(Series {
                rows,
                source: "local".to_string(),
                detail: String::new(),
                as_of: local_last,
                stale: false,
            });
        }
    }

    // 走到这里说明本地分钟线已过新鲜度门禁。1 分钟档不再下探远端（见函数注释），
    // 直接把本地数据标 stale 交出；其余档仍走实时链。
    if !enabled() || period == "1" {
        let rows = trim_rows(local_rows.unwrap_or_default(), count);
        if rows.is_empty() {
            return Err("本地分钟线为空".to_string());
        }
        return Ok(Series {
            rows,
            source: "local".to_string(),
            detail: String::new(),
            as_of: local_last,
            stale: true,
        });
    }

    if is_stock {
        let p = format!("min{period}");
        let want = if count > 0 { count } else { 800 };
        // 远端分钟源链：TQ(front) → MAC(通达信扩展行情，带成交额) → 腾讯(未复权) → tdx-master
        // detail 记录来源的定位信息（MAC 是实际连上的主机 IP）——对表脚本要连同一台才可比。
        let mut remote: Option<(Vec<Agg>, &'static str, String)> = None;
        if let Ok((_, b)) = zg_market::kline::kline_tq_min(code, &p, want) {
            if !b.is_empty() {
                // TQ 服务端已是前复权：只做时间轴归一，不再叠前复权
                remote = Some((kbars_to_agg(&normalize_axis(&b)), "tq", String::new()));
            }
        }
        if remote.is_none() {
            // MAC：与本地 .lc1 同轴 + 带成交额，口径最贴近本地
            if let Ok((rows, host)) = mac_minute_rows(&mkt, &cd, period, want) {
                if !rows.is_empty() {
                    // MAC 服务端的前复权因子体系与本地 gbbq 不同（除权日差 0.33%）→ 逐日对齐基
                    let rows = align_mac_to_local_base(rows, &cd);
                    remote = Some((rows, "mac", host));
                }
            }
        }
        if remote.is_none() {
            if let Ok((_, b)) = zg_market::kline::kline_tencent_min(code, &p, want) {
                if !b.is_empty() {
                    // 腾讯：未复权 + 无成交额 + 多一根集合竞价行 → 全量归一
                    let rows = qfq_align(&b, &mkt, &cd);
                    if !rows.is_empty() {
                        remote = Some((rows, "tencent", String::new()));
                    }
                }
            }
        }
        if remote.is_none() {
            if let Ok((_, b)) = zg_market::kline::kline_tdx_master(code, &p, want, None) {
                if !b.is_empty() {
                    remote = Some((kbars_to_agg(&b), "tdx", String::new()));
                }
            }
        }
        if let Some((mut remote_rows, source, detail)) = remote {
            if !remote_rows.is_empty() {
                if let Some(l) = local_rows.as_ref() {
                    if !minute_mergeable(l, &remote_rows) {
                        remote_rows.clear();
                    }
                }
            }
            if !remote_rows.is_empty() {
                let rows = match local_rows {
                    Some(l) => merge_minute(l, remote_rows),
                    None => remote_rows,
                };
                let rows = trim_rows(rows, count);
                let as_of = rows.last().map(|b| b.date.clone()).unwrap_or_default();
                // 远端独立日没有成交额（腾讯分钟无该字段）→ 提示一次，别让 0 静默过关
                if let Some(last) = rows.last() {
                    if last.amount <= 0.0 {
                        warn_minute_amount_missing(code, &last.date.chars().take(10).collect::<String>());
                    }
                }
                let fresh = if market_open_now() {
                    as_of.as_str() >= minute_cutoff().as_str()
                } else {
                    as_of.get(..10).unwrap_or("") >= reference.as_str()
                };
                return Ok(Series { rows, source: source.to_string(), detail, as_of, stale: !fresh });
            }
            // 远端与本地口径不兼容：落到下面「退回本地 + stale」分支
        }
    }

    // 远端全失败：退回本地旧数据，明确标 stale
    let rows = trim_rows(local_rows.unwrap_or_default(), count);
    if rows.is_empty() {
        return Err("所有分钟线数据源均失败".to_string());
    }
    Ok(Series { rows, source: "local".to_string(), detail: String::new(), as_of: local_last, stale: true })
}

// ---------------------------------------------------------------- 实时报价

#[cfg(test)]

mod tests {
    use super::*;

    fn day(d: &str, c: f64) -> Agg {
        Agg { date: d.to_string(), open: c, high: c, low: c, close: c, vol: 1.0, amount: 1.0 }
    }

    /// 兼容门是双向的：同基（偏差在容差内）必须放行，错基（实测 0.25%~0.53%）必须拒。
    /// 只测一侧会把「恒拒」误当成「门在工作」。
    #[test]
    fn remote_mergeable_rejects_mismatched_adjust_base() {
        let base: Vec<Agg> = (0..10).map(|i| day(&format!("2026-01-{:02}", i + 1), 100.0)).collect();
        // 同源：逐位一致 → 放行
        let same: Vec<Agg> = base.clone();
        assert!(remote_mergeable(&base, &same, "tq"));
        // 基差 0.05% → 仍在容差内 → 放行
        let near: Vec<Agg> = base.iter().map(|b| day(&b.date, b.close * 1.0005)).collect();
        assert!(remote_mergeable(&base, &near, "em"));
        // 基差 0.5%（实测东财/腾讯相对本地 gbbq 的典型差）→ 拒
        let off: Vec<Agg> = base.iter().map(|b| day(&b.date, b.close * 1.005)).collect();
        assert!(!remote_mergeable(&base, &off, "em"));
        // 负价（东财减法复权的老历史）→ 拒
        let neg: Vec<Agg> = base.iter().map(|b| day(&b.date, -1.0)).collect();
        assert!(!remote_mergeable(&base, &neg, "em"));
    }

    /// 本地没有历史（新股/北交所）时不能因为「无重叠段」把错基远端也拒掉。
    #[test]
    fn remote_mergeable_accepts_when_local_empty() {
        let remote = vec![day("2026-01-01", 10.0)];
        assert!(remote_mergeable(&[], &remote, "tq"));
    }

    /// 造一批可排序的日期（1~8 月、每月 28 天）。
    fn dates(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("2025-{:02}-{:02}", 1 + i / 28, 1 + i % 28))
            .collect()
    }

    /// **同基源不能被舍入噪声误杀**（2026-11-04 那次 0.104% 误拒的真因）。
    ///
    /// 老历史被分红压到 1.5 元级时，本地 qfq 是 3 位小数（1.494）、远端是 2 位（1.49），
    /// 半分的相对差 = 0.33% —— 高于 0.1% 的相对门限。实测 600346 vs TQ：
    /// 未对齐精度时最旧 200 根中位 0.1316%，两边都 round2 后 0.0000%。
    #[test]
    fn remote_mergeable_accepts_rounding_noise_on_deflated_history() {
        let ds = dates(200);
        let local: Vec<Agg> = ds.iter().map(|d| day(d, 1.494)).collect();
        // 远端是同一批值四舍五入到分
        let remote: Vec<Agg> = ds
            .iter()
            .map(|d| day(d, (1.494_f64 * 100.0).round() / 100.0))
            .collect();
        assert!(remote_mergeable(&local, &remote, "tq"));
    }

    /// **老历史错基必须拒**：前复权都锚在最新价，基差只随历史深度放大，
    /// 所以「最旧段」才是判别力所在（实测东财 vs 本地：最近 200 根 1.61%、最旧段 228%）。
    /// 只看最近 200 根的版本会让这里变成放行 ⇒ 必须锁住。
    #[test]
    fn remote_mergeable_rejects_old_history_mismatch() {
        let ds = dates(300);
        let local: Vec<Agg> = ds.iter().map(|d| day(d, 100.0)).collect();
        let mut remote = local.clone();
        for b in remote.iter_mut().take(100) {
            b.close *= 1.005;
        }
        assert!(!remote_mergeable(&local, &remote, "em"));
    }

    /// **最近段错基也必须拒**：错基只出现在最近 200 根时，最旧段是干净的 ——
    /// 单段采样会漏放。两段采样里任一段超限即拒。
    #[test]
    fn remote_mergeable_rejects_recent_only_mismatch() {
        let ds = dates(600);
        let local: Vec<Agg> = ds.iter().map(|d| day(d, 100.0)).collect();
        let mut remote = local.clone();
        for b in remote.iter_mut().skip(400) {
            b.close *= 1.005;
        }
        assert!(!remote_mergeable(&local, &remote, "em"));
    }

    // **合并必须本地优先**：同日以本地为准，远端只补本地没有的日期。
    //
    // 远端一次能给 2900 根（≈11.6 年），让它覆盖就等于把整段历史的复权基换成
    // 远端那套 —— 日 K 图与分时图（永远在本地 gbbq 基上）就此错基。
    //
    // 实现与断言已搬到 `src/daymerge.rs`（`merge_days_prefers_local_on_overlap` 等三条），
    // 合并语义与策略枚举在那里一处可见。
}
