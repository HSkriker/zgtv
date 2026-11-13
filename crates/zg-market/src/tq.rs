//! TQ-Local 实时行情适配层 —— 对齐 `modules/market/tq_local.py`。
//!
//! 单位口径（勿改）：get_market_data 的 Volume=股、Amount=万元；
//! Amount ×10000 转元、Volume ÷100 转手。
//! 重量分级（2026-10-25 定案）：重接口一律禁调，`post` 直接拒发不接触网络。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::kbar::py_float;

fn tq_url() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        std::env::var("TQ_URL")
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:17709/".to_string())
    })
    .as_str()
}

fn tq_rt() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("TQ_RT").map(|v| v.trim() != "0").unwrap_or(true))
}

fn tq_live() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("TQ_LIVE").map(|v| v.trim() != "0").unwrap_or(true))
}

fn tq_debug() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("TQ_DEBUG").map(|v| v.trim() == "1").unwrap_or(false))
}

const TIMEOUT_BATCH: u64 = 5;
const TIMEOUT_WARMUP: u64 = 30;
const BATCH: usize = 50;
const BATCH_GAP_MS: u64 = 50;
const COOLDOWN: f64 = 60.0;
const FAIL_N: u32 = 3;
const MAX_BATCH: usize = 50;
const MAX_COUNT: i64 = 3000;
const WARN_COUNT: i64 = 1500;
/// 单次调用最小间隔（秒）：TQ 单线程阻塞，连发会把客户端打挂。
const CALL_GAP: f64 = 2.0;
/// 一次操作内允许的连续调用数（取数 → refresh_kline → 重取）。
const CALL_BURST: u32 = 3;

/// TQ 单线程阻塞，必须串行。
static POST_LOCK: Mutex<()> = Mutex::new(());

static FAIL: Mutex<(u32, f64)> = Mutex::new((0, 0.0)); // (n, until)
static PROBE: Mutex<(f64, bool)> = Mutex::new((0.0, false)); // (ts, ok)
static CALLS: Mutex<(f64, u32)> = Mutex::new((0.0, 0)); // (上次调用时刻, 本轮连续次数)
static LAST: Mutex<(Option<String>, f64, usize, usize, f64)> =
    Mutex::new((None, 0.0, 0, 0, 0.0)); // (src, ms, n, batches, updated)

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 6 位代码 -> TQ 代码（带市场后缀）。
/// 与 `to_thscode` 同口径（`zg_core::reader::market_of_prefix`）：
/// 920xxx→.BJ（北交所）、88xxx→.SH（板块指数）、其余 8xxxx→.BJ、6/9/5xxxxx→.SH、其余→.SZ。
pub fn tq_symbol(code: &str) -> String {
    let c = code.trim();
    match zg_core::reader::market_of_prefix(c) {
        Some(zg_core::reader::MKT_SH) => format!("{c}.SH"),
        Some(zg_core::reader::MKT_BJ) => format!("{c}.BJ"),
        _ => format!("{c}.SZ"),
    }
}

fn banned(method: &str) -> Option<&'static str> {
    match method {
        "get_market_snapshot" => Some("全市场聚合，3/3 致命（原生堆损坏，非 L2 权限问题）；涨跌家数改用 get_scjy_value_by_date SC31"),
        "get_scjy_value" => Some("带日期区间批量 13 字段×30 天直接打崩；全市场统计只走 get_scjy_value_by_date"),
        _ => None,
    }
}

/// 间隔闸门：距上次调用不足 CALL_GAP 且本轮连续次数已用尽时拒绝。
/// 放在 post() 里统一执行 —— 之前只有 2 条路径各自守门，其余直调 post 的路径
/// 可以不受限地把 TQ 客户端连发打挂。
pub fn gate() -> Result<(), String> {
    if !available() {
        return Err("TQ 不可用".into());
    }
    let now_t = now();
    let mut c = CALLS.lock_ok();
    if now_t - c.0 >= CALL_GAP {
        c.0 = now_t;
        c.1 = 1;
        return Ok(());
    }
    if c.1 < CALL_BURST {
        // 一次操作内的连续调用（取数 → refresh → 重取）不算超速
        c.1 += 1;
        c.0 = now_t;
        return Ok(());
    }
    Err("TQ 间隔保护".into())
}

