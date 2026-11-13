//! zgcore.bars 对外入口 —— 与 `zgcore/bars/api.py` 同名同参：读/复权/重采样。

use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::adjust::{adjusted_day, apply_qfq_events, qfq_factor_by_day, FactorDoc};
use crate::error::{py_repr, Result, ZgError};
use crate::model::{
    date_key, py_round, Bar, CHART_PRICE_DECIMALS, QFQ_PRICE_DECIMALS, SHARES_PER_LOT,
};
use crate::reader::{
    day_path, is_index, lru_put, min_freq_minutes, read_day, read_day_tail, read_min, resolve,
    LruMap, MKT_BJ, MIN_BARS,
};
use crate::resample::{resample_dwm, DWM_PERIODS};
use crate::sync::MutexExt;

fn log(msg: &str) {
    eprintln!("[zgcore] {msg}");
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ChartBar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub vol: f64,
    pub amount: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MinuteMeta {
    pub source: String,
    pub last: String,
    pub n: usize,
}

/// 日/周/月 → 图表 K 线（vol 手、amount 元、价格 3 位小数）。
pub fn chart_bars(code: &str, period: &str, lmt: usize, beg: Option<&str>,
                  end: Option<&str>) -> Result<Vec<ChartBar>> {
    let p = period.to_lowercase();
    if !DWM_PERIODS.contains(&p.as_str()) {
        return Err(ZgError::bridge(format!("chart_bars 不支持的周期: {period}")));
    }
    let (mkt, cd) = resolve(code, None)?;
    if mkt == MKT_BJ {
        return Err(ZgError::bridge("北交所暂不支持本地日线"));
    }
    let mut bars: Vec<Bar> = adjusted_day(&mkt, &cd)?;
    if beg.map(|s| !s.is_empty()).unwrap_or(false) || end.map(|s| !s.is_empty()).unwrap_or(false) {
        let b0 = beg.filter(|s| !s.is_empty()).map(|s| s.replace('-', ""));
        let e0 = end.filter(|s| !s.is_empty()).map(|s| s.replace('-', ""));
        bars.retain(|b| {
            let d = b.date.replace('-', "");
            b0.as_ref().map(|v| d >= *v).unwrap_or(true)
                && e0.as_ref().map(|v| d <= *v).unwrap_or(true)
        });
        if bars.is_empty() {
            return Err(ZgError::bridge(format!(
                "区间过滤后无数据: {}~{}",
                beg.unwrap_or("None"),
                end.unwrap_or("None")
            )));
        }
    }
    if p != "day" {
        bars = resample_dwm(&bars, &p);
    }
    if lmt > 0 && bars.len() > lmt {
        bars = bars[bars.len() - lmt..].to_vec();
    }
    // 指数 .day 的 volume 本身就是「手」，再除 100 会把成交量缩小 100 倍（M22）。
    let lot_div = if is_index(&mkt, &cd) { 1.0 } else { SHARES_PER_LOT };
    Ok(bars
        .iter()
        .map(|b| ChartBar {
            date: b.date.clone(),
            open: py_round(b.open, CHART_PRICE_DECIMALS as usize),
            high: py_round(b.high, CHART_PRICE_DECIMALS as usize),
            low: py_round(b.low, CHART_PRICE_DECIMALS as usize),
            close: py_round(b.close, CHART_PRICE_DECIMALS as usize),
            vol: b.volume / lot_div,
            amount: b.amount,
        })
        .collect())
}

struct FactorEntry {
    mtime: std::time::SystemTime,
    doc: FactorDoc,
}

type FactorKey = (String, String, u64);

fn factor_mem() -> &'static Mutex<LruMap<FactorKey, FactorEntry>> {
    static MEM: OnceLock<Mutex<LruMap<FactorKey, FactorEntry>>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(LruMap::new(crate::reader::min_mem_max())))
}

