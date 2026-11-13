use serde_json::{json, Value};

use crate::paths;

/// 11 个宽基/主要指数快照。收盘后本地优先；盘中自动 TQ → 腾讯 → 东财。
/// `TV_LIVE=0` / `TV_OFFLINE=1` 时 offline=true，只读本地日线。
pub fn market_indices(force: bool) -> Value {
    let offline = !crate::live::enabled();
    let v = zg_market::indices::snapshot(offline, force);
    let available = v.get("ok").cloned().unwrap_or(json!(false));
    let mut m = v.as_object().cloned().unwrap_or_default();
    // snapshot 的 ok 表示「11 个指数全部有报价」；tv 需要把部分可用也作为正常响应。
    m.insert("ok".into(), json!(true));
    m.insert("available".into(), available);
    m.insert("offline".into(), json!(offline));
    m.insert("session".into(), json!(crate::live::session_today()));
    Value::Object(m)
}

/// 宽基成分股：读 tv 自带的离线缓存（index_constituents_cache.json，原 zg 落盘快照）。
pub fn constituents(code: &str) -> Result<Value, String> {
    if code.is_empty() {
        return Err("缺少 code 参数".to_string());
    }
    let path = paths::tv_root().join("data").join("market")
        .join("index_constituents_cache.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读不到指数成分股缓存: {e}"))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|e| format!("指数成分股缓存解析失败: {e}"))?;
    let item = doc.get("items").and_then(|x| x.get(code));
    let Some(mut value) = item.and_then(|x| x.get("value")).cloned() else {
        return Err(format!("本地无该指数成分股缓存（{code}）"));
    };
    // 离线快照标注：成分股目录源头在东财，缓存冻结时如实标出来（前端 status 行直接显示）
    if let Some(o) = value.as_object_mut() {
        let src = o.get("source").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if !src.contains("离线") {
            o.insert("source".into(),
                Value::String(if src.is_empty() { "离线快照".into() } else { format!("{src}·离线快照") }));
        }
    }
    Ok(value)
}
