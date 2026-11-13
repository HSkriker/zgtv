//! 8787 板块涨幅矩阵 —— 对齐 `modules/monitor_app/services/flow.py`：
//! 同花顺一级行业(881) 为行、近 N 自然周为列，末列取实时快照；细分(884) 只取快照。

use zg_core::sync::MutexExt;
use chrono::{TimeZone, Timelike};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::monitor::read_json as read_json_pub;
use crate::monitor_ths as ths;

/// 顶部参考标的（`config.BENCH_INDEXES`）。
pub const BENCH_INDEXES: &[(&str, &str, &str)] = &[
    ("000300.SH", "沪深300", "大盘蓝筹"),
    ("000905.SH", "中证500", "中盘个股"),
    ("399006.SZ", "创业板指", "成长风格"),
    ("000688.SH", "科创50", "成长风格"),
    ("883418.TI", "微盘股", "小微盘"),
    ("883404.TI", "情绪指数", "市场情绪"),
    ("881155.TI", "银行", "护盘指数"),
    ("881157.TI", "证券", "观察护盘"),
];

pub const WEEKS: usize = 6;
pub const FLOW_COV_MIN: f64 = 0.90;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn l1_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("industry_l1.json")
}
pub fn weekly_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("flow_weekly.json")
}
pub fn rank_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("flow_rankings.json")
}

/// `FLOW_LIVE` 全局（快照 + 历史矩阵）。
pub static FLOW_LIVE: Mutex<Value> = Mutex::new(Value::Null);

pub fn flow_live() -> Value {
    let mut g = FLOW_LIVE.lock_ok();
    if g.is_null() {
        *g = json!({"ts": 0, "asof": "", "weeks": [], "sectors": {}, "hist": {},
                    "mom": {}, "turn5": {}, "bench": {}, "coverage": {},
                    "error": "尚未取得板块涨幅"});
    }
    g.clone()
}

pub fn set_flow_live(v: Value) {
    *FLOW_LIVE.lock_ok() = v;
}

/// `cache.write_json`（8787 数据文件，UTF-8 原子写）。
pub fn write_json(path: &std::path::Path, v: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(path);
    if let Ok(body) = serde_json::to_vec(v) {
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

// ---------------------------------------------------------------- 目录（881/884）

struct DirCache {
    codes: Vec<String>,
    names: std::collections::HashMap<String, String>,
    ts: f64,
}

static L1: Mutex<Option<DirCache>> = Mutex::new(None);
static SUB: Mutex<Option<DirCache>> = Mutex::new(None);

fn load_dir(kind: &str, prefix: &str, cache: &Mutex<Option<DirCache>>,
            disk: Option<std::path::PathBuf>) -> (Vec<String>, std::collections::HashMap<String, String>) {
    {
        let g = cache.lock_ok();
        if let Some(c) = g.as_ref() {
            if !c.codes.is_empty() && now() - c.ts < 6.0 * 3600.0 {
                return (c.codes.clone(), c.names.clone());
            }
        }
    }
    if kind == "l1" {
        let has = { cache.lock_ok().as_ref().map(|c| !c.codes.is_empty()).unwrap_or(false) };
        if !has {
            if let Some(path) = &disk {
                let d = read_json_pub(path).unwrap_or(json!({}));
                let dc: Vec<String> = d.get("codes").and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                if !dc.is_empty() {
                    let dn: std::collections::HashMap<String, String> = d.get("names")
                        .and_then(|x| x.as_object())
                        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
                        .unwrap_or_default();
                    let ts = d.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let mut g = cache.lock_ok();
                    *g = Some(DirCache { codes: dc.clone(), names: dn.clone(), ts });
                    return (dc, dn);
                }
            }
        }
    }
    match ths::ths_get("/api/a-share-index/catalog/ths-index-list",
                       &[("tag", "industry".to_string())], 15, 2, 1) {
        Ok(d) => {
            let mut seen = std::collections::HashSet::new();
            let mut uniq: Vec<(String, String)> = Vec::new();
            for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                let nm = it.get("name").and_then(|x| x.as_str()).unwrap_or("");
                if !ts.starts_with(prefix) || nm.is_empty() {
                    continue;
                }
                if seen.insert(nm.to_string()) {
                    uniq.push((ts.to_string(), nm.to_string()));
                }
            }
            if !uniq.is_empty() {
                let codes: Vec<String> = uniq.iter().map(|(c, _)| c.clone()).collect();
                let names: std::collections::HashMap<String, String> =
                    uniq.iter().map(|(c, n)| (c.clone(), n.clone())).collect();
                let ts = now();
                if kind == "l1" {
                    write_json(&l1_file(), &json!({"ts": ts, "codes": codes, "names": names}));
                }
                let mut g = cache.lock_ok();
                *g = Some(DirCache { codes: codes.clone(), names: names.clone(), ts });
                return (codes, names);
            }
            eprintln!("[flow] catalog 返回空(疑似上游异常)");
        }
        Err(e) => eprintln!("[flow] {kind} catalog fail: {e}"),
    }
    let g = cache.lock_ok();
    match g.as_ref() {
        Some(c) => (c.codes.clone(), c.names.clone()),
        None => (Vec::new(), std::collections::HashMap::new()),
    }
}

/// `_flow_l1_industries`（881xxx.TI → 名称，6h 内存 + 磁盘）。
pub fn l1_industries() -> (Vec<String>, std::collections::HashMap<String, String>) {
    load_dir("l1", "881", &L1, Some(l1_file()))
}

/// `_flow_sub_industries`（884xxx.TI）。
pub fn sub_industries() -> (Vec<String>, std::collections::HashMap<String, String>) {
    load_dir("sub", "884", &SUB, None)
}

/// `_load_industry_tree`（industry_tree.json，1h 缓存）。
pub fn load_industry_tree() -> Value {
    static TREE: Mutex<Option<(f64, Value)>> = Mutex::new(None);
    {
        let g = TREE.lock_ok();
        if let Some((ts, v)) = g.as_ref() {
            if now() - ts < 3600.0 {
                return v.clone();
            }
        }
    }
    let data = read_json_pub(&crate::monitor::data_dir().join("industry_tree.json"))
        .or_else(|| read_json_pub(&crate::monitor::data_dir().join("sector_data").join("industry_tree.json")))
        .unwrap_or(json!({}));
    *TREE.lock_ok() = Some((now(), data.clone()));
    data
}

// ---------------------------------------------------------------- 周/日时间

/// `_week_dates(weeks)`：近 N 自然周的周一标签（YYYYMMDD）。
pub fn week_dates(weeks: usize) -> Vec<String> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    (0..weeks)
        .map(|i| {
            let d = monday - chrono::Duration::days(((weeks - 1 - i) * 7) as i64);
            d.format("%Y%m%d").to_string()
        })
        .collect()
}

