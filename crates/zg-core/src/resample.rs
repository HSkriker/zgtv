//! 重采样 —— 日→周/月、1 分→N 分（与 `zgcore/bars/resample.py` 逐行等价）。
//!
//! 口径：
//!   - 周界 = ISO 周（周一起），月界 = 自然月；标签 = 区间最后一个交易日；
//!   - 分钟按通达信「桶结束分钟」分桶：09:31–11:30、13:01–15:00；
//!     15:00 那根含收盘集合竞价，勿拆；
//!   - OHLC 取首开/最高/最低/末收，vol/amount 求和。

use chrono::{Datelike, NaiveDate};

use crate::model::{minute_of_day, Bar};

pub const DWM_PERIODS: &[&str] = &["day", "week", "month"];

pub const AM_OPEN: i32 = 9 * 60 + 31;
pub const AM_CLOSE: i32 = 11 * 60 + 30;
pub const PM_OPEN: i32 = 13 * 60 + 1;
pub const PM_CLOSE: i32 = 15 * 60;

/// 日线 → 周/月；period 只接受 week/month。
pub fn resample_dwm(bars: &[Bar], period: &str) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut cur_key: Option<(i32, i32)> = None;
    let mut cur: Option<Bar> = None;

    for b in bars {
        let key = if period == "week" {
            let d = parse_date(&b.date);
            match d {
                Some(d) => {
                    let iso = d.iso_week();
                    (iso.year(), iso.week() as i32)
                }
                None => (0, 0),
            }
        } else {
            let y: i32 = b.date.get(0..4).and_then(|v| v.parse().ok()).unwrap_or(0);
            let m: i32 = b.date.get(5..7).and_then(|v| v.parse().ok()).unwrap_or(0);
            (y, m)
        };

        match cur.as_mut() {
            None => {
                cur = Some(b.clone());
                cur_key = Some(key);
            }
            Some(c) => {
                if Some(key) != cur_key {
                    out.push(c.clone());
                    *c = b.clone();
                    cur_key = Some(key);
                } else {
                    c.date = b.date.clone();
                    if b.high > c.high {
                        c.high = b.high;
                    }
                    if b.low < c.low {
                        c.low = b.low;
                    }
                    c.close = b.close;
                    c.volume += b.volume;
                    c.amount += b.amount;
                }
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    NaiveDate::from_ymd_opt(y, m, d)
}

/// 1 分钟时刻 t（当日分钟数）→ 所属 TDX 桶的结束分钟；不在交易时段 → None。
pub fn bucket_end(t: i32, step: i32) -> Option<i32> {
    let (base, cap) = if (AM_OPEN..=AM_CLOSE).contains(&t) {
        (AM_OPEN, AM_CLOSE)
    } else if (PM_OPEN..=PM_CLOSE).contains(&t) {
        (PM_OPEN, PM_CLOSE)
    } else {
        return None;
    };
    Some((base + ((t - base) / step + 1) * step - 1).min(cap))
}

/// 1 分钟 → N 分钟；minutes<=1 原样返回（同一对象，调用方不得改写）。
pub fn resample_min(bars: &[Bar], minutes: u32) -> Vec<Bar> {
    let step = minutes as i32;
    if step <= 1 {
        return bars.to_vec();
    }
    let mut out: Vec<Bar> = Vec::new();
    let mut cur_key: Option<String> = None;
    let mut cur: Option<Bar> = None;

    for b in bars {
        let t = match minute_of_day(&b.date) {
            Some(t) => t,
            None => continue,
        };
        let end = match bucket_end(t, step) {
            Some(e) => e,
            None => continue,
        };
        let key = format!("{}{:02}:{:02}", &b.date[..11], end / 60, end % 60);
        match cur.as_mut() {
            Some(c) if cur_key.as_deref() == Some(key.as_str()) => {
                if b.high > c.high {
                    c.high = b.high;
                }
                if b.low < c.low {
                    c.low = b.low;
                }
                c.close = b.close;
                c.amount += b.amount;
                c.volume += b.volume;
            }
            _ => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                let mut nb = b.clone();
                nb.date = key.clone();
                cur = Some(nb);
                cur_key = Some(key);
            }
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_edges_match_python() {
        // 与 tests/test_zgcore_bars.py::_bucket_end_old 同输入
        for t in [0, 570, 571, 600, 690, 691, 780, 781, 900, 901, 960] {
            for step in [5, 15, 30, 60] {
                let mut base_cap = None;
                if (AM_OPEN..=AM_CLOSE).contains(&t) {
                    base_cap = Some((AM_OPEN, AM_CLOSE));
                } else if (PM_OPEN..=PM_CLOSE).contains(&t) {
                    base_cap = Some((PM_OPEN, PM_CLOSE));
                }
                let want = base_cap.map(|(base, cap)| {
                    (base + ((t - base) / step + 1) * step - 1).min(cap)
                });
                assert_eq!(bucket_end(t, step), want, "t={t} step={step}");
            }
        }
    }

    #[test]
    fn week_and_month_labels() {
        let bars = vec![
            Bar::new("2026-10-19", 1.0, 2.0, 0.5, 1.5, 10.0, 100.0),
            Bar::new("2026-10-20", 1.5, 3.0, 1.0, 2.5, 20.0, 200.0),
            Bar::new("2026-10-26", 2.5, 4.0, 2.0, 3.5, 30.0, 300.0),
        ];
        let w = resample_dwm(&bars, "week");
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].date, "2026-10-20");
        assert_eq!(w[0].high, 3.0);
        assert_eq!(w[0].volume, 30.0);
        let m = resample_dwm(&bars, "month");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].close, 3.5);
    }
}