/// 拒发层护栏：禁调接口 + 多年/大批量重接口（不触网）。
fn reject_heavy(method: &str, params: &Value) -> Result<(), String> {
    if let Some(reason) = banned(method) {
        return Err(format!("TQ 禁调接口 {method}：{reason}"));
    }
    if method == "get_market_data" {
        let codes = params.get("stock_list")
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if codes > MAX_BATCH {
            return Err(format!(
                "TQ 单批 {codes} 只 > 上限 {MAX_BATCH}：多股大批量属重接口，拆批 ≤50 且批间 ≥30s；有本地 .day 时禁走 TQ"
            ));
        }
        let count = params.get("count").and_then(|x| x.as_i64()).unwrap_or(0);
        if count > MAX_COUNT {
            return Err(format!(
                "TQ count={count} 属多年区间重接口：优先用本地 .day（readtdx）或减小 lmt"
            ));
        }
        if count > WARN_COUNT {
            eprintln!("[tq] count={count} 偏大（>1500）：本地 .day 有全历史，勿依赖 TQ 拉长区间");
        }
    }
    Ok(())
}

/// 解析 TQ 响应体：区分「通道故障」（计入熔断）与「业务错误」（通道正常，不计入）。
fn parse_result(method: &str, raw: &str) -> Result<Value, String> {
    let j: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            _bad(&format!("解析失败: {e}"));
            return Err(format!("TQ 响应解析失败: {e}"));
        }
    };
    if let Some(err) = j.get("error") {
        // 服务端明确回了业务错误 → 通道是通的，不该计入熔断计数
        _ok();
        return Err(format!("TQ {method} error: {err}"));
    }
    _ok();
    Ok(j.get("result").cloned().unwrap_or_else(|| Value::Object(Map::new())))
}

/// 一次 JSON-RPC 调用（含护栏 + 间隔闸门）。锁只包住单次 HTTP；重量护栏在拒发层。
pub fn post(method: &str, params: &Value, timeout: u64) -> Result<Value, String> {
    if let Some(t) = crate::net_stub::get(&format!("tq:{method}")) {
        return parse_result(method, &t);
    }
    reject_heavy(method, params)?;
    gate()?;
    send(method, params, timeout)
}

/// 批量路径内部调用：批间已有 BATCH_GAP_MS 间隔，跳过单次间隔闸门。
fn post_batch(method: &str, params: &Value, timeout: u64) -> Result<Value, String> {
    if let Some(t) = crate::net_stub::get(&format!("tq:{method}")) {
        return parse_result(method, &t);
    }
    reject_heavy(method, params)?;
    send(method, params, timeout)
}

/// 真正发 HTTP（锁内单次请求）。
fn send(method: &str, params: &Value, timeout: u64) -> Result<Value, String> {
    let body = json!({"id": 1, "method": method, "params": params}).to_string();
    // 锁中毒只说明上一次持锁的线程崩了，TQ 通道本身没坏：取回内层数据继续用，
    // 否则一次偶发 panic 会让全部 TQ 调用永久返回 Err 并静默降级到下一数据源。
    let _guard = POST_LOCK.lock_ok();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(timeout))
        .build();
    let resp = match agent
        .post(tq_url())
        .set("Content-Type", "application/json; charset=utf-8")
        .send_string(&body)
    {
        Ok(r) => r,
        Err(e) => {
            _bad(&format!("{method} 传输失败: {e}"));
            return Err(e.to_string());
        }
    };
    let raw = match resp.into_string() {
        Ok(r) => r,
        Err(e) => {
            _bad(&format!("{method} 响应读取失败: {e}"));
            return Err(e.to_string());
        }
    };
    parse_result(method, &raw)
}

fn _ok() {
    FAIL.lock_ok().0 = 0;
}

fn _bad(reason: &str) {
    let mut f = FAIL.lock_ok();
    f.0 += 1;
    if f.0 >= FAIL_N && now() >= f.1 {
        f.1 = now() + COOLDOWN;
        f.0 = 0;
        eprintln!("[tq] 连续失败，熔断 {COOLDOWN:.0}s ({reason})");
    }
}

/// 快速探活（结果缓存 5s）。熔断期内直接 false，不做网络调用。
pub fn available() -> bool {
    {
        let f = FAIL.lock_ok();
        if now() < f.1 {
            return false;
        }
    }
    let now_t = now();
    {
        let p = PROBE.lock_ok();
        if now_t - p.0 < 5.0 {
            return p.1;
        }
    }
    let host = tq_url()
        .split("//")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("");
    let (h, p) = match host.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(17709)),
        None => (host, 17709),
    };
    let ok = std::net::TcpStream::connect_timeout(
        &format!("{h}:{p}").parse().unwrap_or_else(|_| "127.0.0.1:17709".parse().unwrap()),
        Duration::from_millis(300),
    )
    .is_ok() && rpc_alive();
    *PROBE.lock_ok() = (now_t, ok);
    ok
}