/// 前复权分钟 bars + meta；因子按日线末根 mtime + gbbq mtime 缓存（gbbq 晚到不得用旧因子）。
pub fn minute_bars(market: &str, code: &str, freq: &str, count: usize)
    -> Result<(Vec<Bar>, MinuteMeta)> {
    let mkt = market.to_lowercase();
    if mkt == MKT_BJ {
        return Err(ZgError::bridge("北交所暂不支持"));
    }
    let minutes = match min_freq_minutes(freq) {
        Some(m) => m,
        None => {
            return Err(ZgError::bridge(format!(
                "不支持的分钟周期: {}（支持 F1/F5/F15/F30/F60）",
                py_repr(freq)
            )))
        }
    };
    let raw1 = read_min(&mkt, code)?;
    let mut bars: Vec<Bar> = if minutes == 1 {
        (*raw1).clone()
    } else {
        crate::resample::resample_min(&raw1, minutes)
    };
    if count > 0 && bars.len() > count {
        bars = bars[bars.len() - count..].to_vec();
    }
    if bars.is_empty() {
        return Err(ZgError::bridge("本地分钟数据为空"));
    }

    if !is_index(&mkt, code) {
        // 键含 gbbq mtime：事件文件更新而 .day 未变时旧因子必须作废
        let key = (mkt.clone(), code.to_string(), crate::adjust::gbbq_mtime_millis());
        // 日线可能尚未落地（只有 .lc1/.lc5 的新股或盘前）：此时无法复权，
        // 按「本地缺失优雅降级」返回未复权分钟线，而不是直接 IO 失败（M23）。
        let day_mtime = std::fs::metadata(day_path(&mkt, code))
            .ok()
            .and_then(|m| m.modified().ok());
        let mut doc: Option<FactorDoc> = None;
        if let Some(mt) = day_mtime {
            let cached: Option<FactorDoc> = {
                let mut mem = factor_mem().lock_ok();
                match mem.get(&key) {
                    Some(e) if Some(e.mtime) == day_mtime => Some(e.doc.clone()),
                    _ => None,
                }
            };
            let d = match cached {
                Some(d) => d,
                None => {
                    let d = qfq_factor_by_day(&mkt, code)?;
                    let mut mem = factor_mem().lock_ok();
                    lru_put(&mut mem, key, FactorEntry { mtime: mt, doc: d.clone() }, None);
                    d
                }
            };
            doc = Some(d);
        } else {
            crate::events::log_warn(&format!(
                "{mkt}{code} 无日线文件，分钟线按未复权返回（无法确认除权）"
            ));
        }
        if let Some(doc) = doc {
            if !doc.factors.is_empty() {
                apply_minute_factors(&mut bars, doc, &mkt, code)?;
            }
        }
    }

    if bars.len() < MIN_BARS {
        return Err(ZgError::bridge("数据不足60根"));
    }
    let last = bars[bars.len() - 1].date.clone();
    let n = bars.len();
    Ok((bars, MinuteMeta { source: "local".into(), last, n }))
}

fn apply_minute_factors(bars: &mut [Bar], doc: FactorDoc, mkt: &str, code: &str) -> Result<()> {
    let factors = doc.factors;
    let raw_close = doc.raw_close;
    let mut last_factor = 1.0f64;
    let mut last_day: Option<String> = None;
    let mut day_raw_close: Option<f64> = None;
    // 日线末根之后（盘中/当日）.day 尚未落地：因子表里没有该日，
    // 直接沿用上一日因子会在除权生效当日产生假跳空（M24）。
    let last_factor_day: Option<String> = factors.keys().max().cloned();
    let base_factor = last_factor_day
        .as_deref()
        .and_then(|d| factors.get(d))
        .copied()
        .unwrap_or(1.0);

    for b in bars.iter_mut() {
        let d10: String = b.date.chars().take(10).collect();
        if last_day.as_deref() != Some(d10.as_str()) {
            if let Some(ld) = last_day.as_ref() {
                if let (Some(want), Some(got)) = (raw_close.get(ld), day_raw_close) {
                    if (got - want).abs() > (0.01f64).max(want * 1e-4) {
                        return Err(ZgError::bridge(format!(
                            "{mkt}{code} 分钟线与日线不自洽：{ld} 分钟末根 {:.2} vs 日线 {:.2}",
                            got, want
                        )));
                    }
                }
            }
            last_factor = match factors.get(&d10) {
                Some(f) => *f,
                None => match last_factor_day.as_deref() {
                    Some(l) if d10.as_str() > l => {
                        post_gap_factor(mkt, code, &d10, l, base_factor, &raw_close)
                    }
                    _ => last_factor,
                },
            };
            last_day = Some(d10);
        }
        day_raw_close = Some(b.close);
        b.open *= last_factor;
        b.high *= last_factor;
        b.low *= last_factor;
        b.close *= last_factor;
    }
    if let Some(ld) = last_day.as_ref() {
        if let (Some(want), Some(got)) = (raw_close.get(ld), day_raw_close) {
            if (got - want).abs() > (0.01f64).max(want * 1e-4) {
                return Err(ZgError::bridge(format!(
                    "{mkt}{code} 分钟线与日线不自洽：{ld} 分钟末根 {:.2} vs 日线 {:.2}",
                    got, want
                )));
            }
        }
    }
    Ok(())
}

