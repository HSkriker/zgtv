//! 8787 盘中视图 —— 对齐 `modules/monitor_app/services/sentiment.py`：
//! 微盘超额 / 情绪指数 / 行业情绪 / 指数对比 / 细分成分股（均带进程内 TTL 缓存）。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::Mutex;

use chrono::TimeZone;
use serde_json::{json, Map, Value};

use crate::monitor_flow as flow;
use crate::monitor_ths as ths;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Cache {
    ts: f64,
    data: Option<Value>,
}

static IDX_CACHE: Mutex<Option<(f64, Vec<Value>, String)>> = Mutex::new(None);
static MICRO_CACHE: Mutex<Option<(f64, Value)>> = Mutex::new(None);
static SENT_CACHE: Mutex<Option<(f64, Value)>> = Mutex::new(None);
static SECT_SENT_CACHE: Mutex<Option<HashMap<String, (f64, Value)>>> = Mutex::new(None);
static CONS_CACHE: Mutex<Option<HashMap<String, Cache>>> = Mutex::new(None);
static SECT_NAME2CODE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn idx_snapshot(code: &str, timeout: u64, retries: u32) -> Option<Value> {
    let d = ths::ths_get("/api/a-share-index/prices/snapshot",
                         &[("thscodes", code.to_string())], timeout, retries, 1).ok()?;
    d.get("item").and_then(|x| x.as_array()).and_then(|a| a.first()).cloned()
}

fn micro_pct() -> Option<f64> {
    {
        let g = IDX_CACHE.lock_ok();
        if let Some((_ts, items, _asof)) = g.as_ref() {
            for it in items {
                if it.get("code").and_then(|c| c.as_str()) == Some("883418.TI") {
                    if let Some(p) = it.get("pct").and_then(|v| v.as_f64()) {
                        return Some(p);
                    }
                }
            }
        }
    }
    match idx_snapshot("883418.TI", 10, 1) {
        Some(it) => ths::number(it.get("price_change_ratio_pct"))
            .map(|p| zg_core::model::py_round(p, 2)),
        None => {
            eprintln!("[micro] pct fail");
            None
        }
    }
}