/// `_week_days`：本周 5 个交易日 (date_ms, YYYYMMDD)。
pub fn week_days() -> Vec<(i64, String)> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    (0..5)
        .map(|i| {
            let d = monday + chrono::Duration::days(i as i64);
            let ms = d
                .and_hms_opt(0, 0, 0)
                .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);
            (ms, d.format("%Y%m%d").to_string())
        })
        .collect()
}

fn closes_by_ms(bars: &[Value]) -> std::collections::BTreeMap<i64, f64> {
    let mut m = std::collections::BTreeMap::new();
    for b in bars {
        let ms = b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let c = b.get("close_price").and_then(|x| x.as_f64());
        if ms != 0 {
            if let Some(c) = c {
                m.insert(ms, c);
            }
        }
    }
    m
}

/// `_daily_pct_from_bars`：本周 5 个交易日的日度涨跌幅（未到交易日 None）。
pub fn daily_pct_from_bars(bars: &[Value]) -> (Vec<String>, Vec<Value>) {
    let days = week_days();
    let monday_ms = days[0].0;
    let closes = closes_by_ms(bars);
    if closes.is_empty() {
        return (days.iter().map(|d| d.1.clone()).collect(), vec![Value::Null; 5]);
    }
    let mut prev: Option<f64> = None;
    for (ms, c) in &closes {
        if *ms >= monday_ms {
            break;
        }
        prev = Some(*c);
    }
    let mut day_pct: Vec<Value> = Vec::new();
    for (ms, _label) in &days {
        if let Some(c) = closes.get(ms) {
            if let Some(p) = prev {
                if p > 0.0 {
                    day_pct.push(Value::from(zg_core::model::py_round((c / p - 1.0) * 100.0, 2)));
                } else {
                    day_pct.push(Value::Null);
                }
            } else {
                day_pct.push(Value::Null);
            }
            prev = Some(*c);
        } else {
            day_pct.push(Value::Null);
        }
    }
    (days.iter().map(|d| d.1.clone()).collect(), day_pct)
}

