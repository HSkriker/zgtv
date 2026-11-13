//! 8786 业务读接口 —— 对齐 `modules/zgapp/aux_services.py`：
//! 预警、题材、缠论（同进程 zg-chan 替代桥）、行业热度、tdx-master 状态。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Map, Value};

use zg_market::industry::IndustryHeat;

use crate::state::{now, WebState};

// ---------------------------------------------------------------- 预警

const ALERT_NEAR_PCT: f64 = 0.02;

fn alerts_file() -> PathBuf {
    zg_core::paths::get().market_data.join("alerts.json")
}

/// `load_alerts`：读取 + 损坏备份 + 过滤非法记录。
pub fn load_alerts(state: &WebState) {
    let path = alerts_file();
    if !path.is_file() {
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            *state.alert_load_ok.lock_ok() = false;
            eprintln!("[alerts] 读取失败，暂缓落盘: {e}");
            return;
        }
    };
    let parsed: Result<Value, _> = serde_json::from_str(&text);
    let doc = match parsed {
        Ok(Value::Object(o)) => o,
        _ => {
            // 文件损坏：备份 + 清空（顶层不是对象/JSON 非法同 Python except 分支）
            let bak = format!(
                "{}.corrupt.{}",
                path.display(),
                chrono::Local::now().format("%Y%m%d%H%M%S")
            );
            match std::fs::copy(&path, &bak) {
                Ok(_) => eprintln!("[alerts] 文件损坏，已备份到 {bak}"),
                Err(be) => eprintln!("[alerts] 文件损坏且备份失败: {be}"),
            }
            *state.alerts.lock_ok() = HashMap::new();
            *state.alert_log.lock_ok() = Vec::new();
            return;
        }
    };
    let mut alerts = HashMap::new();
    if let Some(Value::Object(map)) = doc.get("alerts") {
        for (k, v) in map {
            if v.is_object()
                && v.get("code").map(|c| !c.is_null()).unwrap_or(false)
                && v.get("target").map(|t| !t.is_null()).unwrap_or(false)
            {
                alerts.insert(k.clone(), v.clone());
            }
        }
    }
    let log: Vec<Value> = doc
        .get("log")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter(|x| x.is_object()).cloned().collect())
        .unwrap_or_default();
    *state.alerts.lock_ok() = alerts;
    {
        let mut l = state.alert_log.lock_ok();
        *l = log;
        trim_alert_log(&mut l);
    }
    *state.alert_load_ok.lock_ok() = true;
}

/// 告警日志上限：只保留最近 N 条触发记录，避免内存与 alerts.json 单调增长。
pub(crate) const ALERT_LOG_MAX: usize = 500;

/// 就地裁剪告警日志（保留最新 N 条）。
pub(crate) fn trim_alert_log(log: &mut Vec<Value>) {
    if log.len() > ALERT_LOG_MAX {
        let drop_n = log.len() - ALERT_LOG_MAX;
        log.drain(0..drop_n);
    }
}

/// `save_alerts`：原子落盘。
pub(crate) fn save_alerts(state: &WebState) -> bool {
    if !*state.alert_load_ok.lock_ok() {
        eprintln!("[alerts] 磁盘读取未恢复，暂缓落盘");
        return false;
    }
    let payload = json!({
        "alerts": Value::Object(state.alerts.lock_ok().iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect()),
        "log": Value::Array(state.alert_log.lock_ok().clone()),
    });
    let path = alerts_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&path);
    match serde_json::to_vec(&payload) {
        Ok(body) => {
            if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                return true;
            }
            false
        }
        Err(_) => false,
    }
}

fn state_code2name(state: &WebState) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for it in state.stock_list.lock_ok().iter() {
        if let Some(code) = it.get("code").and_then(|c| c.as_str()) {
            m.insert(code.to_string(), it.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string());
        }
    }
    m
}

fn records_list(state: &WebState) -> Value {
    let alerts = state.alerts.lock_ok();
    let mut keys: Vec<&String> = alerts.keys().collect();
    keys.sort(); // Python dict 保持插入序；Rust HashMap 无序 → 排序保证确定性
    Value::Array(keys.iter().map(|k| alerts.get(*k).unwrap().clone()).collect())
}

pub(crate) fn alert_hit(price: f64, tp: f64, d: &str) -> bool {
    if price <= 0.0 || tp <= 0.0 {
        return false;
    }
    match d {
        "up" => price >= tp,
        "down" => price <= tp,
        "near" => (price - tp).abs() / tp <= ALERT_NEAR_PCT,
        _ => false,
    }
}

