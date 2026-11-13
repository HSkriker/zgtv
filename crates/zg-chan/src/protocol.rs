//! `zgbridge.protocol` 的 Rust 移植：JSON-lines 协议（stdin/stdout，不开端口）。
//!
//! 协议契约（与旧 `czsc_bridge/worker.py` 完全一致）：
//! ```text
//! {"id":1,"op":"ping"}
//! {"id":2,"op":"analyze","code":"600519","freq":"D"}
//! {"id":3,"op":"analyze","code":"600519","freq":"F1","bars":[...]}
//! {"id":4,"op":"scan","codes":[...],"days":3,"types":["一","二","三"]}
//! {"id":5,"op":"bars","code":"600519","freq":"D","count":800}
//! {"id":6,"op":"shutdown"}
//! ```
//! stdout **只**写响应行（日志一律 stderr）；单条 >2MB 分块回。

use std::io::{BufRead, Write};
use std::sync::Mutex;

use serde_json::{json, Map, Value};
use zg_core::sync::MutexExt;

use crate::common::{date_str, py_round};
use crate::payload::{build_payload, build_payload_from_bars, CZSC_VERSION};
use crate::scan::run_scan;
use crate::service::{bars_to_raw, InlineBar};

/// 单条响应上限（协议 ≤ 2MB，超出分块回）。
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// 桥进程状态：启动自检（gbbq 可读 + 官方信号 SPECS/注册表一致）。
pub struct Bridge {
    pub startup_error: Option<String>,
    pub boot_ms: i64,
    /// SPECS 与 czsc 注册表的漂移项（空 = 一致）。启动即检查（M14），
    /// 漂移只在逐信号 reason 串里出现时很难发现。
    pub selfcheck: Vec<String>,
}

impl Bridge {
    pub fn boot() -> Self {
        let t0 = std::time::Instant::now();
        let startup_error = match std::panic::catch_unwind(|| {
            zg_core::events::ensure_gbbq(false)
        }) {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(payload) => Some(format!(
                "PanicException: {}",
                crate::official::panic_text(payload.as_ref())
            )),
        };
        let selfcheck = std::panic::catch_unwind(crate::official::selfcheck)
            .unwrap_or_else(|payload| {
                vec![format!(
                    "selfcheck panic: {}",
                    crate::official::panic_text(payload.as_ref())
                )]
            });
        for w in &selfcheck {
            eprintln!("[czsc-bridge] selfcheck: {w}");
        }
        let boot_ms = t0.elapsed().as_millis() as i64;
        Bridge {
            startup_error,
            boot_ms,
            selfcheck,
        }
    }

