use std::time::Duration;

use serde_json::Value;
use zg_api::http::{Req, Resp};

pub fn ok(v: Value) -> Resp {
    Resp::json(&v, 200)
}

pub fn err(msg: &str, status: u16) -> Resp {
    Resp::json(&serde_json::json!({"ok": false, "error": msg}), status)
}

/// 每个请求打一行日志：方法 路径 查询串 状态 耗时
pub fn logged(req: &Req, resp: Resp, elapsed: Duration) -> Resp {
    let q = if req.query.is_empty() {
        String::new()
    } else {
        let mut keys: Vec<&str> = req.query.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        format!(" ?{}", keys.join(","))
    };
    eprintln!(
        "[tv] {} {}{} -> {} {}ms",
        req.method,
        req.path,
        q,
        resp.status,
        elapsed.as_millis()
    );
    resp
}