/// `/api/flowlab/micro`：一级行业 vs 微盘股(883418.TI) 当日超额（60s 缓存）。
pub fn micro_payload() -> Value {
    let t = now();
    {
        let g = MICRO_CACHE.lock_ok();
        if let Some((ts, data)) = g.as_ref() {
            if t - ts < 60.0 {
                return data.clone();
            }
        }
    }
    let live = flow::flow_live();
    let sectors = live.get("sectors").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let mp = match micro_pct() {
        Some(p) => p,
        None => return json!({"micro_pct": Value::Null, "item": [], "n": 0, "error": "微盘股快照缺失"}),
    };
    if sectors.is_empty() {
        return json!({"micro_pct": mp, "item": [], "n": 0, "error": "暂无板块数据"});
    }
    let mut rows: Vec<Value> = sectors
        .iter()
        .map(|(n, s)| {
            let pct = s.get("pct").and_then(|x| x.as_f64());
            json!({
                "name": n,
                "pct": pct.map(Value::from).unwrap_or(Value::Null),
                "rel": pct.map(|p| Value::from(zg_core::model::py_round(p - mp, 2))).unwrap_or(Value::Null),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let ra = a.get("rel").and_then(|x| x.as_f64()).unwrap_or(-9999.0);
        let rb = b.get("rel").and_then(|x| x.as_f64()).unwrap_or(-9999.0);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    let out = json!({
        "micro_pct": zg_core::model::py_round(mp, 2),
        "item": rows, "n": rows.len(),
        "asof": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "error": Value::Null,
    });
    *MICRO_CACHE.lock_ok() = Some((t, out.clone()));
    out
}

fn pctile_of(cur: f64, lo: f64, hi: f64) -> Value {
    if hi > lo {
        Value::from(zg_core::model::py_round((cur - lo) / (hi - lo) * 100.0, 1))
    } else {
        Value::Null
    }
}

fn closes_of(bars: &[Value]) -> Vec<f64> {
    bars.iter().filter_map(|b| b.get("close_price").and_then(|x| x.as_f64())).collect()
}

fn d_of(ms: i64) -> String {
    chrono::Local.timestamp_millis_opt(ms).single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn hist_bars(code: &str, start_ms: i64, end_ms: i64, timeout: u64) -> Vec<Value> {
    let params = vec![
        ("thscode", code.to_string()),
        ("interval", "1d".to_string()),
        ("start", start_ms.to_string()),
        ("end", end_ms.to_string()),
    ];
    match ths::ths_get("/api/a-share-index/prices/historical", &params, timeout, 2, 1) {
        Ok(d) => {
            let mut bars: Vec<Value> = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            bars.sort_by_key(|b| b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0));
            bars
        }
        Err(_) => Vec::new(),
    }
}

/// `/api/flowlab/sentiment`：市场情绪指数面板（883404.TI，600s 缓存）。
pub fn sentiment_payload() -> Value {
    let t = now();
    {
        let g = SENT_CACHE.lock_ok();
        if let Some((ts, data)) = g.as_ref() {
            if t - ts < 600.0 {
                return data.clone();
            }
        }
    }
    let result = (|| -> Value {
        let end_ms = (t * 1000.0) as i64;
        let start_ms = end_ms - 370 * 86400 * 1000;
        let bars1 = hist_bars("883404.TI", start_ms, end_ms, 15);
        if bars1.len() < 30 {
            return json!({"error": format!("情绪指数历史不足({}根)", bars1.len())});
        }
        let bars2 = hist_bars("000300.SH", start_ms, end_ms, 15);
        let snap = idx_snapshot("883404.TI", 12, 2).unwrap_or(json!({}));
        let closes = closes_of(&bars1);
        let cur = snap.get("last_price").and_then(|x| x.as_f64())
            .or_else(|| closes.last().copied())
            .unwrap_or(0.0);
        let pct = ths::number(snap.get("price_change_ratio_pct"));
        let lo = closes.iter().cloned().fold(f64::MAX, f64::min);
        let hi = closes.iter().cloned().fold(f64::MIN, f64::max);
        let pctile = pctile_of(cur, lo, hi);
        let pctile_sub = |sub: &[f64]| -> Value {
            let l = sub.iter().cloned().fold(f64::MAX, f64::min);
            let h = sub.iter().cloned().fold(f64::MIN, f64::max);
            pctile_of(cur, l, h)
        };
        let n_len = closes.len();
        let pctiles = json!({
            "m1": pctile_sub(&closes[n_len.saturating_sub(21)..]),
            "m3": pctile_sub(&closes[n_len.saturating_sub(63)..]),
            "m6": pctile_sub(&closes[n_len.saturating_sub(126)..]),
            "y1": pctile.clone(),
        });
        let n = 63usize;
        let s1 = &bars1[bars1.len().saturating_sub(n)..];
        let s2 = &bars2[bars2.len().saturating_sub(n)..];
        let base1 = s1.first().and_then(|b| b.get("close_price")).and_then(|x| x.as_f64()).unwrap_or(1.0);
        let base2 = s2.first().and_then(|b| b.get("close_price")).and_then(|x| x.as_f64()).unwrap_or(1.0);
        let mut seq: Vec<Value> = Vec::new();
        for i in 0..s1.len().min(s2.len()) {
            let c1 = s1[i].get("close_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let c2 = s2[i].get("close_price").and_then(|x| x.as_f64());
            let ms = s1[i].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            let d = d_of(ms);
            seq.push(json!({
                "d": if d.len() >= 5 { &d[5..] } else { &d },
                "sent": zg_core::model::py_round(c1 / base1 * 100.0, 1),
                "hs300": c2.map(|v| Value::from(zg_core::model::py_round(v / base2 * 100.0, 1))).unwrap_or(Value::Null),
            }));
        }
        let lo_idx = closes.iter().position(|c| *c == lo).unwrap_or(0);
        let hi_idx = closes.iter().position(|c| *c == hi).unwrap_or(0);
        json!({
            "name": "同花顺情绪指数",
            "cur": zg_core::model::py_round(cur, 1),
            "pct": pct.map(Value::from).unwrap_or(Value::Null),
            "lo": zg_core::model::py_round(lo, 1),
            "hi": zg_core::model::py_round(hi, 1),
            "mid": zg_core::model::py_round((lo + hi) / 2.0, 1),
            "pctile": pctile, "pctiles": pctiles,
            "lo_date": d_of(bars1[lo_idx].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0)),
            "hi_date": d_of(bars1[hi_idx].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0)),
            "n": bars1.len(), "seq": seq, "error": Value::Null,
            "asof": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    })();
    if result.get("error").map(|e| e.is_null()).unwrap_or(true) {
        *SENT_CACHE.lock_ok() = Some((t, result.clone()));
    }
    result
}

fn sector_name2code() -> HashMap<String, String> {
    {
        let g = SECT_NAME2CODE.lock_ok();
        if let Some(m) = g.as_ref() {
            return m.clone();
        }
    }
    let (_codes, names) = flow::l1_industries();
    let m: HashMap<String, String> = names.iter().map(|(c, n)| (n.clone(), c.clone())).collect();
    // 空表不能固化：目录抓取失败/未落盘时缓存空映射，会让所有行业此后恒「未知行业」
    if !m.is_empty() {
        *SECT_NAME2CODE.lock_ok() = Some(m.clone());
    }
    m
}

fn sector_sentiment(name: &str) -> Result<Value, String> {
    let code = sector_name2code().get(name).cloned()
        .ok_or_else(|| format!("未知行业: {name}"))?;
    let t = now();
    let end_ms = (t * 1000.0) as i64;
    let start_ms = end_ms - 370 * 86400 * 1000;
    let bars = hist_bars(&code, start_ms, end_ms, 15);
    if bars.len() < 30 {
        return Err(format!("行业历史不足({}根)", bars.len()));
    }
    let bars2 = hist_bars("000300.SH", start_ms, end_ms, 15);
    let snap = idx_snapshot(&code, 12, 2).unwrap_or(json!({}));
    let closes = closes_of(&bars);
    let cur = ths::number(snap.get("last_price")).or_else(|| closes.last().copied()).unwrap_or(0.0);
    let pct = ths::number(snap.get("price_change_ratio_pct"));
    let lo = closes.iter().cloned().fold(f64::MAX, f64::min);
    let hi = closes.iter().cloned().fold(f64::MIN, f64::max);
    let pctile = pctile_of(cur, lo, hi);
    let pctile_sub = |sub: &[f64]| -> Value {
        let l = sub.iter().cloned().fold(f64::MAX, f64::min);
        let h = sub.iter().cloned().fold(f64::MIN, f64::max);
        pctile_of(cur, l, h)
    };
    let n_len = closes.len();
    let pctiles = json!({
        "m1": pctile_sub(&closes[n_len.saturating_sub(21)..]),
        "m3": pctile_sub(&closes[n_len.saturating_sub(63)..]),
        "m6": pctile_sub(&closes[n_len.saturating_sub(126)..]),
        "y1": pctile.clone(),
    });
    let seg = &bars[bars.len().saturating_sub(63)..];
    let seg2 = &bars2[bars2.len().saturating_sub(63)..];
    let base = seg.first().and_then(|b| b.get("close_price")).and_then(|x| x.as_f64()).unwrap_or(1.0);
    let base2 = if seg2.is_empty() {
        None
    } else {
        Some(seg2[0].get("close_price").and_then(|x| x.as_f64()).unwrap_or(1.0))
    };
    let mut seq: Vec<Value> = Vec::new();
    for i in 0..seg.len().min(seg2.len()) {
        let c1 = seg[i].get("close_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let c2 = seg2[i].get("close_price").and_then(|x| x.as_f64());
        let ms = seg[i].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let d = d_of(ms);
        seq.push(json!({
            "d": if d.len() >= 5 { &d[5..] } else { &d },
            "sent": zg_core::model::py_round(c1 / base * 100.0, 1),
            "hs300": match (c2, base2) {
                (Some(c2), Some(b2)) => Value::from(zg_core::model::py_round(c2 / b2 * 100.0, 1)),
                _ => Value::Null,
            },
        }));
    }
    let lo_idx = closes.iter().position(|c| *c == lo).unwrap_or(0);
    let hi_idx = closes.iter().position(|c| *c == hi).unwrap_or(0);
    Ok(json!({
        "name": name, "code": code,
        "cur": zg_core::model::py_round(cur, 2),
        "pct": pct.map(Value::from).unwrap_or(Value::Null),
        "lo": zg_core::model::py_round(lo, 2),
        "hi": zg_core::model::py_round(hi, 2),
        "mid": zg_core::model::py_round((lo + hi) / 2.0, 2),
        "pctile": pctile, "pctiles": pctiles,
        "lo_date": d_of(bars[lo_idx].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0)),
        "hi_date": d_of(bars[hi_idx].get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0)),
        "n": bars.len(), "seq": seq, "error": Value::Null,
        "asof": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    }))
}

/// `/api/flowlab/sector_sentiment`：行业情绪（600s 缓存，上限 40）。
pub fn sector_sentiment_payload(name: &str) -> Value {
    if name.is_empty() {
        return json!({"error": "缺少 name 参数"});
    }
    let t = now();
    {
        let g = SECT_SENT_CACHE.lock_ok();
        if let Some(m) = g.as_ref() {
            if let Some((ts, data)) = m.get(name) {
                if t - ts < 600.0 {
                    return data.clone();
                }
            }
        }
    }
    match sector_sentiment(name) {
        Ok(out) => {
            let mut g = SECT_SENT_CACHE.lock_ok();
            let m = g.get_or_insert_with(HashMap::new);
            m.insert(name.to_string(), (t, out.clone()));
            if m.len() > 40 {
                let keys: Vec<String> = m.keys().cloned().collect();
                for k in keys.iter().take(m.len() - 40) {
                    m.remove(k);
                }
            }
            out
        }
        Err(e) => json!({"error": e.chars().take(200).collect::<String>(), "name": name}),
    }
}

/// `/api/flowlab/indexes`：参考标的当前涨跌 + 风格 + 本周日度（60s 缓存）。
pub fn indexes_payload() -> Value {
    let t = now();
    {
        let g = IDX_CACHE.lock_ok();
        if let Some((ts, items, asof)) = g.as_ref() {
            if !items.is_empty() && t - ts < 60.0 {
                return json!({"item": items, "asof": asof, "error": Value::Null});
            }
        }
    }
    let codes: Vec<String> = flow::BENCH_INDEXES.iter().map(|(c, _, _)| c.to_string()).collect();
    match ths::ths_get("/api/a-share-index/prices/snapshot",
                       &[("thscodes", codes.join(","))], 12, 2, 1) {
        Ok(d) => {
            let mut snap: HashMap<String, f64> = HashMap::new();
            for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if let Some(p) = ths::number(it.get("price_change_ratio_pct")) {
                    snap.insert(ts, zg_core::model::py_round(p, 2));
                }
            }
            let live = flow::flow_live();
            let bd = live.get("bench_daily").cloned().unwrap_or(json!({}));
            let daily = bd.get("daily").and_then(|x| x.as_object()).cloned().unwrap_or_default();
            let month = bd.get("month").and_then(|x| x.as_object()).cloned().unwrap_or_default();
            let item: Vec<Value> = flow::BENCH_INDEXES.iter()
                .map(|(c, n, s)| json!({
                    "code": c, "name": n, "style": s,
                    "pct": snap.get(*c).map(|v| Value::from(*v)).unwrap_or(Value::Null),
                    "daily": daily.get(*c).cloned().unwrap_or(Value::Null),
                    "month": month.get(*c).cloned().unwrap_or(Value::Null),
                }))
                .collect();
            let asof = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            *IDX_CACHE.lock_ok() = Some((t, item.clone(), asof.clone()));
            json!({"item": item, "asof": asof, "error": Value::Null})
        }
        Err(e) => {
            let g = IDX_CACHE.lock_ok();
            if let Some((_ts, items, asof)) = g.as_ref() {
                if !items.is_empty() {
                    return json!({"item": items, "asof": asof,
                                  "error": e.chars().take(200).collect::<String>()});
                }
            }
            json!({"item": [], "asof": "", "error": e.chars().take(200).collect::<String>()})
        }
    }
}

/// 板块代码格式校验：6 位数字 + 可选 `.TI`/`.XI` 后缀（THS 板块目录口径）。
/// `?code=` 完全由外部控制，不校验就等于把缓存键交给任意字符串。
fn valid_board_code(code: &str) -> bool {
    if code.len() > 12 {
        return false;
    }
    let mut parts = code.split('.');
    let base = parts.next().unwrap_or("");
    let suffix = parts.next();
    if parts.next().is_some() {
        return false;
    }
    base.len() == 6
        && base.chars().all(|c| c.is_ascii_digit())
        && match suffix {
            None => true,
            Some(s) => s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase()),
        }
}

/// `/api/flowlab/constituents`：细分板块(884)成分股 + 实时行情（5min 缓存）。
pub fn constituents_payload(code: &str) -> Value {
    let code = code.trim();
    if code.is_empty() {
        return json!({"error": "缺少 code 参数", "item": []});
    }
    if !valid_board_code(code) {
        return json!({"error": "code 格式非法（应为 881xxx/884xxx，可带 .TI/.XI 后缀）", "item": []});
    }
    let t = now();
    {
        let g = CONS_CACHE.lock_ok();
        if let Some(m) = g.as_ref() {
            if let Some(c) = m.get(code) {
                if t - c.ts < 300.0 && c.data.as_ref().and_then(|d| d.get("error")).map(|e| e.is_null()).unwrap_or(false) {
                    return c.data.clone().unwrap();
                }
            }
        }
    }
    let result = (|| -> Value {
        let d = ths::ths_get("/api/a-share-index/constituents/ths-stock-list",
                             &[("thscode", code.to_string())], 15, 2, 1);
        let d = match d {
            Ok(d) => d,
            Err(e) => return json!({"error": e.chars().take(200).collect::<String>(), "item": []}),
        };
        let cons: Vec<Value> = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if cons.is_empty() {
            return json!({"error": "该板块无成分股数据", "item": []});
        }
        let codes: Vec<String> = cons.iter()
            .filter_map(|c| c.get("thscode").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .collect();
        let mut snap: HashMap<String, Value> = HashMap::new();
        for seg in codes.chunks(200) {
            let seg: Vec<&String> = seg.iter().filter(|c| !c.is_empty()).collect();
            if seg.is_empty() {
                continue;
            }
            let joined = seg.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
            match ths::ths_get("/api/a-share/prices/snapshot", &[("thscodes", joined)], 12, 2, 1) {
                Ok(sd) => {
                    for it in sd.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                        if let Some(ts) = it.get("thscode").and_then(|x| x.as_str()) {
                            snap.insert(ts.to_string(), it);
                        }
                    }
                }
                Err(e) => eprintln!("[flow] cons snapshot seg fail: {e}"),
            }
        }
        let mut item: Vec<Value> = Vec::new();
        for c in &cons {
            let ts = c.get("thscode").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let s = snap.get(&ts).cloned().unwrap_or(json!({}));
            let pct = ths::number(s.get("price_change_ratio_pct"));
            let price = ths::number(s.get("last_price"));
            item.push(json!({
                "ticker": c.get("ticker").and_then(|x| x.as_str()).unwrap_or_else(|| ts.split('.').next().unwrap_or("")),
                "name": c.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                "thscode": ts,
                "pct": pct.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
                "price": price.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
            }));
        }
        item.sort_by(|a, b| {
            let pa = a.get("pct").and_then(|x| x.as_f64());
            let pb = b.get("pct").and_then(|x| x.as_f64());
            let ka = (pa.is_some(), pa.unwrap_or(-999.0));
            let kb = (pb.is_some(), pb.unwrap_or(-999.0));
            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
        });
        let asof = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        json!({"item": item, "asof": asof, "error": Value::Null})
    })();
    {
        let mut g = CONS_CACHE.lock_ok();
        let m = g.get_or_insert_with(HashMap::new);
        m.insert(code.to_string(), Cache { ts: t, data: Some(result.clone()) });
        // 缓存键虽已校验格式，仍要限长：板块目录会随新行业不断增加
        if m.len() > 40 {
            let mut oldest: Vec<(String, f64)> =
                m.iter().map(|(k, v)| (k.clone(), v.ts)).collect();
            oldest.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            for (k, _) in oldest.iter().take(m.len() - 40) {
                m.remove(k);
            }
        }
    }
    result
}

/// `/api/flowlab` 装配（api.py::api_flowlab 逐行）。
pub fn flowlab_payload() -> Value {
    let f = flow::flow_live();
    let sectors_map = f.get("sectors").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let error = f.get("error").and_then(|e| e.as_str()).unwrap_or("");
    if !error.is_empty() || sectors_map.is_empty() {
        return json!({"error": if !error.is_empty() { error.to_string() } else { "暂无板块涨幅数据".into() }});
    }
    let weeks = f.get("weeks").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let n = weeks.len();
    if n == 0 {
        return json!({"error": "板块涨幅数据包无周序列"});
    }
    let names: Vec<&String> = sectors_map.keys().collect();
    let hist = f.get("hist").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let bench = f.get("bench").and_then(|x| x.as_object()).cloned()
        .unwrap_or_else(|| json!({"红利": 0, "全指": 0}).as_object().cloned().unwrap());
    let mut rel = json!({"红利": {}, "全指": {}, "自身": {}});
    let mut mf = Map::new();
    let mut mfr = Map::new();
    for name in &names {
        let h: Vec<Value> = hist.get(*name).and_then(|x| x.as_array()).cloned()
            .unwrap_or_else(|| vec![Value::Null; n]);
        let bd = bench.get("红利").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let ba = bench.get("全指").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let r_div: Vec<Value> = h.iter().map(|x| x.as_f64()
            .map(|v| Value::from(zg_core::model::py_round(v - bd, 2))).unwrap_or(Value::Null)).collect();
        let r_all: Vec<Value> = h.iter().map(|x| x.as_f64()
            .map(|v| Value::from(zg_core::model::py_round(v - ba, 2))).unwrap_or(Value::Null)).collect();
        let r_self: Vec<Value> = h.iter().map(|x| x.as_f64()
            .map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null)).collect();
        if let Some(o) = rel.as_object_mut() {
            o.get_mut("红利").and_then(|x| x.as_object_mut()).map(|m| m.insert((*name).clone(), Value::Array(r_div)));
            o.get_mut("全指").and_then(|x| x.as_object_mut()).map(|m| m.insert((*name).clone(), Value::Array(r_all)));
            o.get_mut("自身").and_then(|x| x.as_object_mut()).map(|m| m.insert((*name).clone(), Value::Array(r_self)));
        }
        mf.insert((*name).clone(), Value::Array(vec![Value::Null; n]));
        mfr.insert((*name).clone(), Value::Array(vec![Value::Null; n]));
    }
    let i = n - 1;
    for (name, s) in &sectors_map {
        if let Some(o) = rel.as_object_mut() {
            for (k, src) in [("红利", "rel_div"), ("全指", "rel_all"), ("自身", "pct")] {
                if let Some(arr) = o.get_mut(k).and_then(|x| x.as_object_mut()).and_then(|m| m.get_mut(name)).and_then(|x| x.as_array_mut()) {
                    if i < arr.len() {
                        arr[i] = s.get(src).cloned().unwrap_or(Value::Null);
                    }
                }
            }
        }
    }
    let (val_raw, bre_raw) = crate::monitor_fund::fund_state();
    let val = if val_raw.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        let m: Map<String, Value> = val_raw.as_object().cloned().unwrap_or_default();
        Value::Object(crate::monitor_fund::flow_val_pct(&m))
    } else {
        json!({})
    };
    let turn5 = f.get("turn5").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let crowd = Value::Object(crate::monitor_fund::flow_crowd(&sectors_map, &turn5));
    let bre: Map<String, Value> = bre_raw.as_object().cloned().unwrap_or_default();
    let crowd_map: Map<String, Value> = crowd.as_object().cloned().unwrap_or_default();
    let panic = Value::Object(crate::monitor_fund::flow_panic(&bre, &crowd_map));
    let day_hist = f.get("day_hist").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let mom = Value::Object(flow::flow_mom(&sectors_map, &hist, &day_hist));
    let subs = f.get("subs").cloned().unwrap_or(json!({}));
    let subs_n: usize = subs.as_object().map(|o| o.values()
        .map(|v| v.as_array().map(|a| a.len()).unwrap_or(0)).sum()).unwrap_or(0);
    json!({
        "weeks": weeks, "n_weeks": n,
        "rel": rel, "mf": mf, "mfr": mfr,
        "day_labels": f.get("day_labels").cloned().unwrap_or(json!([])),
        "day_hist": f.get("day_hist").cloned().unwrap_or(json!({})),
        "mom": mom,
        "bench_daily": f.get("bench_daily").cloned().unwrap_or(json!({})),
        "subs": subs, "sub_n_weeks": f.get("sub_n_weeks").cloned().unwrap_or(json!(1)),
        "val": val, "crowd": crowd, "breadth": bre_raw, "panic": panic,
        "asof": f.get("asof").cloned().unwrap_or(Value::Null),
        "meta": {"n_industries": names.len(), "n_weeks": n, "n_subs": subs_n},
        "error": Value::Null,
    })
}
