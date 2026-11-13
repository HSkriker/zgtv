use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};

use crate::paths;

type ScanMemo = Option<(String, Arc<Value>)>;

static SCAN_DOC: OnceLock<Mutex<ScanMemo>> = OnceLock::new();

pub fn payload(code: &str, freq: &str, limit: usize) -> Result<Value, String> {
    let path = paths::tv_root().join("data").join("market").join("czsc_scan_cache.json");
    // 按文件指纹 memo：整份扫描缓存只在落盘变化时重读重解析，其余请求零 IO
    let stamp = crate::cache::file_stamp(&path);
    let doc: Arc<Value> = {
        let mut g = SCAN_DOC.get_or_init(|| Mutex::new(None)).lock()
            .map_err(|_| "scan 缓存锁损坏".to_string())?;
        let fresh = matches!(g.as_ref(), Some((s, _)) if *s == stamp);
        if !fresh {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("读不到 {}: {e}", path.display()))?;
            let d: Value = serde_json::from_str(&text).map_err(|e| format!("扫描缓存解析失败: {e}"))?;
            *g = Some((stamp, Arc::new(d)));
        }
        match g.as_ref() {
            Some((_, d)) => d.clone(),
            None => return Err("scan 缓存不可用".to_string()),
        }
    };

    let mut rows: Vec<Value> = doc
        .get("hits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|h| {
            let f = h.get("freq").and_then(|v| v.as_str()).unwrap_or("");
            let c = h.get("code").and_then(|v| v.as_str()).unwrap_or("");
            f == freq && (code.is_empty() || c == code)
        })
        .collect();

    rows.sort_by(|a, b| {
        let key = |x: &Value| {
            (
                x.get("dt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                x.get("confirm_dt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            )
        };
        key(b).cmp(&key(a))
    });
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }

    let data_last = doc.get("data_last").and_then(|v| v.as_str()).unwrap_or("");
    let market_last = crate::bars::market_last_date().unwrap_or_default();
    Ok(json!({
        "ok": true,
        "asof": doc.get("asof").cloned().unwrap_or(Value::Null),
        "data_last": doc.get("data_last").cloned().unwrap_or(Value::Null),
        "session": crate::live::session_today(),
        "stale": data_last != market_last,
        "n": rows.len(),
        "rows": rows,
    }))
}

// ── 本地重扫：zg_chan::run_scan（已随 crate 内嵌）+ zg 的缓存写盘口径（aux_svc.rs:784-808） ──

static REBUILDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn cache_path() -> std::path::PathBuf {
    paths::tv_root().join("data").join("market").join("czsc_scan_cache.json")
}

/// 过期判定：缓存 data_last ≠ 全市场最新交易日（新日线同步后必触发）
pub fn stale() -> bool {
    let Some(last) = crate::bars::market_last_date() else { return false };
    let data_last = std::fs::read_to_string(cache_path()).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|d| d.get("data_last").and_then(|v| v.as_str()).map(|s| s.to_string()));
    data_last.as_deref() != Some(last.as_str())
}

/// REBUILDING 的 RAII 守卫：rebuild_inner panic（栈展开）也会复位标志，重扫端点不再永久 409
struct RebuildGuard(&'static std::sync::atomic::AtomicBool);
impl Drop for RebuildGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 异步触发重扫（端点用）：全市场 1-6 分钟，已在跑时拒绝
pub fn start_rebuild() -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return Err("缠论扫描已在进行".to_string());
    }
    std::thread::spawn(|| {
        let t0 = std::time::Instant::now();
        let _g = RebuildGuard(&REBUILDING);
        let r = rebuild_inner(t0);
        match &r {
            Ok(v) => println!("[tv] 缠论扫描本地重扫完成：{} 命中 / {} 只（{} s）",
                v.get("hits").and_then(|x| x.as_u64()).unwrap_or(0),
                v.get("n").and_then(|x| x.as_u64()).unwrap_or(0),
                v.get("elapsed").and_then(|x| x.as_f64()).unwrap_or(0.0)),
            Err(e) => println!("[tv] 缠论扫描本地重扫失败（{} ms）：{e}", t0.elapsed().as_millis()),
        }
    });
    Ok(())
}

