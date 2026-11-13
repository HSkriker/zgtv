//! 外部 K 线源 —— 对齐 `modules/zgapp/kline_sources.py`。
//!
//! 本模块只放「取数源」：名称/HTTP 工具、源熔断（source_health）、
//! THS / 东财 / 腾讯 / datahub / TQ / 本地 zgcore 的 kline_* 实现。
//! 编排层（get_kline / get_kline_min / api_kline）留在 `market_data.rs`。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::TimeZone;
use serde_json::{json, Map, Value};

use crate::consts;
use crate::kbar::{py_float, KBar};
use crate::{tdx_backup, ths, tq};

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn secid(code: &str) -> String {
    if code.starts_with("6") || code.starts_with("9") || code.starts_with("5") {
        format!("1.{code}")
    } else {
        format!("0.{code}")
    }
}

/// pytdx 市场编号：1=上海 0=深圳。
pub fn market_of(code: &str) -> i32 {
    if code.starts_with("6") || code.starts_with("9") || code.starts_with("5") {
        1
    } else {
        0
    }
}

pub fn tencent_symbol(code: &str) -> String {
    if code.starts_with("6") || code.starts_with("9") || code.starts_with("5") {
        format!("sh{code}")
    } else {
        format!("sz{code}")
    }
}

// ---------------------------------------------------------------- 源熔断

#[derive(Clone, Copy, Debug)]
struct Health {
    fails: u32,
    blocked_until: f64,
}

static SOURCE_HEALTH: OnceLock<Mutex<HashMap<&'static str, Health>>> = OnceLock::new();

fn health() -> &'static Mutex<HashMap<&'static str, Health>> {
    SOURCE_HEALTH.get_or_init(|| {
        Mutex::new(["em", "em_kline", "tx", "ths"]
            .iter()
            .map(|k| (*k, Health { fails: 0, blocked_until: 0.0 }))
            .collect())
    })
}

pub(crate) fn _src_open(name: &str) -> bool {
    health()
        .lock_ok()
        .get(name)
        .map(|h| now() >= h.blocked_until)
        .unwrap_or(true)
}

pub(crate) fn _src_fail(name: &'static str) {
    let mut h = health().lock_ok();
    let e = h.entry(name).or_insert(Health { fails: 0, blocked_until: 0.0 });
    e.fails += 1;
    if e.fails >= 2 {
        e.blocked_until = now() + 600.0;
        e.fails = 0;
    }
}

pub(crate) fn _src_ok(name: &str) {
    if let Some(h) = health().lock_ok().get_mut(name) {
        h.fails = 0;
    }
}