/// `_recent_daily_from_bars(bars, n)`。
pub fn recent_daily_from_bars(bars: &[Value], n: usize) -> (Vec<String>, Vec<Value>) {
    let closes = closes_by_ms(bars);
    if closes.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let all: Vec<(&i64, &f64)> = closes.iter().collect();
    let start = all.len().saturating_sub(n + 1);
    let win = &all[start..];
    let mut labels: Vec<String> = Vec::new();
    let mut pcts: Vec<Value> = Vec::new();
    let mut prev: Option<f64> = None;
    for (i, (ms, c)) in win.iter().enumerate() {
        if i == 0 {
            prev = Some(**c);
            continue;
        }
        let v = match prev {
            Some(p) if p > 0.0 => Value::from(zg_core::model::py_round((**c / p - 1.0) * 100.0, 2)),
            _ => Value::Null,
        };
        pcts.push(v);
        let dt = chrono::Local.timestamp_millis_opt(**ms).single();
        labels.push(dt.map(|d| d.format("%Y%m%d").to_string()).unwrap_or_default());
        prev = Some(**c);
    }
    (labels, pcts)
}

fn monday_ms() -> i64 {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    monday
        .and_hms_opt(0, 0, 0)
        .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- 历史周矩阵

/// `_flow_weekly_hist`：逐板块历史日K → 周序列/本周日度/动量/5日均成交额。
pub fn flow_weekly_hist() -> Result<Value, String> {
    let weeks = week_dates(WEEKS);
    let (codes, names) = l1_industries();
    if codes.is_empty() {
        return Err("无法获取同花顺一级行业目录".into());
    }
    use chrono::Datelike;
    let lt = chrono::Local::now();
    let weekday = lt.weekday().num_days_from_monday() as i64;
    let mday = monday_ms();
    let end_ms = mday + (weekday + 1) * 86400 * 1000 + 3600 * 1000;
    let start_ms = mday - (WEEKS as i64 + 1) * 7 * 86400 * 1000;
    let week_boundaries: Vec<i64> = (0..=WEEKS)
        .rev()
        .map(|i| mday - i as i64 * 7 * 86400 * 1000)
        .collect();
    let mut hist: Map<String, Value> = Map::new();
    let mut day_hist: Map<String, Value> = Map::new();
    let mut mom: Map<String, Value> = Map::new();
    let mut turn5: Map<String, Value> = Map::new();
    let day_labels: Vec<String> = week_days().iter().map(|d| d.1.clone()).collect();
    let mut failed: Vec<String> = Vec::new();
    for code in &codes {
        let params = vec![
            ("thscode", code.clone()),
            ("interval", "1d".to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
        ];
        let d = match ths::ths_get("/api/a-share-index/prices/historical", &params, 12, 2, 1) {
            Ok(d) => d,
            Err(_) => {
                failed.push(code.clone());
                continue;
            }
        };
        let mut bars: Vec<Value> = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if bars.len() < 2 {
            continue;
        }
        bars.sort_by_key(|b| b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0));
        let mut week_close: Vec<Option<f64>> = vec![None; WEEKS];
        let mut prev0: Option<f64> = None;
        for b in &bars {
            let ms = b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            let close = b.get("close_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let mut wi: Option<usize> = None;
            for i in 0..WEEKS {
                if week_boundaries[i] <= ms && ms < week_boundaries[i + 1] {
                    wi = Some(i);
                    break;
                }
            }
            match wi {
                None => prev0 = Some(close),
                Some(i) => week_close[i] = Some(close),
            }
        }
        let mut series: Vec<Value> = Vec::new();
        let mut prev = prev0;
        for wc in &week_close {
            match (*wc, prev) {
                (Some(c), Some(p)) if p > 0.0 => {
                    series.push(Value::from(zg_core::model::py_round((c / p - 1.0) * 100.0, 2)));
                    prev = Some(c);
                }
                (wc, _) => {
                    series.push(Value::Null);
                    if wc.is_some() {
                        prev = wc;
                    }
                }
            }
        }
        let name = names.get(code).cloned().unwrap_or_else(|| code.clone());
        hist.insert(name.clone(), Value::Array(series));
        let (_l, dayp) = daily_pct_from_bars(&bars);
        day_hist.insert(name.clone(), Value::Array(dayp));
        let closes: Vec<f64> = bars
            .iter()
            .filter_map(|b| b.get("close_price").and_then(|x| x.as_f64()))
            .collect();
        if closes.len() >= 2 && *closes.last().unwrap() > 0.0 {
            let mut m = Map::new();
            let last = *closes.last().unwrap();
            for (n, key) in [(1usize, "d1"), (5, "d5"), (10, "d10"), (20, "d20")] {
                let idx = closes.len() as i64 - 1 - n as i64;
                let v = if idx >= 0 && closes[idx as usize] > 0.0 {
                    Value::from(zg_core::model::py_round((last / closes[idx as usize] - 1.0) * 100.0, 2))
                } else {
                    Value::Null
                };
                m.insert(key.into(), v);
            }
            mom.insert(name.clone(), Value::Object(m));
        }
        let turns: Vec<f64> = bars
            .iter()
            .filter_map(|b| b.get("turnover").and_then(|x| x.as_f64()))
            .filter(|t| *t != 0.0)
            .collect();
        if !turns.is_empty() {
            let tail = &turns[turns.len().saturating_sub(5)..];
            let avg = tail.iter().sum::<f64>() / tail.len() as f64;
            turn5.insert(name, Value::from(zg_core::model::py_round(avg / 1e8, 2)));
        }
    }
    write_json(&weekly_file(), &json!({
        "ts": now(), "weeks": weeks, "hist": hist,
        "day_labels": day_labels, "day_hist": day_hist,
        "mom": mom, "turn5": turn5,
    }));
    Ok(json!({
        "weeks": weeks, "hist": hist, "day_labels": day_labels,
        "day_hist": day_hist, "failed": failed, "mom": mom, "turn5": turn5,
    }))
}

// ---------------------------------------------------------------- 基准

/// `_bench_daily`：顶部参考标的日度涨跌幅（4 指数 + 银行/证券）。
pub fn bench_daily() -> Value {
    let codes: Vec<String> = BENCH_INDEXES.iter().map(|(c, _, _)| c.to_string()).collect();
    use chrono::Datelike;
    let lt = chrono::Local::now();
    let weekday = lt.weekday().num_days_from_monday() as i64;
    let mday = monday_ms();
    let end_ms = mday + (weekday + 1) * 86400 * 1000 + 3600 * 1000;
    let start_ms = mday - 37 * 86400 * 1000;
    let mut daily = Map::new();
    let mut month = Map::new();
    for code in &codes {
        let params = vec![
            ("thscode", code.clone()),
            ("interval", "1d".to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
        ];
        let d = match ths::ths_get("/api/a-share-index/prices/historical", &params, 12, 2, 1) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[bench] daily fail {code}: {e}");
                continue;
            }
        };
        let mut bars: Vec<Value> = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if bars.len() < 2 {
            continue;
        }
        bars.sort_by_key(|b| b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0));
        let (_l, dayp) = daily_pct_from_bars(&bars);
        daily.insert(code.clone(), Value::Array(dayp));
        let (_l2, m_pct) = recent_daily_from_bars(&bars, 20);
        month.insert(code.clone(), Value::Array(m_pct));
    }
    json!({"daily": daily, "month": month})
}