    pub fn ping(&self) -> Value {
        if let Some(e) = &self.startup_error {
            return json!({"ok": false, "error": e});
        }
        let p = zg_core::paths::get();
        json!({
            "ok": true,
            "ver": CZSC_VERSION,
            "pid": std::process::id(),
            "ready": true,
            "boot_ms": self.boot_ms,
            "selfcheck": self.selfcheck,
            "gbbq": Value::Object(zg_core::events::gbbq_stats()),
            "roots": {
                "ZG_ROOT": p.root.display().to_string(),
                "ENGINE_ROOT": "local:czsc_bridge",
                "ZG_VIPDOC": p.vipdoc.display().to_string(),
                "ZG_GBBQ": p.gbbq_file.display().to_string(),
            },
        })
    }
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// `req.get("freq") or "D"` 后 `.upper()`。
fn req_freq(req: &Map<String, Value>) -> String {
    match req.get("freq") {
        Some(Value::String(s)) if !s.is_empty() => s.to_uppercase(),
        Some(Value::String(_)) | None | Some(Value::Null) => "D".to_string(),
        Some(other) => py_str(other).to_uppercase(),
    }
}

/// Python `int(req.get(k) or default)` 的等价：`0` / `""` / `None` / `false` 都取默认值。
/// 数值按 Python `int()` 语义向零截断（5.5 → 5），不可解析的字符串仍按异常处理（M10）。
fn py_int_or(v: Option<&Value>, default: i64) -> Result<i64, String> {
    match v {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(false)) => Ok(default),
        Some(Value::Bool(true)) => Ok(1),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                return Ok(if i == 0 { default } else { i });
            }
            match n.as_f64() {
                Some(0.0) => Ok(default),
                Some(f) if f.is_finite() => Ok(f.trunc() as i64),
                _ => Err("ValueError: cannot convert float to int".into()),
            }
        }
        Some(Value::String(s)) => {
            if s.is_empty() {
                Ok(default)
            } else {
                s.trim().parse::<i64>().map_err(|_| {
                    format!("ValueError: invalid literal for int() with base 10: {}", py_repr_str(s))
                })
            }
        }
        Some(_) => Ok(default),
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                n.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// `_write_response` 的分块判定。返回若干条待写响应（1 条 = 不分块）。
pub fn split_response(resp: Value, raw_len: usize) -> Vec<Value> {
    if raw_len <= MAX_RESPONSE_BYTES {
        return vec![resp];
    }
    let Value::Object(mut obj) = resp else {
        return vec![resp];
    };
    // 大包可能来自 hits / skipped / errors（全 skip 时 hits 为空，M12）
    let key = ["hits", "skipped", "errors"]
        .iter()
        .copied()
        .find(|k| {
            obj.get(*k)
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        });
    let Some(key) = key else {
        return vec![Value::Object(obj)];
    };
    let items = match obj.remove(key) {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let per = ((items.len() as f64) * (MAX_RESPONSE_BYTES as f64) * 0.8 / (raw_len.max(1) as f64))
        .max(1.0) as usize;
    let chunks: Vec<&[Value]> = items.chunks(per).collect();
    let mut head = obj;
    head.insert("chunks".into(), json!(chunks.len() as i64));
    let total = chunks.len();
    let mut out = Vec::with_capacity(total);
    for (i, ch) in chunks.into_iter().enumerate() {
        let mut piece = head.clone();
        piece.insert("chunk".into(), json!(i as i64 + 1));
        piece.insert(key.to_string(), Value::Array(ch.to_vec()));
        if i + 1 == total {
            piece.insert("done".into(), Value::Bool(true));
        }
        out.push(Value::Object(piece));
    }
    out
}

/// 处理一条请求。返回 `None` = 已自行写出响应（本 Rust 版不会出现）。
pub fn handle(bridge: &Bridge, req: &Map<String, Value>) -> Option<Value> {
    let rid = req.get("id").cloned().unwrap_or(Value::Null);
    let op = match req.get("op") {
        Some(Value::Null) | None => String::new(),
        Some(Value::String(s)) => s.trim().to_lowercase(),
        Some(other) => py_str(other).trim().to_lowercase(),
    };

    match op.as_str() {
        "ping" => {
            let mut resp = bridge.ping();
            if let Value::Object(ref mut m) = resp {
                m.insert("id".into(), rid);
            }
            Some(resp)
        }
        "analyze" => Some(handle_analyze(bridge, req, rid)),
        "bars" => Some(handle_bars(bridge, req, rid)),
        "scan" => handle_scan(bridge, req, rid),
        "shutdown" => Some(json!({"id": rid, "ok": true, "bye": true})),
        _ => Some(json!({
            "id": rid,
            "ok": false,
            "error": format!("未知操作: {}", py_repr_str(&op)),
        })),
    }
}

/// Python `repr(str)`：单引号、内部单引号转义。
pub fn py_repr_str(s: &str) -> String {
    if !s.contains('\'') {
        format!("'{s}'")
    } else if !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "\\'"))
    }
}

fn startup_err(bridge: &Bridge, rid: Value) -> Option<Value> {
    bridge
        .startup_error
        .as_ref()
        .map(|e| json!({"id": rid, "ok": false, "error": e}))
}

fn handle_analyze(bridge: &Bridge, req: &Map<String, Value>, rid: Value) -> Value {
    if let Some(e) = startup_err(bridge, rid.clone()) {
        return e;
    }
    let code = match req.get("code") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return json!({"id": rid, "ok": false, "error": "缺少参数 code"}),
    };
    let freq = req_freq(req);
    let market = opt_str(req.get("market"));
    let inline = req
        .get("bars")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty());

    let res = match inline {
        Some(arr) => {
            let mut bars: Vec<InlineBar> = Vec::with_capacity(arr.len());
            for v in arr {
                match serde_json::from_value::<InlineBar>(v.clone()) {
                    Ok(b) => bars.push(b),
                    Err(e) => {
                        return json!({
                            "id": rid, "ok": false,
                            "error": format!("ValueError: {e}"),
                            "code": code,
                        })
                    }
                }
            }
            build_payload_from_bars(&code, &freq, &bars, market.as_deref())
        }
        None => build_payload(&code, &freq, market.as_deref()),
    };
    match res {
        Ok(mut payload) => {
            if let Value::Object(ref mut m) = payload {
                m.insert("id".into(), rid);
            }
            payload
        }
        Err(e) => json!({"id": rid, "ok": false, "error": e, "code": code}),
    }
}