/// `GET /api/alerts` 的完整语义（含 add/del/reset/clear/refresh 写副作用）。
pub fn alerts_payload(state: &WebState, q: &HashMap<String, Vec<String>>) -> Value {
    let qget = |k: &str| -> Option<String> {
        q.get(k).and_then(|v| v.first()).cloned()
    };
    let act = qget("action").unwrap_or_default();
    if act == "add" {
        let code = qget("code").unwrap_or_default().trim().to_string();
        let mut name = qget("name").unwrap_or_default().trim().to_string();
        let price_s = qget("price").unwrap_or_default().trim().to_string();
        let dir_raw = qget("dir").unwrap_or_default().trim().to_lowercase();
        let note = qget("note").unwrap_or_default().trim().to_string();
        if !code.is_empty() && name.is_empty() {
            name = state_code2name(state).get(&code).cloned().unwrap_or_default();
        }
        if code.is_empty() {
            return json!({"ok": false, "error": "代码缺失"});
        }
        // 目标价可空仅限 dir=none（「仅记录」）；显式方向 + 空价必须报错，不做静默降级
        let tp_opt: Option<f64> = if price_s.is_empty() {
            None
        } else {
            match price_s.parse::<f64>() {
                Ok(v) if v > 0.0 => Some(v),
                _ => return json!({"ok": false, "error": "目标价格无效"}),
            }
        };
        let watch_only = dir_raw == "none";
        let dirn = if watch_only {
            "none".to_string()
        } else {
            match dir_raw.as_str() {
                "" | "up" => "up".to_string(),
                "down" => "down".to_string(),
                "near" => "near".to_string(),
                _ => return json!({"ok": false, "error": "方向无效"}),
            }
        };
        if !watch_only && tp_opt.is_none() {
            return json!({"ok": false, "error": "目标价格缺失：仅记录请选择「仅记录」方向"});
        }
        let target_val = tp_opt.map(Value::from).unwrap_or(Value::Null);
        let armed = dirn != "none";
        {
            let mut alerts = state.alerts.lock_ok();
            let rec = alerts.entry(code.clone()).or_insert_with(|| {
                json!({
                    "code": code, "name": name, "target": target_val.clone(), "dir": dirn,
                    "note": note,
                    "added": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    "triggered": false, "armed": armed,
                    "triggered_time": Value::Null, "trigger_price": Value::Null,
                    "last_price": Value::Null, "last_pct": Value::Null,
                    "last_time": Value::Null, "hits": 0,
                })
            });
            if let Some(o) = rec.as_object_mut() {
                o.insert("target".into(), target_val);
                o.insert("dir".into(), Value::String(dirn));
                o.insert("note".into(), Value::String(note));
                if !name.is_empty() {
                    o.insert("name".into(), Value::String(name));
                }
                o.insert("armed".into(), Value::Bool(armed));
                o.insert("triggered".into(), Value::Bool(false));
                o.insert("triggered_time".into(), Value::Null);
                o.insert("trigger_price".into(), Value::Null);
            }
        }
        // 锁外落盘：std::Mutex 不可重入，save_alerts 内部还要锁 alerts（Python 用 RLock 才安全）
        save_alerts(state);
        return json!({"ok": true, "records": records_list(state)});
    }
    if act == "del" {
        let code = qget("code").unwrap_or_default().trim().to_string();
        state.alerts.lock_ok().remove(&code);
        save_alerts(state);
        return json!({"ok": true, "records": records_list(state)});
    }
    if act == "reset" {
        let code = qget("code").unwrap_or_default().trim().to_string();
        let mut hit = false;
        {
            let mut alerts = state.alerts.lock_ok();
            if let Some(rec) = alerts.get_mut(&code) {
                let armed = rec
                    .get("dir")
                    .and_then(|d| d.as_str())
                    .map(|d| d != "none")
                    .unwrap_or(true);
                if let Some(o) = rec.as_object_mut() {
                    o.insert("armed".into(), Value::Bool(armed));
                    o.insert("triggered".into(), Value::Bool(false));
                    o.insert("triggered_time".into(), Value::Null);
                    o.insert("trigger_price".into(), Value::Null);
                }
                hit = true;
            }
        }
        if hit {
            save_alerts(state); // 锁外落盘（std::Mutex 不可重入）
        }
        return json!({"ok": true, "records": records_list(state)});
    }
    if act == "clear" {
        state.alerts.lock_ok().clear();
        state.alert_log.lock_ok().clear();
        save_alerts(state);
        return json!({"ok": true, "records": []});
    }
    if act == "refresh" {
        let codes: Vec<String> = state.alerts.lock_ok().keys().cloned().collect();
        let mut quotes: HashMap<String, Value> = HashMap::new();
        for code in codes {
            if let Ok(qq) = zg_market::quotes::get_quote(&code) {
                if qq.get("price").and_then(|p| p.as_f64()).unwrap_or(0.0) != 0.0 {
                    quotes.insert(code, qq);
                }
            }
        }
        {
            let mut alerts = state.alerts.lock_ok();
            let mut log = state.alert_log.lock_ok();
            for (code, qq) in &quotes {
                let rec = match alerts.get_mut(code) {
                    Some(r) => r,
                    None => continue,
                };
                let price = qq.get("price").and_then(|p| p.as_f64()).unwrap_or(0.0);
                let o = rec.as_object_mut().unwrap();
                o.insert("last_price".into(), Value::from(price));
                o.insert("last_pct".into(), qq.get("pct").cloned().unwrap_or(Value::Null));
                o.insert("last_time".into(),
                         Value::String(chrono::Local::now().format("%H:%M:%S").to_string()));
                let tp = rec.get("target").and_then(|t| t.as_f64()).unwrap_or(0.0);
                let armed = rec.get("armed").and_then(|a| a.as_bool()).unwrap_or(true);
                let dirn = rec.get("dir").and_then(|d| d.as_str()).unwrap_or("up").to_string();
                let name = rec.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let hits = rec.get("hits").and_then(|h| h.as_i64()).unwrap_or(0);
                if armed && alert_hit(price, tp, &dirn) {
                    let t = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let o = rec.as_object_mut().unwrap();
                    o.insert("triggered".into(), Value::Bool(true));
                    o.insert("armed".into(), Value::Bool(false));
                    o.insert("triggered_time".into(), Value::String(t.clone()));
                    o.insert("trigger_price".into(), Value::from(price));
                    o.insert("hits".into(), Value::from(hits + 1));
                    log.push(json!({"code": code,
                        "name": name, "price": price, "dir": dirn, "target": tp, "time": t}));
                }
            }
            trim_alert_log(&mut log);
        }
        save_alerts(state); // 锁外落盘（std::Mutex 不可重入）
        return json!({"ok": true, "records": records_list(state),
                      "log": state.alert_log.lock_ok().clone()});
    }
    let recs = records_list(state);
    let arr = recs.as_array().cloned().unwrap_or_default();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let up_n = arr.iter().filter(|r| r.get("dir").and_then(|d| d.as_str()) == Some("up")).count();
    let down_n = arr.iter().filter(|r| r.get("dir").and_then(|d| d.as_str()) == Some("down")).count();
    let near_n = arr.iter().filter(|r| r.get("dir").and_then(|d| d.as_str()) == Some("near")).count();
    let watch_n = arr.iter().filter(|r| r.get("dir").and_then(|d| d.as_str()) == Some("none")).count();
    let triggered: Vec<&Value> = arr
        .iter()
        .filter(|r| r.get("triggered").and_then(|t| t.as_bool()).unwrap_or(false))
        .collect();
    let trig_today = triggered
        .iter()
        .filter(|r| r.get("triggered_time").and_then(|t| t.as_str()).unwrap_or("").starts_with(&today))
        .count();
    let tracking_n = arr.len().saturating_sub(triggered.len()).saturating_sub(watch_n);
    let stats = json!({"total": arr.len(), "up": up_n, "down": down_n, "near": near_n,
                       "watch": watch_n, "tracking": tracking_n, "triggered": triggered.len(),
                       "triggered_today": trig_today});
    json!({"ok": true, "records": recs,
           "log": state.alert_log.lock_ok().clone(), "stats": stats})
}

// ---------------------------------------------------------------- 个股题材

fn _fetch_stock_theme(code: &str) -> Result<Value, String> {
    let mut industry = String::new();
    let mut area = String::new();
    let mut concepts: Vec<String> = Vec::new();
    let mut ok1 = false;
    let mut ok2 = false;
    let url = format!(
        "https://push2delay.eastmoney.com/api/qt/stock/get?secid={}&invt=2&fltt=2&fields=f57,f58,f127,f128",
        zg_market::kline::secid(code)
    );
    if let Ok(text) = zg_core::net::http_get(&url, 8, 0, None, None, None) {
        if let Ok(j) = serde_json::from_str::<Value>(&text) {
            ok1 = true;
            let d = j.get("data").cloned().unwrap_or(json!({}));
            industry = d.get("f127").and_then(|v| v.as_str()).unwrap_or("").to_string();
            area = d.get("f128").and_then(|v| v.as_str()).unwrap_or("").to_string();
        }
    }
    let suffix = if code.starts_with("6") || code.starts_with("9") || code.starts_with("5") { "SH" } else { "SZ" };
    let sym = format!("{code}.{suffix}");
    let url2 = format!(
        "https://datacenter.eastmoney.com/securities/api/data/v1/get\
?reportName=RPT_F10_CORETHEME_BOARDTYPE\
&columns=SECUCODE,SECURITY_CODE,SECURITY_NAME_ABBR,BOARD_NAME,BOARD_TYPE,IS_PRECISE\
&filter=(SECUCODE%3D%22{sym}%22)\
&pageNumber=1&pageSize=50&sortTypes=-1&sortColumns=IS_PRECISE&source=HSF10&client=PC"
    );
    if let Ok(text) = zg_core::net::http_get(
        &url2, 8, 0, None,
        Some(&[("Referer", "https://emweb.securities.eastmoney.com/")]), None,
    ) {
        if let Ok(j) = serde_json::from_str::<Value>(&text) {
            ok2 = true;
            let rows = j.get("result").and_then(|r| r.get("data"))
                .and_then(|d| d.as_array()).cloned().unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            for d in rows {
                let btype = d.get("BOARD_TYPE").and_then(|v| v.as_str()).unwrap_or("");
                let name = d.get("BOARD_NAME").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() || btype == "行业" || btype == "板块" {
                    continue;
                }
                if !seen.insert(name.to_string()) {
                    continue;
                }
                concepts.push(name.to_string());
                if concepts.len() >= 8 {
                    break;
                }
            }
        }
    }
    if !ok1 && !ok2 {
        return Err("东财题材接口均失败".into());
    }
    Ok(json!({"industry": industry, "area": area, "concepts": concepts}))
}