/// `_fetch_benchmark_pcts`：红利=000015.SH / 全指=000300.SH。
pub fn fetch_benchmark_pcts() -> Map<String, Value> {
    let mut out = Map::new();
    match ths::ths_get("/api/a-share-index/prices/snapshot",
                       &[("thscodes", "000015.SH,000300.SH".to_string())], 12, 2, 1) {
        Ok(d) => {
            for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                let pct = match ths::number(it.get("price_change_ratio_pct")) {
                    Some(p) => p,
                    None => continue,
                };
                let v = Value::from(zg_core::model::py_round(pct, 2));
                if ts == "000015.SH" {
                    out.insert("红利".into(), v);
                } else if ts == "000300.SH" {
                    out.insert("全指".into(), v);
                }
            }
        }
        Err(e) => eprintln!("[bench] ths fail: {e}"),
    }
    out
}

/// `_flow_sub_snapshot`：884 细分最新快照（60/批）。
pub fn flow_sub_snapshot() -> Map<String, Value> {
    let (codes, names) = sub_industries();
    let mut snap = Map::new();
    if codes.is_empty() {
        return snap;
    }
    let bench = fetch_benchmark_pcts();
    let ba = bench.get("全指").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let bd = bench.get("红利").and_then(|v| v.as_f64()).unwrap_or(0.0);
    for seg in codes.chunks(60) {
        let params = vec![("thscodes", seg.join(","))];
        match ths::ths_get("/api/a-share-index/prices/snapshot", &params, 12, 2, 1) {
            Ok(d) => {
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let c = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let pct = ths::number(it.get("price_change_ratio_pct"));
                    if c.is_empty() || pct.is_none() {
                        continue;
                    }
                    let pct = pct.unwrap();
                    let name = names.get(c).cloned().unwrap_or_else(|| c.to_string());
                    snap.insert(name, json!({
                        "pct": zg_core::model::py_round(pct, 2),
                        "rel_div": zg_core::model::py_round(pct - bd, 2),
                        "rel_all": zg_core::model::py_round(pct - ba, 2),
                    }));
                }
            }
            Err(e) => eprintln!("[flow] sub snapshot seg fail: {e}"),
        }
    }
    snap
}

