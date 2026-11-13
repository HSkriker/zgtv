//! 8787 板块轮动三维度 —— 对齐 `modules/monitor_app/services/fund.py`：
//! 成分股清单 / 估值分位 / 广度 / 拥挤 / 恐慌 + 后台刷新循环。

use zg_core::sync::MutexExt;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::monitor::read_json as read_json_pub;
use crate::monitor_flow as flow;
use crate::monitor_ths as ths;

const MEMBERS_RETRY_GAP: f64 = 120.0;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn members_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("industry_members.json")
}
fn val_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("industry_val.json")
}
fn val_hist_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("industry_val_hist.json")
}
fn bre_file() -> std::path::PathBuf {
    crate::monitor::data_dir().join("industry_breadth.json")
}

struct FundCache {
    members: (f64, Value),
    val: (f64, Value),
    bre: (f64, Value),
}

static FUND: Mutex<Option<FundCache>> = Mutex::new(None);
static FUND_LOCK: Mutex<()> = Mutex::new(());
static MEMBERS_RETRY: Mutex<f64> = Mutex::new(0.0);
static FLOW_COV: Mutex<(f64, f64)> = Mutex::new((1.0, 1.0));

fn with_fund<R>(f: impl FnOnce(&mut FundCache) -> R) -> R {
    let mut g = FUND.lock_ok();
    if g.is_none() {
        *g = Some(FundCache {
            members: (0.0, json!({})),
            val: (0.0, json!({})),
            bre: (0.0, json!({})),
        });
    }
    f(g.as_mut().unwrap())
}

