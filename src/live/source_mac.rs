//! MAC（通达信扩展行情，`crate::tdxmac` 自实现协议）取数：日线/分钟行，
//! 以及与本地 gbbq 复权基的逐日比例对齐。

use super::*;
use crate::tdxmac;

/// MAC 日线（**不复权**）→ 事件法复权到本地基。
///
/// 为什么不用 MAC 自带的前复权（`fq=1`）：MAC 的因子体系与本地 gbbq 事件法不同
/// （实测 000338 近 250 个交易日中位差 2.28%、最差 7.05%），并进来会让日 K 与分时图错基。
/// 而 MAC 的**不复权**日线与本地 `.day` **逐位一致**（实测 000338 / 600519 最大收差 0.0000），
/// 于是取不复权 + 用本地同一套除权事件复权 → 与本地日线/分钟同基。
///
/// 返回 (日线序列, 实际主机 IP)。
pub(super) fn mac_daily_rows(mkt: &str, cd: &str, want: usize) -> Result<(Vec<Agg>, String), String> {
    let market = tdxmac::market_code(mkt);
    let want = want.clamp(60, 2900);
    // 单页上限 700 根；2900 根需要 5 页
    let page_count = ((want + 699) / 700).clamp(1, 6);
    let mut conn = tdxmac::MacConn::connect_any(std::time::Duration::from_secs(6))?;
    let host = conn.host().to_string();
    // 分页顺序：第 0 页是**最新**的一页，start 偏移往后是更早的数据 → 收集后逆序拼接。
    // 直接顺次 extend 会得到时间倒序的序列（末根变成最旧一根，新鲜度检查必挂）。
    let mut pages: Vec<Vec<zg_core::Bar>> = Vec::new();
    for p in 0..page_count {
        let page = conn.bars(market, cd, tdxmac::PERIOD_DAY, (p * 700) as u32, 700, tdxmac::FQ_NONE)?;
        if page.is_empty() {
            break;
        }
        let n = page.len();
        // MAC 日线：vol 单位股 → 手（tv 日线口径）；amount 单位元
        pages.push(
            page.into_iter()
                .map(|b| zg_core::Bar {
                    date: b.date,
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.vol / 100.0,
                    amount: b.amount,
                })
                .collect(),
        );
        if n < 700 {
            break;
        }
    }
    let mut bars: Vec<zg_core::Bar> = Vec::new();
    for page in pages.iter_mut().rev() {
        bars.append(page);
    }
    if bars.is_empty() {
        return Err("MAC 日线为空".to_string());
    }
    // 事件法前复权到本地基（与 qfq_align 同一套事件、同一个锚点口径）
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
            eprintln!("[live] MAC 日线前复权失败（{e}），按未复权使用");
        }
    }
    Ok((
        bars.into_iter()
            .map(|b| Agg {
                date: b.date,
                // 取整到 2 位（A 股最小变动单位）：与 TQ front 同精度，
                // 免得切换数据源时 K 线上的数字长度跳动（复权会放大这个差异）
                open: round2(b.open),
                high: round2(b.high),
                low: round2(b.low),
                close: round2(b.close),
                vol: b.volume,
                amount: b.amount,
            })
            .collect(),
        host,
    ))
}

/// MAC 分钟 → 本地复权基（逐日比例缩放）。
///
/// MAC 服务端的前复权因子体系与本地 gbbq 事件法**不是同一套**：除权日当天差 0.33%
/// （实测 000338 2026-10-08：MAC 24.34 vs 本地/TQ 24.26，原始 24.86 → 因子 0.97908
/// vs 0.97587），非除权日则逐位相同。差值会随历史累积（MAC 日线 250 日中位差 2.28%），
/// 所以不能只靠「两端都前复权」就认为同基。
///
/// 这里逐日用「本地日线收盘 ÷ MAC 当日收盘」做**统一比例缩放**：同一天内所有价乘同一系数，
/// 形状不变、只平移复权基 → 接缝偏差从 0.33% 降到 0。
/// 本地没有该天日线（当天）时跳过——当天没有除权事件，MAC 值本来就等于本地基。
pub(super) fn align_mac_to_local_base(rows: Vec<Agg>, cd: &str) -> Vec<Agg> {
    if rows.is_empty() {
        return rows;
    }
    // 本地日线（复权后，与 tv 日线 TQ-front 同基：实测 250 个交易日偏差 0.0000%）
    let local_daily: HashMap<String, f64> = zg_core::api::chart_bars(cd, "day", 60, None, None)
        .unwrap_or_default()
        .iter()
        .map(|b| (b.date.chars().take(10).collect::<String>(), b.close))
        .collect();
    if local_daily.is_empty() {
        return rows;
    }
    let mut out = rows;
    let mut i = 0usize;
    while i < out.len() {
        let day: String = out[i].date.chars().take(10).collect();
        let mut j = i;
        while j + 1 < out.len() && out[j + 1].date.starts_with(day.as_str()) {
            j += 1;
        }
        // 该天最后一根（收盘根）；半截交易日（盘中）没有 15:00 根，用最后一根亦可
        let last_close = out[j].close;
        if let Some(&lc) = local_daily.get(&day) {
            if lc > 0.0 && last_close > 0.0 {
                let ratio = lc / last_close;
                // 阈值 0.02%：非除权日实测偏差 0.0000%，除权日 0.33% → 只在真有差时缩放
                if (ratio - 1.0).abs() > 0.0002 {
                    for b in out.iter_mut().take(j + 1).skip(i) {
                        b.open *= ratio;
                        b.high *= ratio;
                        b.low *= ratio;
                        b.close *= ratio;
                    }
                }
            }
        }
        i = j + 1;
    }
    out
}

/// MAC 分钟（通达信扩展行情协议，`src/tdxmac.rs`）。
///
/// 与腾讯分钟源相比：轴与本地 `.lc1` 完全一致（09:31 起 240 根/日，无集合竞价伪行）、
/// 带真实成交额、服务端直接给前复权（`fq=1`）。
/// 返回值已归一到 live 层口径：vol 手、amount 元。
///
/// 同时返回实际连上的主机 IP：各 MAC 主机的**日汇总一致但桶级分桶有微差**，
/// 对表/复现必须固定同一台主机。
pub(super) fn mac_minute_rows(
    mkt: &str,
    cd: &str,
    period: &str,
    want: usize,
) -> Result<(Vec<Agg>, String), String> {
    let per = tdxmac::period_code(period).ok_or_else(|| format!("MAC 不支持周期: {period}"))?;
    let market = tdxmac::market_code(mkt);
    // 单页上限 700 根（服务端自行截断）；最多 6 页 ≈ 17 个交易日 —— 更早的由本地 .lc1 覆盖
    let pages = ((want.max(1) + 699) / 700).clamp(1, 6);
    let mut conn = tdxmac::MacConn::connect_any(std::time::Duration::from_secs(6))?;
    let host = conn.host().to_string();
    let mut out: Vec<Agg> = Vec::new();
    for p in 0..pages {
        let bars = conn.bars(market, cd, per, (p * 700) as u32, 700, tdxmac::FQ_QFQ)?;
        if bars.is_empty() {
            break;
        }
        let n = bars.len();
        out.extend(bars.into_iter().map(|b| Agg {
            date: b.date,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol / 100.0, // 股 → 手（与本地分钟同口径）
            amount: b.amount,   // 元
        }));
        if n < 700 {
            break; // 服务端没有更多历史了
        }
    }
    Ok((out, host))
}