// ---------------------------------------------------------------- 快照层

/// `_flow_snapshot_part`：末列快照 + 基准 + 细分快照 + 参考标的日度。
pub type SectorsMap = Map<String, Value>;
pub type SnapshotParts = (SectorsMap, SectorsMap, SectorsMap, Value);

pub fn flow_snapshot_part() -> Result<SnapshotParts, String> {
    let (codes, names) = l1_industries();
    if codes.is_empty() {
        return Err("无法获取同花顺一级行业目录".into());
    }
    let mut snap: Map<String, Value> = Map::new();
    let mut snap_sum = 0.0;
    let mut snap_n = 0usize;
    for seg in codes.chunks(60) {
        let params = vec![("thscodes", seg.join(","))];
        match ths::ths_get("/api/a-share-index/prices/snapshot", &params, 12, 2, 1) {
            Ok(d) => {
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let c = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let pct = ths::number(it.get("price_change_ratio_pct"));
                    if c.is_empty() || pct.is_none() {
                        continue;
                    }
                    let pct = pct.unwrap();
                    let turn = ths::number(it.get("turnover"));
                    let name = names.get(c).cloned().unwrap_or_else(|| c.to_string());
                    snap.insert(name, json!({
                        "pct": zg_core::model::py_round(pct, 2),
                        "turn": turn.map(|t| Value::from(zg_core::model::py_round(t / 1e8, 2))).unwrap_or(Value::Null),
                    }));
                    snap_sum += pct;
                    snap_n += 1;
                }
            }
            Err(e) => eprintln!("[flow] snapshot seg fail: {e}"),
        }
    }
    if snap.len() < 10 {
        return Err(format!("板块涨幅快照覆盖不足: {} 个一级行业", snap.len()));
    }
    let mut bench = fetch_benchmark_pcts();
    if !bench.contains_key("全指") && snap_n > 0 {
        bench.insert("全指".into(), Value::from(zg_core::model::py_round(snap_sum / snap_n as f64, 2)));
    }
    if !bench.contains_key("红利") {
        bench.insert("红利".into(), Value::from(0.0));
    }
    let bd = bench.get("红利").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let ba = bench.get("全指").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let mut sectors: Map<String, Value> = Map::new();
    for (name, sv) in &snap {
        let pct = sv.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sectors.insert(name.clone(), json!({
            "pct": sv.get("pct").cloned().unwrap_or(Value::Null),
            "rel_div": zg_core::model::py_round(pct - bd, 2),
            "rel_all": zg_core::model::py_round(pct - ba, 2),
            "turn": sv.get("turn").cloned().unwrap_or(Value::Null),
            "main_yi": Value::Null,
            "flow_rank": Value::Null,
        }));
    }
    // 细分（884）按 industry_tree 归属到一级
    let mut subs: Map<String, Value> = Map::new();
    let sub_snap = flow_sub_snapshot();
    if !sub_snap.is_empty() {
        let tree = load_industry_tree();
        let mut sub_to_l1: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if let Some(obj) = tree.as_object() {
            for (l1_code, node) in obj {
                if let Some(subs_arr) = node.get("subs").and_then(|x| x.as_array()) {
                    for s in subs_arr {
                        if let Some(code) = s.get("code").and_then(|x| x.as_str()) {
                            sub_to_l1.insert(code.to_string(), l1_code.clone());
                        }
                    }
                }
            }
        }
        let l1_name: std::collections::HashMap<String, String> =
            codes.iter().map(|c| (c.clone(), names.get(c).cloned().unwrap_or_else(|| c.clone()))).collect();
        let (_sub_codes, sub_names) = sub_industries();
        let sub_code_by_name: std::collections::HashMap<String, String> =
            sub_names.iter().map(|(c, n)| (n.clone(), c.clone())).collect();
        let mut grouped: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
        for (sub_name, ss) in &sub_snap {
            let sc = match sub_code_by_name.get(sub_name) {
                Some(c) => c.clone(),
                None => continue,
            };
            let l1c = match sub_to_l1.get(&sc) {
                Some(l) => l.clone(),
                None => continue,
            };
            let parent = match l1_name.get(&l1c) {
                Some(p) => p.clone(),
                None => continue,
            };
            grouped.entry(parent).or_default().push(json!({
                "name": sub_name,
                "code": sc,
                "pct": ss.get("pct").cloned().unwrap_or(Value::Null),
                "rel_div": ss.get("rel_div").cloned().unwrap_or(Value::Null),
                "rel_all": ss.get("rel_all").cloned().unwrap_or(Value::Null),
            }));
        }
        for (p, mut arr) in grouped {
            arr.sort_by(|a, b| {
                let pa = a.get("pct").and_then(|x| x.as_f64()).unwrap_or(-1e9);
                let pb = b.get("pct").and_then(|x| x.as_f64()).unwrap_or(-1e9);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            subs.insert(p, Value::Array(arr));
        }
    }
    let bd_daily = bench_daily();
    Ok((sectors, subs, bench, bd_daily))
}

/// `_fetch_flow_live(full)`：一级行业板块涨幅矩阵。
pub fn fetch_flow_live(full: bool) -> Result<Value, String> {
    let (weeks, hist, day_labels, day_hist, mom, turn5) = if full {
        let r = flow_weekly_hist()?;
        (
            r.get("weeks").and_then(|x| x.as_array()).cloned().unwrap_or_default(),
            r.get("hist").and_then(|x| x.as_object()).cloned().unwrap_or_default(),
            r.get("day_labels").and_then(|x| x.as_array()).cloned().unwrap_or_default(),
            r.get("day_hist").and_then(|x| x.as_object()).cloned().unwrap_or_default(),
            r.get("mom").and_then(|x| x.as_object()).cloned().unwrap_or_default(),
            r.get("turn5").and_then(|x| x.as_object()).cloned().unwrap_or_default(),
        )
    } else {
        let live = flow_live();
        let weeks = live.get("weeks").and_then(|x| x.as_array()).cloned()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| week_dates(WEEKS).into_iter().map(Value::String).collect());
        // (weeks, hist, day_labels, day_hist, mom, turn5)
        (weeks, Map::new(), Vec::new(), Map::new(), Map::new(), Map::new())
    };
    let (sectors, subs, bench, bd_daily) = flow_snapshot_part()?;
    let live_prev = flow_live();
    let mut full_hist: Map<String, Value> = Map::new();
    for name in sectors.keys() {
        if full {
            full_hist.insert(name.clone(), hist.get(name).cloned()
                .unwrap_or_else(|| Value::Array(vec![Value::Null; WEEKS])));
        } else {
            full_hist.insert(name.clone(), live_prev.get("hist")
                .and_then(|h| h.get(name)).cloned()
                .unwrap_or_else(|| Value::Array(vec![Value::Null; WEEKS])));
        }
    }
    let t = now();
    let asof = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let pick = |key: &str| -> Value {
        if full {
            match key {
                "mom" => Value::Object(mom.clone()),
                "turn5" => Value::Object(turn5.clone()),
                "day_labels" => Value::Array(day_labels.clone()),
                "day_hist" => Value::Object(day_hist.clone()),
                _ => Value::Null,
            }
        } else {
            live_prev.get(key).cloned().unwrap_or_else(|| match key {
                "day_labels" => json!([]),
                _ => json!({}),
            })
        }
    };
    let bench_rounded: Map<String, Value> = bench.iter()
        .map(|(k, v)| {
            let f = v.as_f64().unwrap_or(0.0);
            (k.clone(), Value::from(zg_core::model::py_round(f, 2)))
        })
        .collect();
    let subs_n: usize = subs.values().map(|v| v.as_array().map(|a| a.len()).unwrap_or(0)).sum();
    Ok(json!({
        "ts": t, "asof": asof,
        "weeks": weeks, "sectors": sectors, "hist": full_hist,
        "mom": pick("mom"), "turn5": pick("turn5"),
        "day_labels": pick("day_labels"), "day_hist": pick("day_hist"),
        "bench_daily": bd_daily,
        "bench": bench_rounded,
        "coverage": {"industries": sectors.len(), "weeks": WEEKS, "subs": subs_n},
        "subs": subs, "sub_n_weeks": 1,
        "error": Value::Null,
    }))
}

