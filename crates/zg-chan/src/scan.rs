//! `zgbridge.scan` 的 Rust 移植：全市场缠论买点扫描。
//!
//! 与 Python 一致的关键点：
//!   * `workers = max(1, min(8, cpu//2))`，再 `min(workers, len(codes))`；空输入返回
//!     **不含 days/types/freq** 的短对象（协议事实，别"补齐"）。
//!   * `chunk_size = ceil(n / (workers*4))`。
//!   * 每片内逐 code 取数 → `buy_sell_summary` → `_point_hits`（买 + 类型白名单 +
//!     `confirm_dt >= cutoff`）。
//!   * 汇总：同 `(code, point_type)` 只留 `confirm_dt` 最大的一条；再按
//!     `(confirm_dt, code)` 降序**稳定**排序。
//!
//! 并行只影响 `skipped/errors` 的先后（Python 侧 `as_completed` 同样不确定）；
//! 命中列表由排序键唯一确定，故与 Python 可逐值对表。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use czsc_core::analyze::CZSC;
use serde_json::{json, Value};
use zg_core::sync::MutexExt;

use crate::common::{date_str, py_round};
use crate::points::{buy_sell_summary, BuySellPoint};
use crate::service::{build_czsc, MKT_BJ};

pub const ALL_TYPES: [&str; 3] = ["第一类", "第二类", "第三类"];

fn type_alias(s: &str) -> Option<&'static str> {
    Some(match s {
        "1" | "一" | "一类" | "第一类" => "第一类",
        "2" | "二" | "二类" | "第二类" => "第二类",
        "3" | "三" | "三类" | "第三类" => "第三类",
        _ => return None,
    })
}

fn all_types() -> Vec<String> {
    ALL_TYPES.iter().map(|s| (*s).to_string()).collect()
}

/// `str(x)` 的窄口径（scan 只用于 types 归一；数字按 Python 规则）。
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
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// `types` 归一成 `('第一类','第二类','第三类')` 的子集；空/None → 全要。
pub fn normalize_types(types: Option<&Value>) -> Vec<String> {
    let Some(v) = types else { return all_types() };
    if v.is_null() {
        return all_types();
    }
    let items: Vec<String> = match v {
        Value::String(s) => vec![s.clone()],
        Value::Number(_) | Value::Bool(_) => vec![py_str(v)],
        Value::Array(a) => a.iter().map(py_str).collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for t in items {
        let s = t.trim();
        if s.is_empty() {
            continue;
        }
        let mut key = type_alias(s);
        if key.is_none() {
            let stripped = s.trim_start_matches('第').trim_end_matches('类');
            key = type_alias(stripped);
        }
        if let Some(k) = key {
            if !out.iter().any(|x| x == k) {
                out.push(k.to_string());
            }
        }
    }
    if out.is_empty() {
        all_types()
    } else {
        out
    }
}

fn r3(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(py_round(x, 3)),
        None => Value::Null,
    }
}

fn r4(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(py_round(x, 4)),
        None => Value::Null,
    }
}

/// 从 points 里挑出命中项（买 + 类型白名单 + `confirm_dt` 在窗口内）。
pub fn point_hits(
    code: &str,
    market: &str,
    freq: &str,
    points: &[BuySellPoint],
    cutoff: Option<&str>,
    wanted: &[String],
) -> Vec<Value> {
    let mut hits = Vec::new();
    for p in points {
        if p.direction != "买" {
            continue;
        }
        let pt = p.point_type.as_str();
        if !ALL_TYPES.iter().any(|t| pt.starts_with(t)) {
            continue;
        }
        if !wanted.iter().any(|w| w == pt) {
            continue;
        }
        let cdt = p.confirm_dt.map(|d| date_str(&d)).unwrap_or_default();
        if cdt.is_empty() {
            continue;
        }
        if let Some(cut) = cutoff {
            if cdt.as_str() < cut {
                continue;
            }
        }
        let dt = p.dt.map(|d| date_str(&d)).unwrap_or_default();
        hits.push(json!({
            "code": code,
            "market": market,
            "freq": freq,
            "point_type": pt,
            "direction": p.direction,
            "operate": p.operate,
            "price": r3(p.price),
            "dt": dt,
            "confirm_dt": cdt,
            "ref_price": r4(p.ref_price),
            "ref_price_basis": p.ref_price_basis,
            "signal_name": p.signal_name,
            "in_finished_bis": p.in_finished_bis,
            "method_label": p.method_label,
            "qualification": Value::Object(p.qualification.clone()),
            "stop_ref": Value::Object(p.stop_ref.clone()),
            "level": p.level,
            "source_method": p.source_method,
        }));
    }
    hits
}