fn _em_kline_circuit_check() -> Result<(), String> {
    if !_src_open("em_kline") {
        return Err("东财K线熔断中".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- 名称表

static THS_NAME_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();

/// stocks.json -> {code: name}（失败返回空表，同 Python try/except pass）。
pub fn ths_name_map() -> &'static HashMap<String, String> {
    THS_NAME_MAP.get_or_init(|| {
        let mut m = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(zg_core::paths::get().market_data.join("stocks.json")) {
            if let Ok(list) = serde_json::from_str::<Vec<Value>>(&text) {
                for it in list {
                    if let Some(code) = it.get("code").and_then(|c| c.as_str()) {
                        m.insert(code.to_string(), it.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string());
                    }
                }
            }
        }
        m
    })
}

// ---------------------------------------------------------------- 通用 HTTP

pub(crate) fn http_get(url: &str, timeout: u64, retries: u32) -> Result<String, String> {
    if let Some(t) = crate::net_stub::get(url) {
        return Ok(t);
    }
    zg_core::net::http_get(url, timeout, retries, None, None, None)
}

fn http_get_referer(url: &str, timeout: u64, retries: u32) -> Result<String, String> {
    if let Some(t) = crate::net_stub::get(url) {
        return Ok(t);
    }
    zg_core::net::http_get(
        url,
        timeout,
        retries,
        None,
        Some(&[("Referer", "https://quote.eastmoney.com/")]),
        None,
    )
}

pub(crate) fn http_get_raw(url: &str, timeout: u64, headers: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    if let Some(t) = crate::net_stub::get(url) {
        return Ok(t.into_bytes());
    }
    zg_core::net::read_url(url, Some(headers), timeout, 0, None, None)
}

// ---------------------------------------------------------------- THS

fn _ths_ms_of_date(beg: &str) -> i64 {
    let y: i32 = beg.get(0..4).and_then(|v| v.parse().ok()).unwrap_or(0);
    let m: u32 = beg.get(4..6).and_then(|v| v.parse().ok()).unwrap_or(1);
    let d: u32 = beg.get(6..8).and_then(|v| v.parse().ok()).unwrap_or(1);
    let dt = chrono::NaiveDate::from_ymd_opt(y, m, d)
        .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    chrono::NaiveDateTime::new(dt, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        .and_utc()
        .timestamp()
        * 1000
}

fn _ths_date_str(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn kline_ths(code: &str, lmt: usize, beg: Option<&str>) -> Result<(Option<String>, Vec<KBar>), String> {
    if !consts::ths_available() {
        return Err("同花顺源不可用".into());
    }
    let now_ms = now() as i64 * 1000;
    let start_ms = match beg {
        Some(b) => _ths_ms_of_date(b),
        None => now_ms - ((lmt as f64 * 1.5 + 30.0) * 86400.0 * 1000.0) as i64,
    };
    let params = vec![
        ("thscode".to_string(), ths::to_thscode(code)),
        ("interval".to_string(), "1d".to_string()),
        ("start".to_string(), start_ms.to_string()),
        ("end".to_string(), now_ms.to_string()),
        ("adjust".to_string(), "forward".to_string()),
    ];
    let d = ths::ths_get("/api/a-share/prices/historical", &params, 20, 5)?;
    let mut bars: Vec<KBar> = Vec::new();
    for it in (d.get("item").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
        let date = match it.get("date_ms").and_then(|x| x.as_i64()) {
            Some(ms) => _ths_date_str(ms),
            None => continue,
        };
        let open = match py_float(it.get("open_price")) { Ok(v) => v, Err(_) => continue };
        let close = match py_float(it.get("close_price")) { Ok(v) => v, Err(_) => continue };
        let high = match py_float(it.get("high_price")) { Ok(v) => v, Err(_) => continue };
        let low = match py_float(it.get("low_price")) { Ok(v) => v, Err(_) => continue };
        let vol = match py_float(it.get("volume")) { Ok(v) => v, Err(_) => continue };
        let amount = match it.get("turnover") {
            Some(v) if !v.is_null() => py_float(Some(v)).unwrap_or(0.0),
            _ => 0.0,
        };
        bars.push(KBar { date, open, high, low, close, vol: vol / 100.0, amount });
    }
    if bars.is_empty() {
        return Err("同花顺K线为空".into());
    }
    if lmt > 0 && bars.len() > lmt && beg.is_none() {
        bars = bars[bars.len() - lmt..].to_vec();
    }
    Ok((ths_name_map().get(code).cloned(), bars))
}

// ---------------------------------------------------------------- 东财

pub fn kline_em(code: &str, klt: i64, lmt: usize, beg: Option<&str>)
    -> Result<(Option<String>, Vec<KBar>), String> {
    _em_kline_circuit_check()?;
    let rng = match beg {
        Some(b) => format!("beg={b}&end=20500101"),
        None => format!("lmt={lmt}&end=20500101"),
    };
    let url = format!(
        "https://push2his.eastmoney.com/api/qt/stock/kline/get?\
secid={}&klt={}&fqt=1&{}&fields1=f1,f2,f3,f4,f5,f6\
&fields2=f51,f52,f53,f54,f55,f56,f57,f58,f59,f60,f61",
        secid(code), klt, rng
    );
    let text = match http_get_referer(&url, 6, 1) {
        Ok(t) => t,
        Err(e) => {
            _src_fail("em_kline");
            return Err(e);
        }
    };
    let j: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            _src_fail("em_kline");
            return Err(format!("东财K线响应解析失败: {e}"));
        }
    };
    let d = j.get("data").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let mut bars: Vec<KBar> = Vec::new();
    for line in (d.get("klines").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
        let s = line.as_str().unwrap_or("");
        let p: Vec<&str> = s.split(',').collect();
        if p.len() < 7 {
            continue;
        }
        let open = match p[1].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        let close = match p[2].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        let high = match p[3].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        let low = match p[4].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        let vol = match p[5].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        let amount = match p[6].parse::<f64>() { Ok(v) => v, Err(_) => continue };
        bars.push(KBar { date: p[0].to_string(), open, close, high, low, vol, amount });
    }
    if bars.is_empty() {
        // 空 K 线是「这只票没数据」（新股/停牌/退市），不是数据源故障：
        // 计一次熔断的话，两只坏票就能把东财源封掉 10 分钟、整个市场取不到 K 线。
        return Err("东财K线为空".into());
    }
    _src_ok("em_kline");
    Ok((d.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()), bars))
}

// ---------------------------------------------------------------- 腾讯

pub fn kline_tencent(code: &str, period: &str, count: usize)
    -> Result<(Option<String>, Vec<KBar>), String> {
    kline_tencent_symbol(&tencent_symbol(code), period, count)
}

/// 显式腾讯代码（sh000300/881333.SH 不可用）版本，供指数/板块 K 线复用。
pub(crate) fn kline_tencent_symbol(sym: &str, period: &str, count: usize)
    -> Result<(Option<String>, Vec<KBar>), String> {
    let url = format!(
        "https://ifzq.gtimg.cn/appstock/app/fqkline/get?param={sym},{period},,,{count},qfq"
    );
    let text = http_get(&url, 6, 0)?;
    let j: Value = serde_json::from_str(&text).map_err(|e| format!("腾讯K线响应解析失败: {e}"))?;
    let d = j.get("data").and_then(|x| x.get(sym)).cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let qfq_key = format!("qfq{period}");
    let rows = d.get(&qfq_key).cloned()
        .or_else(|| d.get(period).cloned())
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let mut bars: Vec<KBar> = Vec::new();
    for x in (rows.as_array().cloned()).unwrap_or_default() {
        let arr = x.as_array().cloned().unwrap_or_default();
        if arr.len() < 6 {
            continue;
        }
        let open = match arr[1].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let close = match arr[2].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let high = match arr[3].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let low = match arr[4].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let vol = match arr[5].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        bars.push(KBar {
            date: arr[0].as_str().unwrap_or("").to_string(),
            open, close, high, low, vol, amount: 0.0,
        });
    }
    let mut name = None;
    let qt = d.get("qt").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(qt_sym) = qt.get(sym) {
        if let Some(items) = qt_sym.as_array() {
            if items.len() > 1 {
                name = items[1].as_str().map(|s| s.to_string());
            }
        }
    }
    if bars.is_empty() {
        return Err("腾讯K线为空".into());
    }
    Ok((name, bars))
}

pub fn kline_tencent_min(code: &str, period: &str, count: usize)
    -> Result<(Option<String>, Vec<KBar>), String> {
    let bars = kline_tencent_min_symbol(&tencent_symbol(code), period, count)?;
    Ok((ths_name_map().get(code).cloned(), bars))
}

/// 显式腾讯代码版本，供指数分钟 K 线复用。
pub(crate) fn kline_tencent_min_symbol(sym: &str, period: &str, count: usize)
    -> Result<Vec<KBar>, String> {
    let key = format!("m{}", consts::MIN_PITCH.iter().find(|(p, _)| *p == period)
        .map(|(_, v)| *v).unwrap_or(0));
    let mut got: HashMap<String, KBar> = HashMap::new();
    let mut end = String::new();
    let mut guard = 0usize;
    // 分页上限自适应（每页至多 800 根，需 ceil(count/800)+3 页余量）
    let guard_max = {
        let pages = (count as f64 / 800.0).ceil() as i64;
        (pages + 3).max(8)
    };
    while got.len() < count && guard < guard_max as usize {
        guard += 1;
        let n = (800usize).min(count - got.len());
        let url = format!("http://ifzq.gtimg.cn/appstock/app/kline/mkline?param={sym},{key},{end},{n}");
        let text = http_get(&url, 8, 0)?;
        let j: Value = serde_json::from_str(&text).map_err(|e| format!("腾讯分钟K线响应解析失败: {e}"))?;
        let rows = j.get("data").and_then(|x| x.get(sym)).and_then(|x| x.get(&key))
            .and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        for x in &rows {
            let arr = x.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                continue;
            }
            let t = arr[0].as_str().unwrap_or("");
            if t.len() < 12 {
                continue;
            }
            let s = format!("{}-{}-{} {}:{}", &t[0..4], &t[4..6], &t[6..8], &t[8..10], &t[10..12]);
            let open = match arr.get(1).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let close = match arr.get(2).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let high = match arr.get(3).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let low = match arr.get(4).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => v,
                None => continue,
            };
            let vol = match arr.get(5).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => v,
                None => continue,
            };
            got.insert(s.clone(), KBar { date: s, open, close, high, low, vol, amount: 0.0 });
        }
        if rows.len() < n {
            break;
        }
        let nxt: String = rows[0].as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(12)
            .collect();
        if nxt == end {
            break;
        }
        end = nxt;
    }
    let mut bars: Vec<KBar> = got.into_values().collect();
    bars.sort_by(|a, b| a.date.cmp(&b.date));
    if count > 0 && bars.len() > count {
        bars = bars[bars.len() - count..].to_vec();
    }
    if bars.is_empty() {
        return Err("腾讯分钟K线为空".into());
    }
    Ok(bars)
}