// ---------------------------------------------------------------- 题材本地化（点击路径零网络）

fn theme_disk_path() -> PathBuf {
    zg_core::paths::get().market_data.join("theme_cache.json")
}

/// 启动时装载落盘题材；文件缺失返回 0，损坏改名 .json.bad 保留现场
pub fn load_theme_disk(state: &WebState) -> usize {
    let path = theme_disk_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let bad = || {
        let _ = std::fs::rename(&path, path.with_extension("json.bad"));
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            bad();
            return 0;
        }
    };
    let stocks = match v.get("stocks").and_then(|s| s.as_object()) {
        Some(s) => s.clone(),
        None => {
            bad();
            return 0;
        }
    };
    let mut cache = state.theme_cache.lock_ok();
    let mut n = 0usize;
    for (code, data) in stocks {
        if data.get("industry").is_some() || data.get("concepts").is_some() {
            cache.insert(code, data);
            n += 1;
        }
    }
    n
}

fn save_theme_disk(state: &WebState) {
    let snapshot: HashMap<String, Value> = state.theme_cache.lock_ok().clone();
    let body = json!({
        "version": 1,
        "saved_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "count": snapshot.len(),
        "stocks": snapshot,
    });
    // 唯一 tmp 名：6 个 worker 每 500 条并发落盘时互不覆盖（固定名会写出半截 JSON）
    static SAVE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SAVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = theme_disk_path();
    let tmp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), seq));
    if let Ok(text) = serde_json::to_string(&body) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub fn get_stock_theme(state: &WebState, code: &str) -> Value {
    match state.theme_cache.lock_ok().get(code).cloned() {
        Some(data) => data,
        None => json!({
            "industry": "", "area": "", "concepts": [],
            "note": "题材未缓存，运行 刷新题材.bat 更新"
        }),
    }
}

/// 触发题材后台刷新：默认只抓缺失/超过 days 天的；force=1 全量；limit 限量（测试用）。
/// 点击路径永不调用本函数——网络抓取只发生在 bat 触发或首次引导。
pub fn theme_refresh_start(state: &Arc<WebState>, force: bool, days: i64, limit: Option<usize>) -> Value {
    if state.offline {
        return json!({"ok": false, "error": "offline 模式不访问网络，题材只读本地缓存"});
    }
    if state.theme_refreshing.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return json!({"ok": false, "running": true, "error": "已有刷新在进行"});
    }
    let mut codes: Vec<String> = state
        .stock_list
        .lock_ok()
        .iter()
        .filter_map(|v| v.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .collect();
    if codes.is_empty() {
        let path = zg_core::paths::get().market_data.join("stocks.json");
        if let Ok(t) = std::fs::read_to_string(&path) {
            if let Ok(list) = serde_json::from_str::<Vec<Value>>(t.trim_start_matches('\u{feff}')) {
                for v in list {
                    if let Some(c) = v.get("code").and_then(|c| c.as_str()) {
                        codes.push(c.to_string());
                    }
                }
            }
        }
    }
    // 北交所(8/920)/老三板(4)/沪B(9) 不参与题材抓取：SECUCODE 拼不通，只会产生无效请求与虚高覆盖率分母
    codes.retain(|c| !matches!(c.chars().next(), Some('4') | Some('8') | Some('9')));
    let cutoff = now() - (days.max(0) as f64) * 86400.0;
    let todo: Vec<String> = {
        let cache = state.theme_cache.lock_ok();
        codes.into_iter()
            .filter(|c| {
                if force {
                    return true;
                }
                match cache.get(c) {
                    None => true,
                    Some(d) => d
                        .get("fetched_at")
                        .and_then(|t| t.as_f64())
                        .map(|t| t < cutoff)
                        .unwrap_or(true),
                }
            })
            .collect()
    };
    let total = limit.map(|l| todo.len().min(l)).unwrap_or(todo.len());
    if total == 0 {
        state.theme_refreshing.store(false, std::sync::atomic::Ordering::SeqCst);
        return json!({"ok": true, "running": false, "done": 0, "total": 0, "msg": "全部题材已在有效期内，无需抓取"});
    }
    let todo: Vec<String> = todo.into_iter().take(total).collect();
    *state.theme_prog.lock_ok() = json!({
        "running": true, "done": 0, "total": total,
        "started": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    let st = Arc::clone(state);
    let spawned = std::thread::Builder::new().name("theme-refresh".into()).spawn(move || {
        // 任何路径退出（含收尾 panic）都必须复位刷新闸门，否则题材刷新永久锁死
        let _flag = zg_core::sync::OnDrop::new(|| {
            st.theme_refreshing.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        let t0 = std::time::Instant::now();
        let done_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fail_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let workers = 6usize;
        let chunk_size = todo.len().div_ceil(workers);
        // 任一 worker panic 都不能跳过收尾（否则 theme_refreshing 永为 true，刷新永久锁死）
        let scope_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::thread::scope(|s| {
                for chunk in todo.chunks(chunk_size) {
                    let st = &st;
                    let done_counter = &done_counter;
                    let fail_counter = &fail_counter;
                    s.spawn(move || {
                        for code in chunk {
                            // 抓取失败不写缓存、不盖时间戳，下次刷新会重试；旧条目保留
                            if let Ok(mut data) = _fetch_stock_theme(code) {
                                data["fetched_at"] = json!(now());
                                data["fetched_at_txt"] =
                                    json!(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
                                st.theme_cache.lock_ok().insert(code.clone(), data);
                            } else {
                                fail_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            let done = done_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            if done.is_multiple_of(500) {
                                save_theme_disk(st);
                            }
                            let mut p = st.theme_prog.lock_ok();
                            p["done"] = json!(done);
                            p["failed"] = json!(fail_counter.load(std::sync::atomic::Ordering::SeqCst));
                        }
                    });
                }
            });
        }));
        save_theme_disk(&st);
        let n = st.theme_cache.lock_ok().len();
        let failed = fail_counter.load(std::sync::atomic::Ordering::SeqCst);
        *st.theme_prog.lock_ok() = json!({
            "running": false, "done": total, "total": total,
            "failed": failed, "panic": scope_result.is_err(),
            "finished": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "cache_n": n, "secs": t0.elapsed().as_secs_f64(),
        });
        eprintln!("[theme] 刷新结束 {total} 只（失败 {failed}），内存 {n} 条，用时 {:.1}s，已落盘",
                  t0.elapsed().as_secs_f64());
    });
    if spawned.is_err() {
        state.theme_refreshing.store(false, std::sync::atomic::Ordering::SeqCst);
        return json!({"ok": false, "error": "刷新线程启动失败"});
    }
    json!({"ok": true, "running": true, "total": total, "msg": "后台刷新已启动"})
}

// ---------------------------------------------------------------- 缠论（同进程 zg-chan）

pub fn czsc_enabled() -> bool {
    std::env::var("CZSC").map(|v| v.trim() != "0").unwrap_or(true)
}

pub fn czsc_status() -> Value {
    json!({"enabled": czsc_enabled(), "ok": czsc_enabled(), "ver": "1.0.2", "cooldown": 0.0})
}

/// `czsc_analyze`：min 周期用内联 bars（与 /api/kline 同一份），其余由 zg-chan 自行取数。
pub fn czsc_analyze(code: &str, period: &str, inline_bars: Option<&Vec<Value>>) -> Value {
    czsc_analyze_ex(code, period, inline_bars, None)
}

/// 指数/板块（board=1）版本：bars 一律内联（来自 /api/board-kline），
/// market 显式指定 —— 000001 在该语境是上证指数而非平安银行，不能走自动 resolve。
pub fn czsc_analyze_ex(
    code: &str, period: &str, inline_bars: Option<&Vec<Value>>, market: Option<&str>,
) -> Value {
    if !czsc_enabled() {
        return json!({"ok": false, "error": "缠论已关闭（CZSC=0）"});
    }
    let freq = match crate::consts::czsc_freq_of_period(period) {
        Some(f) => f,
        None => {
            return json!({"ok": false, "error": format!(
                "不支持的周期: {period}（支持 day/week/month/min1/min5/min15/min30/min60）")})
        }
    };
    let res = match inline_bars {
        Some(bars) => {
            let mut parsed = Vec::with_capacity(bars.len());
            for v in bars {
                match serde_json::from_value::<zg_chan::service::InlineBar>(v.clone()) {
                    Ok(b) => parsed.push(b),
                    Err(e) => {
                        return json!({"ok": false, "code": code,
                                      "error": format!("ValueError: {e}")})
                    }
                }
            }
            zg_chan::payload::build_payload_from_bars(code, freq, &parsed, market)
        }
        None => zg_chan::payload::build_payload(code, freq, market),
    };
    match res {
        Ok(mut v) => {
            // Python 桥回包含请求 id（matrix volatile 已声明剥离）；同进程补固定值保持键集一致
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), Value::from(1));
            }
            v
        }
        Err(e) => json!({"ok": false, "error": e, "code": code}),
    }
}

