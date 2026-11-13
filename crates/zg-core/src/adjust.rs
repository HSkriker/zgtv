//! 前复权 —— 与 `zgcore/bars/adjust.py` 逐行等价。
//!
//! 公式：
//!   P_ex = (P_close - D + P_rights × R_rights) / (1 + R_bonus + R_rights)
//!   factor = P_ex / P_close
//!   D=fenhong/10（元/股）、R_bonus=songzhuangu/10、R_rights=peigu/10
//!   P_rights=peigujia（元/股）
//!
//! 铁律：
//!   ① 先全历史复权、再裁剪（因子需要事件前一日收盘作基准）；
//!   ② 晚于末根 K 线的「已公布未生效」事件必须剔除，否则末根也会被乘下去；
//!   ③ 复权后末根 close 必须不动（锚点断言，漂移只可能来自 bug）；
//!   ④ 价格最终 round 到 6 位小数。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::error::{Result, ZgError};
use crate::events::{events_of_detailed, EventSource, XdxrEvent};
use crate::sync::MutexExt;
use crate::model::{date_key, is_sorted_dates, py_round, Bar, QFQ_PRICE_DECIMALS};
use crate::reader::{day_path, is_index, lru_put, read_day, LruMap};

pub const FQ_NONE: i32 = 0;
pub const FQ_QFQ: i32 = 1;
pub const FQ_HFQ: i32 = 2;

pub const QFQ_ANCHOR_TOL: f64 = 1e-6;

#[derive(Default, Debug)]
pub struct AdjustOutcome {
    pub warnings: Vec<String>,
}

pub fn calc_qfq_factor(close_before: f64, fenhong: f64, songzhuangu: f64, peigu: f64,
                       peigujia: f64) -> f64 {
    let div_per_share = fenhong / 10.0;
    let bonus_ratio = songzhuangu / 10.0;
    let rights_ratio = peigu / 10.0;
    let rights_price = peigujia;
    let denominator = close_before * (1.0 + bonus_ratio + rights_ratio);
    let numerator = close_before - div_per_share + rights_price * rights_ratio;
    if denominator.abs() < 1e-10 || close_before.abs() < 1e-10 {
        return 1.0;
    }
    numerator / denominator
}

/// 事件前一交易日收盘价：正向取最后一个日期 < 事件日；否则回看 context。
pub fn find_close_before_event(bars: &[Bar], date_key_int: i32,
                               context_bars: Option<&[Bar]>) -> Option<f64> {
    let mut result = None;
    for b in bars {
        if date_key(&b.date) < date_key_int {
            result = Some(b.close);
        } else {
            break;
        }
    }
    if result.is_some() {
        return result;
    }
    if let Some(ctx) = context_bars {
        for b in ctx.iter().rev() {
            if date_key(&b.date) < date_key_int {
                return Some(b.close);
            }
        }
    }
    None
}

/// `find_close_before_event` 的二分版（数值等价，见 Python 侧差分测试）。
fn close_before_event(keys: &[i32], bars: &[Bar], date_key_int: i32,
                      context_bars: Option<&[Bar]>) -> Option<f64> {
    let idx = keys.partition_point(|k| *k < date_key_int);
    if idx > 0 {
        let close = bars[idx - 1].close;
        if !close.is_nan() {
            return Some(close);
        }
    }
    if let Some(ctx) = context_bars {
        for b in ctx.iter().rev() {
            if date_key(&b.date) < date_key_int {
                return Some(b.close);
            }
        }
    }
    None
}

fn ymd_to_key(y: i32, m: i32, d: i32) -> i32 {
    y * 10000 + m * 100 + d
}

