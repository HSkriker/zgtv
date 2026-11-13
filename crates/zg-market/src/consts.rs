//! 配置常量 —— 对齐 `modules/zgapp/config.py`（env 读取语义同 Python import 时求值）。

use std::sync::OnceLock;

pub const DEFAULT_PORT: u16 = 8786;

pub const MINUTE_PERIODS: &[&str] = &["min1", "min5", "min15", "min30", "min60"];

pub const MINUTE_LMT: &[(&str, usize)] = &[
    ("min1", 1200),
    ("min5", 800),
    ("min15", 480),
    ("min30", 480),
    ("min60", 480),
];

pub const MINUTE_FREQ: &[(&str, &str)] = &[
    ("min1", "F1"),
    ("min5", "F5"),
    ("min15", "F15"),
    ("min30", "F30"),
    ("min60", "F60"),
];

pub const TQ_PITCH: &[(&str, &str)] = &[
    ("min1", "1m"),
    ("min5", "5m"),
    ("min15", "15m"),
    ("min30", "30m"),
    ("min60", "60m"),
];

pub const MIN_PITCH: &[(&str, u32)] = &[
    ("min1", 1),
    ("min5", 5),
    ("min15", 15),
    ("min30", 30),
    ("min60", 60),
];

pub const WEEKLY_K_LMT: usize = 200;
pub const MIN_K_PERIODS: &[&str] = &["min30", "min60"];
pub const MIN_K_LMT: &[(&str, usize)] = &[("min30", 480), ("min60", 480)];
pub const DAILY_K_LMT: usize = 480;
pub const LIVE_CYCLE: u64 = 60;

pub const MIN_K_BOUNDARIES: &[(&str, &[&str])] = &[
    ("min30", &["10:00", "10:30", "11:00", "11:30", "13:30", "14:00", "14:30", "15:00"]),
    ("min60", &["10:30", "11:30", "14:00", "15:00"]),
];

/// Python `BJ_CODE_RE = ^(43|83|87|92)`。
pub fn is_bj_code(code: &str) -> bool {
    code.starts_with("43") || code.starts_with("83") || code.starts_with("87")
        || code.starts_with("92")
}

fn env_flag(key: &str, default_on: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => v.trim() != "0",
        Err(_) => default_on,
    }
}

fn env_str(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

pub fn kline_local_on() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("KLINE_LOCAL", true))
}

pub fn min_local_on() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("MIN_LOCAL", true))
}

pub fn candle_engine() -> String {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let e = env_str("CANDLE_ENGINE", "talib").to_lowercase();
        if e == "legacy" { "legacy".into() } else { "talib".into() }
    })
    .clone()
}

pub fn candle_patterns() -> String {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let p = env_str("CANDLE_PATTERNS", "rec27").to_lowercase();
        if p == "rec20" { "rec27".into() }
        else if p == "all" { "all".into() }
        else { "rec27".into() }
    })
    .clone()
}

pub fn job_daily() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("ZG_JOB_DAILY", true))
}

pub fn job_weekly() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("ZG_JOB_WEEKLY", true))
}

pub fn job_min() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("ZG_JOB_MIN", true))
}

pub fn job_live() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("ZG_JOB_LIVE", true))
}

pub fn job_list() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_flag("ZG_JOB_LIST", true))
}

/// 同花顺数据源可用性（Python `fetch_boards._FB is not None` 的等价判据）。
pub fn ths_available() -> bool {
    crate::ths::api_key().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bj_code_re() {
        assert!(is_bj_code("430047"));
        assert!(is_bj_code("830799"));
        assert!(is_bj_code("871981"));
        assert!(is_bj_code("920001"));
        assert!(!is_bj_code("600519"));
        assert!(!is_bj_code("000001"));
    }
}