// ---------------------------------------------------------------- datahub

const DATAHUB_KEY: &str = "dba548a206a453c197f9175189b757374fa6db9554bb29e69efea127";
const DATAHUB_BASE: &str = "http://datahubco.com/app-api/openapi/v1/tushare/";

pub fn kline_datahub(code: &str, lmt: usize) -> Result<(Option<String>, Vec<KBar>), String> {
    let suffix = if code.starts_with("6") || code.starts_with("9") || code.starts_with("5") { "SH" } else { "SZ" };
    let sym = format!("{code}.{suffix}");
    let url = format!("{DATAHUB_BASE}daily?ts_code={sym}&limit={lmt}");
    let raw = http_get_raw(&url, 15, &[("X-API-Key", DATAHUB_KEY)])?;
    let j: Value = serde_json::from_slice(&raw).map_err(|e| format!("datahub 响应解析失败: {e}"))?;
    if j.get("code").and_then(|c| c.as_i64()).unwrap_or(-1) != 0 {
        return Err(format!("datahub daily 失败: {}", j.get("msg").and_then(|m| m.as_str()).unwrap_or("")));
    }
    let d = j.get("data").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let fields: Vec<&str> = d.get("fields")
        .and_then(|f| f.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut bars: Vec<KBar> = Vec::new();
    for item in (d.get("items").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
        let arr = item.as_array().cloned().unwrap_or_default();
        // Python `rec.get(name, 0)`：字段缺失 → 0（保留该条）；值为 None → TypeError → 跳过
        let get = |name: &str| -> Option<&Value> {
            fields.iter().position(|f| *f == name).and_then(|i| arr.get(i))
        };
        let td = get("trade_date").and_then(|v| v.as_str()).unwrap_or("");
        if td.len() < 8 {
            continue;
        }
        let mut ok = true;
        let mut f = |v: Option<&Value>| -> f64 {
            match v {
                None => 0.0,
                Some(Value::Null) => { ok = false; 0.0 }
                Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                Some(Value::String(s)) => match s.trim().parse::<f64>() {
                    Ok(x) => x,
                    Err(_) => { ok = false; 0.0 }
                },
                Some(_) => { ok = false; 0.0 }
            }
        };
        let open = f(get("open"));
        let close = f(get("close"));
        let high = f(get("high"));
        let low = f(get("low"));
        let vol = f(get("vol"));
        // datahub 沿用 tushare-daily 口径：vol 单位「手」、amount 单位「千元」。
        // 反算验证（600519.SH 20261027）：amount 5226798.679 千元 / (vol 38669.14 手 × 100 股)
        // ≈ 1351.6 元/股，与收盘 1355.9 同量级；若按元读则仅 1.35 元/股，明显不成立。
        let amount = f(get("amount")) * 1000.0;
        if !ok {
            continue;
        }
        bars.push(KBar {
            date: format!("{}-{}-{}", &td[0..4], &td[4..6], &td[6..8]),
            open, close, high, low, vol, amount,
        });
    }
    if bars.is_empty() {
        return Err("datahub daily 为空".into());
    }
    bars.reverse();
    Ok((None, bars))
}

// ---------------------------------------------------------------- TQ 层

static TQ_DATE: Mutex<(String, f64)> = Mutex::new((String::new(), 0.0));

/// 本地新鲜度参照日：优先用本地指数日线的最近交易日（本地优先，TQ 不可用时不会
/// 把已是最新的本地数据误判为过期），本地指数缺失才回退 TQ，再退日历（M32）。
pub fn local_reference_day() -> String {
    // 收盘后以日历最近交易日为参照：本地 .day 尚未回写时不能把 T-1 当最新
    let now = chrono::Local::now();
    if now.format("%H:%M").to_string().as_str() >= "15:05" {
        return zg_core::calendar::last_trading_date(Some(now));
    }
    if let Some(d) = zg_core::calendar::market_last_day() {
        return d;
    }
    let d = tq_trading_date_cached();
    if d.is_empty() { zg_core::calendar::last_trading_date(None) } else { d }
}

/// TQ 最近交易日（缓存 60s）。
pub fn tq_trading_date_cached() -> String {
    {
        let g = TQ_DATE.lock_ok();
        if !g.0.is_empty() && now() - g.1 < 60.0 {
            return g.0.clone();
        }
    }
    let mut d = String::new();
    if let Ok(res) = tq::post(
        "get_market_data",
        &json!({"stock_list": ["999999.SH"], "period": "1d", "count": 1,
                "dividend_type": "none"}),
        8,
    ) {
        let arr = res.get("Value").and_then(|x| x.get("999999.SH")).cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let dates = arr.get("Date").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if let Some(last) = dates.last().and_then(|x| x.as_str()) {
            let s: String = last.chars().filter(|c| *c != '-').collect();
            if s.len() >= 8 {
                d = format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]);
            }
        }
    }
    *TQ_DATE.lock_ok() = (d.clone(), now());
    d
}

