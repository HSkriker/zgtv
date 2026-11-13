//! bar 数据结构与单位约定 —— 与 Python `zgcore/bars/model.py` 逐值等价。
//!
//! 单位（不可改，改了所有下游指标全错）：
//!   day    : open/high/low/close 元；volume 股；amount 元
//!   min    : 同上，价格为未复权原始价
//!   chart  : chart_bars 输出 vol 手（÷100）、amount 元
//!
//! 日期：
//!   day bar 的 date 为 "YYYY-MM-DD"
//!   min bar 的 date 为 "YYYY-MM-DD HH:MM"（分钟为 bar 结束时刻）

use serde::{Deserialize, Serialize};

pub const DATE_FMT: &str = "%Y-%m-%d";
pub const DATETIME_FMT: &str = "%Y-%m-%d %H:%M";
pub const SHARES_PER_LOT: f64 = 100.0;
pub const QFQ_PRICE_DECIMALS: u32 = 6;
pub const CHART_PRICE_DECIMALS: u32 = 3;

/// 一根 K 线（未复权原始价；`volume` 单位股、`amount` 单位元）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub amount: f64,
}

impl Bar {
    pub fn new(date: impl Into<String>, open: f64, high: f64, low: f64, close: f64,
               volume: f64, amount: f64) -> Self {
        Self { date: date.into(), open, high, low, close, volume, amount }
    }
}

/// 'YYYY-MM-DD[...]' → YYYYMMDD；解析失败 → 0（沿用旧 `_to_date_key`）。
pub fn date_key(value: &str) -> i32 {
    let head: String = value.chars().take(10).collect();
    let digits: String = head.chars().filter(|c| *c != '-').collect();
    digits.parse::<i32>().unwrap_or(0)
}

pub fn day_of(value: &str) -> String {
    value.chars().take(10).collect()
}

/// 'YYYY-MM-DD HH:MM' → 当日分钟数；越界/非数字按 Python int() 的失败口径返回 None。
pub fn minute_of_day(value: &str) -> Option<i32> {
    let hh = value.get(11..13)?.parse::<i32>().ok()?;
    let mm = value.get(14..16)?.parse::<i32>().ok()?;
    Some(hh * 60 + mm)
}

/// 'YYYY-MM-DD' → YYYYMMDD（Python `day_int`：不做合法性校验）。
pub fn day_int(s: &str) -> i32 {
    let g = |a: usize, b: usize| s.get(a..b).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    g(0, 4) * 10000 + g(5, 7) * 100 + g(8, 10)
}

pub fn day_str(i: i32) -> String {
    format!("{:04}-{:02}-{:02}", i / 10000, i / 100 % 100, i % 100)
}

/// ('YYYY-MM-DD HH:MM') → (YYYYMMDD, 当日分钟数)；解析失败返回 (0, 0)。
pub fn minute_parts(s: &str) -> (i32, i32) {
    (day_int(s), minute_of_day(s).unwrap_or(0))
}

pub fn minute_str(d: i32, m: i32) -> String {
    format!("{} {:02}:{:02}", day_str(d), m / 60, m % 60)
}

/// 日线重采样沿用输入的键名：read_day 用 volume，TQ 日线用 vol。
pub fn volume_key(bars: &[Bar]) -> bool {
    // true = volume 字段（本项目 Bar 只有 volume；保留函数以对齐 Python 语义）
    !bars.is_empty()
}

pub fn is_sorted_dates(bars: &[Bar]) -> bool {
    bars.windows(2).all(|w| date_key(&w[0].date) <= date_key(&w[1].date))
}

/// Python `round(x, n)` 的位级等价实现。
///
/// Python 的 round 是对二进制值做「十进制正确舍入（半偶）」，等价于用同样规则
/// 格式化到 n 位小数再解析回 f64。Rust 的 `{:.n}` 走正确舍入，因此这样最接近。
pub fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let s = format!("{:.*}", digits, x);
    s.parse::<f64>().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_helpers() {
        assert_eq!(date_key("2026-10-22"), 20261022);
        assert_eq!(date_key("2026-10-22 09:31"), 20261022);
        assert_eq!(date_key("bad"), 0);
        assert_eq!(minute_of_day("2026-10-22 09:31"), Some(571));
        assert_eq!(day_int("2026-10-22"), 20261022);
        assert_eq!(day_str(20261022), "2026-10-22");
        assert_eq!(minute_parts("2026-10-22 15:00"), (20261022, 900));
        assert_eq!(minute_str(20261022, 900), "2026-10-22 15:00");
    }

    #[test]
    fn round_matches_python() {
        // Python: round(2.675, 2) == 2.67（二进制值小于 2.675）
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(1.005, 2), 1.0);
        assert_eq!(py_round(10.1234565, 6), 10.123456);
    }
}
