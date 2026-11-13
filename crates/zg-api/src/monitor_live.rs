//! 8787 实时层 —— 对齐 `modules/monitor_app/services/live.py`（个股快照部分）。
//! 注：新浪期货 `fetch_futures` 随 commodity 功能按用户裁定（2026-10-27）一并下线。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use zg_core::sync::MutexExt;

use crate::monitor_ths as ths;

/// 后台轮询单轮隔离：panic 只丢本轮、不杀线程（对齐 Python `except Exception: pass`）。
pub(crate) fn guard_round<T>(tag: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        eprintln!("[{tag}] 单轮异常已隔离: {msg}");
        msg
    })
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `in_trading`：周内 09:20–15:05。
pub fn in_trading() -> bool {
    use chrono::{Datelike, Timelike};
    let lt = chrono::Local::now();
    let weekday = lt.weekday().num_days_from_monday();
    let m = lt.hour() * 60 + lt.minute();
    weekday < 5 && (9 * 60 + 20..=15 * 60 + 5).contains(&m)
}

/// `LIVE` 全局快照（ts/quotes；8787 /api/state 读取）。
pub static LIVE: Mutex<(f64, Value)> = Mutex::new((0.0, Value::Null));

/// `/api/state` 的 live 块（ts 为秒级时间戳；quotes 为 {code: 行情}）。
pub fn live_snapshot() -> Value {
    let g = LIVE.lock_ok();
    let quotes = if g.1.is_null() { serde_json::json!({}) } else { g.1.clone() };
    serde_json::json!({"ts": g.0, "quotes": quotes})
}

/// `fetch_live`：个股实时行情（同花顺主源）+ 主力资金（东财兜底）。
/// codes: (code6, name) 列表。
pub fn fetch_live(codes: &[(String, String)]) -> HashMap<String, Value> {
    let mut out: HashMap<String, Value> = HashMap::new();
    if codes.is_empty() {
        return out;
    }
    let name_map: HashMap<&str, &str> =
        codes.iter().map(|(c, n)| (c.as_str(), n.as_str())).collect();
    let thscodes: Vec<String> = codes.iter().map(|(c, _)| ths::to_thscode(c)).collect();
    let mut ok = true;
    if zg_market::ths::enabled() {
    for seg in thscodes.chunks(200) {
        let params = vec![("thscodes", seg.join(","))];
        match ths::ths_get("/api/a-share/prices/snapshot", &params, 12, 2, 1) {
            Ok(d) => {
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let tscode = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let code: String = tscode.chars().take_while(|c| *c != '.').collect();
                    if code.is_empty() {
                        continue;
                    }
                    let f = |k: &str| ths::number(it.get(k)).unwrap_or(0.0);
                    let price = f("last_price");
                    let prev = f("prev_price");
                    let high = f("high_price");
                    let low = f("low_price");
                    let openp = f("open_price");
                    let amp = if prev > 0.0 {
                        zg_core::model::py_round((high - low) / prev * 100.0, 2)
                    } else {
                        0.0
                    };
                    out.insert(
                        code.clone(),
                        json!({
                            "name": name_map.get(code.as_str()).copied().unwrap_or(""),
                            "price": price,
                            "pct": zg_core::model::py_round(f("price_change_ratio_pct"), 2),
                            "vol_ratio": Value::Null,
                            "amp": amp,
                            "turnover": f("turnover"),
                            "open": openp, "high": high, "low": low, "prev": prev,
                        }),
                    );
                }
            }
            Err(e) => {
                eprintln!("[live] ths snapshot fail: {e}");
                ok = false;
                break;
            }
        }
    }
    }
    let _ = ok;
    if out.is_empty() {
        // 同花顺未配置或整体失败：降级到 zg-market 的 TQ→腾讯 链（与 8786 live 同款），
        // 不再每轮打 "ths snapshot fail" 且行情为空
        let code6: Vec<String> = codes.iter().map(|(c, _)| c.clone()).collect();
        let (q, _src) = zg_market::live::fetch_live(&code6);
        let fnum = |v: Option<&Value>| v.and_then(|x| x.as_f64()).unwrap_or(0.0);
        for (code, v) in q {
            let price = fnum(v.get("price"));
            let prev = fnum(v.get("preClose"));
            if price <= 0.0 {
                continue;
            }
            let high = fnum(v.get("high"));
            let low = fnum(v.get("low"));
            let openp = fnum(v.get("open"));
            let amp = if prev > 0.0 {
                zg_core::model::py_round((high - low) / prev * 100.0, 2)
            } else {
                0.0
            };
            out.insert(
                code.clone(),
                json!({
                    "name": name_map.get(code.as_str()).copied().unwrap_or(""),
                    "price": price,
                    "pct": if prev > 0.0 { zg_core::model::py_round((price / prev - 1.0) * 100.0, 2) } else { 0.0 },
                    "vol_ratio": Value::Null, "amp": amp, "turnover": 0.0,
                    "open": openp, "high": high, "low": low, "prev": prev,
                }),
            );
        }
        if out.is_empty() {
            return out;
        }
    }
    // 东财主力资金/量比（前 80 只）
    let secids: Vec<String> = codes
        .iter()
        .take(80)
        .map(|(c, _)| {
            let prefix = if c.starts_with('6') { "1." } else { "0." };
            format!("{prefix}{c}")
        })
        .collect();
    let url = format!(
        "https://push2.eastmoney.com/api/qt/ulist.np/get?secids={}&fields=f12,f62,f184,f50",
        secids.join(",")
    );
    if let Ok(text) = zg_core::net::http_get(
        &url, 10, 0, None,
        Some(&[("Referer", "https://quote.eastmoney.com/")]), None,
    ) {
        if let Ok(j) = serde_json::from_str::<Value>(&text) {
            for d in (j.get("data").and_then(|x| x.get("diff")).and_then(|x| x.as_array()).cloned())
                .unwrap_or_default()
            {
                let c = match d.get("f12").and_then(|v| v.as_str()) {
                    Some(c) => c.to_string(),
                    None => continue,
                };
                if let Some(entry) = out.get_mut(&c) {
                    if let Some(o) = entry.as_object_mut() {
                        if let Some(v) = d.get("f62").and_then(|v| v.as_f64()) {
                            o.insert("main_yi".into(), json!(zg_core::model::py_round(v / 1e8, 2)));
                        }
                        if let Some(v) = d.get("f184").and_then(|v| v.as_f64()) {
                            o.insert("main_pct".into(), json!(zg_core::model::py_round(v / 100.0, 2)));
                        }
                        if let Some(v) = d.get("f50").and_then(|v| v.as_f64()) {
                            o.insert("vol_ratio".into(), json!(zg_core::model::py_round(v, 2)));
                        }
                    }
                }
            }
        }
    }
    out
}