fn _tq_min_date(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| *c != '-').collect();
    if s.len() < 12 {
        return None;
    }
    Some(format!("{}-{}-{} {}:{}", &s[0..4], &s[4..6], &s[6..8], &s[8..10], &s[10..12]))
}

pub fn kline_tq_min(code: &str, period: &str, count: usize)
    -> Result<(Option<String>, Vec<KBar>), String> {
    let sym = tq::tq_symbol(code);
    let pitch = consts::TQ_PITCH.iter().find(|(p, _)| *p == period)
        .map(|(_, v)| *v).unwrap_or("1m");
    let req_count = if count > 0 { count } else { 800 }; // Python `int(count or 800)`
    let res = tq::post(
        "get_market_data",
        &json!({"stock_list": [sym], "period": pitch, "count": req_count,
                "dividend_type": "front"}),
        10,
    )?;
    let arr = res.get("Value").and_then(|x| x.get(&sym)).cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut got: HashMap<String, KBar> = HashMap::new();
    let cols = if let Some(o) = arr.as_object() {
        if o.get("Date").is_some() { Some(o.clone()) } else { None }
    } else {
        None
    };
    let rows = arr.get("Value").and_then(|x| x.as_array()).cloned();
    if let Some(cols) = cols {
        let keys: Vec<&str> = cols.get("Date")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for (i, ds) in keys.iter().enumerate() {
            let key = match _tq_min_date(ds) {
                Some(k) => k,
                None => continue,
            };
            // TQ 数组元素可能是字符串（Python `float()` 接受），统一走 py_float
            let at = |k: &str| -> f64 {
                cols.get(k).and_then(|x| x.as_array())
                    .and_then(|a| a.get(i))
                    .and_then(|v| py_float(Some(v)).ok())
                    .unwrap_or(0.0)
            };
            let open = at("Open");
            let close = at("Close");
            let high = at("High");
            let low = at("Low");
            // Python: (cols.get("Volume") or [0]*999)[i] —— 缺列按 0
            let vol = cols.get("Volume").and_then(|x| x.as_array())
                .and_then(|a| a.get(i)).map(|v| py_float(Some(v)).unwrap_or(0.0)).unwrap_or(0.0) / 100.0;
            let amount = cols.get("Amount").and_then(|x| x.as_array())
                .and_then(|a| a.get(i)).map(|v| py_float(Some(v)).unwrap_or(0.0)).unwrap_or(0.0) * 10000.0;
            got.insert(key.clone(), KBar { date: key, open, close, high, low, vol, amount });
        }
    } else if let Some(rows) = rows {
        for x in &rows {
            let arr_v = x.as_array().cloned().unwrap_or_default();
            if arr_v.is_empty() {
                continue;
            }
            let ds = arr_v[0].as_str().unwrap_or("");
            let key = match _tq_min_date(ds) {
                Some(k) => k,
                None => continue,
            };
            let f = |i: usize| arr_v.get(i).and_then(|v| py_float(Some(v)).ok()).unwrap_or(0.0);
            let vol = f(5) / 100.0;
            let amount = if arr_v.len() > 6 { f(6) } else { 0.0 };
            got.insert(key.clone(), KBar {
                date: key, open: f(1), close: f(2), high: f(3), low: f(4), vol, amount,
            });
        }
    }
    let mut bars: Vec<KBar> = got.into_values().collect();
    bars.sort_by(|a, b| a.date.cmp(&b.date));
    if count > 0 && bars.len() > count {
        bars = bars[bars.len() - count..].to_vec();
    }
    if bars.is_empty() {
        return Err("TQ 分钟K线为空".into());
    }
    Ok((ths_name_map().get(code).cloned(), bars))
}

