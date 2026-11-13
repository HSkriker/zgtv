//! 8787 同花顺客户端 —— 对齐 `modules/monitor_app/services/ths.py`：
//! AIMD 令牌桶（1.0~4.0s、429 乘性放大 1.5×、成功缓慢回缩 0.95×）+ 全进程冷却 + 429/5xx 退避。
//! 与 `zg-market/ths.rs`（fetch_boards 语义）不同：本模块是 8787 数据链路的一部分，
//! 限速状态对外可见（/api/ths_debug）。

use zg_core::sync::MutexExt;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

const THS_BASE: &str = "https://fuyao.aicubes.cn";
const THS_MIN_INTERVAL: f64 = 1.00;
const THS_MAX_INTERVAL: f64 = 4.00;
const THS_FAST_INC: f64 = 1.50;
const THS_SLOW_DEC: f64 = 0.95;

struct Rate {
    next_at: f64,
    cooldown: f64,
    interval: f64,
    ok: u64,
    r429: u64,
    last_429: f64,
    high_pending: u32,
    conn_reused: u64,
    conn_alive: bool,
}

static RATE: Mutex<Rate> = Mutex::new(Rate {
    next_at: 0.0,
    cooldown: 0.0,
    interval: THS_MIN_INTERVAL,
    ok: 0,
    r429: 0,
    last_429: 0.0,
    high_pending: 0,
    conn_reused: 0,
    conn_alive: false,
});

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn sleep_secs(s: f64) {
    if s > 0.0 {
        std::thread::sleep(Duration::from_millis((s * 1000.0) as u64));
    }
}

fn penalize(seconds: f64) {
    let mut r = RATE.lock_ok();
    r.cooldown = r.cooldown.max(now() + seconds);
    r.interval = THS_MAX_INTERVAL.min(r.interval * THS_FAST_INC);
    r.r429 += 1;
    r.last_429 = now();
}

fn succeeded() {
    let mut r = RATE.lock_ok();
    r.ok += 1;
    r.interval = THS_MIN_INTERVAL.max(r.interval * THS_SLOW_DEC);
}

fn throttle(prio: u8) {
    if prio >= 1 {
        RATE.lock_ok().high_pending += 1;
    }
    {
        let mut r = RATE.lock_ok();
        let t = now();
        if prio < 1 && r.high_pending > 0 {
            let base = r.next_at.max(r.cooldown).max(t);
            r.next_at = base + r.interval;
        }
        let wait = r.next_at.max(r.cooldown) - t;
        if wait > 0.0 {
            drop(r);
            sleep_secs(wait);
            let mut r = RATE.lock_ok();
            r.next_at = now() + r.interval;
        } else {
            r.next_at = t + r.interval;
        }
    }
    if prio >= 1 {
        let mut r = RATE.lock_ok();
        r.high_pending = r.high_pending.saturating_sub(1);
    }
}

/// `_ths_debug_state`（8787 /api/ths_debug 的状态块；matrix volatile 覆盖数值）。
pub fn debug_state() -> Value {
    let r = RATE.lock_ok();
    let t = now();
    serde_json::json!({
        "interval": (r.interval * 100.0).round() / 100.0,
        "next_in": ((r.next_at - t).max(0.0) * 100.0).round() / 100.0,
        "cooldown_in": ((r.cooldown - t).max(0.0) * 100.0).round() / 100.0,
        "ok": r.ok,
        "r429": r.r429,
        "last_429_age": if r.last_429 > 0.0 { Some(((t - r.last_429) * 10.0).round() / 10.0) } else { None },
        "conn_alive": r.conn_alive,
        "conn_reused": r.conn_reused,
    })
}

/// `to_thscode`（8787 版：920/43/83/87/8/4 → .BJ；6/9 → .SH；其余 .SZ）。
pub fn to_thscode(code6: &str) -> String {
    if code6.starts_with("920") || code6.starts_with("43") || code6.starts_with("83")
        || code6.starts_with("87") || code6.starts_with("8") || code6.starts_with("4") {
        format!("{code6}.BJ")
    } else if code6.starts_with("6") || code6.starts_with("9") {
        format!("{code6}.SH")
    } else {
        format!("{code6}.SZ")
    }
}

/// `number`：东财空值可能是 '-' / None → None。
pub fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn urlencode(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlencode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 同花顺 REST 共享 Agent（连接池跨请求复用）。
fn ths_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .redirects(5)
            .build()
    })
}

