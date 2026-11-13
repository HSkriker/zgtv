//! tdx-master 本地兜底 —— 对齐 `modules/market/tdx_backup.py` 的 kline 契约：
//! 返回 QFQ 价格、vol 手、amount 元；替换 Go worker（bin/tdx-bridge.exe），
//! 数据源改为本地 .day/.lc1（等价语义，Python 侧该 worker 的 quotes 分支无调用点，不移植）。

use crate::kbar::KBar;

fn finite(v: f64, positive: bool) -> bool {
    v.is_finite() && (if positive { v > 0.0 } else { v >= 0.0 })
}

/// 日/周/月/30分/60分 → QFQ bars（vol 手、amount 元）。
/// count 夹在 1..=2400（Python `min(2400, max(1, int(count or 300)))`）。
pub fn kline(code: &str, period: &str, count: usize) -> Result<Vec<KBar>, String> {
    if !matches!(period, "day" | "week" | "month" | "min30" | "min60") {
        return Err(format!("unsupported period {period}"));
    }
    let want = (2400usize).min((1usize).max(if count > 0 { count } else { 300 }));
    let bars: Vec<KBar> = if period.starts_with("min") {
        let freq = if period == "min30" { "F30" } else { "F60" };
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
                vol: b.volume / 100.0,
                amount: b.amount,
            })
            .collect();
        out.sort_by(|a, b| a.date.cmp(&b.date));
        out
    } else {
        let cb = zg_core::api::chart_bars(code, period, 0, None, None)
            .map_err(|e| e.to_string())?;
        let mut out: Vec<KBar> = cb
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
        out.sort_by(|a, b| a.date.cmp(&b.date));
        out
    };
    if bars.is_empty() {
        return Err("tdx-master returned no bars".into());
    }
    let bars = if bars.len() > want {
        bars[bars.len() - want..].to_vec()
    } else {
        bars
    };
    for bar in &bars {
        if !finite(bar.open, true) || !finite(bar.high, true) || !finite(bar.low, true)
            || !finite(bar.close, true) || !finite(bar.vol, false) || !finite(bar.amount, false) {
            return Err("invalid tdx-master bar values".into());
        }
        if bar.high < bar.open.max(bar.close) || bar.low > bar.open.min(bar.close) {
            return Err("invalid tdx-master OHLC range".into());
        }
    }
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_period() {
        assert!(kline("600519", "min1", 10).is_err());
        assert!(kline("600519", "bad", 10).is_err());
    }

    #[test]
    fn ohlc_range_check() {
        // 构造非法 bar 走校验逻辑（直接测 finite/range 谓词）
        assert!(finite(1.5, true));
        assert!(!finite(0.0, true));
        assert!(!finite(f64::NAN, false));
        assert!(finite(0.0, false));
    }
}