pub fn kline_tq_day(code: &str, period: &str, lmt: usize, beg: Option<&str>)
    -> Result<(Option<String>, Vec<KBar>), String> {
    let sym = tq::tq_symbol(code);
    let want = if lmt > 0 { lmt } else { 300 };
    let count = want * (if period != "day" { 6 } else { 1 }) + 40;
    let res = tq::post(
        "get_market_data",
        &json!({"stock_list": [sym], "period": "1d", "count": count,
                "dividend_type": "front"}),
        10,
    )?;
    let arr = res.get("Value").and_then(|x| x.get(&sym)).cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let cols = match arr.as_object() {
        Some(o) if o.get("Date").is_some() => o.clone(),
        _ => return Err("TQ 日线结构异常".into()),
    };
    let dates: Vec<&str> = cols.get("Date")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let len = dates.len();
    let zero = || Value::from(0.0);
    let vols = cols.get("Volume").and_then(|x| x.as_array()).cloned()
        .unwrap_or_else(|| vec![zero(); len]);
    let amts = cols.get("Amount").and_then(|x| x.as_array()).cloned()
        .unwrap_or_else(|| vec![zero(); len]);
    let mut got: HashMap<String, KBar> = HashMap::new();
    for (i, ds) in dates.iter().enumerate() {
        let s: String = ds.chars().filter(|c| *c != '-').collect();
        if s.len() < 8 {
            continue;
        }
        let key = format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]);
        let at = |k: &str| -> f64 {
            cols.get(k).and_then(|x| x.as_array())
                .and_then(|a| a.get(i))
                .and_then(|v| py_float(Some(v)).ok())
                .unwrap_or(0.0)
        };
        let vol = vols.get(i).and_then(|v| py_float(Some(v)).ok()).unwrap_or(0.0) / 100.0;
        let amount = amts.get(i).and_then(|v| py_float(Some(v)).ok()).unwrap_or(0.0) * 10000.0;
        got.insert(key.clone(), KBar {
            date: key, open: at("Open"), close: at("Close"), high: at("High"),
            low: at("Low"), vol, amount,
        });
    }
    let mut bars: Vec<KBar> = got.into_values().collect();
    bars.sort_by(|a, b| a.date.cmp(&b.date));
    if let Some(b) = beg {
        let bstr = format!("{}-{}-{}", &b[0..4], &b[4..6], &b[6..8]);
        bars.retain(|x| x.date >= bstr);
    }
    if period != "day" && !bars.is_empty() {
        // Python `resample_dwm(bars, period)`：周一起始分组/末根标签/OHLC 聚合，
        // 与 market_data::synth_wm_from_daily 同一口径
        bars = crate::market_data::synth_wm_from_daily(&bars, period);
    }
    if want > 0 && bars.len() > want {
        bars = bars[bars.len() - want..].to_vec();
    }
    if bars.is_empty() {
        return Err("TQ 日线为空".into());
    }
    Ok((ths_name_map().get(code).cloned(), bars))
}