fn codes_of(d: &Value) -> HashSet<String> {
    d.as_object()
        .map(|o| {
            o.values()
                .filter_map(|v| v.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// `_flow_members`：一级行业成分股清单（22h；残缺落盘 ts=0 下轮补缺）。
pub fn flow_members() -> Value {
    let (codes, names) = flow::l1_industries();
    let want: HashSet<String> = codes.iter().cloned().collect();
    if want.is_empty() {
        return with_fund(|c| c.members.1.clone());
    }
    {
        let (ts, data) = with_fund(|c| c.members.clone());
        if !data.as_object().map(|o| o.is_empty()).unwrap_or(true)
            && codes_of(&data).is_superset(&want)
            && now() - ts < 22.0 * 3600.0
        {
            return data;
        }
    }
    let disk = read_json_pub(&members_file()).unwrap_or(json!({}));
    let disk_data = disk.get("data").cloned().filter(|d| d.is_object());
    if let Some(dd) = &disk_data {
        let ts = disk.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
        if codes_of(dd).is_superset(&want) && now() - ts < 22.0 * 3600.0 {
            let dd2 = dd.clone();
            with_fund(|c| c.members = (ts, dd2.clone()));
            return dd2;
        }
    }
    let mem_data = with_fund(|c| c.members.1.clone());
    let base: Option<Value> = [Some(mem_data), disk_data]
        .into_iter()
        .flatten()
        .filter(|v| v.as_object().map(|o| !o.is_empty()).unwrap_or(false))
        .max_by_key(|v| codes_of(v).len());
    let have = base.as_ref().map(codes_of).unwrap_or_default();
    let stale_complete = have.is_superset(&want);
    let todo: Vec<String> = if stale_complete {
        codes.clone()
    } else {
        codes.iter().filter(|c| !have.contains(*c)).cloned().collect()
    };
    if todo.is_empty() {
        let b = base.unwrap_or(json!({}));
        with_fund(|c| c.members = (now(), b.clone()));
        return b;
    }
    {
        let mut last = MEMBERS_RETRY.lock_ok();
        if now() - *last < MEMBERS_RETRY_GAP {
            return base.unwrap_or(json!({}));
        }
        *last = now();
    }
    let mut out: Map<String, Value> =
        base.clone().and_then(|b| b.as_object().cloned()).unwrap_or_default();
    let mut new_ok = 0;
    for c in &todo {
        let params = vec![("thscode", c.clone())];
        match ths::ths_get("/api/a-share-index/constituents/ths-stock-list", &params, 12, 2, 1) {
            Ok(d) => {
                let members: Vec<Value> = d
                    .get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default()
                    .iter()
                    .filter_map(|it| it.get("thscode").cloned())
                    .collect();
                if !members.is_empty() {
                    let name = names.get(c).cloned().unwrap_or_else(|| c.clone());
                    out.insert(name, json!({"code": c, "members": members}));
                    new_ok += 1;
                }
            }
            Err(e) => eprintln!("[fund] members fail {c}: {e}"),
        }
    }
    let out_v = Value::Object(out);
    let got = codes_of(&out_v);
    let complete = got.is_superset(&want);
    if out_v.as_object().map(|o| !o.is_empty()).unwrap_or(false) && (new_ok > 0 || base.is_none()) {
        flow::write_json(&members_file(), &json!({
            "ts": if complete { now() } else { 0.0 },
            "data": out_v,
        }));
    }
    with_fund(|c| c.members = (if complete { now() } else { 0.0 }, out_v.clone()));
    if complete {
        eprintln!("[fund] members 完整 {}/{}", got.len(), want.len());
    } else {
        eprintln!("[fund] members 残缺 {}/{}，下轮补 {} 个", got.len(), want.len(), want.len() - got.len());
    }
    out_v
}

fn stock2ind(members: &Value) -> HashMap<String, Vec<String>> {
    let mut s2i: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(obj) = members.as_object() {
        for (name, info) in obj {
            if let Some(arr) = info.get("members").and_then(|x| x.as_array()) {
                for t in arr {
                    if let Some(ts) = t.as_str() {
                        s2i.entry(ts.to_string()).or_default().push(name.clone());
                    }
                }
            }
        }
    }
    s2i
}

/// `_flow_valuations`：PE_TTM/PB 批量快照 → 行业正值中位数（100/批）。
pub fn flow_valuations() -> Map<String, Value> {
    let members = flow_members();
    let s2i = stock2ind(&members);
    let mut out = Map::new();
    if s2i.is_empty() {
        return out;
    }
    let mut codes_all: Vec<String> = s2i.keys().cloned().collect();
    codes_all.sort();
    let mut pe_by: HashMap<String, Vec<f64>> = HashMap::new();
    let mut pb_by: HashMap<String, Vec<f64>> = HashMap::new();
    let mut cnt: HashMap<String, i64> = HashMap::new();
    let mut n_seg = 0;
    let mut n_bad = 0;
    let mut last_err = String::new();
    for seg in codes_all.chunks(100) {
        n_seg += 1;
        let params = vec![("thscodes", seg.join(","))];
        match ths::ths_get("/api/a-share/valuations/snapshot", &params, 25, 2, 1) {
            Ok(d) => {
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let pe = ths::number(it.get("pe_ttm"));
                    let pb = ths::number(it.get("pb_mrq"));
                    for ind in s2i.get(ts).cloned().unwrap_or_default() {
                        *cnt.entry(ind.clone()).or_insert(0) += 1;
                        if let Some(pe) = pe {
                            if pe > 0.0 {
                                pe_by.entry(ind.clone()).or_default().push(pe);
                            }
                        }
                        if let Some(pb) = pb {
                            if pb > 0.0 {
                                pb_by.entry(ind).or_default().push(pb);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                n_bad += 1;
                last_err = e;
            }
        }
    }
    if n_bad > 0 {
        // 整轮汇总一行：单批失败不再逐条打印（无 key 时曾每 5 分钟刷 ~50 行）
        eprintln!("[fund] val seg fail {n_bad}/{n_seg} 批（{last_err}）");
    }
    let cov = if n_seg == 0 { 1.0 } else { (n_seg - n_bad) as f64 / n_seg as f64 };
    FLOW_COV.lock_ok().1 = cov;
    let med = |arr: &mut Vec<f64>| -> Option<f64> {
        arr.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if arr.is_empty() { None } else { Some(arr[arr.len() / 2]) }
    };
    if let Some(obj) = members.as_object() {
        for name in obj.keys() {
            let pes = med(pe_by.get_mut(name).unwrap_or(&mut Vec::new()));
            let pbs = med(pb_by.get_mut(name).unwrap_or(&mut Vec::new()));
            out.insert(name.clone(), json!({
                "pe": pes.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
                "pb": pbs.map(|v| Value::from(zg_core::model::py_round(v, 2))).unwrap_or(Value::Null),
                "n": cnt.get(name).copied().unwrap_or(0),
            }));
        }
    }
    out
}

/// `_flow_val_pct`：估值分位（续日样本 <20 天 → None）。
pub fn flow_val_pct(vals: &Map<String, Value>) -> Map<String, Value> {
    let hist = read_json_pub(&val_hist_file()).unwrap_or(json!({"days": {}}));
    let days = hist.get("days").and_then(|d| d.as_object()).cloned().unwrap_or_default();
    let mut out = Map::new();
    for (name, v) in vals {
        let mut pe_h: Vec<f64> = Vec::new();
        let mut pb_h: Vec<f64> = Vec::new();
        for dd in days.values() {
            if let Some(entry) = dd.get(name) {
                if let Some(pe) = entry.get("pe").and_then(|x| x.as_f64()) {
                    pe_h.push(pe);
                }
                if let Some(pb) = entry.get("pb").and_then(|x| x.as_f64()) {
                    pb_h.push(pb);
                }
            }
        }
        let pct = |cur: Option<f64>, arr: &Vec<f64>| -> Value {
            match cur {
                Some(c) if arr.len() >= 20 => {
                    let below = arr.iter().filter(|x| **x < c).count();
                    Value::from(zg_core::model::py_round(below as f64 / arr.len() as f64 * 100.0, 1))
                }
                _ => Value::Null,
            }
        };
        let mut entry = v.as_object().cloned().unwrap_or_default();
        entry.insert("pe_pct".into(), pct(v.get("pe").and_then(|x| x.as_f64()), &pe_h));
        entry.insert("pb_pct".into(), pct(v.get("pb").and_then(|x| x.as_f64()), &pb_h));
        entry.insert("n_days".into(), Value::from(pe_h.len()));
        out.insert(name.clone(), Value::Object(entry));
    }
    out
}

/// `_record_val_hist`：交易日盘后 15:10–16:00 落库（保留近 250 个样本日）。
pub fn record_val_hist(vals: &Map<String, Value>) {
    use chrono::Timelike;
    let lt = chrono::Local::now();
    let hm = lt.hour() as i64 * 60 + lt.minute() as i64;
    if !(15 * 60 + 10..=16 * 60).contains(&hm) {
        return;
    }
    if !flow::is_trading_day() {
        return;
    }
    let today = lt.format("%Y-%m-%d").to_string();
    let _g = FUND_LOCK.lock_ok();
    let mut d = read_json_pub(&val_hist_file()).unwrap_or(json!({"days": {}}));
    let mut days = d.get("days").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    if days.contains_key(&today) {
        return;
    }
    let mut row = Map::new();
    for (n, v) in vals {
        if v.get("pe").map(|x| !x.is_null()).unwrap_or(false)
            || v.get("pb").map(|x| !x.is_null()).unwrap_or(false)
        {
            row.insert(n.clone(), json!({
                "pe": v.get("pe").cloned().unwrap_or(Value::Null),
                "pb": v.get("pb").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    days.insert(today, Value::Object(row));
    let mut keys: Vec<String> = days.keys().cloned().collect();
    keys.sort();
    let keep: Vec<String> = keys.iter().rev().take(250).cloned().collect();
    let mut new_days = Map::new();
    for k in keys.iter().filter(|k| keep.contains(k)) {
        if let Some(v) = days.get(k) {
            new_days.insert(k.clone(), v.clone());
        }
    }
    if let Some(o) = d.as_object_mut() {
        o.insert("days".into(), Value::Object(new_days));
        o.insert("updated".into(), Value::String(lt.format("%Y-%m-%d %H:%M:%S").to_string()));
    }
    flow::write_json(&val_hist_file(), &d);
}

/// `_flow_breadth`：成分股快照聚合（600/批）。
pub fn flow_breadth() -> Map<String, Value> {
    let members = flow_members();
    let s2i = stock2ind(&members);
    let mut out = Map::new();
    if s2i.is_empty() {
        return out;
    }
    let mut codes_all: Vec<String> = s2i.keys().cloned().collect();
    codes_all.sort();
    struct Stat { n: i64, up: i64, big_up: i64, big_dn: i64 }
    let mut stats: HashMap<String, Stat> = HashMap::new();
    let mut n_seg = 0;
    let mut n_bad = 0;
    let mut last_err = String::new();
    for seg in codes_all.chunks(600) {
        n_seg += 1;
        let params = vec![("thscodes", seg.join(","))];
        match ths::ths_get("/api/a-share/prices/snapshot", &params, 40, 2, 1) {
            Ok(d) => {
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let pct = match ths::number(it.get("price_change_ratio_pct")) {
                        Some(p) => p,
                        None => continue,
                    };
                    let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    for ind in s2i.get(ts).cloned().unwrap_or_default() {
                        let s = stats.entry(ind).or_insert(Stat { n: 0, up: 0, big_up: 0, big_dn: 0 });
                        s.n += 1;
                        if pct > 0.0 {
                            s.up += 1;
                        }
                        if pct >= 5.0 {
                            s.big_up += 1;
                        }
                        if pct <= -5.0 {
                            s.big_dn += 1;
                        }
                    }
                }
            }
            Err(e) => {
                n_bad += 1;
                last_err = e;
            }
        }
    }
    if n_bad > 0 {
        eprintln!("[fund] breadth seg fail {n_bad}/{n_seg} 批（{last_err}）");
    }
    let cov = if n_seg == 0 { 1.0 } else { (n_seg - n_bad) as f64 / n_seg as f64 };
    FLOW_COV.lock_ok().0 = cov;
    for (name, s) in stats {
        let n = if s.n == 0 { 1 } else { s.n } as f64;
        out.insert(name, json!({
            "up": zg_core::model::py_round(s.up as f64 / n * 100.0, 1),
            "big_up": zg_core::model::py_round(s.big_up as f64 / n * 100.0, 1),
            "big_dn": zg_core::model::py_round(s.big_dn as f64 / n * 100.0, 1),
            "n": s.n,
        }));
    }
    out
}

/// `_flow_crowd`：拥挤维度。
pub fn flow_crowd(sectors: &Map<String, Value>, turn5: &Map<String, Value>) -> Map<String, Value> {
    let tot: f64 = sectors.values().filter_map(|s| s.get("turn").and_then(|x| x.as_f64())).sum();
    let mut out = Map::new();
    for (name, s) in sectors {
        let t = match s.get("turn").and_then(|x| x.as_f64()) {
            Some(v) => v,
            None => continue,
        };
        let a5 = turn5.get(name).and_then(|x| x.as_f64());
        out.insert(name.clone(), json!({
            "turn": t,
            "share": if tot != 0.0 { Value::from(zg_core::model::py_round(t / tot * 100.0, 2)) } else { Value::Null },
            "amp": a5.map(|a| Value::from(zg_core::model::py_round(t / a, 2))).unwrap_or(Value::Null),
        }));
    }
    out
}

/// `_flow_panic`：行业恐慌度。
pub fn flow_panic(bre: &Map<String, Value>, crowd: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, b) in bre {
        let up = b.get("up").and_then(|x| x.as_f64());
        let amp = crowd.get(name).and_then(|c| c.get("amp")).and_then(|x| x.as_f64());
        let (up, amp) = match (up, amp) {
            (Some(u), Some(a)) => (u, a),
            _ => continue,
        };
        let down = 1.0 - up / 100.0;
        out.insert(name.clone(), json!({
            "panic": zg_core::model::py_round(down * amp, 2),
            "down_share": zg_core::model::py_round(down * 100.0, 1),
            "amp": amp,
        }));
    }
    out
}

/// `_flow_fund_loop`：估值/广度后台刷新（低频控量，失败 300s 自愈）。
pub fn fund_loop() {
    let mut told = false;
    let mut fails = 0u32;
    loop {
        let r = crate::monitor_live::guard_round("fund", || -> u64 {
            let now_t = now();
            let (mut bts, mut vts) = with_fund(|c| (c.bre.0, c.val.0));
            if bts == 0.0 {
                let d = read_json_pub(&bre_file()).unwrap_or(json!({}));
                let ts = d.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
                if d.get("data").map(|x| x.is_object()).unwrap_or(false) && now_t - ts < 3600.0 {
                    let data = d.get("data").cloned().unwrap_or(json!({}));
                    with_fund(|c| c.bre = (ts, data));
                    bts = ts;
                }
            }
            if vts == 0.0 {
                let d = read_json_pub(&val_file()).unwrap_or(json!({}));
                let ts = d.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
                if d.get("data").map(|x| x.is_object()).unwrap_or(false) && now_t - ts < 6.0 * 3600.0 {
                    let data = d.get("data").cloned().unwrap_or(json!({}));
                    with_fund(|c| c.val = (ts, data));
                    vts = ts;
                }
            }
            // 成分股/广度/估值全依赖同花顺：未配置 key 时不进批次循环（磁盘旧缓存仍已在此前装载），
            // 只提示一次并长睡；配置 key 后下一轮自动恢复
            if !zg_market::ths::enabled() {
                if !told {
                    eprintln!("[fund] 未配置同花顺 API key，行业估值/广度降级停更（配置后自动恢复）");
                    told = true;
                }
                return 1800;
            }
            told = false;
            flow_members();
            if now_t - bts >= 3600.0 {
                let bre = flow_breadth();
                if !bre.is_empty() {
                    let cov = FLOW_COV.lock_ok().0;
                    let bts_new = if cov >= flow::FLOW_COV_MIN { now_t } else { now_t - 3600.0 };
                    let bre_v = Value::Object(bre);
                    with_fund(|c| c.bre = (bts_new, bre_v.clone()));
                    flow::write_json(&bre_file(), &json!({"ts": bts_new, "data": bre_v}));
                }
            }
            if now_t - vts >= 6.0 * 3600.0 {
                let vals = flow_valuations();
                if !vals.is_empty() {
                    let cov = FLOW_COV.lock_ok().1;
                    let vts_new = if cov >= flow::FLOW_COV_MIN { now_t } else { now_t - 6.0 * 3600.0 };
                    let vals_v = Value::Object(vals.clone());
                    with_fund(|c| c.val = (vts_new, vals_v.clone()));
                    flow::write_json(&val_file(), &json!({"ts": vts_new, "data": vals_v}));
                    if cov >= flow::FLOW_COV_MIN {
                        record_val_hist(&vals);
                    }
                }
            }
            300
        });
        let secs = match r {
            Ok(s) => {
                fails = 0;
                s
            }
            Err(_) => {
                fails += 1;
                eprintln!("[fund] 连续失败 {fails} 轮，60s 后重试");
                60
            }
        };
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

/// `/api/flowlab` 需要的内部数据块。
pub fn fund_state() -> (Value, Value) {
    with_fund(|c| (c.val.1.clone(), c.bre.1.clone()))
}