// ---------------------------------------------------------------- 每日强弱榜

static RANK_LOCK: Mutex<()> = Mutex::new(());

pub fn is_trading_day() -> bool {
    use chrono::Datelike;
    static TD_CACHE: Mutex<Option<(String, bool)>> = Mutex::new(None);
    let lt = chrono::Local::now();
    let today = lt.format("%Y%m%d").to_string();
    if lt.weekday().num_days_from_monday() >= 5 {
        return false;
    }
    {
        let g = TD_CACHE.lock_ok();
        if let Some((d, v)) = g.as_ref() {
            if *d == today {
                return *v;
            }
        }
    }
    let t = now();
    let mut is_td = false;
    let params = vec![
        ("thscode", "000300.SH".to_string()),
        ("interval", "1d".to_string()),
        ("start", (((t - 20.0 * 86400.0) * 1000.0) as i64).to_string()),
        ("end", (((t + 86400.0) * 1000.0) as i64).to_string()),
    ];
    match ths::ths_get("/api/a-share-index/prices/historical", &params, 12, 1, 1) {
        Ok(d) => {
            let mut bars: Vec<Value> = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            bars.sort_by_key(|b| b.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0));
            if let Some(last) = bars.last() {
                let ms = last.get("date_ms").and_then(|x| x.as_i64()).unwrap_or(0);
                let label = chrono::Local.timestamp_millis_opt(ms).single()
                    .map(|d| d.format("%Y%m%d").to_string()).unwrap_or_default();
                is_td = label == today;
            }
        }
        Err(e) => {
            eprintln!("[rank] trading-day check fail: {e}");
            is_td = true;
        }
    }
    *TD_CACHE.lock_ok() = Some((today, is_td));
    is_td
}