fn handle_bars(bridge: &Bridge, req: &Map<String, Value>, rid: Value) -> Value {
    if let Some(e) = startup_err(bridge, rid.clone()) {
        return e;
    }
    let code = match req.get("code") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return json!({"id": rid, "ok": false, "error": "缺少参数 code"}),
    };
    let freq = req_freq(req);
    let market = opt_str(req.get("market"));
    let raw = match bars_to_raw(&code, &freq, market.as_deref()) {
        Ok(v) => v,
        Err(e) => return json!({"id": rid, "ok": false, "error": e, "code": code}),
    };
    // Python `int(req.get("count") or 800)`：0/缺省/空串都是 800（falsy 语义）
    let n = match py_int_or(req.get("count"), 800) {
        Ok(v) => v,
        Err(e) => return json!({"id": rid, "ok": false, "error": e, "code": code}),
    };
    // Python `raw[-n:]`：n>0 取尾部 n 根；n<0 取 `raw[|n|:]`（去掉前 |n| 根）
    let tail: &[czsc_core::objects::bar::RawBar] = if n > 0 {
        let k = n as usize;
        if raw.len() > k {
            &raw[(raw.len() - k)..]
        } else {
            &raw[..]
        }
    } else {
        let k = (-n) as usize;
        if k < raw.len() {
            &raw[k..]
        } else {
            &raw[raw.len()..]
        }
    };
    let bars: Vec<Value> = tail
        .iter()
        .map(|b| {
            json!({
                "dt": date_str(&b.dt),
                "open": py_round(b.open, 3),
                "high": py_round(b.high, 3),
                "low": py_round(b.low, 3),
                "close": py_round(b.close, 3),
            })
        })
        .collect();
    json!({
        "id": rid,
        "ok": true,
        "code": code,
        "freq": freq,
        "n": tail.len() as i64,
        "bars": bars,
    })
}

/// 单次扫描上限（对齐 Python `_SCAN_TIMEOUT`）。
const SCAN_TIMEOUT_SECS: u64 = 900;

struct ScanGate {
    running: bool,
    done: usize,
    total: usize,
    responded: bool,
}

static SCAN_GATE: Mutex<ScanGate> =
    Mutex::new(ScanGate { running: false, done: 0, total: 0, responded: false });

/// 尝试占用扫描槽；已在进行中则返回 (done, total) 供「扫描进行中」回包（M11）。
fn scan_try_start(total: usize) -> Option<(usize, usize)> {
    let mut g = SCAN_GATE.lock_ok();
    if g.running {
        return Some((g.done, g.total));
    }
    g.running = true;
    g.done = 0;
    g.total = total;
    g.responded = false;
    None
}

fn scan_finish() {
    let mut g = SCAN_GATE.lock_ok();
    g.running = false;
    g.done = g.total;
}

/// 抢本次扫描的回包权（扫描线程与看门狗只允许一个回包）。
fn scan_claim_response() -> bool {
    let mut g = SCAN_GATE.lock_ok();
    if g.responded {
        false
    } else {
        g.responded = true;
        true
    }
}

fn scan_running() -> bool {
    SCAN_GATE.lock_ok().running
}

fn handle_scan(bridge: &Bridge, req: &Map<String, Value>, rid: Value) -> Option<Value> {
    if let Some(e) = startup_err(bridge, rid.clone()) {
        return Some(e);
    }
    let codes: Vec<String> = match req.get("codes") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().map(py_str).collect(),
        _ => Vec::new(),
    };
    if codes.is_empty() {
        return Some(json!({"id": rid, "ok": false, "error": "缺少参数 codes"}));
    }
    // Python `int(req.get("days") or 3)`：0 视为缺省
    let days = match py_int_or(req.get("days"), 3) {
        Ok(v) => v,
        Err(e) => return Some(json!({"id": rid, "ok": false, "error": e})),
    };
    let types = req.get("types").cloned();
    let freq = req_freq(req);
    if let Some((done, total)) = scan_try_start(codes.len()) {
        return Some(json!({
            "id": rid, "ok": false, "running": true,
            "error": "扫描进行中",
            "progress": format!("{done}/{total}"),
        }));
    }
    // 扫描在后台线程执行、由它自己回包（M11）：主循环继续响应 ping/analyze/bars。
    let rid_work = rid.clone();
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_scan(&codes, days, types.as_ref(), &freq, None)
        }));
        let resp = match res {
            Ok(r) => scan_response(rid_work, &r),
            Err(payload) => json!({
                "id": rid_work,
                "ok": false,
                "error": format!("PanicException: {}", crate::official::panic_text(payload.as_ref())),
            }),
        };
        scan_finish();
        if scan_claim_response() {
            write_stdout(resp);
        }
    });
    // 看门狗：超时先回错误；扫描线程无法中断，完成后因回包权已用掉不再重复回包。
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(SCAN_TIMEOUT_SECS));
        if scan_claim_response() {
            scan_finish();
            write_stdout(json!({
                "id": rid,
                "ok": false,
                "error": format!("扫描超时(>{SCAN_TIMEOUT_SECS}s)"),
            }));
        }
    });
    None
}

