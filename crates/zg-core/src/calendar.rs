//! 交易日/盘中判定 —— 纯本地、无网络。
//!
//! 口径与旧实现一致（8746 的 market_open_now / _last_trading_date）：
//!   - 盘中窗口含集合竞价前后缓冲：09:25–11:35、12:55–15:05；
//!   - 周末一律非交易日；
//!   - 法定假日从本地指数日线（sh000001.day）的历史交易日推断：落在数据范围内、
//!     又是工作日却没有 K 线的日期即为休市日（M27）。数据范围之外（今天/未来）无法
//!     预知，按工作日视为开市。

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::sync::MutexExt;

const AM: (&str, &str) = ("09:25", "11:35");
const PM: (&str, &str) = ("12:55", "15:05");

fn in_window(hm: &str, w: (&str, &str)) -> bool {
    w.0 <= hm && hm <= w.1
}

fn key_of(d: NaiveDate) -> i32 {
    d.year() * 10000 + d.month() as i32 * 100 + d.day() as i32
}

fn fmt_key(k: i32) -> String {
    format!("{:04}-{:02}-{:02}", k / 10000, k / 100 % 100, k % 100)
}

struct DaysCache {
    stamp: Option<(SystemTime, u64)>,
    days: Option<Arc<Vec<i32>>>,
}

fn days_cache() -> &'static Mutex<DaysCache> {
    static C: OnceLock<Mutex<DaysCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(DaysCache { stamp: None, days: None }))
}

/// 本地上证指数日线的全部交易日（升序去重），按文件 mtime+len 缓存。
fn trading_days() -> Option<Arc<Vec<i32>>> {
    let path = crate::reader::day_path("sh", "000001");
    let md = std::fs::metadata(&path).ok()?;
    let stamp = (md.modified().ok()?, md.len());
    {
        let g = days_cache().lock_ok();
        if g.stamp == Some(stamp) {
            return g.days.clone();
        }
    }
    let raw = std::fs::read(&path).ok()?;
    let mut days: Vec<i32> = raw
        .chunks_exact(32)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as i32)
        .filter(|k| (19900101..=21001231).contains(k))
        .collect();
    days.sort_unstable();
    days.dedup();
    let arc = Arc::new(days);
    let mut g = days_cache().lock_ok();
    *g = DaysCache { stamp: Some(stamp), days: Some(arc.clone()) };
    Some(arc)
}

/// 本地行情覆盖到的最近交易日（market_last_day）。缺失时 None（调用方回退日历/TQ）。
pub fn market_last_day() -> Option<String> {
    trading_days()?.last().copied().map(fmt_key)
}

/// 已确认休市（数据范围内的工作日缺口，即法定假日）。
fn known_closed(days: &[i32], d: NaiveDate) -> bool {
    match days.last() {
        Some(last) => {
            let k = key_of(d);
            k <= *last && days.binary_search(&k).is_err()
        }
        None => false,
    }
}

pub fn market_open_now(now: Option<chrono::DateTime<Local>>) -> bool {
    let now = now.unwrap_or_else(Local::now);
    if now.weekday().num_days_from_monday() >= 5 {
        return false;
    }
    let hm = format!("{:02}:{:02}", now.hour(), now.minute());
    if !(in_window(&hm, AM) || in_window(&hm, PM)) {
        return false;
    }
    if let Some(days) = trading_days() {
        if known_closed(&days, now.date_naive()) {
            return false;
        }
    }
    true
}

/// 最近交易日（跳过周末与已知法定假日），返回 "YYYY-MM-DD"。
pub fn last_trading_date(now: Option<chrono::DateTime<Local>>) -> String {
    let mut d = now.unwrap_or_else(Local::now).date_naive();
    let days = trading_days();
    for _ in 0..60 {
        if d.weekday().num_days_from_monday() >= 5 {
            d -= Duration::days(1);
            continue;
        }
        if let Some(days) = days.as_deref() {
            if known_closed(days, d) {
                d -= Duration::days(1);
                continue;
            }
        }
        break;
    }
    d.format("%Y-%m-%d").to_string()
}

/// 指定日期的「最近交易日」版本（仅跳过周末，测试用）。
pub fn last_trading_date_of(d: NaiveDate) -> String {
    let mut d = d;
    while d.weekday().num_days_from_monday() >= 5 {
        d -= Duration::days(1);
    }
    d.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn weekend_is_closed() {
        // 2026-10-24 是周六，2026-10-23 周五
        let sat = Local.with_ymd_and_hms(2026, 10, 24, 10, 0, 0).unwrap();
        assert!(!market_open_now(Some(sat)));
        assert_eq!(last_trading_date(Some(sat)), "2026-10-23");
    }

    #[test]
    fn intraday_windows() {
        let t = |h, m| Local.with_ymd_and_hms(2026, 10, 23, h, m, 0).unwrap();
        assert!(!market_open_now(Some(t(9, 24))));
        assert!(market_open_now(Some(t(9, 25))));
        assert!(market_open_now(Some(t(11, 35))));
        assert!(!market_open_now(Some(t(11, 36))));
        assert!(market_open_now(Some(t(12, 55))));
        assert!(market_open_now(Some(t(15, 5))));
        assert!(!market_open_now(Some(t(15, 6))));
    }

    #[test]
    fn known_closed_only_inside_data_range() {
        let days = [20260930, 20261009];
        let holiday = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let future = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        assert!(known_closed(&days, holiday));
        assert!(!known_closed(&days, future));
    }
}