/// 一片代码的扫描结果。
#[derive(Default)]
pub struct ChunkResult {
    pub hits: Vec<Value>,
    pub done: usize,
    pub skipped: Vec<Value>,
    pub errors: Vec<Value>,
    pub data_last: Option<String>,
}

fn skip(code: &str, reason: impl Into<String>) -> Value {
    json!({"code": code, "reason": reason.into()})
}

fn err(code: &str, error: impl Into<String>) -> Value {
    json!({"code": code, "error": error.into()})
}

/// 除权事件源/前复权锚点类错误：整只股票的分析结果不可信，按「数据源异常」归类。
fn is_data_integrity_error(msg: &str) -> bool {
    ["gbbq 不可用", "前复权锚点", "除权事件", "除权记录", "xdxr"]
        .iter()
        .any(|k| msg.contains(k))
}

/// 单只股票的扫描（Python `scan_chunk` 的循环体）。
fn scan_one(code_raw: &str, days: i64, wanted: &[String], freq: &str, out: &mut ChunkResult) {
    let code = code_raw.trim();
    let (mkt, cd) = match zg_core::reader::resolve(code, None) {
        Ok(v) => v,
        Err(e) => {
            out.skipped.push(skip(code, e.to_string()));
            return;
        }
    };
    if mkt == MKT_BJ {
        out.skipped.push(skip(code, "北交所暂不支持"));
        return;
    }
    if zg_core::reader::is_index(&mkt, &cd) {
        out.skipped.push(skip(code, "指数/板块不参与扫描"));
        return;
    }
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        build_czsc(&cd, freq, Some(&mkt))
    }));
    let (c, bars): (CZSC, Vec<czsc_core::objects::bar::RawBar>) = match built {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            // 除权/复权/事件源类错误让整只股票的结论不可信：进 errors 而不是 skipped，
            // 调用方据此判「数据源异常」，而不是把「扫不出来」当成「没有信号」缓存下来
            if is_data_integrity_error(&e) {
                out.errors.push(err(code, e));
            } else {
                out.skipped.push(skip(code, e));
            }
            return;
        }
        Err(payload) => {
            out.errors.push(err(
                code,
                format!(
                    "PanicException: {}",
                    crate::official::panic_text(payload.as_ref())
                ),
            ));
            return;
        }
    };
    if bars.is_empty() {
        out.skipped.push(skip(code, "无K线"));
        return;
    }
    let bar_last = date_str(&bars[bars.len() - 1].dt);
    if out.data_last.as_deref().is_none_or(|d| bar_last.as_str() > d) {
        out.data_last = Some(bar_last);
    }
    let n = days.max(1) as usize;
    let idx = if bars.len() >= n { bars.len() - n } else { 0 };
    let cutoff = date_str(&bars[idx].dt);

    let summary = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        buy_sell_summary(&c, None)
    }));
    match summary {
        Ok(s) => {
            out.hits
                .extend(point_hits(&cd, &mkt, freq, &s.points, Some(&cutoff), wanted));
        }
        Err(payload) => out.errors.push(err(
            code,
            format!(
                "PanicException: {}",
                crate::official::panic_text(payload.as_ref())
            ),
        )),
    }
}

/// 顶层扫描入口（Python `scan_chunk`）：一片代码 → hits/计数/错误。
pub fn scan_chunk(
    codes: &[String],
    days: i64,
    wanted: &[String],
    freq: &str,
) -> ChunkResult {
    let mut out = ChunkResult::default();
    for code in codes {
        // 单票 panic 隔离：只丢这一只，同片其余股票的结论必须保留。
        // scan_one 内部已对建仓与摘要各自兜底，这里兜住的是其余环节
        // （如 point_hits 的形态匹配）——否则整片的 hits 会被一次 panic 抹掉。
        let one = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut one = ChunkResult::default();
            scan_one(code, days, wanted, freq, &mut one);
            one
        }));
        match one {
            Ok(mut r) => {
                out.hits.append(&mut r.hits);
                out.skipped.append(&mut r.skipped);
                out.errors.append(&mut r.errors);
                if out.data_last.as_deref().is_none_or(|d| {
                    r.data_last.as_deref().is_some_and(|x| x > d)
                }) {
                    out.data_last = r.data_last;
                }
            }
            Err(payload) => out.errors.push(err(
                code.trim(),
                format!(
                    "PanicException: {}",
                    crate::official::panic_text(payload.as_ref())
                ),
            )),
        }
    }
    out.done = codes.len();
    out
}