/// 真实 RPC 探活：端口 LISTENING ≠ 客户端能用 —— HTTP 200 空 body = 假死
/// （进程活着但不出数，只能人工重启），只看端口会把假死当活。
/// 用纪律文档钦定的探活接口 get_match_stkinfo（参数名 key_word，🟢轻、连发安全），
/// 直接发原始请求不走 gate/post（避免 available 递归）。
fn rpc_alive() -> bool {
    let body = json!({"id": 1, "method": "get_match_stkinfo",
                      "params": {"key_word": "600519"}})
        .to_string();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build();
    match agent
        .post(tq_url())
        .set("Content-Type", "application/json; charset=utf-8")
        .send_string(&body)
    {
        Ok(resp) => resp.into_string().map(|s| !s.trim().is_empty()).unwrap_or(false),
        Err(_) => false,
    }
}

/// get_market_data 单票数组 -> 快照结构；任何不满足返回 None（交 THS 兜底）。
pub fn parse_bar(arr: &Value, _code: &str) -> Option<Value> {
    let o = arr.as_object()?;
    if o.get("ErrorId").and_then(|x| x.as_str()).unwrap_or("0") != "0" {
        return None;
    }
    let dates = o.get("Date").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let closes = o.get("Close").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if dates.len() < 2 || closes.len() < 2 {
        return None; // 新股/无昨收
    }
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    if dates.last().and_then(|x| x.as_str()).unwrap_or("") != today {
        return None; // 盘前/客户端未更新 → 绝不给陈旧数据
    }
    let close = closes.last().and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0);
    if close <= 0.0 {
        return None;
    }
    let num = |i: usize| closes.get(i).and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0);
    let get = |k: &str| o.get(k).and_then(|x| x.as_array()).and_then(|a| a.last());
    Some(json!({
        "price": close,
        "open": get("Open").and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0),
        "high": get("High").and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0),
        "low": get("Low").and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0),
        "preClose": num(closes.len() - 2),
        "vol": get("Volume").and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0) / 100.0,
        "amount": get("Amount").and_then(|x| py_float(Some(x)).ok()).unwrap_or(0.0) * 10000.0,
    }))
}

/// 批量实时行情（get_market_data，50 只/批 × count=2）。
/// 返回 {code6: {...}}；空/失败返回 {}（调用方回落 THS）。
pub fn quotes(codes: &[String], dividend_type: &str, live_gate: bool) -> Result<HashMap<String, Value>, String> {
    let codes: Vec<String> = codes.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    if codes.is_empty() || (live_gate && !tq_live()) {
        return Ok(HashMap::new());
    }
    let t0 = now();
    let mut out = HashMap::new();
    let mut batches = 0usize;
    let mut first_err: Option<String> = None;
    for i in (0..codes.len()).step_by(BATCH) {
            let batch = &codes[i..codes.len().min(i + BATCH)];
            let tqcodes: Vec<String> = batch.iter().map(|c| tq_symbol(c)).collect();
            // 单批失败不再中止整轮：后续批次的票照取，部分成功也比把全量
            // 票砸给下一数据源好；只有全部批次都失败才报错（调用方整体回落）
            match post_batch(
                "get_market_data",
                &json!({"stock_list": tqcodes, "period": "1d", "count": 2,
                        "dividend_type": dividend_type}),
                TIMEOUT_BATCH,
            ) {
                Ok(res) => {
                    let value = res.get("Value").cloned().unwrap_or_else(|| Value::Object(Map::new()));
                    for (c, tq) in batch.iter().zip(tqcodes.iter()) {
                        if let Some(bar) = parse_bar(value.get(tq).unwrap_or(&Value::Null), c) {
                            out.insert(c.clone(), bar);
                        }
                    }
                    batches += 1;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        if i + BATCH < codes.len() {
            std::thread::sleep(Duration::from_millis(BATCH_GAP_MS));
        }
    }
    let result = match first_err {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(()),
    };
    match result {
        Ok(()) => {
            let ms = (now() - t0) * 1000.0;
            *LAST.lock_ok() = (Some("quotes".into()), ms, out.len(), batches, now());
            if tq_debug() {
                eprintln!("[tq] live n={}/{} batches={} ms={:.0}", out.len(), codes.len(), batches, ms);
            }
            Ok(out)
        }
        Err(e) => {
            // 失败计数已在 send/parse_result 里按「传输故障 vs 业务错误」分别记账
            Err(e)
        }
    }
}

/// 单只实时快照（get_market_data 单票，避开已冻结的 get_market_snapshot）。
pub fn snapshot(code: &str) -> Result<Value, String> {
    let code = code.trim().to_string();
    if !tq_rt() {
        return Err("TQ 单票已关闭 (TQ_RT=0)".into());
    }
    let d = quotes(std::slice::from_ref(&code), "none", false)?
        .get(&code)
        .cloned()
        .ok_or_else(|| format!("TQ 无 {code} 当日行情"))?;
    let price = d.get("price").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let pre = d.get("preClose").and_then(|x| x.as_f64()).unwrap_or(0.0);
    if price <= 0.0 || pre <= 0.0 {
        return Err(format!("TQ 快照价格无效: {code} price={price} preClose={pre}"));
    }
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code));
    m.insert("name".into(), Value::Null); // server 层用 ths_name_map 补全
    m.insert("price".into(), Value::from(price));
    m.insert("open".into(), d.get("open").cloned().unwrap_or(Value::Null));
    m.insert("high".into(), d.get("high").cloned().unwrap_or(Value::Null));
    m.insert("low".into(), d.get("low").cloned().unwrap_or(Value::Null));
    m.insert("vol".into(), d.get("vol").cloned().unwrap_or(Value::Null));
    m.insert("amount".into(), d.get("amount").cloned().unwrap_or(Value::Null));
    m.insert("preClose".into(), Value::from(pre));
    m.insert("pct".into(), Value::from(zg_core::model::py_round((price / pre - 1.0) * 100.0, 2)));
    m.insert("time".into(), Value::String(chrono::Local::now().format("%H:%M:%S").to_string()));
    LAST.lock_ok().0 = Some("snapshot".into());
    Ok(Value::Object(m))
}