/// `universe`：market_facts.json 的 by_code → {code: name}。
pub fn universe() -> Vec<(String, String)> {
    let path = crate::monitor::data_dir().join("market_facts.json");
    let mf = crate::monitor::read_json(&path).unwrap_or(json!({}));
    let by = mf.get("by_code").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    by.iter()
        .map(|(c, v)| {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or(c).to_string();
            (c.clone(), name)
        })
        .collect()
}

/// `live_loop`：全市场快照刷新（300s 盘中 / 1800s 盘后）。
pub fn live_loop() {
    let mut fails = 0u32;
    loop {
        let r = guard_round("live", || {
            let uni = universe();
            eprintln!("[live] universe={}", uni.len());
            if !uni.is_empty() {
                let q = fetch_live(&uni);
                eprintln!("[live] quotes={}", q.len());
                if !q.is_empty() {
                    let mut cache = LIVE.lock_ok();
                    cache.0 = now();
                    cache.1 = serde_json::to_value(q).unwrap_or(Value::Null);
                }
            }
        });
        if r.is_ok() {
            fails = 0;
        } else {
            fails += 1;
            // 暴露给 /api/state：前端能看到「在跑但异常」，而不是只看到数据不更新
            let mut cache = LIVE.lock_ok();
            if !cache.1.is_object() {
                cache.1 = json!({});
            }
            if let Some(o) = cache.1.as_object_mut() {
                o.insert("error".into(), json!(format!("采集异常（连续 {fails} 轮）")));
            }
        }
        let secs = if in_trading() { 300 } else { 1800 };
        std::thread::sleep(Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trading_window() {
        // 语义检查（时间相关，只验证不 panic 且返回 bool）
        let _ = in_trading();
    }
}