/// `Bridge.scan_response(rid)`：把 run_scan 结果拼成协议响应（键序照抄）。
///
/// run_scan 整体失败时结果里带 `error`，必须原样回 `ok:false`（H3）：
/// 否则全市场扫描全失败会被伪装成 `ok:true`，上层据此写「0 命中」缓存。
fn scan_response(rid: Value, res: &Value) -> Value {
    if let Some(err) = res.get("error") {
        return json!({"id": rid, "ok": false, "error": err});
    }
    let g = |k: &str| res.get(k).cloned().unwrap_or(Value::Null);
    json!({
        "id": rid,
        "ok": true,
        "ver": CZSC_VERSION,
        "elapsed_ms": g("elapsed_ms"),
        "workers": g("workers"),
        "n": g("n"),
        "asof": g("asof"),
        "data_last": g("data_last"),
        "days": g("days"),
        "types": g("types"),
        "freq": g("freq"),
        "hits": res.get("hits").and_then(|v| v.as_array().cloned()).unwrap_or_default(),
        "skipped": res.get("skipped").and_then(|v| v.as_array().cloned()).unwrap_or_default(),
        "errors": res.get("errors").and_then(|v| v.as_array().cloned()).unwrap_or_default(),
    })
}

/// 主循环：stdin 逐行 → 响应写 stdout。
pub fn serve() -> i32 {
    let bridge = Bridge::boot();
    if let Some(e) = &bridge.startup_error {
        eprintln!("[czsc-bridge] 启动失败: {e}");
    } else {
        eprintln!(
            "[czsc-bridge] 启动完成 boot={}ms czsc={CZSC_VERSION}",
            bridge.boot_ms
        );
    }
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => {
                // Python 在 EOF 后等扫描线程收尾（最多 timeout+60s）再退出
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(SCAN_TIMEOUT_SECS + 60);
                while scan_running() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                eprintln!("[czsc-bridge] stdin EOF，退出");
                return 0;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[czsc-bridge] stdin 读取异常: {e}");
                return 0;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_stdout(
                    json!({"id": Value::Null, "ok": false, "error": format!("非法JSON: {e}")}),
                );
                continue;
            }
        };
        let Value::Object(obj) = req else {
            write_stdout(
                json!({"id": Value::Null, "ok": false, "error": "非法JSON: 请求必须是 JSON 对象"}),
            );
            continue;
        };
        let is_shutdown = obj
            .get("op")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .as_deref()
            == Some("shutdown");
        let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle(&bridge, &obj)
        })) {
            Ok(r) => r,
            Err(payload) => Some(json!({
                "id": obj.get("id").cloned().unwrap_or(Value::Null),
                "ok": false,
                "error": format!(
                    "PanicException: {}",
                    crate::official::panic_text(payload.as_ref())
                ),
            })),
        };
        // None = scan 已转后台、由扫描线程自行回包
        if let Some(resp) = resp {
            write_stdout(resp);
            if is_shutdown {
                return 0;
            }
        }
    }
}

fn write_stdout(resp: Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write_one(&mut out, resp);
}

fn write_one<W: Write>(out: &mut W, resp: Value) {
    let raw = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec());
    for piece in split_response(resp, raw.len()) {
        let line = serde_json::to_vec(&piece).unwrap_or_else(|_| b"{}".to_vec());
        let _ = out.write_all(&line);
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_python() {
        assert_eq!(py_repr_str("nope"), "'nope'");
        assert_eq!(py_repr_str("a'b"), "\"a'b\"");
    }

    #[test]
    fn unknown_op_shape() {
        let b = Bridge {
            startup_error: None,
            boot_ms: 0,
            selfcheck: Vec::new(),
        };
        let req: Map<String, Value> =
            serde_json::from_str(r#"{"id":7,"op":" NOPE "}"#).unwrap();
        let resp = handle(&b, &req).unwrap();
        assert_eq!(resp["ok"], json!(false));
        assert_eq!(resp["error"], json!("未知操作: 'nope'"));
    }

    #[test]
    fn missing_code() {
        let b = Bridge {
            startup_error: None,
            boot_ms: 0,
            selfcheck: Vec::new(),
        };
        let req: Map<String, Value> = serde_json::from_str(r#"{"id":16,"op":"analyze"}"#).unwrap();
        let resp = handle(&b, &req).unwrap();
        assert_eq!(resp["error"], json!("缺少参数 code"));
    }

    #[test]
    fn split_keeps_small_response() {
        let resp = json!({"ok": true, "hits": [1, 2, 3]});
        let v = split_response(resp.clone(), 10);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], resp);
    }
}