/// 同步重扫（启动线程与验证用）
pub fn rebuild() -> Result<Value, String> {
    use std::sync::atomic::Ordering;
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return Err("缠论扫描已在进行".to_string());
    }
    let _g = RebuildGuard(&REBUILDING);
    let t0 = std::time::Instant::now();
    let r = rebuild_inner(t0);
    if let Ok(v) = &r {
        println!("[tv] 缠论扫描本地重扫完成：{} 命中 / {} 只（{} s）",
            v.get("hits").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("n").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("elapsed").and_then(|x| x.as_f64()).unwrap_or(0.0));
    }
    r.clone()
}

fn rebuild_inner(t0: std::time::Instant) -> Result<Value, String> {
    // 股票列表口径对齐 zg aux_svc.rs:709-714：6 位数字，排除 4/8/92 开头（北交所/退市）
    let rows = crate::symbols::all()?;
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut codes: Vec<String> = Vec::new();
    for r in rows.iter() {
        let Some(c) = r.get("code").and_then(|v| v.as_str()) else { continue };
        if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_digit())
            && !(c.starts_with('4') || c.starts_with('8') || c.starts_with("92"))
        {
            codes.push(c.to_string());
            names.insert(c.to_string(),
                r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string());
        }
    }
    if codes.is_empty() {
        return Err("股票列表为空（data/symbols.json）".to_string());
    }

    let resp = zg_chan::scan::run_scan(&codes, 5, None, "D", None);
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err(resp.get("error").and_then(|e| e.as_str()).unwrap_or("扫描失败").to_string());
    }
    let mut hits = resp.get("hits").and_then(|h| h.as_array()).cloned().unwrap_or_default();
    for h in hits.iter_mut() {
        if let Some(o) = h.as_object_mut() {
            let code = o.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
            if !o.contains_key("name") || o.get("name").map(|n| n.is_null()).unwrap_or(true) {
                o.insert("name".into(), Value::String(names.get(&code).cloned().unwrap_or_default()));
            }
        }
    }
    let errors = resp.get("errors").cloned().unwrap_or(json!([]));
    let skipped = resp.get("skipped").cloned().unwrap_or(json!([]));
    let errors_n = errors.as_array().map(|a| a.len()).unwrap_or(0) as i64;
    let skipped_n = skipped.as_array().map(|a| a.len()).unwrap_or(0) as i64;
    let n_total = resp.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
    // 覆盖率判据同 zg aux_svc.rs:777：skip+err 超 20% 视为数据源异常，不落缓存
    if n_total > 0 && (skipped_n + errors_n) * 100 > n_total * 20 {
        return Err(format!(
            "扫描仅覆盖 {}/{}（skip {skipped_n} / err {errors_n}），数据源异常，不落缓存",
            (n_total - skipped_n - errors_n).max(0), n_total));
    }
    if hits.is_empty() && errors_n > 0 {
        return Err("零命中且有错误，不落缓存".to_string());
    }
    let data_last = resp.get("data_last").and_then(|d| d.as_str()).map(|s| s.to_string())
        .or_else(crate::bars::market_last_date)
        .ok_or("无 data_last 且无交易日基准")?;
    let cache = json!({
        "asof": data_last,
        "data_last": data_last,
        "scan_date": zg_core::calendar::last_trading_date(None),
        "elapsed": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "days": 5, "types": ["1", "2", "3"],
        "ver": zg_chan::payload::CZSC_VERSION,
        "n": n_total,
        "method_ver": zg_chan::CZSC_METHOD_VER,
        "hits": hits, "errors": errors, "skipped": skipped,
    });
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&path);
    std::fs::write(&tmp, serde_json::to_vec(&cache).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写扫描缓存失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落扫描缓存失败: {e}"))?;
    Ok(json!({"ok": true, "n": n_total, "hits": cache.get("hits").and_then(|h| h.as_array()).map(|a| a.len()).unwrap_or(0),
              "elapsed": cache.get("elapsed").cloned().unwrap_or(Value::Null)}))
}
