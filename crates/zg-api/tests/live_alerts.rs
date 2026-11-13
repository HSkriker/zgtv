//! 预警端到端（独立进程，避免污染生产 alerts.json）。
//!
//! 用 `paths::set_for_test` 把 `market_data` 指向临时目录，验证：
//! - `action=add/reset/del` 不死锁（Python 用 RLock，Rust std::Mutex 不可重入）；
//! - 触发 → armed=false / hits+1 / alert_log+1 → 落盘；重复调用不再触发。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use zg_api::aux_svc::alerts_payload;
use zg_api::live::check_alerts;
use zg_api::state::WebState;

fn q(pairs: &[(&str, &str)]) -> HashMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
        .collect()
}

/// 5s 超时内必须返回，否则判定死锁（线程泄漏无所谓，测试进程会退出）。
fn call_with_timeout(state: &Arc<WebState>, query: HashMap<String, Vec<String>>) -> serde_json::Value {
    let (tx, rx) = mpsc::channel();
    let st = Arc::clone(state);
    std::thread::spawn(move || {
        let _ = tx.send(alerts_payload(&st, &query));
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("alerts_payload 超时 = 死锁（save_alerts 在持锁时被调用？）")
}

#[test]
fn alerts_crud_and_trigger() {
    let tmp = std::env::temp_dir().join(format!("zg_alert_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let mut p = zg_core::paths::get();
    p.market_data = tmp.clone();
    zg_core::paths::set_for_test(p);

    let st = Arc::new(WebState::new(0, true, true));

    // --- add：曾因持锁调 save_alerts 死锁 ---
    let v = call_with_timeout(
        &st,
        q(&[("action", "add"), ("code", "600519"), ("price", "1300"), ("dir", "up")]),
    );
    assert_eq!(v["ok"], json!(true), "add 应成功: {v}");
    assert!(st.alerts.lock_ok().contains_key("600519"));
    let body = std::fs::read_to_string(tmp.join("alerts.json")).expect("add 应落盘");
    assert!(body.contains("600519"));

    // --- reset：同样曾死锁 ---
    let v = call_with_timeout(&st, q(&[("action", "reset"), ("code", "600519")]));
    assert_eq!(v["ok"], json!(true), "reset 应成功: {v}");

    // --- 触发：现价 1330.75 ≥ 目标 1300 ---
    let mut stocks = serde_json::Map::new();
    stocks.insert(
        "600519".into(),
        json!({"price": 1330.75, "pct": -1.85, "date": "2026-10-28", "updated": "15:00:00"}),
    );
    check_alerts(&st, &stocks);
    {
        let alerts = st.alerts.lock_ok();
        let rec = alerts.get("600519").expect("记录应存在");
        assert_eq!(rec["triggered"], json!(true), "涨破目标应触发");
        assert_eq!(rec["armed"], json!(false), "触发后应解除武装");
        assert_eq!(rec["hits"], json!(1));
        assert_eq!(rec["trigger_price"], json!(1330.75));
        assert_eq!(rec["last_price"], json!(1330.75));
    }
    assert_eq!(st.alert_log.lock_ok().len(), 1);

    // 第二次：已 armed=false，不得重复触发
    check_alerts(&st, &stocks);
    assert_eq!(st.alert_log.lock_ok().len(), 1, "不得重复触发");

    let body = std::fs::read_to_string(tmp.join("alerts.json")).expect("应落盘 alerts.json");
    assert!(body.contains("600519"));
    assert!(body.contains("1330.75"), "触发价应落盘");

    // --- 仅记录：只填代码，无方向/目标价 ---
    let v = call_with_timeout(
        &st,
        q(&[("action", "add"), ("code", "000001"), ("name", "平安银行"), ("dir", "none")]),
    );
    assert_eq!(v["ok"], json!(true), "仅记录应成功: {v}");
    {
        let alerts = st.alerts.lock_ok();
        let rec = alerts.get("000001").expect("仅记录应存在");
        assert_eq!(rec["target"], json!(null));
        assert_eq!(rec["dir"], json!("none"));
        assert_eq!(rec["armed"], json!(false));
    }
    // 显式方向但缺目标价 → 报错，不做静默降级（仅记录必须显式 dir=none）
    let v = call_with_timeout(&st, q(&[("action", "add"), ("code", "000002"), ("dir", "up")]));
    assert_eq!(v["ok"], json!(false), "显式方向缺价格应报错: {v}");
    assert!(!st.alerts.lock_ok().contains_key("000002"), "报错时不得落记录");

    // 仅记录只更新现价，不触发、不写日志
    let mut stocks2 = stocks.clone();
    stocks2.insert("000001".into(), json!({"price": 11.54, "pct": 0.0}));
    check_alerts(&st, &stocks2);
    {
        let alerts = st.alerts.lock_ok();
        assert_eq!(alerts["000001"]["last_price"], json!(11.54));
        assert_eq!(alerts["000001"]["triggered"], json!(false));
    }
    assert_eq!(st.alert_log.lock_ok().len(), 1, "仅记录不得写触发日志");
    let v = call_with_timeout(&st, q(&[]));
    assert_eq!(v["stats"]["watch"], json!(1), "统计应包含仅记录数: {v}");

    // --- del / clear ---
    let v = call_with_timeout(&st, q(&[("action", "del"), ("code", "000001")]));
    assert_eq!(v["ok"], json!(true));
    let v = call_with_timeout(&st, q(&[("action", "del"), ("code", "000002")]));
    assert_eq!(v["ok"], json!(true));
    let v = call_with_timeout(&st, q(&[("action", "del"), ("code", "600519")]));
    assert_eq!(v["ok"], json!(true));
    assert!(st.alerts.lock_ok().is_empty());
    let v = call_with_timeout(&st, q(&[("action", "clear")]));
    assert_eq!(v["ok"], json!(true));

    zg_core::paths::reset();
    let _ = std::fs::remove_dir_all(&tmp);
}
