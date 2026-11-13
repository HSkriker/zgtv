//! 日/周/月/分钟前复权 bars —— 本地替代 easy_tdx MAC（`modules/zgapp/mac_source.py`）。
//!
//! 行为变化（门 2 MAC 替换专项对表）：数据源从 easy_tdx MAC 网络接口换成
//! 本地 .day/.lc1 + QFQ（zg-core）。输出契约不变：
//! - 价格 = QFQ 前复权；
//! - **vol 一律「手」（1 手 = 100 股）**：日/分钟源数据是股 → ÷100；周/月由日线(股)
//!   重采样求和后同样 ÷100（不再沿用 easy_tdx「周/月本来就是手」的特例，本地数据无此
//!   保证，统一口径见用户 2026-10-27 指令）；
//! - amount 元。
//!
//! 失败冷却 60s（对齐 `_MAC_FAIL`/`_MAC_COOLDOWN`）。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::kbar::KBar;

const MAC_COOLDOWN: f64 = 60.0;

/// 熔断键 = (code, period)。单只坏票（新股无 K 线、代码写错）只冷却自己，
/// 不能像旧实现那样用全局单值封死所有标的。
static MAC_FAIL_UNTIL: Mutex<Option<HashMap<(String, String), f64>>> = Mutex::new(None);

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn mac_key(code: &str, period: &str) -> (String, String) {
    (code.trim().to_string(), period.to_string())
}

fn _mac_check(key: &(String, String)) -> Result<(), String> {
    let g = MAC_FAIL_UNTIL.lock_ok();
    if let Some(m) = g.as_ref() {
        if let Some(until) = m.get(key) {
            if now() < *until {
                return Err(format!("MAC 已关闭或冷却中({} {})", key.0, key.1));
            }
        }
    }
    Ok(())
}

fn _mac_fail(key: &(String, String)) {
    let mut g = MAC_FAIL_UNTIL.lock_ok();
    let m = g.get_or_insert_with(HashMap::new);
    let t = now();
    m.retain(|_, until| *until > t);
    m.insert(key.clone(), t + MAC_COOLDOWN);
}

fn _bars_of_day(mkt: &str, cd: &str, period: &str) -> Result<Vec<KBar>, String> {
    let bars = zg_core::adjust::adjusted_day(mkt, cd).map_err(|e| e.to_string())?;
    let mut kbars: Vec<KBar> = bars
        .iter()
        .map(|b| KBar {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.volume / 100.0, // 股 → 手（统一 1 手 = 100 股）
            amount: b.amount,
        })
        .collect();
    if period != "day" {
        let resampled = zg_core::resample::resample_dwm(&bars, period);
        kbars = resampled
            .iter()
            .map(|b| KBar {
                date: b.date.clone(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                vol: b.volume / 100.0, // 日线(股)求和 → 手，统一口径
                amount: b.amount,
            })
            .collect();
    }
    kbars.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(kbars)
}

/// 日/周/月前复权 bars；beg 给定时返回 beg 起全部，否则取最后 lmt 根。
pub fn get_dwm(code: &str, period: &str, lmt: usize, beg: Option<&str>)
    -> Result<Vec<KBar>, String> {
    let key = mac_key(code, period);
    _mac_check(&key)?;
    let result = (|| -> Result<Vec<KBar>, String> {
        let (mkt, cd) = zg_core::reader::resolve(code, None).map_err(|e| e.to_string())?;
        let mut bars = _bars_of_day(&mkt, &cd, period)?;
        if let Some(b) = beg {
            // beg 由 CLI 原样透传，短于 8 位/非 ASCII 的切片会 panic（M15）。
            let digits: String = b.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() < 8 {
                return Err(format!("beg 需为 YYYYMMDD：{b:?}"));
            }
            let bstr = format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8]);
            bars.retain(|x| x.date >= bstr);
        } else if lmt > 0 && bars.len() > lmt {
            bars = bars[bars.len() - lmt..].to_vec();
        }
        if bars.is_empty() {
            return Err("easy_tdx MAC K线为空".into());
        }
        Ok(bars)
    })();
    match result {
        Err(e) => {
            _mac_fail(&key);
            Err(e)
        }
        Ok(b) => Ok(b),
    }
}

/// 分钟前复权 bars（13:00 午休尾根已规范化）。
pub fn get_min(code: &str, period: &str, count: usize) -> Result<Vec<KBar>, String> {
    let key = mac_key(code, period);
    _mac_check(&key)?;
    let result = (|| -> Result<Vec<KBar>, String> {
        let want = if count > 0 { count } else { 800 };
        let freq = match period {
            "min1" => "F1",
            "min5" => "F5",
            "min15" => "F15",
            "min30" => "F30",
            "min60" => "F60",
            _ => return Err(format!("unsupported period {period}")),
        };
        let (mkt, cd) = zg_core::reader::resolve(code, None).map_err(|e| e.to_string())?;
        let (lbars, _meta) = zg_core::api::minute_bars(&mkt, &cd, freq, want)
            .map_err(|e| e.to_string())?;
        let mut out: Vec<KBar> = lbars
            .iter()
            .map(|b| KBar {
                date: b.date.clone(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                vol: b.volume / 100.0, // 股 → 手（统一 1 手 = 100 股）
                amount: b.amount,
            })
            .collect();
        out.sort_by(|a, b| a.date.cmp(&b.date));
        if out.is_empty() {
            return Err("easy_tdx MAC 分钟K线为空".into());
        }
        Ok(crate::market_data::normalize_minute_bars(&out, period))
    })();
    match result {
        Err(e) => {
            _mac_fail(&key);
            Err(e)
        }
        Ok(b) => Ok(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_isolated_per_key() {
        *MAC_FAIL_UNTIL.lock_ok() = None;
        _mac_fail(&mac_key("600519", "day"));

        // 同一 (code, period) 命中冷却
        let r = get_dwm("600519", "day", 10, None);
        assert!(r.unwrap_err().contains("冷却中"));

        // 同票不同周期、不同票都不得被牵连（旧全局单值实现会一起封死）
        let other = get_dwm("600519", "week", 10, None);
        assert!(!other.unwrap_err().contains("冷却中"));
        let other = get_dwm("000001", "day", 10, None);
        assert!(!other.unwrap_err().contains("冷却中"));

        *MAC_FAIL_UNTIL.lock_ok() = None;
    }
}