/// 启动预热：TQ 首次调用有 ~6s 冷启动，由后台线程吃掉。
pub fn warmup() {
    let t0 = now();
    match post(
        "get_market_data",
        &json!({"stock_list": ["600519.SH"], "period": "1d", "count": 2,
                "dividend_type": "none"}),
        TIMEOUT_WARMUP,
    ) {
        Ok(_) => eprintln!("[tq] 预热完成 {:.0}ms", (now() - t0) * 1000.0),
        Err(e) => eprintln!("[tq] 预热失败: {e}"),
    }
}

/// 给 /api/status 暴露 TQ 健康度。
pub fn status() -> Value {
    let ok = available(); // 先探活（内部会锁 FAIL/PROBE），再取熔断状态，避免重入死锁
    let cooldown = {
        let f = FAIL.lock_ok();
        (f.1 - now()).max(0.0).round()
    };
    let last = LAST.lock_ok();
    json!({
        "rt": tq_rt(),
        "live": tq_live(),
        "ok": ok,
        "cooldown": cooldown,
        "last": {
            "src": last.0,
            "ms": (last.1 * 10.0).round() / 10.0,
            "n": last.2,
            "batches": last.3,
            "updated": last.4,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tq_symbol_mapping() {
        assert_eq!(tq_symbol("600519"), "600519.SH");
        assert_eq!(tq_symbol("900001"), "900001.SH");
        assert_eq!(tq_symbol("000001"), "000001.SZ");
        assert_eq!(tq_symbol("920001"), "920001.BJ");
        assert_eq!(tq_symbol("881333"), "881333.SH");
        assert_eq!(tq_symbol("830799"), "830799.BJ");
        assert_eq!(tq_symbol("430047"), "430047.BJ");
        assert_eq!(tq_symbol(" 000001 "), "000001.SZ");
    }

    #[test]
    fn banned_methods_rejected() {
        assert!(post("get_market_snapshot", &json!({}), 5).is_err());
        assert!(post("get_scjy_value", &json!({}), 5).is_err());
    }

    #[test]
    fn batch_guard_rejects() {
        let codes: Vec<String> = (0..51).map(|i| format!("{i:06}")).collect();
        let err = post("get_market_data",
                       &json!({"stock_list": codes, "period": "1d", "count": 2}),
                       5).unwrap_err();
        assert!(err.contains("> 上限 50"), "{err}");
    }

    #[test]
    fn count_guard_rejects() {
        let err = post("get_market_data",
                       &json!({"stock_list": ["600519.SH"], "period": "1d", "count": 3001}),
                       5).unwrap_err();
        assert!(err.contains("多年区间重接口"), "{err}");
    }

    #[test]
    fn parse_bar_freshness() {
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let arr = json!({
            "ErrorId": "0",
            "Date": ["20261026", today],
            "Open": [100.0, 101.0],
            "High": [102.0, 103.0],
            "Low": [99.0, 100.0],
            "Close": [101.0, 102.5],
            "Volume": [10000.0, 20000.0],
            "Amount": [3000.0, 4000.0],
        });
        let b = parse_bar(&arr, "600519").unwrap();
        assert_eq!(b["price"], 102.5);
        assert_eq!(b["preClose"], 101.0);
        assert_eq!(b["vol"], 200.0);
        assert_eq!(b["amount"], 40000000.0);
        // 陈旧日期 → None
        let stale = json!({
            "ErrorId": "0",
            "Date": ["20261024", "20261025"],
            "Close": [1.0, 2.0],
        });
        assert!(parse_bar(&stale, "600519").is_none());
        // ErrorId 非 0 → None
        let err = json!({"ErrorId": "1", "Date": [today], "Close": [1.0, 2.0]});
        assert!(parse_bar(&err, "600519").is_none());
    }
}