/// CPU 数（Python `os.cpu_count()`）；拿不到按 2 算。
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}

/// 全市场扫描（被 protocol 调用）。
pub fn run_scan(
    codes: &[String],
    days: i64,
    types: Option<&Value>,
    freq: &str,
    workers: Option<usize>,
) -> Value {
    let codes: Vec<String> = codes
        .iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    let wanted = normalize_types(types);
    let n_cpu = cpu_count();
    let mut n_workers = workers.unwrap_or_else(|| {
        let half = n_cpu / 2;
        half.clamp(1, 8)
    });
    n_workers = n_workers.max(1).min(codes.len().max(1));
    let t0 = Instant::now();
    let freq = freq.to_uppercase();

    if codes.is_empty() {
        // Python 空输入分支**没有** days/types/freq 三键，照抄。
        // `ok` 是调用方（zg-api 后台扫描）判成功的依据：协议层 scan_response 会自己补，
        // 直接调 run_scan 的调用方拿不到 → 必须在这里给。
        return json!({
            "ok": true,
            "hits": [],
            "n": 0,
            "elapsed_ms": 0,
            "workers": 0,
            "skipped": [],
            "errors": [],
            "asof": Value::Null,
            "data_last": Value::Null,
        });
    }

    let chunk_size = ((codes.len() as f64) / ((n_workers * 4) as f64)).ceil() as usize;
    let chunk_size = chunk_size.max(1);
    let chunks: Vec<Vec<String>> = codes
        .chunks(chunk_size)
        .map(|c| c.to_vec())
        .collect();

    // 多线程跑分片：结果按分片序号回填，保证 skipped/errors 顺序稳定
    // （Python 用 as_completed，顺序本就不确定；命中列表由排序键唯一确定）。
    let chunk_refs: Vec<&Vec<String>> = chunks.iter().collect();
    let next = AtomicUsize::new(0);
    // 分片结果即时写入共享表：线程若在后续步骤崩溃，先前完成的分片不会丢
    let results: Mutex<Vec<(usize, ChunkResult)>> = Mutex::new(Vec::new());
    let panicked = std::thread::scope(|s| {
        let results = &results;
        let handles: Vec<_> = (0..n_workers)
            .map(|_| {
                let next = &next;
                let chunk_refs = &chunk_refs;
                let wanted = &wanted;
                let freq = &freq;
                s.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(chunk) = chunk_refs.get(i) else { break };
                    // 分片内 panic 只丢这一片，已完成的 chunk 必须保留
                    let cr = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        scan_chunk(chunk, days, wanted, freq)
                    })) {
                        Ok(cr) => cr,
                        Err(p) => ChunkResult {
                            errors: vec![err(
                                "*",
                                format!(
                                    "PanicException: {}",
                                    crate::official::panic_text(p.as_ref())
                                ),
                            )],
                            ..Default::default()
                        },
                    };
                    results.lock_ok().push((i, cr));
                })
            })
            .collect();
        let mut panicked = 0usize;
        for h in handles {
            if h.join().is_err() {
                panicked += 1;
            }
        }
        panicked
    });
    let mut collected = results.into_inner().unwrap_or_else(|e| e.into_inner());
    for _ in 0..panicked {
        collected.push((
            usize::MAX,
            ChunkResult {
                errors: vec![err("*", "PanicException: 扫描分片线程崩溃")],
                ..Default::default()
            },
        ));
    }
    collected.sort_by_key(|(i, _)| *i);

    let mut hits: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut data_last: Option<String> = None;
    let mut done = 0usize;
    let n_chunks = collected.len();
    for (_, r) in collected {
        hits.extend(r.hits);
        skipped.extend(r.skipped);
        errors.extend(r.errors);
        if let Some(dl) = r.data_last {
            if data_last.as_deref().is_none_or(|d| dl.as_str() > d) {
                data_last = Some(dl);
            }
        }
        done += r.done;
    }
    // 进度可观测：done = 非崩溃分片实际处理的票数（n 是请求总数，两者可能不等）
    eprintln!("[scan] 完成 {done}/{} 只，分片 {} 个", codes.len(), n_chunks);

    // best：dict 插入序 + `confirm_dt` 严格更大才替换
    let mut order: Vec<(String, String)> = Vec::new();
    let mut best: HashMap<(String, String), Value> = HashMap::new();
    for h in hits {
        let key = (
            h.get("code").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            h.get("point_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        );
        let cdt = h
            .get("confirm_dt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match best.get(&key) {
            Some(cur) => {
                let cur_cdt = cur
                    .get("confirm_dt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if cdt.as_str() > cur_cdt {
                    best.insert(key, h);
                }
            }
            None => {
                order.push(key.clone());
                best.insert(key, h);
            }
        }
    }
    let mut ordered: Vec<Value> = order
        .into_iter()
        .map(|k| best.remove(&k).expect("best 键必存在"))
        .collect();
    // sorted(key=(confirm_dt, code), reverse=True) 的稳定等价：
    // 降序比较、相等保持原相对次序。
    ordered.sort_by(|a, b| {
        let ka = (
            a.get("confirm_dt").and_then(|v| v.as_str()).unwrap_or(""),
            a.get("code").and_then(|v| v.as_str()).unwrap_or(""),
        );
        let kb = (
            b.get("confirm_dt").and_then(|v| v.as_str()).unwrap_or(""),
            b.get("code").and_then(|v| v.as_str()).unwrap_or(""),
        );
        kb.cmp(&ka)
    });
    let asof: Option<String> = ordered
        .iter()
        .filter_map(|h| h.get("confirm_dt").and_then(|v| v.as_str()))
        .max()
        .map(|s| s.to_string());
    let elapsed = t0.elapsed().as_millis() as i64;
    let skipped_n = skipped.len();
    let errors_n = errors.len();
    // 全部代码都未能完成（全 skip / 全 error）时 ok=false：这不是「无信号」，
    // 调用方不能把它当成有效结果落缓存
    let ok = skipped_n + errors_n < codes.len() || !ordered.is_empty();

    let mut out = json!({
        "ok": ok,
        "hits": ordered,
        "n": codes.len() as i64,
        "elapsed_ms": elapsed,
        "workers": n_workers as i64,
        "skipped": skipped,
        "errors": errors,
        "asof": match asof { Some(s) => json!(s), None => Value::Null },
        "data_last": match data_last { Some(s) => json!(s), None => Value::Null },
        "days": days,
        "types": wanted,
        "freq": freq,
    });
    if !ok {
        out["error"] = json!(format!(
            "扫描全部失败：{} 只均未完成（skip {skipped_n} / err {errors_n}）",
            codes.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_aliases() {
        let v = json!(["一", "第二类", "3", "x", "", "第三类"]);
        assert_eq!(
            normalize_types(Some(&v)),
            vec!["第一类".to_string(), "第二类".to_string(), "第三类".to_string()]
        );
    }

    #[test]
    fn types_strip_prefix_suffix() {
        // `第1类` → lstrip("第")/rstrip("类") → "1" → 第一类（Python 同表）；
        // `第 二 类` 中间带空格 → 归一失败被丢弃（lstrip/rstrip 不 trim）。
        let v = json!(["第1类", "第 二 类", "三类"]);
        assert_eq!(
            normalize_types(Some(&v)),
            vec!["第一类".to_string(), "第三类".to_string()]
        );
    }

    #[test]
    fn types_none_is_all() {
        assert_eq!(normalize_types(None), all_types());
        assert_eq!(normalize_types(Some(&Value::Null)), all_types());
        let empty: Vec<Value> = vec![];
        assert_eq!(normalize_types(Some(&json!(empty))), all_types());
    }

    #[test]
    fn workers_formula() {
        // nproc=16 → min(8, 16//2) = 8
        assert_eq!(cpu_count().max(2) / 2, 8);
    }
}