/// 就地复权；保留末根锚点断言与 6 位小数舍入。
pub fn adjust_bars(bars: &mut [Bar], xdxr_list: Option<&[XdxrEvent]>, fq_type: i32,
                   context_bars: Option<&[Bar]>) -> Result<AdjustOutcome> {
    let mut out = AdjustOutcome::default();
    if fq_type == FQ_NONE || bars.is_empty() {
        return Ok(out);
    }
    let empty: &[XdxrEvent] = &[];
    let xdxr_list = xdxr_list.unwrap_or(empty);

    if !is_sorted_dates(bars) {
        bars.sort_by_key(|b| date_key(&b.date));
    }

    if xdxr_list.is_empty() {
        let last = date_key(&bars[bars.len() - 1].date);
        let first = date_key(&bars[0].date);
        if last > 0 && first > 0 {
            let days = days_between(first, last);
            if days > 730 {
                out.warnings.push(format!(
                    "除权事件表为空但 K 线跨度 {} 天({} ~ {}): 无法区分「确实从未分红」与「xdxr 数据缺失」, 价格断层可能未被复权 —— 请核对该标的的除权记录",
                    days, bars[0].date, bars[bars.len() - 1].date
                ));
            }
        }
        return Ok(out);
    }

    let mut events: Vec<(i32, f64, f64, f64, f64)> = Vec::new();
    for xd in xdxr_list {
        if xd.category != 1 {
            continue;
        }
        events.push((
            ymd_to_key(xd.year, xd.month, xd.day),
            xd.fenhong,
            xd.songzhuangu,
            xd.peigu,
            xd.peigujia,
        ));
    }
    events.sort_by_key(|e| e.0);
    if events.is_empty() {
        return Ok(out);
    }

    if fq_type == FQ_QFQ {
        let anchor_key = date_key(&bars[bars.len() - 1].date);
        let pending: Vec<i32> =
            events.iter().map(|e| e.0).filter(|k| *k > anchor_key).collect();
        if !pending.is_empty() {
            out.warnings.push(format!(
                "{} 条除权事件晚于末根K线({}), 视为未生效已剔除: {:?}",
                pending.len(),
                anchor_key,
                pending
            ));
            events.retain(|e| e.0 <= anchor_key);
            if events.is_empty() {
                return Ok(out);
            }
        }
    }

    let keys: Vec<i32> = bars.iter().map(|b| date_key(&b.date)).collect();
    let mut factor_map: HashMap<i32, f64> = HashMap::new();
    let mut skipped: Vec<i32> = Vec::new();
    for evt in &events {
        match close_before_event(&keys, bars, evt.0, context_bars) {
            None => skipped.push(evt.0),
            Some(p_close) => {
                factor_map.insert(evt.0, calc_qfq_factor(p_close, evt.1, evt.2, evt.3, evt.4));
            }
        }
    }

    if !skipped.is_empty() {
        let first_key = date_key(&bars[0].date);
        let in_window: Vec<i32> = skipped.iter().copied().filter(|k| *k > first_key).collect();
        let pre_window: Vec<i32> = skipped.iter().copied().filter(|k| *k <= first_key).collect();
        if !in_window.is_empty() {
            out.warnings.push(format!(
                "{} 条除权事件位于窗口内却找不到事件前收盘价(K线缺口?): {:?}",
                in_window.len(),
                in_window
            ));
        }
        if !pre_window.is_empty() && fq_type == FQ_HFQ {
            out.warnings.push(format!(
                "{} 条事件早于窗口首根({}), 后复权累计因子缺失 ⇒ 本窗口为相对基准, 与全史后复权相差一个常数; 若本窗口系裁剪所得请用 context_bars 补窗口前历史: {:?}",
                pre_window.len(),
                first_key,
                pre_window
            ));
        }
    }

    if factor_map.is_empty() {
        return Ok(out);
    }

    let anchor_close = bars[bars.len() - 1].close;
    if fq_type == FQ_QFQ {
        let mut cumulative = 1.0f64;
        let mut event_idx = events.len() as i64 - 1;
        for i in (0..bars.len()).rev() {
            let bar_key = date_key(&bars[i].date);
            while event_idx >= 0 && events[event_idx as usize].0 > bar_key {
                let evt_date = events[event_idx as usize].0;
                if let Some(f) = factor_map.get(&evt_date) {
                    cumulative *= *f;
                }
                event_idx -= 1;
            }
            if (cumulative - 1.0).abs() > 1e-10 {
                bars[i].open *= cumulative;
                bars[i].high *= cumulative;
                bars[i].low *= cumulative;
                bars[i].close *= cumulative;
            }
        }
    } else if fq_type == FQ_HFQ {
        let mut cumulative = 1.0f64;
        let mut event_idx = 0usize;
        for bar in bars.iter_mut() {
            let bar_key = date_key(&bar.date);
            while event_idx < events.len() && events[event_idx].0 <= bar_key {
                let evt_date = events[event_idx].0;
                if let Some(f) = factor_map.get(&evt_date) {
                    cumulative *= 1.0 / *f;
                }
                event_idx += 1;
            }
            if (cumulative - 1.0).abs() > 1e-10 {
                bar.open *= cumulative;
                bar.high *= cumulative;
                bar.low *= cumulative;
                bar.close *= cumulative;
            }
        }
    }

    let d = QFQ_PRICE_DECIMALS as usize;
    for bar in bars.iter_mut() {
        bar.open = py_round(bar.open, d);
        bar.high = py_round(bar.high, d);
        bar.low = py_round(bar.low, d);
        bar.close = py_round(bar.close, d);
    }

    if fq_type == FQ_QFQ {
        let last = bars.len() - 1;
        let drift = (bars[last].close - anchor_close).abs();
        if drift > QFQ_ANCHOR_TOL.max(anchor_close * 1e-9) {
            return Err(ZgError::assertion(format!(
                "前复权锚点被破坏: 末根({}) close {} → {} (漂移 {:.9})。前复权必须以最新价为锚、末根不动。",
                bars[last].date, anchor_close, bars[last].close, drift
            )));
        }
    }
    Ok(out)
}