/// 日线覆盖范围之后某日的 qfq 因子：`factor(day) = factor(最后日) / ∏(最后日, day] 事件因子`。
fn post_gap_factor(mkt: &str, code: &str, day: &str, last_day: &str, base: f64,
                   raw_close: &std::collections::HashMap<String, f64>) -> f64 {
    let evs = match crate::events::events_of_detailed(mkt, code, Some(day)) {
        Ok((v, _)) => v,
        Err(e) => {
            crate::events::log_warn(&format!("{mkt}{code} {day} 除权事件不可用({e})，沿用旧因子"));
            return base;
        }
    };
    let lk = date_key(last_day);
    let dk = date_key(day);
    let mut f = base;
    let mut close_before = raw_close.get(last_day).copied().unwrap_or(0.0);
    for e in evs.iter().filter(|e| e.category == 1) {
        let ek = e.year * 10000 + e.month * 100 + e.day;
        if ek <= lk || ek > dk || close_before <= 0.0 {
            continue;
        }
        let ef = crate::adjust::calc_qfq_factor(
            close_before, e.fenhong, e.songzhuangu, e.peigu, e.peigujia,
        );
        f /= ef;
        close_before *= ef;
    }
    f
}

/// 便捷：复权全历史并返回（Python `apply_qfq` 的对外签名）。
///
/// 在复权之外补一步「取整补齐」，让**尾部窗口**与**全历史**口径逐值一致：
///
/// - `adjust_bars` 只在「有可解析的除权事件」时才把价格取整到 6 位小数（`QFQ_PRICE_DECIMALS`）；
/// - 尾部窗口（`read_day_tail`）把窗口外的老事件裁掉后，可能一个事件都解析不出来
///   ⇒ `factor_map` 为空 ⇒ 提前返回 ⇒ 少了这一步取整，
///   与「全历史读取后复权」逐值差 1 ULP（如 `28.740000000000002` vs `28.74`）；
/// - 只要该标的**存在生效中的 cat1 除权事件**，就把价格取整到 6 位；
///   `py_round` 幂等，已取整的值不受影响。
///
/// 全历史与尾部窗口走的是同一段补齐逻辑 ⇒ 两者逐值一致。
/// 事件表只取一次（`apply_qfq_events` 复用），不重复读除权缓存。
pub fn apply_qfq_bars(bars: &mut [Bar], market: &str, code: &str) -> Result<()> {
    if bars.is_empty() {
        return Ok(());
    }
    let anchor_date: String = bars[bars.len() - 1].date.chars().take(10).collect();
    let (events, source) = crate::events::events_of_detailed(market, code, Some(&anchor_date))?;
    let has_live_event = apply_qfq_events(bars, market, code, events, source)?;
    if has_live_event {
        let d = QFQ_PRICE_DECIMALS as usize;
        for bar in bars.iter_mut() {
            bar.open = py_round(bar.open, d);
            bar.high = py_round(bar.high, d);
            bar.low = py_round(bar.low, d);
            bar.close = py_round(bar.close, d);
        }
    }
    Ok(())
}

/// 便捷：读日线（未复权）。
pub fn read_day_bars(market: &str, code: &str) -> Result<Vec<Bar>> {
    read_day(market, code)
}

/// 便捷：只读日线尾部 `max_bars` 根（未复权；全市场扫描用）。
pub fn read_day_bars_tail(market: &str, code: &str, max_bars: usize) -> Result<Vec<Bar>> {
    read_day_tail(market, code, max_bars)
}

/// 便捷：读分钟线（未复权，Arc 共享）。
pub fn read_min_bars(market: &str, code: &str) -> Result<std::sync::Arc<Vec<Bar>>> {
    read_min(market, code)
}

/// 便捷：重采样（分钟）。
pub fn resample_minutes(bars: &[Bar], minutes: u32) -> Vec<Bar> {
    crate::resample::resample_min(bars, minutes)
}

/// 便捷：重采样（日→周/月）。
pub fn resample_dwm_bars(bars: &[Bar], period: &str) -> Vec<Bar> {
    resample_dwm(bars, period)
}

/// 兼容 Python `log` 调用点（诊断）。
pub fn log_line(msg: &str) {
    log(msg);
}