/// `_record_daily_ranking`：每日板块强弱快照（相对沪深300超额降序）。
pub fn record_daily_ranking(sectors: &Map<String, Value>) {
    use chrono::Datelike;
    let lt = chrono::Local::now();
    let hm = lt.hour() as i64 * 60 + lt.minute() as i64;
    if !(9 * 60 + 20..=16 * 60).contains(&hm) {
        return;
    }
    if !is_trading_day() {
        return;
    }
    let today = lt.format("%Y%m%d").to_string();
    let monday = lt.date_naive() - chrono::Duration::days(lt.weekday().num_days_from_monday() as i64);
    let week = monday.format("%Y%m%d").to_string();
    let mut rows: Vec<(String, f64)> = sectors.iter()
        .filter(|(_, s)| s.get("pct").map(|x| !x.is_null()).unwrap_or(false))
        .map(|(n, s)| {
            let rel = s.get("rel_all").and_then(|x| x.as_f64()).unwrap_or(0.0);
            (n.clone(), zg_core::model::py_round(rel, 2))
        })
        .collect();
    if rows.len() < 10 {
        return;
    }
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let _g = RANK_LOCK.lock_ok();
    let mut d = read_json_pub(&rank_file()).unwrap_or(json!({"week": "", "days": {}, "locked": ""}));
    if d.get("week").and_then(|x| x.as_str()) != Some(week.as_str()) {
        d = json!({"week": week, "days": {}, "locked": ""});
    }
    let rows_v: Vec<Value> = rows.iter().map(|(n, p)| json!([n, p])).collect();
    if let Some(o) = d.as_object_mut() {
        if let Some(days) = o.get_mut("days").and_then(|x| x.as_object_mut()) {
            days.insert(today.clone(), Value::Array(rows_v));
        }
        if lt.weekday().num_days_from_monday() == 4 {
            o.insert("locked".into(), Value::String(today));
        }
    }
    write_json(&rank_file(), &d);
}

// ---------------------------------------------------------------- 动量

