//! 8786 盘中实时巡检（`live_loop`）与预警判定（`check_alerts`）。
//!
//! 逐行对齐 `modules/zgapp/jobs/live.py`：
//! - 只看 `watch_extra ∪ alerts`，最多 300 只，60s 周期；
//! - 三源降级 TQ → 同花顺 → 腾讯（取数在 `zg_market::live`）；
//! - 非交易时段只更新 `watch_n`，5s 空转；
//! - 触发即落盘，否则 60s 周期落盘以捕捉 `last_price` 变化。

use zg_core::sync::MutexExt;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::state::WebState;

const LIVE_MAX_CODES: usize = 300;

/// Python `config.JOB_ALERTS`：`ZG_JOB_ALERTS=0` 关闭预警判定。
fn job_alerts() -> bool {
    std::env::var("ZG_JOB_ALERTS").map(|v| v.trim() != "0").unwrap_or(true)
}

/// `live_codes`：自选 ∪ 预警代码，排序后截断 300。
pub fn live_codes(state: &WebState) -> Vec<String> {
    let mut set: HashSet<String> = state.watch_extra.lock_ok().iter().cloned().collect();
    for k in state.alerts.lock_ok().keys() {
        set.insert(k.clone());
    }
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v.truncate(LIVE_MAX_CODES);
    v
}

fn set_live(state: &WebState, key: &str, val: Value) {
    let mut live = state.live.lock_ok();
    if let Some(o) = live.as_object_mut() {
        o.insert(key.to_string(), val);
    }
}

/// 0..100 的伪随机整数（Python `random.random() < 0.2` 的替身）。
fn rand_pct() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() % 100)
        .unwrap_or(0)
}

pub fn live_loop(state: Arc<WebState>) {
    let mut fails = 0u32;
    loop {
        // Python 用 try/except 包住整轮（失败只跳过本轮）。Rust 里 panic 会永久杀死线程，
        // 所以显式 catch_unwind，语义等价于 Python 的 `except Exception: pass`；
        // 连续失败按 5s×N 退避（上限 60s），避免毒锁后每 5 秒空转刷屏。
        let st = Arc::clone(&state);
        let nap = match crate::monitor_live::guard_round("alerts", || live_cycle(&st)) {
            Ok(d) => {
                fails = 0;
                d
            }
            Err(_) => {
                fails += 1;
                Duration::from_secs((5 * fails as u64).min(60))
            }
        };
        std::thread::sleep(nap);
    }
}

/// 单轮巡检。返回本轮结束后的休眠时长（Python：空转 5s、正常轮 60s）。
fn live_cycle(state: &WebState) -> Duration {
    let codes = live_codes(state);
    set_live(state, "watch_n", json!(codes.len()));
    if codes.is_empty() || !zg_core::calendar::market_open_now(None) {
        return Duration::from_secs(5);
    }

    let t0 = Instant::now();
    let (quotes, src) = zg_market::live::fetch_live(&codes);
    let fetch_ms = t0.elapsed().as_millis() as i64;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let nowstr = chrono::Local::now().format("%H:%M:%S").to_string();

    let mut stocks = Map::new();
    for code in &codes {
        let Some(q) = quotes.get(code) else { continue };
        let price = q.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let pre = q.get("preClose").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if price <= 0.0 || pre <= 0.0 {
            continue;
        }
        let pct = zg_core::model::py_round((price / pre - 1.0) * 100.0, 2);
        stocks.insert(
            code.clone(),
            json!({"price": price, "pct": pct, "date": today, "updated": nowstr}),
        );
    }

    // Python 顺序：先写 stocks，再判定预警，最后写 updated/fetch_ms/cycle_ms/source
    set_live(state, "stocks", Value::Object(stocks.clone()));
    if job_alerts() {
        check_alerts(state, &stocks);
    }
    set_live(state, "updated", json!(nowstr));
    set_live(state, "fetch_ms", json!(fetch_ms));
    set_live(state, "cycle_ms", json!(t0.elapsed().as_millis() as i64));
    set_live(
        state,
        "source",
        match src {
            Some(s) => json!(s),
            None => Value::Null,
        },
    );
    // Python：quote_age 为空或 20% 概率刷新
    let need_age = {
        let live = state.live.lock_ok();
        live.get("quote_age").map(|v| v.is_null()).unwrap_or(true)
    };
    if need_age || rand_pct() < 20 {
        let age = zg_market::live::tencent_quote_age();
        set_live(
            state,
            "quote_age",
            match age {
                Some(a) => json!(a),
                None => Value::Null,
            },
        );
    }
    Duration::from_secs(zg_market::consts::LIVE_CYCLE)
}