// ---------------------------------------------------------------- 本地层

pub fn kline_local_dwm(code: &str, period: &str, lmt: usize, beg: Option<&str>,
                       end: Option<&str>) -> Result<(Option<String>, Vec<KBar>), String> {
    let bars = zg_core::api::chart_bars(code, period, 0, beg, end)
        .map_err(|e| e.to_string())?;
    if bars.is_empty() {
        return Err("本地日/周/月为空".into());
    }
    let expected = local_reference_day();
    let last = bars.last().map(|b| b.date.as_str()).unwrap_or("");
    // 查历史窗口（end 早于最近交易日）时末根天然不是最新交易日，不能因此判「本地未更新」回落远端
    let end_historical = end
        .map(|e| e.replace('-', ""))
        .map(|e| !e.is_empty() && e < expected.replace('-', ""))
        .unwrap_or(false);
    if !end_historical && last < expected.as_str() {
        return Err(format!("本地日线未更新: 末根 {last} < 最近交易日 {expected}"));
    }
    let mut out: Vec<KBar> = bars
        .iter()
        .map(|b| KBar {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol,
            amount: b.amount,
        })
        .collect();
    if lmt > 0 && out.len() > lmt {
        out = out[out.len() - lmt..].to_vec();
    }
    Ok((ths_name_map().get(code).cloned(), out))
}