/// `_flow_mom`：板块动量 {name: {today,d5,d10,d20}}（周环比连乘近似）。
pub fn flow_mom(sectors: &Map<String, Value>, hist: &Map<String, Value>, day_hist: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, s) in sectors {
        let r1 = match s.get("pct").and_then(|x| x.as_f64()) {
            Some(v) => v,
            None => continue,
        };
        let dh: Vec<Value> = day_hist.get(name).and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let mut p5 = 1.0f64;
        let mut got = false;
        for v in &dh {
            if let Some(x) = v.as_f64() {
                p5 *= 1.0 + x / 100.0;
                got = true;
            }
        }
        let mut r5 = if got { Some((p5 - 1.0) * 100.0) } else { None };
        let h: Vec<Value> = hist.get(name).and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if r5.is_none() {
            if let Some(last) = h.last().and_then(|v| v.as_f64()) {
                r5 = Some(last);
            }
        }
        let w = |i: i64| -> Option<f64> {
            let len = h.len() as i64;
            if i >= 0 || -len > i {
                return None;
            }
            h[(len + i) as usize].as_f64()
        };
        let mut r10: Option<f64> = None;
        let mut r20: Option<f64> = None;
        if let Some(r5v) = r5 {
            let p = 1.0 + r5v / 100.0;
            if let Some(w2) = w(-2) {
                r10 = Some((p * (1.0 + w2 / 100.0) - 1.0) * 100.0);
                if let (Some(w3), Some(w4)) = (w(-3), w(-4)) {
                    r20 = Some((p * (1.0 + w2 / 100.0) * (1.0 + w3 / 100.0) * (1.0 + w4 / 100.0) - 1.0) * 100.0);
                }
            }
        }
        out.insert(name.clone(), json!({
            "today": zg_core::model::py_round(r1, 2),
            "d5": r5.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
            "d10": r10.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
            "d20": r20.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
        }));
    }
    out
}

// ---------------------------------------------------------------- 双频刷新循环

/// `_flow_live_loop`：盘中 2 分钟快照 / 30 分钟全量；盘后 30 分钟全量。
pub fn flow_live_loop() {
    let mut last_hist = 0.0f64;
    if let Some(d) = read_json_pub(&weekly_file()) {
        let has_hist = d.get("hist").and_then(|x| x.as_object()).map(|o| !o.is_empty()).unwrap_or(false);
        let ts = d.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
        if has_hist && now() - ts < 1800.0 {
            last_hist = ts;
            let mut live = flow_live();
            if let Some(o) = live.as_object_mut() {
                for k in ["weeks", "hist", "day_labels", "day_hist", "mom", "turn5"] {
                    if let Some(v) = d.get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
            }
            set_flow_live(live);
            eprintln!("[flow] 冷启动复用磁盘历史矩阵(ts={})",
                      chrono::Local.timestamp_opt(last_hist as i64, 0).single()
                          .map(|d| d.format("%H:%M:%S").to_string()).unwrap_or_default());
        }
    }
    let mut fails = 0u32;
    loop {
        let r = crate::monitor_live::guard_round("flow", || -> u64 {
            // 监控流水线整体依赖同花顺指数快照；未配置 key 时不做无意义轮询，长睡并只提示一次
            if !zg_market::ths::enabled() {
                eprintln!("[flow] 未配置同花顺 API key，行情监控降级停更（配置后自动恢复）");
                return 1800;
            }
            let t = now();
            let trading = crate::monitor_live::in_trading();
            let need_hist = (t - last_hist) >= 1800.0;
            let mut ok = false;
            let sectors_empty = flow_live().get("sectors").and_then(|x| x.as_object()).map(|o| o.is_empty()).unwrap_or(true);
            if need_hist && sectors_empty {
                match fetch_flow_live(false) {
                    Ok(pre) => {
                        set_flow_live(pre.clone());
                        if let Some(sec) = pre.get("sectors").and_then(|x| x.as_object()) {
                            record_daily_ranking(sec);
                        }
                        eprintln!("[flow] 冷启动先发布快照层(历史矩阵后台补)");
                    }
                    Err(e) => eprintln!("[flow] 快照层首发布失败: {}", e.chars().take(120).collect::<String>()),
                }
            }
            match fetch_flow_live(need_hist) {
                Ok(fresh) => {
                    if need_hist {
                        last_hist = t;
                    }
                    set_flow_live(fresh.clone());
                    if let Some(sec) = fresh.get("sectors").and_then(|x| x.as_object()) {
                        record_daily_ranking(sec);
                    }
                    ok = true;
                }
                Err(e) => {
                    let mut live = flow_live();
                    if let Some(o) = live.as_object_mut() {
                        o.insert("error".into(), Value::String(e.chars().take(300).collect()));
                    }
                    set_flow_live(live);
                }
            }
            if ok { if trading { 120 } else { 1800 } } else { 60 }
        });
        let secs = match r {
            Ok(s) => {
                fails = 0;
                s
            }
            Err(_) => {
                fails += 1;
                let mut live = flow_live();
                if !live.is_object() {
                    live = json!({});
                }
                if let Some(o) = live.as_object_mut() {
                    o.insert("error".into(), json!(format!("采集循环异常（连续 {fails} 轮）")));
                }
                set_flow_live(live);
                60
            }
        };
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}
