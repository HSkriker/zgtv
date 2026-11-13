//! `zgbridge.service` 的 Rust 移植：桥数据适配层（zg-core 取数 → czsc RawBar）。
//!
//! 与 Python 一一对应：
//!   `bars_to_raw`             code → `.day` 全历史 → 前复权 → 周/月重采样
//!   `build_czsc`              code → (CZSC, RawBar 列表)
//!   `raw_from_inline_bars`    analyze 内联 bars → RawBar（不过复权、不重采样）
//!
//! 口径纪律：W/M 重采样必须走 `czsc_utils::resample::resample_bars`
//! （Python 侧就是 czsc 的 `resample_bars`），**不用** zg-core 的 `resample_dwm`。

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use czsc_core::analyze::CZSC;
use czsc_core::objects::bar::RawBar;
use czsc_core::objects::freq::Freq;
use czsc_utils::resample::resample_bars;
use serde::Deserialize;
use zg_core::{adjust, reader};

/// Python `reader.MIN_BARS`。
pub const MIN_BARS: usize = 60;
pub const MKT_BJ: &str = "bj";
/// Python `CZSC` 默认 `min_bi_len`（P0.5 实测）。
pub const MIN_BI_LEN: usize = 6;

/// 协议 freq → czsc `Freq`（与 `analyze._FREQ_ATTR` 同表）。
pub fn freq_of(name: &str) -> Option<Freq> {
    match name {
        "D" => Some(Freq::D),
        "W" => Some(Freq::W),
        "M" => Some(Freq::M),
        "F1" => Some(Freq::F1),
        "F5" => Some(Freq::F5),
        "F15" => Some(Freq::F15),
        "F30" => Some(Freq::F30),
        "F60" => Some(Freq::F60),
        _ => None,
    }
}

/// 内联 bars 的一行（契约：date/open/close/high/low/vol/amount）。
#[derive(Clone, Debug, Deserialize)]
pub struct InlineBar {
    pub date: String,
    pub open: f64,
    pub close: f64,
    pub high: f64,
    pub low: f64,
    pub vol: f64,
    pub amount: f64,
}

/// `pd.to_datetime` 的窄口径实现：`YYYY-MM-DD[ HH:MM[:SS]]` + RFC3339。
pub fn parse_dt(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    let date_part: String = s.chars().take(10).collect();
    let d = NaiveDate::parse_from_str(&date_part, "%Y-%m-%d").ok()?;
    let rest = s.get(10..).unwrap_or("").trim();
    if rest.is_empty() {
        return Some(DateTime::from_naive_utc_and_offset(d.and_hms_opt(0, 0, 0)?, Utc));
    }
    for fmt in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(rest, fmt) {
            return Some(DateTime::from_naive_utc_and_offset(d.and_time(t), Utc));
        }
    }
    // 退化：接受 "YYYY-MM-DDTHH:MM:SS"
    NaiveDateTime::parse_from_str(&s.replace('T', " "), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|x| DateTime::from_naive_utc_and_offset(x, Utc))
}

fn raw_of(b: &zg_core::Bar, freq: Freq, symbol: &str) -> Option<RawBar> {
    let dt = parse_dt(&b.date)?;
    Some(RawBar {
        symbol: Arc::from(symbol),
        dt,
        freq,
        id: 0, // 生产链路 RawBar.id 恒 0（format_standard_kline 不传 id）
        open: b.open,
        close: b.close,
        high: b.high,
        low: b.low,
        vol: b.volume,
        amount: b.amount,
    })
}

/// code → czsc RawBar（`.day` 全历史 → 前复权 → 周/月重采样）。
pub fn bars_to_raw(
    code: &str,
    freq: &str,
    market: Option<&str>,
) -> Result<Vec<RawBar>, String> {
    let (mkt, cd) = reader::resolve(code, market).map_err(|e| e.to_string())?;
    if mkt == MKT_BJ {
        return Err("北交所暂不支持".to_string());
    }
    let f = freq.to_uppercase();
    let target = match f.as_str() {
        "D" => Freq::D,
        "W" => Freq::W,
        "M" => Freq::M,
        _ => {
            return Err(format!(
                "不支持的周期: {}（只支持 D/W/M）",
                zg_core::py_repr(freq)
            ))
        }
    };
    let bars = adjust::adjusted_day(&mkt, &cd).map_err(|e| e.to_string())?;
    let mut raw: Vec<RawBar> = bars
        .iter()
        .filter_map(|b| raw_of(b, Freq::D, &cd))
        .collect();
    if matches!(target, Freq::W | Freq::M) {
        raw = resample_bars(&raw, target, true).map_err(|e| e.to_string())?;
    }
    if raw.len() < MIN_BARS {
        return Err("数据不足60根".to_string());
    }
    Ok(raw)
}

/// code → (CZSC 对象, RawBar 列表)。
pub fn build_czsc(
    code: &str,
    freq: &str,
    market: Option<&str>,
) -> Result<(CZSC, Vec<RawBar>), String> {
    let raw = bars_to_raw(code, freq, market)?;
    let c = CZSC::new(raw.clone(), reader::max_bi_num_for(freq), MIN_BI_LEN);
    Ok((c, raw))
}

/// `analyze.build_payload_from_bars` 的取数段：内联 bars → RawBar（已排序）。
pub fn raw_from_inline_bars(
    code: &str,
    freq: &str,
    bars: &[InlineBar],
    market: Option<&str>,
) -> Result<(Vec<RawBar>, String, String), String> {
    let (mkt, cd) = reader::resolve(code, market).map_err(|e| e.to_string())?;
    if mkt == MKT_BJ {
        return Err("北交所暂不支持".to_string());
    }
    let f = freq.to_uppercase();
    let Some(target) = freq_of(&f) else {
        return Err(format!("不支持的周期: {}（只支持 D/W/M）", zg_core::py_repr(freq)));
    };
    if bars.len() < MIN_BARS {
        return Err(format!("内联数据不足{}根: {}", MIN_BARS, bars.len()));
    }
    let mut rows: Vec<(DateTime<Utc>, RawBar)> = Vec::with_capacity(bars.len());
    for b in bars {
        let dt = parse_dt(&b.date)
            .ok_or_else(|| format!("无法解析日期: {}", zg_core::py_repr(&b.date)))?;
        rows.push((
            dt,
            RawBar {
                symbol: Arc::from(cd.as_str()),
                dt,
                freq: target,
                id: 0,
                open: b.open,
                close: b.close,
                high: b.high,
                low: b.low,
                vol: b.vol,
                amount: b.amount,
            },
        ));
    }
    // Python `df.sort_values("dt")`（稳定排序，同刻保持输入顺序）
    rows.sort_by_key(|r| r.0);
    let raw: Vec<RawBar> = rows.into_iter().map(|(_, b)| b).collect();
    Ok((raw, cd, mkt))
}

/// 内联 bars 的完整载荷入口（排序 + CZSC + 组装），供 CLI/serve 复用。
pub fn czsc_from_inline(
    code: &str,
    freq: &str,
    bars: &[InlineBar],
    market: Option<&str>,
) -> Result<(CZSC, Vec<RawBar>, String, String), String> {
    let (raw, cd, mkt) = raw_from_inline_bars(code, freq, bars, market)?;
    let c = CZSC::new(raw.clone(), reader::max_bi_num_for(freq), MIN_BI_LEN);
    Ok((c, raw, cd, mkt))
}
