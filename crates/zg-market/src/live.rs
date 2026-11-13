//! 8786 盘中实时行情取数（`modules/zgapp/jobs/live.py` 的取数段）。
//!
//! 三源降级：TQ（50 只/批）→ 同花顺快照 → 腾讯快照。
//! 只返回 `{code6: {price, open, high, low, preClose, vol, amount}}`，
//! 判定与落盘（stocks/pct/预警）留在 zg-api 的 live 循环里。

use std::collections::HashMap;

use serde_json::{json, Value};

/// 腾讯行情自带时间戳 → 「行情滞后秒数」。
///
/// Python `tencent_quote_age`：`qt.gtimg.cn/q=sh600519` 的第 31 字段
/// （`~` 分隔，0 基 f[30]）是 `%Y%m%d%H%M%S`；解析失败返回 None（前端显示「—」）。
pub fn tencent_quote_age() -> Option<f64> {
    let raw = crate::kline::http_get_raw("https://qt.gtimg.cn/q=sh600519", 8, &[]).ok()?;
    let txt = crate::quotes::decode_gbk_lossy(&raw);
    let body = txt.split('"').nth(1)?;
    let f: Vec<&str> = body.split('~').collect();
    let ts = f.get(30)?.trim();
    let dt = chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%d%H%M%S").ok()?;
    use chrono::TimeZone;
    let qt = chrono::Local.from_local_datetime(&dt).single()?;
    let age = (chrono::Local::now() - qt).num_milliseconds() as f64 / 1000.0;
    // Python round(max(0.0, age), 1)
    Some((age.max(0.0) * 10.0).round() / 10.0)
}

/// 批量实时行情：TQ → 同花顺 → 腾讯。返回 (行情, 源标签)。
///
/// 源标签与 Python 一致：`tq` / `ths` / `tencent`；全部为空时仍返回 `tencent`
/// （Python 在腾讯分支无条件置 src，即使 rows 为空）。
pub fn fetch_live(codes: &[String]) -> (HashMap<String, Value>, Option<&'static str>) {
    if codes.is_empty() {
        return (HashMap::new(), None);
    }
    // 主源：TQ get_market_data 50 只/批；available() 内含探活缓存与熔断
    if crate::tq::available() {
        if let Ok(q) = crate::tq::quotes(codes, "front", true) {
            if !q.is_empty() {
                return (q, Some("tq"));
            }
        }
    }
    // 次源：同花顺快照（200 只/批，含坏码剔除）
    if crate::consts::ths_available() {
        let q = crate::quotes::fetch_snapshot_ths(codes);
        if !q.is_empty() {
            return (q, Some("ths"));
        }
    }
    // 末源：腾讯（180 只/批）
    let rows = crate::quotes::fetch_snapshot_tencent(codes);
    let mut out = HashMap::new();
    for r in &rows {
        let Some(code) = r.get("f12").and_then(|v| v.as_str()) else {
            continue;
        };
        if code.is_empty() {
            continue;
        }
        out.insert(
            code.to_string(),
            json!({
                "price": r.get("f2").cloned().unwrap_or(Value::Null),
                "open": r.get("f17").cloned().unwrap_or(Value::Null),
                "high": r.get("f15").cloned().unwrap_or(Value::Null),
                "low": r.get("f16").cloned().unwrap_or(Value::Null),
                "preClose": r.get("f18").cloned().unwrap_or(Value::Null),
                "vol": r.get("f5").cloned().unwrap_or(Value::Null),
                "amount": 0,
            }),
        );
    }
    (out, Some("tencent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_codes_returns_empty() {
        let (q, src) = fetch_live(&[]);
        assert!(q.is_empty());
        assert!(src.is_none());
    }
}