/// `czsc_quotes`：TQ(front) → 同花顺快照，15s 缓存；返回 {code: {price, pct}}。
pub fn czsc_quotes(state: &WebState, codes: &[String]) -> Value {
    let now_t = now();
    let mut out = Map::new();
    let mut missing: Vec<String> = Vec::new();
    {
        let cache = state.czq_cache.lock_ok();
        for c in codes {
            match cache.get(c) {
                Some((ts, item)) if now_t - ts < 15.0 => {
                    out.insert(c.clone(), item.clone());
                }
                _ => missing.push(c.clone()),
            }
        }
    }
    if !missing.is_empty() {
        let mut quotes: Option<HashMap<String, Value>> = None;
        if zg_market::tq::available() {
            quotes = zg_market::tq::quotes(&missing, "front", true).ok().filter(|m| !m.is_empty());
        }
        if quotes.is_none() && zg_market::consts::ths_available() {
            let snap = zg_market::quotes::fetch_snapshot_ths(&missing[..missing.len().min(300)]);
            quotes = if snap.is_empty() { None } else { Some(snap) };
        }
        let empty = HashMap::new();
        let q = quotes.as_ref().unwrap_or(&empty);
        let mut cache = state.czq_cache.lock_ok();
        for c in &missing {
            let item = match q.get(c) {
                Some(x) => x,
                None => continue,
            };
            let price = item.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let pre = item.get("preClose").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if price <= 0.0 || pre <= 0.0 {
                continue;
            }
            let item = json!({"price": zg_core::model::py_round(price, 2),
                              "pct": zg_core::model::py_round((price / pre - 1.0) * 100.0, 2)});
            out.insert(c.clone(), item.clone());
            cache.insert(c.clone(), (now_t, item));
        }
        let expired: Vec<String> = cache.iter()
            .filter(|(_, v)| now_t - v.0 > 60.0).map(|(k, _)| k.clone()).collect();
        for k in expired {
            cache.remove(&k);
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------- 扫描

fn czsc_scan_file() -> PathBuf {
    zg_core::paths::get().market_data.join("czsc_scan_cache.json")
}

pub fn czsc_load_cache() -> Option<Value> {
    let text = std::fs::read_to_string(czsc_scan_file()).ok()?;
    serde_json::from_str(&text).ok()
}

fn local_data_date() -> String {
    zg_market::market_data::local_data_date()
}

/// `_czsc_codes`：6 位数字、排除 4/8/92 开头。
fn czsc_codes(state: &WebState) -> Vec<String> {
    state
        .stock_list
        .lock_ok()
        .iter()
        .filter_map(|r| r.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .filter(|c| {
            c.len() == 6 && c.chars().all(|ch| ch.is_ascii_digit())
                && !(c.starts_with('4') || c.starts_with('8') || c.starts_with("92"))
        })
        .collect()
}

/// 后台启动扫描（`czsc_start_scan`）。返回是否已启动/已在跑。
pub fn czsc_start_scan(state: Arc<WebState>) {
    {
        let mut running = state.czsc_scan_running.lock_ok();
        if *running {
            return;
        }
        *running = true;
        *state.czsc_scan_state.lock_ok() = json!({
            "running": true,
            "started": chrono::Local::now().format("%H:%M:%S").to_string(),
            "total": 0,
            "error": "",
            // 失败退避用：成功启动即清零，失败时写 now()
            "failed_at": 0.0,
            "last_attempt": now(),
        });
    }
    std::thread::spawn(move || {
        // panic 也要复位闸门，否则面板永远停在「扫描中」
        let _flag = zg_core::sync::OnDrop::new(|| {
            *state.czsc_scan_running.lock_ok() = false;
            if let Some(o) = state.czsc_scan_state.lock_ok().as_object_mut() {
                o.insert("running".into(), Value::Bool(false));
            }
        });
        let t0 = now();
        let codes = czsc_codes(&state);
        if let Some(o) = state.czsc_scan_state.lock_ok().as_object_mut() {
            o.insert("total".into(), Value::from(codes.len()));
        }
        let result = if codes.is_empty() {
            Err("股票列表未就绪".to_string())
        } else {
            let resp = zg_chan::scan::run_scan(&codes, 5, None, "D", None);
            let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            if !ok {
                Err(resp.get("error").and_then(|e| e.as_str()).unwrap_or("扫描失败").to_string())
            } else {
                let names: HashMap<String, String> = state.stock_list.lock_ok().iter()
                    .filter_map(|r| {
                        let c = r.get("code").and_then(|x| x.as_str())?.to_string();
                        Some((c, r.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string()))
                    })
                    .collect();
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
                let errors_n = errors.as_array().map(|a| a.len()).unwrap_or(0);
                let skipped_n = resp.get("skipped").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
                let n_total = resp.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                let invalid = (skipped_n + errors_n) as i64;
                // 覆盖率判据：skip+err 超过 20% 视为数据源异常（此前「全 skip 0 error」会被写成
                // 0 命中的新鲜缓存 → 面板永久空白且不再重扫）
                let coverage_bad = n_total > 0 && invalid * 100 > n_total * 20;
                if coverage_bad || (hits.is_empty() && errors_n > 0) {
                    Err(format!(
                        "扫描仅覆盖 {}/{}（skip {skipped_n} / err {errors_n}），数据源异常，不落缓存",
                        (n_total - invalid).max(0), n_total))
                } else {
                    let skipped = resp.get("skipped").cloned().unwrap_or(json!([]));
                    let data_last = resp.get("data_last").and_then(|d| d.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(local_data_date);
                    let cache = json!({
                        "asof": data_last,
                        "data_last": data_last,
                        "scan_date": zg_core::calendar::last_trading_date(None),
                        "elapsed": ((now() - t0) * 10.0).round() / 10.0,
                        "days": 5, "types": ["1", "2", "3"],
                        // `run_scan` 不带 ver（协议层 scan_response 才补），缓存里要自己写
                        "ver": Value::from(zg_chan::payload::CZSC_VERSION),
                        "n": resp.get("n").cloned().unwrap_or(Value::Null),
                        "method_ver": crate::consts::CZSC_METHOD_VER,
                        "hits": hits, "errors": errors, "skipped": skipped,
                    });
                    let path = czsc_scan_file();
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Ok(body) = serde_json::to_vec(&cache) {
                        let tmp = zg_core::jsonio::tmp_name(&path);
                        if std::fs::write(&tmp, body).is_ok() {
                            let _ = std::fs::rename(&tmp, &path);
                        }
                    }
                    // 部分失败：只记 error 状态，不中断
                    Ok(())
                }
            }
        };
        if let Err(e) = result {
            if let Some(o) = state.czsc_scan_state.lock_ok().as_object_mut() {
                o.insert("error".into(), Value::String(e));
                o.insert("failed_at".into(), Value::from(now()));
            }
        } else {
            let errors_n = czsc_load_cache()
                .and_then(|c| c.get("errors").and_then(|e| e.as_array()).map(|a| a.len()))
                .unwrap_or(0);
            if errors_n > 0 {
                if let Some(o) = state.czsc_scan_state.lock_ok().as_object_mut() {
                    o.insert("error".into(), Value::String(format!(
                        "扫描部分失败 {errors_n} 项（结果可能不全，详见缓存 errors）")));
                }
            }
        }
    });
}

fn czsc_pct_enrich(state: &WebState, mut cache: Value) -> Value {
    let base = state.daily_date.lock_ok().clone();
    let daily = state.market.daily_cache.lock_ok();
    let data = daily.as_ref();
    if let Some(hits) = cache.get_mut("hits").and_then(|h| h.as_array_mut()) {
        for h in hits.iter_mut() {
            let code = h.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let mut pct = Value::Null;
            if let Some(store) = data {
                if let Some(bars) = store.dicts(&code) {
                    if bars.len() >= 2 {
                        let c1 = bars[bars.len() - 1].get("close").and_then(|v| v.as_f64());
                        let c0 = bars[bars.len() - 2].get("close").and_then(|v| v.as_f64());
                        if let (Some(c1), Some(c0)) = (c1, c0) {
                            if c0 > 0.0 {
                                pct = Value::from(zg_core::model::py_round((c1 / c0 - 1.0) * 100.0, 2));
                            }
                        }
                    }
                }
            }
            if let Some(o) = h.as_object_mut() {
                o.insert("pct".into(), pct);
            }
        }
    }
    if let Some(o) = cache.as_object_mut() {
        o.insert("pct_base".into(), Value::String(base));
    }
    cache
}

/// 扫描失败后的自动重试退避（秒）：前端 15s 轮询不得把失败扫描打成死循环。
const CZSC_SCAN_BACKOFF_SECS: f64 = 300.0;

/// 上次扫描失败且仍在退避窗口内 → 本轮不再自动重扫。
fn czsc_scan_backoff_active(state: &WebState) -> bool {
    let st = state.czsc_scan_state.lock_ok();
    let failed_at = st.get("failed_at").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let err = st.get("error").and_then(|e| e.as_str()).unwrap_or("");
    !err.is_empty() && failed_at > 0.0 && now() - failed_at < CZSC_SCAN_BACKOFF_SECS
}

fn czsc_method_filter() -> bool {
    std::env::var("THINKING_METHOD_FILTER").map(|v| v == "1").unwrap_or(false)
}

pub fn czsc_scan_payload(state: &Arc<WebState>) -> Value {
    let cache = czsc_load_cache();
    let data_date = local_data_date();
    let fresh = match &cache {
        Some(c) => {
            let d = c.get("data_last").or_else(|| c.get("asof"))
                .and_then(|x| x.as_str()).unwrap_or("");
            let mv = c.get("method_ver").and_then(|m| m.as_i64()).unwrap_or(0);
            d == data_date && mv >= crate::consts::CZSC_METHOD_VER
        }
        None => false,
    };
    let running = *state.czsc_scan_running.lock_ok();
    if !fresh && !running && !czsc_scan_backoff_active(state) {
        czsc_start_scan(Arc::clone(state));
    }
    if *state.czsc_scan_running.lock_ok() {
        let st = state.czsc_scan_state.lock_ok().clone();
        let started = st.get("started").cloned().unwrap_or(Value::Null);
        let total = st.get("total").cloned().unwrap_or(Value::Null);
        // 扫描中仍返回上次结果：旧命中比空白页有用（stale=true 供前端标注）。
        if let Some(mut c) = cache {
            if let Some(o) = c.as_object_mut() {
                o.insert("running".into(), Value::Bool(true));
                o.insert("stale".into(), Value::Bool(true));
                o.insert("started".into(), started);
                o.insert("total".into(), total);
                o.insert("method_filter".into(), Value::Bool(czsc_method_filter()));
            }
            return czsc_pct_enrich(state, c);
        }
        return json!({"ok": true, "running": true, "started": started, "total": total});
    }
    if let Some(mut c) = cache {
        let error = state.czsc_scan_state.lock_ok().get("error").cloned()
            .unwrap_or_else(|| json!(""));
        if let Some(o) = c.as_object_mut() {
            o.insert("running".into(), Value::Bool(false));
            o.insert("stale".into(), Value::Bool(!fresh));
            o.insert("error".into(), error);
            o.insert("method_filter".into(), Value::Bool(czsc_method_filter()));
        }
        return czsc_pct_enrich(state, c);
    }
    let error = state.czsc_scan_state.lock_ok().get("error").cloned()
        .and_then(|e| e.as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "暂无扫描结果".into());
    json!({"ok": false, "running": false, "error": error})
}

// ---------------------------------------------------------------- 行业热度

type HeatProvider = dyn Fn(&[Value]) -> Result<(Vec<Value>, String), String> + Send + Sync;

fn industry_provider() -> Arc<HeatProvider> {
    Arc::new(move |members: &[Value]| {
        // 等价 Python `_industry_quotes` 快照链：优先 THS 批量快照（含 turnover=amount），
        // 失败再退腾讯快照（腾讯行已补 f6=成交额，与新浪/AKShare 源同形）。
        let codes: Vec<String> = members.iter()
            .filter_map(|m| m.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
            .collect();
        let ths_rows = if zg_market::consts::ths_available() {
            // 8787 限速客户端（200/批，避免 THS 429）：快照含 turnover=amount
            let mut rows: Vec<Value> = Vec::new();
            let name_map: std::collections::HashMap<&str, &str> = members.iter()
                .filter_map(|m| {
                    let c = m.get("code").and_then(|x| x.as_str())?;
                    let n = m.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    Some((c, n))
                })
                .collect();
            for seg in codes.chunks(200) {
                let thscodes: Vec<String> = seg.iter()
                    .map(|c| crate::monitor_ths::to_thscode(c))
                    .collect();
                let d = match crate::monitor_ths::ths_get(
                    "/api/a-share/prices/snapshot",
                    &[("thscodes", thscodes.join(","))], 12, 2, 1) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                for it in d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    let tscode = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let code: String = tscode.chars().take_while(|c| *c != '.').collect();
                    if code.is_empty() {
                        continue;
                    }
                    let price = crate::monitor_ths::number(it.get("last_price")).unwrap_or(0.0);
                    let pct = crate::monitor_ths::number(it.get("price_change_ratio_pct"));
                    let vol = crate::monitor_ths::number(it.get("volume"));
                    let amount = crate::monitor_ths::number(it.get("turnover"));
                    rows.push(serde_json::json!({
                        "code": code,
                        "name": name_map.get(code.as_str()).copied().unwrap_or(""),
                        "price": price,
                        "pct": pct,
                        "vol": vol.map(|v| v / 100.0),
                        "amount": amount,
                    }));
                }
            }
            if rows.is_empty() { None } else { Some(rows) }
        } else {
            None
        };
        if let Some(rows) = ths_rows {
            return Ok((rows, "共享全市场快照".to_string()));
        }
        let rows = zg_market::quotes::fetch_snapshot_tencent(&codes);
        if rows.is_empty() {
            return Err("行情源暂不可用(快照链与easytdx均无有效数据)".into());
        }
        Ok((rows, "共享全市场快照".to_string()))
    })
}

fn heat_holder() -> &'static Mutex<Option<Arc<IndustryHeat>>> {
    static H: OnceLock<Mutex<Option<Arc<IndustryHeat>>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

pub fn init_industry(_state: &Arc<WebState>) {
    let paths = zg_core::paths::get();
    let catalog = paths.root.join("modules").join("market").join("industry_catalog.json");
    let heat_cache = paths.market_data.join("industry_heat_cache.json");
    let mut h = heat_holder().lock_ok();
    if h.is_none() {
        *h = Some(Arc::new(IndustryHeat::new(
            &catalog.to_string_lossy(),
            &heat_cache.to_string_lossy(),
            industry_provider(),
        )));
    }
}

// ---------------------------------------------------------------- 日K缓存构建

/// 全市场近 2 年日K缓存：本地 .day（前复权）→ kcache v1 落盘。
/// 与 Python `jobs/cache_build.build_daily_k_cache` 同语义：交易时段不重建（当日K未终值），
/// force=1 可强制重建（结果按末根日期标记 stale）。
pub fn build_daily_cache(state: &Arc<WebState>, force: bool) -> Value {
    {
        let b = state.daily_build.lock_ok();
        if b.get("running").and_then(|v| v.as_bool()).unwrap_or(false) {
            return json!({"ok": true, "started": false, "running": true,
                          "message": "构建进行中"});
        }
    }
    if !force && zg_core::calendar::market_open_now(None) {
        return json!({"ok": true, "started": false, "running": false,
                      "message": "交易时段不重建日K缓存（收盘后当日K才有终值），可用 force=1 强制重建"});
    }
    let codes: Vec<String> = state.stock_list.lock_ok().iter()
        .filter_map(|v| v.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .filter(|c| c.len() == 6)
        .collect();
    if codes.is_empty() {
        return json!({"ok": false, "started": false, "error": "股票列表为空，无法构建日K缓存"});
    }
    {
        let mut b = state.daily_build.lock_ok();
        if let Some(o) = b.as_object_mut() {
            o.insert("running".into(), json!(true));
            o.insert("total".into(), json!(codes.len()));
            o.insert("done".into(), json!(0));
            o.insert("attempted".into(), json!(true));
        }
    }
    let st = Arc::clone(state);
    let total = codes.len();
    // 覆盖率门：本地日线大面积不可读（除权源缺失等）时绝不用残缺数据覆盖旧缓存
    let min_ok = {
        let old = st.market.daily_cache.lock_ok();
        match old.as_ref() {
            Some(s) if !s.is_empty() => s.len(),
            _ => total / 2,
        }
    };
    let spawned = std::thread::Builder::new().name("daily-k-build".into()).spawn(move || {
        let lmt = zg_market::consts::DAILY_K_LMT;
        let prefix = zg_core::paths::get().market_data
            .join("daily_k_cache").to_string_lossy().to_string();
        let mut store = zg_core::kcache::KCacheStore::create(
            &prefix, zg_core::kcache::KIND_DAY, lmt, codes.len());
        let mut ok = 0usize;
        let mut max_date = String::new();
        for (i, code) in codes.iter().enumerate() {
            if let Ok((mkt, cd)) = zg_core::reader::resolve(code, None) {
                if let Ok(bars) = zg_core::adjust::adjusted_day(&mkt, &cd) {
                    let tail: &[zg_core::model::Bar] =
                        if bars.len() > lmt { &bars[bars.len() - lmt..] } else { &bars[..] };
                    let rows = zg_market::kbuild::rows_hand(tail);
                    if !rows.is_empty() {
                        if let Some(d) = tail.last().map(|b| b.date.replace('-', "")) {
                            if d > max_date {
                                max_date = d;
                            }
                        }
                        store.write(code, &rows);
                        ok += 1;
                    }
                }
            }
            if i % 100 == 0 {
                let mut b = st.daily_build.lock_ok();
                if let Some(o) = b.as_object_mut() {
                    o.insert("done".into(), json!(i + 1));
                }
            }
        }
        let date = if max_date.len() == 8 {
            format!("{}-{}-{}", &max_date[0..4], &max_date[4..6], &max_date[6..8])
        } else {
            String::new()
        };
        store.date = date.clone();
        let saved = ok >= min_ok && ok > 0 && store.save(&prefix);
        if saved {
            *st.daily_date.lock_ok() = date.clone();
            *st.market.daily_date.lock_ok() = date.clone();
            *st.market.daily_cache.lock_ok() = Some(store);
        } else if ok < min_ok {
            let msg = format!("本地日线可读率过低（{ok}/{total}，需 ≥ {min_ok}），已保留旧缓存");
            eprintln!("[daily_k] {msg}");
            let mut b = st.daily_build.lock_ok();
            if let Some(o) = b.as_object_mut() {
                o.insert("error".into(), json!(msg));
            }
        }
        let mut b = st.daily_build.lock_ok();
        if let Some(o) = b.as_object_mut() {
            o.insert("running".into(), json!(false));
            o.insert("done".into(), json!(codes.len()));
            o.insert("date".into(), json!(date));
            o.insert("codes".into(), json!(ok));
        }
        eprintln!("[daily_k] 构建完成: {ok}/{total} 只, 日期={date}, 落盘={saved}", );
    });
    if spawned.is_err() {
        let mut b = state.daily_build.lock_ok();
        if let Some(o) = b.as_object_mut() {
            o.insert("running".into(), json!(false));
        }
        return json!({"ok": false, "started": false, "error": "构建线程启动失败"});
    }
    json!({"ok": true, "started": true, "running": true, "total": total,
           "message": "已启动后台构建"})
}

// ---------------------------------------------------------------- 周K / 分钟K缓存构建

/// 股票池 + 代码→名称（对齐 Python `_load_cached_codes` / `_stock_names`）。
fn stock_universe(state: &Arc<WebState>) -> (Vec<String>, HashMap<String, String>) {
    let list = state.stock_list.lock_ok();
    let mut codes = Vec::with_capacity(list.len());
    let mut names = HashMap::new();
    for item in list.iter() {
        if let Some(code) = item.get("code").and_then(|c| c.as_str()) {
            if code.len() == 6 {
                codes.push(code.to_string());
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    names.insert(code.to_string(), name.to_string());
                }
            }
        }
    }
    (codes, names)
}

/// 覆盖率门：新缓存可读只数不得低于旧缓存的 90%（防除权源缺失时残缺覆盖）。
fn coverage_gate(old_len: usize, total: usize) -> usize {
    if old_len > 0 {
        (old_len as f64 * 0.9) as usize
    } else {
        total / 2
    }
}

/// 全市场周K缓存：本地 `.day` 前复权 → 周线 → kcache v1。
/// 交易时段不重建（本周最后一根未终值），`force=1` 可强制。
pub fn build_weekly_cache(state: &Arc<WebState>, force: bool) -> Value {
    {
        let b = state.weekly_build.lock_ok();
        if b.get("running").and_then(|v| v.as_bool()).unwrap_or(false) {
            return json!({"ok": true, "started": false, "running": true,
                          "message": "构建进行中"});
        }
    }
    if !force && zg_core::calendar::market_open_now(None) {
        return json!({"ok": true, "started": false, "running": false,
                      "message": "交易时段不重建周K缓存（本周最后一根未终值），可用 force=1 强制重建"});
    }
    let (codes, names) = stock_universe(state);
    if codes.is_empty() {
        return json!({"ok": false, "started": false, "error": "股票列表为空，无法构建周K缓存"});
    }
    let total = codes.len();
    let min_ok = {
        let old = state.market.weekly_cache.lock_ok();
        coverage_gate(old.as_ref().map(|s| s.len()).unwrap_or(0), total)
    };
    {
        let mut b = state.weekly_build.lock_ok();
        if let Some(o) = b.as_object_mut() {
            o.insert("running".into(), json!(true));
            o.insert("total".into(), json!(total));
            o.insert("done".into(), json!(0));
            o.insert("attempted".into(), json!(true));
            o.insert("error".into(), Value::Null);
        }
    }
    let st = Arc::clone(state);
    let spawned = std::thread::Builder::new().name("weekly-k-build".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let prefix = zg_core::paths::get().market_data
            .join("weekly_k_cache").to_string_lossy().to_string();
        let mut out = zg_market::kbuild::build_weekly(
            &prefix, &codes, &names, zg_market::consts::WEEKLY_K_LMT, |done| {
                if done % 200 == 0 {
                    if let Some(o) = st.weekly_build.lock_ok().as_object_mut() {
                        o.insert("done".into(), json!(done));
                    }
                }
            });
        let saved = out.ok >= min_ok && out.ok > 0 && out.store.save(&prefix);
        let date = out.date.clone();
        if saved {
            *st.market.weekly_date.lock_ok() = date.clone();
            *st.market.weekly_cache.lock_ok() = Some(out.store);
            *st.market.weekly_names.lock_ok() = names.clone();
            *st.market.weekly_ready.lock_ok() = true;
        } else {
            let msg = format!("周K可读率过低（{}/{total}，需 ≥ {min_ok}），已保留旧缓存",
                              out.ok);
            eprintln!("[weekly_k] {msg}");
            if let Some(o) = st.weekly_build.lock_ok().as_object_mut() {
                o.insert("error".into(), json!(msg));
            }
        }
        let secs = (t0.elapsed().as_millis() as f64) / 1000.0;
        if let Some(o) = st.weekly_build.lock_ok().as_object_mut() {
            o.insert("running".into(), json!(false));
            o.insert("done".into(), json!(total));
            o.insert("date".into(), json!(date));
            o.insert("secs".into(), json!((secs * 10.0).round() / 10.0));
            o.insert("src".into(), json!(format!("本地.day:{}/{}", out.ok, out.total)));
        }
        eprintln!("[weekly_k] 构建完成: {}/{} 只, 日期={date}, 落盘={saved}, 用时 {secs:.1}s",
                  out.ok, total);
    });
    if spawned.is_err() {
        if let Some(o) = state.weekly_build.lock_ok().as_object_mut() {
            o.insert("running".into(), json!(false));
        }
        return json!({"ok": false, "started": false, "error": "构建线程启动失败"});
    }
    json!({"ok": true, "started": true, "running": true, "total": total,
           "message": "已启动后台构建"})
}

/// 全市场分钟K缓存（默认 min30+min60）：本地 `.lc1` → N 分钟 → kcache v1。
pub fn build_min_cache(state: &Arc<WebState>, force: bool, periods: Option<Vec<String>>) -> Value {
    {
        let b = state.min_build.lock_ok();
        if b.get("running").and_then(|v| v.as_bool()).unwrap_or(false) {
            return json!({"ok": true, "started": false, "running": true,
                          "message": "构建进行中"});
        }
    }
    let plist: Vec<String> = match periods {
        Some(v) if !v.is_empty() => v.into_iter()
            .filter(|p| zg_market::consts::MIN_K_LMT.iter().any(|(k, _)| k == p))
            .collect(),
        _ => zg_market::consts::MIN_K_PERIODS.iter().map(|s| s.to_string()).collect(),
    };
    if plist.is_empty() {
        return json!({"ok": false, "started": false,
                      "error": "period 非法（支持 min30 / min60）"});
    }
    if !force && zg_core::calendar::market_open_now(None) {
        return json!({"ok": true, "started": false, "running": false,
                      "message": "交易时段不重建分钟K缓存（末根未走完），可用 force=1 强制重建"});
    }
    let (codes, names) = stock_universe(state);
    if codes.is_empty() {
        return json!({"ok": false, "started": false, "error": "股票列表为空，无法构建分钟K缓存"});
    }
    let total = codes.len();
    let gates: Vec<(String, usize, usize)> = plist.iter().map(|p| {
        let lmt = zg_market::consts::MIN_K_LMT.iter()
            .find(|(k, _)| k == p).map(|(_, v)| *v).unwrap_or(480);
        let old_len = state.market.min_cache.lock_ok()
            .get(p.as_str()).map(|s| s.len()).unwrap_or(0);
        (p.clone(), lmt, coverage_gate(old_len, total))
    }).collect();
    {
        let mut b = state.min_build.lock_ok();
        if let Some(o) = b.as_object_mut() {
            o.insert("running".into(), json!(true));
            o.insert("total".into(), json!(total));
            o.insert("done".into(), json!(0));
            o.insert("attempted".into(), json!(true));
            o.insert("error".into(), Value::Null);
            o.insert("period".into(), json!(gates[0].0));
        }
    }
    let st = Arc::clone(state);
    let spawned = std::thread::Builder::new().name("min-k-build".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let data = zg_core::paths::get().market_data;
        let mut built = 0usize;
        let mut ok_total = 0usize;
        let mut last_date = String::new();
        for (period, lmt, min_ok) in &gates {
            let prefix = data.join(format!("{period}_k_cache")).to_string_lossy().to_string();
            if let Some(o) = st.min_build.lock_ok().as_object_mut() {
                o.insert("period".into(), json!(period));
                o.insert("done".into(), json!(0));
            }
            let mut out = zg_market::kbuild::build_min(
                &prefix, &codes, &names, period, *lmt, |done| {
                    if done % 200 == 0 {
                        if let Some(o) = st.min_build.lock_ok().as_object_mut() {
                            o.insert("done".into(), json!(done));
                        }
                    }
                });
            let saved = out.ok >= *min_ok && out.ok > 0 && out.store.save(&prefix);
            if saved {
                built += 1;
                ok_total += out.ok;
                if out.date > last_date {
                    last_date = out.date.clone();
                }
                let date = out.date.clone();
                st.market.min_cache.lock_ok().insert(period.clone(), out.store);
                *st.market.min_names.lock_ok() = names.clone();
                *st.market.min_date.lock_ok() = last_date.clone();
                *st.market.min_ready.lock_ok() = true;
                // 与 Python `_mark_min_k` 同口径：落盘即记当前周期最后一个闭合边界
                if !date.is_empty() {
                    st.market.min_marks.lock_ok()
                        .insert(period.clone(), format!("{date} 15:00"));
                }
                eprintln!("[min_k] {period} 完成: {}/{total} 只, 日期={date}", out.ok);
            } else {
                let msg = format!("{period} 可读率过低（{}/{total}，需 ≥ {min_ok}），已保留旧缓存",
                                  out.ok);
                eprintln!("[min_k] {msg}");
                if let Some(o) = st.min_build.lock_ok().as_object_mut() {
                    o.insert("error".into(), json!(msg));
                }
            }
        }
        // marks 落盘（min_k_marks.json），与 Python 同文件同结构
        let marks: HashMap<String, String> = st.market.min_marks.lock_ok().clone();
        let marks_path = zg_core::paths::get().market_data.join("min_k_marks.json");
        if let Ok(body) = serde_json::to_string(&marks) {
            let tmp = zg_core::jsonio::tmp_name(&marks_path);
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, &marks_path);
            }
        }
        let secs = (t0.elapsed().as_millis() as f64) / 1000.0;
        if let Some(o) = st.min_build.lock_ok().as_object_mut() {
            o.insert("running".into(), json!(false));
            o.insert("done".into(), json!(total));
            o.insert("date".into(), json!(last_date));
            o.insert("secs".into(), json!((secs * 10.0).round() / 10.0));
            o.insert("src".into(), json!(format!("本地.lc1:{ok_total}/{total}×{built}")));
        }
        eprintln!("[min_k] 构建完成: {built} 个周期, 日期={last_date}, 用时 {secs:.1}s");
    });
    if spawned.is_err() {
        if let Some(o) = state.min_build.lock_ok().as_object_mut() {
            o.insert("running".into(), json!(false));
        }
        return json!({"ok": false, "started": false, "error": "构建线程启动失败"});
    }
    json!({"ok": true, "started": true, "running": true, "total": total,
           "periods": plist, "message": "已启动后台构建"})
}

pub fn industry_heat_request(state: &Arc<WebState>, parent: &str, order: &str, offline: bool) -> Value {
    init_industry(state);
    match heat_holder().lock_ok().clone() {
        Some(h) => h.request(parent, order, offline),
        None => json!({"error": "行业热度模块初始化失败", "rows": []}),
    }
}

/// 侧栏预载：128 个二级行业 + 全部成分股（约 580KB），一次取全。
pub fn industry_heat_all_request(state: &Arc<WebState>, order: &str, offline: bool) -> Value {
    init_industry(state);
    match heat_holder().lock_ok().clone() {
        Some(h) => h.request_all(order, offline),
        None => json!({"error": "行业热度模块初始化失败", "rows": [], "industries": [], "stocks": {}}),
    }
}

// ---------------------------------------------------------------- tdx-master 状态

pub fn tdx_backup_status(state: &WebState) -> Value {
    json!({
        "source": "tdx-master",
        "available": state.tdx_backup_enabled,
        "running": false,
        "cooldown_seconds": 0,
        "last_error": "",
        "last_success": Value::Null,
        "successes": 0,
    })
}

// ---------------------------------------------------------------- 股票列表刷新

/// `refresh_stock_list`：五源轮换刷新（同花顺→东财→Ashare→腾讯→新浪）。
///
/// 90s 内存缓存 + 双检锁（对齐 Python）；成功后写 `stocks.json` 并同步
/// `list_time` / `data_source`。`force=true` 跳缓存。
pub fn refresh_stock_list(state: &Arc<WebState>, force: bool) -> Result<Vec<Value>, String> {
    if !force {
        let rows = state.raw_rows.lock_ok();
        if !rows.is_empty() && now() - *state.raw_time.lock_ok() < 90.0 {
            return Ok(rows.clone());
        }
    }
    let _guard = state.list_lock.lock_ok();
    if !force {
        let rows = state.raw_rows.lock_ok();
        if !rows.is_empty() && now() - *state.raw_time.lock_ok() < 90.0 {
            return Ok(rows.clone());
        }
    }
    let (rows, src) = zg_market::stock_list::fetch_all()?;
    let stocks: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            let code = r.get("f12").and_then(|v| v.as_str())?;
            Some(json!({
                "code": code,
                "name": r.get("f14").and_then(|v| v.as_str()).unwrap_or(""),
            }))
        })
        .collect();
    if stocks.is_empty() {
        return Err("股票列表为空".into());
    }
    *state.raw_rows.lock_ok() = rows.clone();
    *state.raw_time.lock_ok() = now();
    *state.stock_list.lock_ok() = stocks.clone();
    *state.list_time.lock_ok() =
        Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
    *state.data_source.lock_ok() = Some(src.to_string());
    let path = zg_core::paths::get().market_data.join("stocks.json");
    if !zg_core::jsonio::write_json_atomic(&path, &Value::Array(stocks), false) {
        eprintln!("[stock_list] 落盘失败: {}", path.display());
    }
    eprintln!("[stock_list] 刷新完成: {} 只, 源={src}", rows.len());
    Ok(rows)
}

/// `bg_list`：启动刷新一次，失败 10s 后重试一次。
pub fn bg_list(state: Arc<WebState>) {
    if refresh_stock_list(&state, false).is_err() {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let _ = refresh_stock_list(&state, false);
    }
}