fn days_between(first: i32, last: i32) -> i64 {
    use chrono::NaiveDate;
    let to_date = |k: i32| NaiveDate::from_ymd_opt(k / 10000, (k / 100 % 100) as u32, (k % 100) as u32);
    match (to_date(first), to_date(last)) {
        (Some(a), Some(b)) => (b - a).num_days(),
        _ => 0,
    }
}

/// 就地前复权并校验末根锚点；事件晚于末根的一律剔除。
pub fn apply_qfq(bars: &mut [Bar], market: &str, code: &str) -> Result<()> {
    if bars.is_empty() {
        return Ok(());
    }
    let anchor_date: String = bars[bars.len() - 1].date.chars().take(10).collect();
    let (evs, source) = events_of_detailed(market, code, Some(&anchor_date))?;
    apply_qfq_events(bars, market, code, evs, source).map(|_| ())
}

/// 同 `apply_qfq`，但事件表由调用方取好（同一只股票的除权缓存只读一遍盘）。
///
/// 返回「是否存在生效中的 cat1 除权事件」（晚于末根的未生效事件不算）。
/// 复权口径、告警文案、锚点断言与 `apply_qfq` 完全一致，只是不自己去取事件表。
pub fn apply_qfq_events(bars: &mut [Bar], market: &str, code: &str, evs: Vec<XdxrEvent>,
                        source: EventSource) -> Result<bool> {
    if bars.is_empty() {
        return Ok(false);
    }
    let anchor = date_key(&bars[bars.len() - 1].date);
    let mut live = Vec::new();
    let mut pending = 0usize;
    for e in evs {
        let k = e.year * 10000 + e.month * 100 + e.day;
        if k > anchor {
            pending += 1;
        } else {
            live.push(e);
        }
    }
    if pending > 0 {
        eprintln!(
            "[zgcore] {market}{code} 剔除 {pending} 条晚于末根({anchor})的未生效除权事件"
        );
    }
    let has_live_event = live.iter().any(|e| e.category == 1);
    if live.is_empty() {
        if source == EventSource::Gbbq {
            // 共享缓存缺失/陈旧且 gbbq 也无记录：不能断言「从未分红」，
            // 交 adjust_bars 让「跨度 > 730 天但事件表为空」的告警可达（H4）。
            let outcome = adjust_bars(bars, Some(&[]), FQ_QFQ, None)?;
            crate::events::log_warn(&format!(
                "{market}{code} 除权事件不可用（共享缓存缺失/陈旧，gbbq 无记录），价格可能未复权"
            ));
            for w in &outcome.warnings {
                crate::events::log_warn(&format!("{market}{code} 复权告警: {w}"));
            }
        }
        return Ok(false);
    }
    let before = bars[bars.len() - 1].close;
    let outcome = adjust_bars(bars, Some(&live), FQ_QFQ, None)?;
    for w in &outcome.warnings {
        crate::events::log_warn(&format!("{market}{code} 复权告警: {w}"));
    }
    let drift = (bars[bars.len() - 1].close - before).abs();
    if drift > 1e-6f64.max(before.abs() * 1e-9) {
        return Err(ZgError::bridge(format!(
            "{market}{code} 前复权锚点被破坏：末根 close {} → {}（漂移 {:.9}）。qfq 必须以最新价为锚、末根不动。",
            before, bars[bars.len() - 1].close, drift
        )));
    }
    Ok(has_live_event)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FactorDoc {
    pub factors: HashMap<String, f64>,
    pub raw_close: HashMap<String, f64>,
}

/// 日线全历史前复权 → 因子表 + 原始收盘表。
pub fn qfq_factor_by_day(market: &str, code: &str) -> Result<FactorDoc> {
    let daily = read_day(market, code)?;
    if is_index(market, code) {
        return Ok(FactorDoc { factors: HashMap::new(), raw_close: HashMap::new() });
    }
    let mut adj = daily.clone();
    apply_qfq(&mut adj, market, code)?;
    let mut factors = HashMap::new();
    let mut raw_close = HashMap::new();
    for (r, a) in daily.iter().zip(adj.iter()) {
        let d10: String = r.date.chars().take(10).collect();
        if r.close != 0.0 {
            factors.insert(d10.clone(), a.close / r.close);
            raw_close.insert(d10, r.close);
        }
    }
    Ok(FactorDoc { factors, raw_close })
}

// ---------------------------------------------------------------------------
// 全历史前复权日线缓存
// ---------------------------------------------------------------------------

struct AdjEntry {
    mtime: SystemTime,
    bars: Vec<Bar>,
}

type AdjKey = (String, String, u64);

fn adj_mem() -> &'static Mutex<LruMap<AdjKey, AdjEntry>> {
    static MEM: OnceLock<Mutex<LruMap<AdjKey, AdjEntry>>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(LruMap::new(qfq_mem_max())))
}