/// `ths_get`：AIMD 限速 + 429/5xx 退避重试；`prio=0` 为低优先级（让位高优先级请求）。
pub fn ths_get(path: &str, params: &[(&str, String)], timeout: u64, retries: u32, prio: u8)
    -> Result<Value, String> {
    let key = crate::monitor::ths_api_key().ok_or_else(|| "no_key".to_string())?;
    let mut target = format!("{THS_BASE}{path}");
    if !params.is_empty() {
        target.push('?');
        target.push_str(&urlencode(params));
    }
    // 共享 Agent：连接池跨请求复用（此前每请求新建 Agent，keep-alive 形同虚设）；
    // 单次超时按调用参数挂在请求上。
    let agent = ths_agent();
    let mut last = String::new();
    for attempt in 0..retries {
        throttle(prio);
        let resp = agent
            .get(&target)
            .timeout(Duration::from_secs(timeout))
            .set("X-api-key", &key)
            .set("User-Agent", "Mozilla/5.0")
            .set("Accept", "application/json")
            .set("Connection", "keep-alive")
            .call();
        match resp {
            Ok(r) => {
                // ureq 2.x 对非 2xx 直接返回 Err(Status)，此处必为成功响应
                let body = r.into_string().unwrap_or_default();
                let j: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => {
                        last = format!("响应非 JSON({} 字节)", body.len());
                        if attempt + 1 < retries {
                            sleep_secs((0.5 * (attempt as f64 + 1.0)).min(1.0));
                        }
                        continue;
                    }
                };
                let code = j.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                if code == 0 {
                    succeeded();
                    let mut r = RATE.lock_ok();
                    r.conn_alive = true;
                    // 共享 Agent + keep-alive 下，首连之后的成功请求大概率走了复用连接；
                    // ureq 不暴露精确复用计数，这里以成功次数近似（对外字段名不变）
                    r.conn_reused += 1;
                    drop(r);
                    return Ok(j.get("data").cloned().unwrap_or_else(|| serde_json::json!({})));
                }
                if code == 429 || code == 4001 {
                    last = format!(
                        "频率超限({code}): {}",
                        j.get("message").and_then(|m| m.as_str()).unwrap_or("")
                    );
                    penalize((1.5 * 2f64.powi(attempt as i32)).min(8.0));
                    continue;
                }
                if code == 2001 || code == 2003 || code == 2004 {
                    return Err("bad_key".into());
                }
                if code == 1001 || code == 1002 || code == 1003 {
                    return Err(format!("ths_param code={code}"));
                }
                if (5000..6000).contains(&code) {
                    last = format!("服务端错误 code={code}");
                    penalize((1.5 * 2f64.powi(attempt as i32)).min(8.0));
                    continue;
                }
                return Err(format!(
                    "ths code={code} msg={}",
                    j.get("message").and_then(|m| m.as_str()).unwrap_or("")
                ));
            }
            Err(ureq::Error::Status(status, r)) => {
                // 429/5xx 走退避重试（ureq 把非 2xx 包在 Err(Status) 里，必须在这里判）
                let retry_after = r.header("Retry-After").and_then(|v| v.parse::<f64>().ok());
                if status == 429 || (500..600).contains(&status) {
                    last = format!("HTTP {status}");
                    let secs = retry_after
                        .map(|v| v.min(8.0))
                        .unwrap_or_else(|| (1.5 * 2f64.powi(attempt as i32)).min(8.0));
                    penalize(secs);
                    continue;
                }
                return Err(format!("ths HTTP {status}"));
            }
            Err(e) => {
                last = format!("{e}");
                {
                    let mut r = RATE.lock_ok();
                    r.conn_alive = false;
                }
                if attempt + 1 < retries {
                    sleep_secs((0.5 * (attempt as f64 + 1.0)).min(1.0));
                }
            }
        }
    }
    Err(format!("同花顺请求失败(已试{retries}次): {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thscode_8787_semantics() {
        assert_eq!(to_thscode("920001"), "920001.BJ");
        assert_eq!(to_thscode("430047"), "430047.BJ");
        assert_eq!(to_thscode("830799"), "830799.BJ");
        assert_eq!(to_thscode("600519"), "600519.SH");
        assert_eq!(to_thscode("000001"), "000001.SZ");
        assert_eq!(to_thscode("300750"), "300750.SZ");
    }

    #[test]
    fn number_semantics() {
        assert_eq!(number(Some(&serde_json::json!(1.5))), Some(1.5));
        assert_eq!(number(Some(&serde_json::json!("2.5"))), Some(2.5));
        assert_eq!(number(Some(&serde_json::json!("-"))), None);
        assert_eq!(number(None), None);
    }
}