/// `check_alerts`：复用已拉取的实时价判定价格触发，并记录触发日志。
///
/// `dir=up` 现价 ≥ 目标；`dir=down` 现价 ≤ 目标；`dir=near` 偏离 ≤ 2%。
/// 触发后 `armed=false` 防重复，`hits` 累加。
pub fn check_alerts(state: &WebState, stocks: &Map<String, Value>) {
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let nowstr = now.format("%H:%M:%S").to_string();
    let mut changed = false;

    {
        let mut alerts = state.alerts.lock_ok();
        for (code, rec) in alerts.iter_mut() {
            let s = stocks.get(code);
            let price = s.and_then(|x| x.get("price")).and_then(|v| v.as_f64());
            if let Some(p) = price.filter(|p| *p > 0.0) {
                rec["last_price"] = json!(p);
                rec["last_pct"] = s
                    .and_then(|x| x.get("pct"))
                    .cloned()
                    .unwrap_or(Value::Null);
                rec["last_time"] = json!(nowstr);
            }
            if !rec.get("armed").and_then(|v| v.as_bool()).unwrap_or(true) {
                continue;
            }
            let tp = rec.get("target").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let dir = rec
                .get("dir")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if crate::aux_svc::alert_hit(price.unwrap_or(0.0), tp, &dir) {
                let p = price.unwrap_or(0.0);
                rec["triggered"] = json!(true);
                rec["armed"] = json!(false);
                rec["triggered_time"] = json!(format!("{today} {nowstr}"));
                rec["trigger_price"] = json!(p);
                let hits = rec.get("hits").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
                rec["hits"] = json!(hits);
                let name = rec
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                {
                    let mut log = state.alert_log.lock_ok();
                    log.push(json!({
                        "code": code, "name": name, "price": p, "dir": dir,
                        "target": tp, "time": format!("{today} {nowstr}"),
                    }));
                    crate::aux_svc::trim_alert_log(&mut log);
                }
                changed = true;
            }
        }
    }

    // 触发时立即落盘；否则每 60s 周期性落盘
    let flush_due = {
        let last = *state.alert_flush.lock_ok();
        crate::state::now() - last > 60.0
    };
    if changed || flush_due {
        *state.alert_flush.lock_ok() = crate::state::now();
        if !*state.alert_load_ok.lock_ok() {
            crate::aux_svc::load_alerts(state);
        }
        if *state.alert_load_ok.lock_ok() {
            crate::aux_svc::save_alerts(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_codes_merges_and_sorts() {
        let state = WebState::new(0, true, true);
        state.watch_extra.lock_ok().extend(["600519".to_string(), "000001".to_string()]);
        state.alerts.lock_ok().insert("300750".into(), json!({}));
        let v = live_codes(&state);
        assert_eq!(v, vec!["000001", "300750", "600519"]);
    }

    #[test]
    fn alert_hit_semantics() {
        assert!(crate::aux_svc::alert_hit(10.0, 9.5, "up"));
        assert!(!crate::aux_svc::alert_hit(9.0, 9.5, "up"));
        assert!(crate::aux_svc::alert_hit(9.0, 9.5, "down"));
        assert!(crate::aux_svc::alert_hit(10.1, 10.0, "near"));
        assert!(!crate::aux_svc::alert_hit(10.5, 10.0, "near"));
        assert!(!crate::aux_svc::alert_hit(0.0, 10.0, "up"));
    }
}