pub fn qfq_mem_max() -> usize {
    std::env::var("ZG_QFQ_MEM_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(32)
}

/// gbbq 事件文件的 mtime（毫秒，缺失为 0）。除权事件是前复权因子的输入，
/// 因子缓存键必须包含它：gbbq 晚于 .day 更新时（除权日当天补同步），
/// 只盯 .day mtime 会整天供旧因子。zg 上游同病，tv 内嵌副本先修。
pub fn gbbq_mtime_millis() -> u64 {
    std::fs::metadata(&crate::paths::get().gbbq_file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 全历史前复权日线（每次返回逐 bar 拷贝，调用方可放心改）。
pub fn adjusted_day(market: &str, code: &str) -> Result<Vec<Bar>> {
    let path = day_path(market, code);
    if !path.exists() {
        return read_day(market, code);
    }
    let mtime = std::fs::metadata(&path)?.modified()?;
    // 缓存键含 gbbq mtime：事件文件更新而 .day 未变时旧因子必须作废
    let key = (market.to_string(), code.to_string(), gbbq_mtime_millis());
    {
        let mut mem = adj_mem().lock_ok();
        if let Some(e) = mem.get(&key) {
            if e.mtime == mtime {
                return Ok(e.bars.clone());
            }
        }
    }
    let mut bars = read_day(market, code)?;
    if !is_index(market, code) {
        apply_qfq(&mut bars, market, code)?;
    }
    {
        let mut mem = adj_mem().lock_ok();
        lru_put(&mut mem, key, AdjEntry { mtime, bars: bars.clone() }, Some(qfq_mem_max()));
    }
    Ok(bars)
}

/// 测试辅助：清空复权缓存。
pub fn clear_cache() {
    adj_mem().lock_ok().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qfq_factor_formula() {
        assert_eq!(calc_qfq_factor(10.0, 2.0, 3.0, 1.0, 5.0), 0.7357142857142857);
        assert_eq!(calc_qfq_factor(3.3, 1.2, 0.1, 0.0, 0.0), 0.9540954095409541);
        assert_eq!(calc_qfq_factor(0.0, 1.0, 0.0, 0.0, 0.0), 1.0);
        assert_eq!(calc_qfq_factor(7.5, 0.0, 0.0, 0.0, 0.0), 1.0);
    }

    #[test]
    fn bisect_matches_linear() {
        let bars: Vec<Bar> = (0..10)
            .map(|i| Bar::new(format!("2020-01-{:02}", i + 1), 0.0, 0.0, 0.0, i as f64, 0.0, 0.0))
            .collect();
        let keys: Vec<i32> = bars.iter().map(|b| date_key(&b.date)).collect();
        for k in 20200100..20200115 {
            assert_eq!(
                close_before_event(&keys, &bars, k, None),
                find_close_before_event(&bars, k, None)
            );
        }
        let ctx = bars[..3].to_vec();
        assert_eq!(close_before_event(&keys, &bars, 20200101, Some(&ctx)),
                   find_close_before_event(&bars, 20200101, Some(&ctx)));
    }
}