pub fn kline_tdx_master(code: &str, period: &str, lmt: usize, beg: Option<&str>)
    -> Result<(Option<String>, Vec<KBar>), String> {
    let mut bars = tdx_backup::kline(code, period, if lmt > 0 { lmt } else { 300 })?;
    if let Some(b) = beg {
        let digits: String = b.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 8 {
            return Err(format!("beg 需为 YYYYMMDD：{b:?}"));
        }
        let bstr = format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8]);
        bars.retain(|x| x.date.chars().take(10).cmp(bstr.chars()).is_ge());
    }
    if bars.is_empty() {
        return Err("tdx-master backup has no bars in requested range".into());
    }
    Ok((ths_name_map().get(code).cloned(), crate::market_data::normalize_minute_bars(&bars, period)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_helpers() {
        assert_eq!(secid("600519"), "1.600519");
        assert_eq!(secid("000001"), "0.000001");
        assert_eq!(secid("920001"), "1.920001"); // Python secid：9 开头 → 1.
        assert_eq!(market_of("600519"), 1);
        assert_eq!(market_of("000001"), 0);
        assert_eq!(tencent_symbol("600519"), "sh600519");
        assert_eq!(tencent_symbol("000001"), "sz000001");
    }

    #[test]
    fn circuit_breaker() {
        _src_ok("em_kline");
        _src_fail("em_kline");
        _src_fail("em_kline");
        assert!(_em_kline_circuit_check().is_err());
        // 熔断 600s，直接改 blocked_until 模拟超时后恢复
        health().lock_ok().get_mut("em_kline").unwrap().blocked_until = 0.0;
        assert!(_em_kline_circuit_check().is_ok());
    }

    #[test]
    fn tq_min_date_format() {
        assert_eq!(_tq_min_date("202610271400"), Some("2026-10-27 14:00".into()));
        assert_eq!(_tq_min_date("20261027"), None);
    }
}
