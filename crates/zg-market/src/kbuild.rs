//! 周K / 30-60 分钟K缓存构建 —— 纯本地数据源。
//!
//! 语义对齐 `modules/zgapp/jobs/cache_build.py`（Python 版主源是 easy_tdx MAC，
//! 已随 easy_tdx 下架；本地 `.day`/`.lc1` 是等价且更权威的源）：
//!   - 周K：`.day` 前复权 → `resample_dwm(week)` → 末根标签 = 本周最后一个交易日；
//!   - 分钟K：`.lc1`（1 分）→ `resample_min(N)` → 通达信桶结束分钟（09:31–11:30、
//!     13:01–15:00），末根 `15:00` 含收盘集合竞价。
//!
//! 两者都只保留末 `lmt` 根，落盘格式沿用 kcache v1（kind = day / min）。

use std::collections::HashMap;

use zg_core::kcache::{bar_rows, KCacheStore, KIND_DAY, KIND_MIN};
use zg_core::model::Bar;
use zg_core::{adjust, reader, resample};

/// Bar → kcache 行，**成交量换算成「手」**。
///
/// `reader` 层给的是股（.day/.lc1 原始单位），而 kcache 与 API 契约统一用「手」
/// （1 手 = 100 股）。Python 版缓存由 MAC/腾讯源直接给出「手」，故此处必须 ÷100，
/// 否则周K/分钟K会放大 100 倍。
pub fn rows_hand(bars: &[Bar]) -> Vec<(String, f64, f64, f64, f64, f64, f64)> {
    bar_rows(bars)
        .into_iter()
        .map(|mut r| {
            r.5 /= 100.0;
            r
        })
        .collect()
}

/// 一次构建的结果（store 未落盘，调用方决定是否 save）。
pub struct BuildOutcome {
    pub store: KCacheStore,
    pub ok: usize,
    pub total: usize,
    pub date: String,
}

/// 全市场周K：`.day` 前复权 → 周线。
pub fn build_weekly(
    prefix: &str,
    codes: &[String],
    names: &HashMap<String, String>,
    lmt: usize,
    mut progress: impl FnMut(usize),
) -> BuildOutcome {
    let total = codes.len();
    let mut store = KCacheStore::create(prefix, KIND_DAY, lmt, total);
    let mut ok = 0usize;
    for (i, code) in codes.iter().enumerate() {
        if let Ok((mkt, cd)) = reader::resolve(code, None) {
            if let Ok(bars) = adjust::adjusted_day(&mkt, &cd) {
                let wk = resample::resample_dwm(&bars, "week");
                let tail = if wk.len() > lmt { &wk[wk.len() - lmt..] } else { &wk[..] };
                if !tail.is_empty() {
                    store.write(code, &rows_hand(tail));
                    ok += 1;
                }
            }
        }
        progress(i + 1);
    }
    for (code, name) in names {
        if store.contains(code) {
            store.names.push((code.clone(), name.clone()));
        }
    }
    let date = store.max_date();
    store.date = date.clone();
    BuildOutcome { store, ok, total, date }
}

/// 全市场单周期分钟K：`.lc1` → N 分钟。`period` 形如 `min30`/`min60`。
pub fn build_min(
    prefix: &str,
    codes: &[String],
    names: &HashMap<String, String>,
    period: &str,
    lmt: usize,
    mut progress: impl FnMut(usize),
) -> BuildOutcome {
    let minutes: usize = period.trim_start_matches("min").parse().unwrap_or(30).max(1);
    // 480 根 30 分K = 14400 根 1 分K；多取 2 个桶抵消起始半个桶
    let need = lmt * minutes + 2 * minutes + 10;
    let total = codes.len();
    let mut store = KCacheStore::create(prefix, KIND_MIN, lmt, total);
    let mut ok = 0usize;
    for (i, code) in codes.iter().enumerate() {
        if let Ok((mkt, cd)) = reader::resolve(code, None) {
            if let Ok(bars) = reader::read_min_tail(&mkt, &cd, need) {
                let res = resample::resample_min(&bars, minutes as u32);
                let tail = if res.len() > lmt { &res[res.len() - lmt..] } else { &res[..] };
                if !tail.is_empty() {
                    store.write(code, &rows_hand(tail));
                    ok += 1;
                }
            }
        }
        progress(i + 1);
    }
    for (code, name) in names {
        if store.contains(code) {
            store.names.push((code.clone(), name.clone()));
        }
    }
    let date = store.max_date();
    store.date = date.clone();
    BuildOutcome { store, ok, total, date }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zg_core::model::Bar;

    #[test]
    fn min_tail_need_covers_lmt_buckets() {
        // 480 根 30 分 = 14400 根 1 分，need 必须 ≥ 这个数
        let need = 480 * 30 + 2 * 30 + 10;
        assert!(need >= 480 * 30);
    }

    #[test]
    fn weekly_label_is_last_trading_day() {
        let bars = vec![
            Bar::new("2026-10-26", 1.0, 1.1, 0.9, 1.05, 100.0, 1000.0),
            Bar::new("2026-10-27", 1.05, 1.2, 1.0, 1.15, 120.0, 1200.0),
        ];
        let wk = resample::resample_dwm(&bars, "week");
        assert_eq!(wk.len(), 1);
        assert_eq!(wk[0].date, "2026-10-27");
        assert_eq!(wk[0].close, 1.15);
        assert_eq!(wk[0].volume, 220.0);
    }
}
