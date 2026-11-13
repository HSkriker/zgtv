//! 市场主要指数快照 —— 侧栏「主要指数」面板数据源。
//!
//! 取数顺序与其它模块一致：本地 .day（末根 >= 最近交易日）→ TQ → 腾讯 → 东财。
//! 指数代码的市场归属必须显式给出：000001 是上证指数（sh），不是平安银行（sz）；
//! 中证系列（932000/930050）在 TQ 用 .CSI，在腾讯不存在，在东财是 2.xxxxxx。

use zg_core::sync::MutexExt;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::kbar::{py_float, KBar};
use crate::{kline, tq};

#[derive(Clone, Copy, Debug)]
pub struct IndexDef {
    pub code: &'static str,
    pub name: &'static str,
    /// 本地 vipdoc 市场；中证系列无本地日线 → ""
    pub market: &'static str,
    pub tq: &'static str,
    /// 腾讯代码；中证系列腾讯无行情 → None
    pub tencent: Option<&'static str>,
    /// 东财 secid
    pub em: &'static str,
    /// 东财 datacenter `RPT_INDEX_TS_COMPONENT.TYPE`；交易所指数没有成分列表 → None
    pub members_type: Option<&'static str>,
}

pub const INDICES: &[IndexDef] = &[
    IndexDef { code: "000001", name: "上证指数", market: "sh", tq: "000001.SH", tencent: Some("sh000001"), em: "1.000001", members_type: None },
    IndexDef { code: "399001", name: "深成指", market: "sz", tq: "399001.SZ", tencent: Some("sz399001"), em: "0.399001", members_type: None },
    IndexDef { code: "399006", name: "创业板指数", market: "sz", tq: "399006.SZ", tencent: Some("sz399006"), em: "0.399006", members_type: None },
    IndexDef { code: "000688", name: "科创板指数", market: "sh", tq: "000688.SH", tencent: Some("sh000688"), em: "1.000688", members_type: None },
    IndexDef { code: "000300", name: "沪深300", market: "sh", tq: "000300.SH", tencent: Some("sh000300"), em: "1.000300", members_type: Some("1") },
    IndexDef { code: "000905", name: "中证500", market: "sh", tq: "000905.SH", tencent: Some("sh000905"), em: "1.000905", members_type: Some("3") },
    IndexDef { code: "000852", name: "中证1000", market: "sh", tq: "000852.SH", tencent: Some("sh000852"), em: "1.000852", members_type: Some("7") },
    IndexDef { code: "932000", name: "中证2000", market: "", tq: "932000.CSI", tencent: None, em: "2.932000", members_type: Some("13") },
    IndexDef { code: "000016", name: "上证50", market: "sh", tq: "000016.SH", tencent: Some("sh000016"), em: "1.000016", members_type: Some("2") },
    IndexDef { code: "930050", name: "中证A50", market: "", tq: "930050.CSI", tencent: None, em: "2.930050", members_type: Some("5") },
    IndexDef { code: "000510", name: "中证A500", market: "sh", tq: "000510.SH", tencent: Some("sh000510"), em: "1.000510", members_type: Some("6") },
];

#[derive(Clone, Debug)]
struct Quote {
    price: f64,
    /// None = 昨收不可信（TQ 缓存断档 / 只有 1 根）
    pre_close: Option<f64>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    date: String,
    time: Option<String>,
    source: &'static str,
}

fn num_of(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| py_float(Some(x)).ok()).filter(|x| x.is_finite())
}

fn ymd(d: &str) -> Option<chrono::NaiveDate> {
    let s: String = d.chars().filter(|c| *c != '-').collect();
    if s.len() < 8 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(
        s[0..4].parse().ok()?,
        s[4..6].parse().ok()?,
        s[6..8].parse().ok()?,
    )
}

fn norm_date(d: &str) -> String {
    let s: String = d.chars().filter(|c| *c != '-').collect();
    if s.len() >= 8 {
        format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8])
    } else {
        d.to_string()
    }
}

/// TQ 相邻两根间隔超过 10 天（含长假）即视为缓存断档，昨收不可信。
fn adjacent(prev: &str, last: &str) -> bool {
    match (ymd(prev), ymd(last)) {
        (Some(a), Some(b)) => (b - a).num_days() <= 10,
        _ => false,
    }
}

// ---------------------------------------------------------------- 本地

fn local_quote(d: &IndexDef) -> Option<Quote> {
    if d.market.is_empty() {
        return None;
    }
    let bars = zg_core::api::read_day_bars(d.market, d.code).ok()?;
    let last = bars.last()?;
    let expected = zg_core::calendar::last_trading_date(None);
    if last.date.as_str() < expected.as_str() {
        return None;
    }
    let prev = bars.iter().rev().nth(1)?;
    Some(Quote {
        price: last.close,
        pre_close: Some(prev.close),
        open: Some(last.open),
        high: Some(last.high),
        low: Some(last.low),
        date: last.date.clone(),
        time: None,
        source: "local",
    })
}

// ---------------------------------------------------------------- TQ

fn tq_quotes(defs: &[&IndexDef], offline: bool) -> HashMap<String, Quote> {
    let mut out = HashMap::new();
    if offline || defs.is_empty() || !tq::available() {
        return out;
    }
    let symbols: Vec<&str> = defs.iter().map(|d| d.tq).collect();
    let res = match tq::post(
        "get_market_data",
        &json!({"stock_list": symbols, "period": "1d", "count": 2,
                "dividend_type": "none"}),
        8,
    ) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let value = res.get("Value").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let today = chrono::Local::now().date_naive();
    for d in defs {
        let arr = value.get(d.tq).cloned().unwrap_or(Value::Null);
        let o = match arr.as_object() {
            Some(o) => o,
            None => continue,
        };
        if o.get("ErrorId").and_then(|x| x.as_str()).unwrap_or("0") != "0" {
            continue;
        }
        let dates: Vec<String> = o.get("Date").and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let closes: Vec<f64> = o.get("Close").and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|v| py_float(Some(v)).ok()).collect())
            .unwrap_or_default();
        let Some(last_date) = dates.last() else { continue };
        let Some(&price) = closes.last() else { continue };
        if price <= 0.0 {
            continue;
        }
        // 停止更新超过 15 天的 TQ 数据视为陈旧
        if let Some(dt) = ymd(last_date) {
            if (today - dt).num_days() > 15 {
                continue;
            }
        }
        let pre_close = if dates.len() >= 2 && closes.len() >= 2
            && adjacent(&dates[dates.len() - 2], last_date)
        {
            Some(closes[closes.len() - 2])
        } else {
            None
        };
        let idx = closes.len() - 1;
        let at = |k: &str| -> Option<f64> {
            o.get(k).and_then(|x| x.as_array()).and_then(|a| a.get(idx)).and_then(|v| num_of(Some(v)))
        };
        out.insert(d.code.to_string(), Quote {
            price,
            pre_close,
            open: at("Open"),
            high: at("High"),
            low: at("Low"),
            date: norm_date(last_date),
            time: Some(chrono::Local::now().format("%H:%M:%S").to_string()),
            source: "tq",
        });
    }
    out
}

// ---------------------------------------------------------------- 腾讯

fn tencent_quotes(defs: &[&IndexDef], offline: bool) -> HashMap<String, Quote> {
    let mut out = HashMap::new();
    let mut want: HashMap<&str, &IndexDef> = HashMap::new();
    let mut syms = Vec::new();
    for d in defs {
        if let Some(s) = d.tencent {
            want.insert(d.code, d);
            syms.push(s);
        }
    }
    if offline || syms.is_empty() {
        return out;
    }
    let url = format!("https://qt.gtimg.cn/q={}", syms.join(","));
    let raw = match kline::http_get_raw(&url, 8, &[]) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let txt = crate::quotes::decode_gbk_lossy(&raw);
    for seg in txt.split(';') {
        if !seg.contains('"') {
            continue;
        }
        let f: Vec<&str> = seg.split('"').nth(1).unwrap_or("").split('~').collect();
        if f.len() < 38 {
            continue;
        }
        let Some(d) = want.get(f[2]) else { continue };
        let price = f[3].parse::<f64>().unwrap_or(0.0);
        let pre = f[4].parse::<f64>().unwrap_or(0.0);
        if price <= 0.0 || pre <= 0.0 {
            continue;
        }
        let ts = f[30];
        let (date, time) = if ts.len() >= 14 {
            (format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8]),
             Some(format!("{}:{}:{}", &ts[8..10], &ts[10..12], &ts[12..14])))
        } else {
            (String::new(), None)
        };
        let n = |s: &str| s.parse::<f64>().ok().filter(|x| x.is_finite());
        out.insert(d.code.to_string(), Quote {
            price,
            pre_close: Some(pre),
            open: n(f[5]),
            high: n(f[33]),
            low: n(f[34]),
            date,
            time,
            source: "tencent",
        });
    }
    out
}

// ---------------------------------------------------------------- 东财

fn em_quote(d: &IndexDef, offline: bool) -> Option<Quote> {
    if offline || !kline::_src_open("em") {
        return None;
    }
    let url = format!(
        "https://push2delay.eastmoney.com/api/qt/stock/get?secid={}&invt=2&fltt=2\
&fields=f43,f44,f45,f46,f58,f60,f86,f170",
        d.em
    );
    let text = if let Some(t) = crate::net_stub::get(&url) {
        t
    } else {
        match zg_core::net::http_get(
            &url, 8, 2, None,
            Some(&[("Referer", "https://quote.eastmoney.com/")]), None,
        ) {
            Ok(t) => t,
            Err(_) => return None,
        }
    };
    let j: Value = serde_json::from_str(&text).ok()?;
    let data = j.get("data").filter(|v| !v.is_null())?;
    let price = num_of(data.get("f43"))?;
    let pre = num_of(data.get("f60"))?;
    if price <= 0.0 || pre <= 0.0 {
        return None;
    }
    use chrono::TimeZone;
    let ts = data.get("f86").and_then(|x| x.as_i64())
        .and_then(|t| chrono::Local.timestamp_opt(t, 0).single());
    let date = ts.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let time = ts.map(|t| t.format("%H:%M:%S").to_string());
    kline::_src_ok("em");
    Some(Quote {
        price,
        pre_close: Some(pre),
        open: num_of(data.get("f46")),
        high: num_of(data.get("f44")),
        low: num_of(data.get("f45")),
        date,
        time,
        source: "em",
    })
}

// ---------------------------------------------------------------- 编排

fn item_json(d: &IndexDef, q: Option<&Quote>) -> Value {
    let mut m = Map::new();
    m.insert("code".into(), Value::String(d.code.into()));
    m.insert("name".into(), Value::String(d.name.into()));
    m.insert("members".into(), Value::Bool(d.members_type.is_some()));
    match q {
        Some(q) => {
            let pct = q.pre_close.filter(|p| *p > 0.0)
                .map(|p| zg_core::model::py_round((q.price / p - 1.0) * 100.0, 2));
            m.insert("price".into(), json!(q.price));
            m.insert("preClose".into(), q.pre_close.map(Value::from).unwrap_or(Value::Null));
            m.insert("pct".into(), pct.map(Value::from).unwrap_or(Value::Null));
            m.insert("open".into(), q.open.map(Value::from).unwrap_or(Value::Null));
            m.insert("high".into(), q.high.map(Value::from).unwrap_or(Value::Null));
            m.insert("low".into(), q.low.map(Value::from).unwrap_or(Value::Null));
            m.insert("date".into(), Value::String(q.date.clone()));
            m.insert("time".into(), q.time.clone().map(Value::String).unwrap_or(Value::Null));
            m.insert("source".into(), Value::String(q.source.into()));
            if pct.is_none() {
                m.insert("error".into(), Value::String("昨收不可用".into()));
            }
        }
        None => {
            for k in ["price", "preClose", "pct", "open", "high", "low", "time", "source"] {
                m.insert(k.into(), Value::Null);
            }
            m.insert("date".into(), Value::String(String::new()));
            m.insert("error".into(), Value::String("所有指数源均失败".into()));
        }
    }
    Value::Object(m)
}

fn build(offline: bool) -> Value {
    let mut got: HashMap<String, Quote> = HashMap::new();
    let mut pending: Vec<&IndexDef> = Vec::new();

    for d in INDICES {
        match local_quote(d) {
            Some(q) => { got.insert(d.code.to_string(), q); }
            None => pending.push(d),
        }
    }

    let tq = tq_quotes(&pending, offline);
    let mut partial: HashMap<String, Quote> = HashMap::new();
    let mut next: Vec<&IndexDef> = Vec::new();
    for d in pending {
        match tq.get(d.code).cloned() {
            Some(q) if q.pre_close.is_some() => { got.insert(d.code.to_string(), q); }
            Some(q) => { partial.insert(d.code.to_string(), q); next.push(d); }
            None => next.push(d),
        }
    }

    let tx = tencent_quotes(&next, offline);
    let mut next2: Vec<&IndexDef> = Vec::new();
    for d in next {
        match tx.get(d.code).cloned() {
            Some(q) => { got.insert(d.code.to_string(), q); }
            None => next2.push(d),
        }
    }

    for d in next2 {
        if let Some(q) = em_quote(d, offline) {
            got.insert(d.code.to_string(), q);
        }
    }

    // 索引源没有昨收时，退一步只显示价格（pct=null），绝不拿断档昨收算涨跌
    for (code, q) in partial {
        got.entry(code).or_insert(q);
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    let items: Vec<Value> = INDICES.iter().map(|d| {
        let q = got.get(d.code);
        if let Some(q) = q {
            *counts.entry(q.source).or_insert(0) += 1;
        }
        item_json(d, q)
    }).collect();
    let ok_n = items.iter().filter(|it| it.get("price").map(|p| !p.is_null()).unwrap_or(false)).count();

    json!({
        "ok": ok_n == INDICES.len(),
        "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "offline": offline,
        "count": INDICES.len(),
        "ok_count": ok_n,
        "items": items,
        "sources": {
            "local": counts.get("local").copied().unwrap_or(0),
            "tq": counts.get("tq").copied().unwrap_or(0),
            "tencent": counts.get("tencent").copied().unwrap_or(0),
            "em": counts.get("em").copied().unwrap_or(0),
        },
    })
}

static CACHE: Mutex<Option<(Instant, bool, Value)>> = Mutex::new(None);
const TTL: Duration = Duration::from_secs(10);

/// 11 个主要指数快照；`force` 跳过 10s 缓存。
pub fn snapshot(offline: bool, force: bool) -> Value {
    if !force {
        if let Ok(g) = CACHE.lock() {
            if let Some((ts, off, v)) = g.as_ref() {
                if *off == offline && ts.elapsed() < TTL {
                    return v.clone();
                }
            }
        }
    }
    let v = build(offline);
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), offline, v.clone()));
    }
    v
}

// ---------------------------------------------------------------- 成分股

/// 成分股目录变动很慢（半年调样），内存缓存 6 小时；行情随点击实时取。
/// 成分股目录：code -> (取数时刻, [(code, name), ...])
type MembersMap = HashMap<String, (Instant, Vec<(String, String)>)>;
/// 成分股整体响应缓存：code -> (取数时刻, 数量, 负载)
type ConstCacheMap = HashMap<String, (Instant, i64, Value)>;
/// 成分股行情：code -> (price, pct, amount)
type MemberQuotes = HashMap<String, (Option<f64>, Option<f64>, Option<f64>)>;
/// 成分股行情取数函数
type MemberQuoteFn = fn(&[String]) -> MemberQuotes;

static MEMBERS: Mutex<Option<MembersMap>> = Mutex::new(None);
const MEMBERS_TTL: Duration = Duration::from_secs(6 * 3600);
/// 同一指数的整体响应缓存：60s 内直接命中；过期仍先返回旧值，后台静默刷新（SWR）。
/// 落盘 `data/market/index_constituents_cache.json`，重启后第一次打开也是热的。
static CONST_CACHE: Mutex<Option<ConstCacheMap>> = Mutex::new(None);
const CONST_TTL: Duration = Duration::from_secs(60);
/// 后台刷新的最小间隔 + 同代码去重，避免连点把 TQ/东财打成风暴。
static CONST_REFRESH: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
static CONST_INFLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);
const CONST_REFRESH_GAP: Duration = Duration::from_secs(15);
static CONST_DISK_LOADED: AtomicBool = AtomicBool::new(false);

fn const_cache_path() -> std::path::PathBuf {
    zg_core::paths::get().market_data.join("index_constituents_cache.json")
}

/// 首次访问时把落盘缓存灌进内存；age 由 `cached_at` 还原（进程重启后仍知道新旧）。
fn const_cache_init() {
    if CONST_DISK_LOADED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(const_cache_path()) else { return };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else { return };
    let Some(items) = doc.get("items").and_then(|v| v.as_object()) else { return };
    let now_epoch = chrono::Utc::now().timestamp();
    let mut loaded: HashMap<String, (Instant, i64, Value)> = HashMap::new();
    for (code, entry) in items {
        let (Some(value), Some(at)) = (
            entry.get("value").cloned(),
            entry.get("cached_at").and_then(|x| x.as_i64()),
        ) else {
            continue;
        };
        let age = now_epoch.saturating_sub(at).max(0) as u64;
        let ts = Instant::now().checked_sub(Duration::from_secs(age))
            .unwrap_or_else(Instant::now);
        loaded.insert(code.clone(), (ts, at, value));
    }
    if loaded.is_empty() {
        return;
    }
    if let Ok(mut g) = CONST_CACHE.lock() {
        let slot = g.get_or_insert_with(HashMap::new);
        for (code, entry) in loaded {
            slot.entry(code).or_insert(entry);
        }
    }
}

/// 原子落盘：tmp + rename（失败静默，不影响请求）。
fn const_cache_save() {
    let doc = {
        let Ok(g) = CONST_CACHE.lock() else { return };
        let Some(m) = g.as_ref() else { return };
        if m.is_empty() {
            return;
        }
        let items: Map<String, Value> = m.iter().map(|(code, (_, at, v))| {
            (code.clone(), json!({"cached_at": at, "value": v}))
        }).collect();
        json!({
            "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "items": Value::Object(items),
        })
    };
    let path = const_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&path);
    if let Ok(text) = serde_json::to_string(&doc) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn const_lookup(code: &str) -> Option<(Instant, i64, Value)> {
    let g = CONST_CACHE.lock().ok()?;
    g.as_ref().and_then(|m| m.get(code)).cloned()
}

/// 成功取到行情才写缓存；取数失败保留旧值（下次点击继续用旧的）。
fn const_remember(code: &str, value: &Value) {
    let quoted = value.get("quoted").and_then(|v| v.as_u64()).unwrap_or(0);
    if quoted == 0 {
        return;
    }
    let epoch = chrono::Utc::now().timestamp();
    if let Ok(mut g) = CONST_CACHE.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(code.to_string(), (Instant::now(), epoch, value.clone()));
    }
    const_cache_save();
}

/// 在途标记的 Drop 清理：无论正常结束还是 panic，都要摘掉，否则该指数不再后台刷新。
struct InflightClear<'a>(&'a str);

impl Drop for InflightClear<'_> {
    fn drop(&mut self) {
        let mut g = CONST_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            s.remove(self.0);
        }
    }
}

/// 后台刷新（同代码去重 + 15s 最小间隔）。
fn spawn_const_refresh(code: &str) {
    let Some(d) = INDICES.iter().find(|d| d.code == code) else { return };
    {
        let Ok(mut inflight) = CONST_INFLIGHT.lock() else { return };
        if !inflight.get_or_insert_with(HashSet::new).insert(code.to_string()) {
            return;
        }
    }
    {
        let Ok(mut g) = CONST_REFRESH.lock() else { return };
        let m = g.get_or_insert_with(HashMap::new);
        if let Some(ts) = m.get(code) {
            if ts.elapsed() < CONST_REFRESH_GAP {
                if let Ok(mut inflight) = CONST_INFLIGHT.lock() {
                    if let Some(s) = inflight.as_mut() { s.remove(code); }
                }
                return;
            }
        }
        m.insert(code.to_string(), Instant::now());
    }
    let owned = code.to_string();
    let spawned = std::thread::Builder::new()
        .name("const-refresh".into())
        .spawn(move || {
            let _g = InflightClear(&owned);
            let value = build_constituents(d, false);
            const_remember(&owned, &value);
        });
    if spawned.is_err() {
        if let Ok(mut inflight) = CONST_INFLIGHT.lock() {
            if let Some(s) = inflight.as_mut() {
                s.remove(code);
            }
        }
    }
}

/// 北交所（4/8/92 开头）与 B 股不在项目数据范围内，成分股列表一并剔除。
fn out_of_scope(code: &str) -> bool {
    matches!(code.as_bytes().first(), Some(b'4') | Some(b'8') | Some(b'9'))
}

fn em_secid(code: &str) -> String {
    if code.starts_with('6') { format!("1.{code}") } else { format!("0.{code}") }
}

fn em_fetch(url: &str, referer: &str, timeout: u64) -> Result<String, String> {
    if let Some(t) = crate::net_stub::get(url) {
        return Ok(t);
    }
    zg_core::net::http_get(url, timeout, 2, None, Some(&[("Referer", referer)]), None)
}

fn members_of(d: &IndexDef, offline: bool) -> Result<Vec<(String, String)>, String> {
    let Some(kind) = d.members_type else {
        return Err("该指数不提供成分股".into());
    };
    let _ = kind;
    if offline {
        return Err("离线模式不拉取成分股".into());
    }
    if let Ok(g) = MEMBERS.lock() {
        if let Some(m) = g.as_ref() {
            if let Some((ts, list)) = m.get(d.code) {
                if ts.elapsed() < MEMBERS_TTL {
                    return Ok(list.clone());
                }
            }
        }
    }
    let out = fetch_members(d)?;
    if let Ok(mut g) = MEMBERS.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(d.code.to_string(), (Instant::now(), out.clone()));
    }
    Ok(out)
}

/// 成分股目录拉取（东财 datacenter，每次都走网络）。
fn fetch_members(d: &IndexDef) -> Result<Vec<(String, String)>, String> {
    let Some(kind) = d.members_type else {
        return Err("该指数不提供成分股".into());
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut fetched = 0usize;
    for page in 1..=8 {
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_INDEX_TS_COMPONENT\
&columns=SECURITY_CODE,SECURITY_NAME_ABBR&filter=(TYPE%3D%22{kind}%22)\
&pageNumber={page}&pageSize=500&source=WEB&client=WEB"
        );
        let text = em_fetch(&url, "https://data.eastmoney.com/", 10)
            .map_err(|e| format!("成分股接口失败: {e}"))?;
        let j: Value = serde_json::from_str(&text)
            .map_err(|e| format!("成分股响应解析失败: {e}"))?;
        let res = j.get("result").filter(|v| !v.is_null()).ok_or("成分股接口无数据")?;
        let data = res.get("data").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if data.is_empty() {
            break;
        }
        fetched += data.len();
        for it in &data {
            let code = it.get("SECURITY_CODE").and_then(|x| x.as_str()).unwrap_or("");
            let name = it.get("SECURITY_NAME_ABBR").and_then(|x| x.as_str()).unwrap_or("");
            if code.len() == 6 && !out_of_scope(code) {
                out.push((code.to_string(), name.to_string()));
            }
        }
        let total = res.get("count").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        if fetched >= total || data.len() < 500 {
            break;
        }
    }
    if out.is_empty() {
        return Err("成分股为空".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- 成分股目录磁盘缓存

/// 磁盘缓存：data/market/index_members_cache.json
/// {"updated":"…","indices":{"000300":{"count":300,"members":[["000001","平安银行"],…]}}}
fn members_cache_path() -> std::path::PathBuf {
    zg_core::paths::get().market_data.join("index_members_cache.json")
}

fn load_members_cache() -> HashMap<String, (Instant, Vec<(String, String)>)> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(members_cache_path()) else { return out; };
    let Ok(j) = serde_json::from_str::<Value>(&text) else { return out; };
    let Some(indices) = j.get("indices").and_then(|x| x.as_object()) else { return out; };
    for (code, v) in indices {
        let Some(list) = v.get("members").and_then(|x| x.as_array()) else { continue };
        let members: Vec<(String, String)> = list.iter().filter_map(|it| {
            let p = it.as_array()?;
            Some((p.first()?.as_str()?.to_string(), p.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string()))
        }).collect();
        if !members.is_empty() {
            out.insert(code.clone(), (Instant::now(), members));
        }
    }
    out
}

/// 回写磁盘缓存（仅在校对发现列表变化时调用）。
fn save_members_cache(entries: &HashMap<String, Vec<(String, String)>>) {
    let mut indices = serde_json::Map::new();
    for (code, list) in entries {
        let members: Vec<Value> = list.iter()
            .map(|(c, n)| Value::Array(vec![Value::String(c.clone()), Value::String(n.clone())]))
            .collect();
        indices.insert(code.clone(), json!({"count": list.len(), "members": members}));
    }
    let doc = json!({
        "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "indices": Value::Object(indices),
    });
    let path = members_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&path);
    if let Ok(text) = serde_json::to_string(&doc) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// 启动时载入磁盘缓存（毫秒级，首点即出），并在后台校对一次：列表有变才回写。
pub fn start_members_cache() {
    let cached = load_members_cache();
    if !cached.is_empty() {
        if let Ok(mut g) = MEMBERS.lock() {
            *g = Some(cached);
        }
    }
    std::thread::Builder::new().name("index-members".into()).spawn(|| loop {
        verify_members();
        std::thread::sleep(Duration::from_secs(6 * 3600));
    }).ok();
}

/// 逐个指数拉源目录与缓存比对；变化才更新内存并回写磁盘（拉取失败保留旧缓存）。
fn verify_members() {
    let mut changed: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for d in INDICES.iter().filter(|d| d.members_type.is_some()) {
        let Ok(fresh) = fetch_members(d) else { continue };
        let old = MEMBERS.lock().ok()
            .and_then(|g| g.as_ref().and_then(|m| m.get(d.code)).map(|(_, v)| v.clone()));
        if old.as_deref() == Some(fresh.as_slice()) {
            continue;
        }
        changed.insert(d.code.to_string(), fresh);
    }
    if changed.is_empty() {
        return;
    }
    if let Ok(mut g) = MEMBERS.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        for (code, list) in &changed {
            m.insert(code.clone(), (Instant::now(), list.clone()));
        }
    }
    let mut entries: HashMap<String, Vec<(String, String)>> = HashMap::new();
    if let Ok(g) = MEMBERS.lock() {
        if let Some(m) = g.as_ref() {
            for (code, (_, list)) in m {
                entries.insert(code.clone(), list.clone());
            }
        }
    }
    save_members_cache(&entries);
}

/// TQ 批量实时行情 → 成分股行情（price / pct（由 preClose 推算）/ amount）。
/// 本地通达信源：不限 IP、无外部限流，300/1000/1961 只实测 0.5s/1.7s/3.3s。
/// dividend_type=front：除权日 preClose 用除权参考价，涨跌幅与行情软件一致。
fn tq_member_quotes(
    codes: &[String],
) -> MemberQuotes {
    let mut out = HashMap::new();
    if codes.is_empty() || !crate::tq::available() {
        return out;
    }
    let Ok(snap) = crate::tq::quotes(codes, "front", true) else { return out; };
    for (code, v) in snap {
        let Some(price) = num_of(v.get("price")).filter(|p| *p > 0.0) else { continue };
        let pct = match num_of(v.get("preClose")).filter(|p| *p > 0.0) {
            Some(pre) => Some((price / pre - 1.0) * 100.0),
            None => num_of(v.get("pct")),
        };
        // 万元×1e4 的浮点尾数抹平到分，与东财口径一致
        let amount = num_of(v.get("amount")).map(|a| (a * 100.0).round() / 100.0);
        out.insert(code, (Some(price), pct, amount));
    }
    out
}

/// 东财 ulist 单次上限约 500 个 secid（1000 会 502），按 200 分批。
/// push2delay 会按 IP 限流（连接被重置），故超时收紧到 5s，交由换源兜底。
fn em_member_quotes(
    codes: &[String],
) -> MemberQuotes {
    let mut out = HashMap::new();
    if codes.is_empty() || !kline::_src_open("em") {
        return out;
    }
    for chunk in codes.chunks(200) {
        let secids: Vec<String> = chunk.iter().map(|c| em_secid(c)).collect();
        let url = format!(
            "https://push2delay.eastmoney.com/api/qt/ulist.np/get?fltt=2&invt=2\
&fields=f2,f3,f6,f12&secids={}",
            secids.join(",")
        );
        let Ok(text) = em_fetch(&url, "https://quote.eastmoney.com/", 5) else { continue };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { continue };
        let data = j.get("data").and_then(|d| d.get("diff"))
            .and_then(|x| x.as_array()).cloned().unwrap_or_default();
        for it in &data {
            let Some(code) = it.get("f12").and_then(|x| x.as_str()) else { continue };
            let price = num_of(it.get("f2")).filter(|p| *p > 0.0);
            out.insert(code.to_string(), (price, num_of(it.get("f3")), num_of(it.get("f6"))));
        }
    }
    out
}

/// 腾讯批量快照 → 成分股行情（f12/f2/f3/f6）。
fn tencent_member_quotes(
    codes: &[String],
) -> MemberQuotes {
    let mut out = HashMap::new();
    if codes.is_empty() {
        return out;
    }
    for row in crate::quotes::fetch_snapshot_tencent(codes) {
        let Some(code) = row.get("f12").and_then(|v| v.as_str()) else { continue };
        let Some(price) = num_of(row.get("f2")).filter(|p| *p > 0.0) else { continue };
        out.insert(code.to_string(), (Some(price), num_of(row.get("f3")), num_of(row.get("f6"))));
    }
    out
}

/// 同花顺批量快照 → 成分股行情（price/pct/amount）。
fn ths_member_quotes(
    codes: &[String],
) -> MemberQuotes {
    let mut out = HashMap::new();
    if codes.is_empty() || !crate::consts::ths_available() {
        return out;
    }
    for (code, v) in crate::ths::ths_snapshot(codes, 200) {
        let Some(price) = num_of(v.get("price")).filter(|p| *p > 0.0) else { continue };
        out.insert(code, (Some(price), num_of(v.get("pct")), num_of(v.get("amount"))));
    }
    out
}

/// 成分股行情取值链：TQ（本地通达信）→ 东财 ulist → 腾讯快照 → 同花顺快照。
/// 前一源未覆盖的代码才交给下一源补齐；全部失败才算"行情源暂不可用"。
fn member_quotes(
    codes: &[String], offline: bool,
) -> (MemberQuotes, String) {
    let mut out: MemberQuotes = HashMap::new();
    let mut sources: Vec<&str> = Vec::new();
    if offline || codes.is_empty() {
        return (out, String::new());
    }
    let chain: [(&str, MemberQuoteFn); 4] = [
        ("TQ", tq_member_quotes),
        ("东财", em_member_quotes),
        ("腾讯", tencent_member_quotes),
        ("同花顺", ths_member_quotes),
    ];
    let mut missing: Vec<String> = codes.to_vec();
    for (name, fetch) in chain {
        if missing.is_empty() {
            break;
        }
        let quotes = fetch(&missing);
        if quotes.is_empty() {
            continue;
        }
        let before = missing.len();
        missing.retain(|c| match quotes.get(c) {
            Some(q) => {
                out.insert(c.clone(), *q);
                false
            }
            None => true,
        });
        if missing.len() < before {
            sources.push(name);
        }
    }
    (out, sources.join("+"))
}

/// 真正取数并组装（不判缓存）。失败返回 ok=false，成功（quoted>0）才由调用方写缓存。
fn build_constituents(d: &IndexDef, offline: bool) -> Value {
    let members = match members_of(d, offline) {
        Ok(m) => m,
        Err(e) => return json!({"ok": false, "code": d.code, "name": d.name, "error": e}),
    };
    let codes: Vec<String> = members.iter().map(|(c, _)| c.clone()).collect();
    let (quotes, source) = member_quotes(&codes, offline);
    let rows: Vec<Value> = members.iter().map(|(code, name)| {
        let (price, pct, amount) = quotes.get(code).cloned().unwrap_or((None, None, None));
        json!({"code": code, "name": name, "price": price, "pct": pct, "amount": amount})
    }).collect();
    let quoted = rows.iter()
        .filter(|r| r.get("price").map(|p| !p.is_null()).unwrap_or(false))
        .count();
    json!({
        "ok": quoted > 0,
        "code": d.code,
        "name": d.name,
        "count": rows.len(),
        "quoted": quoted,
        "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "source": source,
        "offline": offline,
        "error": if quoted == 0 { Value::String("行情源暂不可用".into()) } else { Value::Null },
        "rows": rows,
    })
}

fn const_payload(d: &IndexDef, offline: bool) -> Value {
    const_cache_init();
    if offline {
        // 离线优先用落盘缓存（仍是本地数据，只是不能刷新）；无缓存保持原错误语义。
        if let Some((_, _, v)) = const_lookup(d.code) {
            let mut v = v;
            if let Some(o) = v.as_object_mut() {
                o.insert("offline".into(), Value::Bool(true));
            }
            return v;
        }
        return build_constituents(d, true);
    }
    if let Some((ts, _, v)) = const_lookup(d.code) {
        if ts.elapsed() >= CONST_TTL {
            spawn_const_refresh(d.code);
        }
        return v;
    }
    let value = build_constituents(d, false);
    const_remember(d.code, &value);
    value
}

pub fn constituents(code: &str, offline: bool, force: bool) -> Value {
    let Some(d) = INDICES.iter().find(|d| d.code == code) else {
        return json!({"ok": false, "code": code, "error": "未知指数"});
    };
    if d.members_type.is_none() {
        return json!({"ok": false, "code": d.code, "name": d.name,
                      "error": "该指数不提供成分股列表"});
    }
    if force {
        let value = build_constituents(d, offline);
        if !offline {
            const_remember(d.code, &value);
        }
        return value;
    }
    const_payload(d, offline)
}

// ---------------------------------------------------------------- 指数/板块 K 线

/// 指数/板块 K 线整体缓存：日线 45s、周月 300s、分钟 15s。
static BOARD_CACHE: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);
const BOARD_TTL_DAY: Duration = Duration::from_secs(45);
const BOARD_TTL_WM: Duration = Duration::from_secs(300);
const BOARD_TTL_MIN: Duration = Duration::from_secs(15);
/// 板块缓存条目上限：键含 lmt/period/offline 组合，不淘汰就会无界增长。
const BOARD_CACHE_MAX: usize = 200;
/// TQ 缓存刷新闸门（同一代码 10 分钟内最多 refresh 一次）。
static BOARD_REFRESH: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

static SECTORS: OnceLock<HashMap<String, (String, String)>> = OnceLock::new();

fn parse_sectors(j: &Value) -> HashMap<String, (String, String)> {
    let mut m = HashMap::new();
    let Some(nodes) = j.get("nodes").and_then(|x| x.as_array()) else { return m };
    for n in nodes {
        let Some(code) = n.get("code").and_then(|x| x.as_str())
            .filter(|c| c.starts_with("88")) else { continue };
        let name = n.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let pair = (code.to_string(), name);
        m.insert(code.to_string(), pair.clone());
        if let Some(r) = n.get("ref").and_then(|x| x.as_str()).filter(|r| !r.is_empty()) {
            m.insert(r.to_string(), pair);
        }
    }
    m
}

/// 行业目录：881xxx 代码与 X 开头的 ref 都能解析到 (881 代码, 名称)。
fn sectors() -> &'static HashMap<String, (String, String)> {
    SECTORS.get_or_init(|| {
        let p = zg_core::paths::get().root.join("modules").join("market")
            .join("industry_catalog.json");
        match std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            Some(j) => parse_sectors(&j),
            None => HashMap::new(),
        }
    })
}

enum Board {
    Index(&'static IndexDef),
    Sector { code: String, name: String },
}

fn resolve_board(code: &str) -> Option<Board> {
    if let Some(d) = INDICES.iter().find(|d| d.code == code) {
        return Some(Board::Index(d));
    }
    sectors().get(code)
        .map(|(c, n)| Board::Sector { code: c.clone(), name: n.clone() })
}

/// 本地 .day → KBar；指数/板块的 volume 本身就是手，不能再除 100。
fn local_board_bars(market: &str, code: &str, period: &str, lmt: usize, fresh: bool)
    -> Result<Vec<KBar>, String> {
    let bars = zg_core::api::read_day_bars(market, code).map_err(|e| e.to_string())?;
    let Some(last) = bars.last() else { return Err("本地日线为空".into()) };
    if fresh {
        let expected = zg_core::calendar::last_trading_date(None);
        if last.date.as_str() < expected.as_str() {
            return Err(format!("本地日线未更新: 末根 {} < 最近交易日 {}", last.date, expected));
        }
    }
    let mut out: Vec<KBar> = bars.iter().map(|b| KBar {
        date: b.date.clone(), open: b.open, high: b.high, low: b.low,
        close: b.close, vol: b.volume, amount: b.amount,
    }).collect();
    if period != "day" {
        out = crate::market_data::synth_wm_from_daily(&out, period);
    }
    if lmt > 0 && out.len() > lmt {
        out = out[out.len() - lmt..].to_vec();
    }
    Ok(out)
}

fn fetch_tq_board(symbol: &str, count: usize) -> Result<Vec<KBar>, String> {
    let res = tq::post(
        "get_market_data",
        &json!({"stock_list": [symbol], "period": "1d", "count": count,
                "dividend_type": "none"}),
        10,
    )?;
    let arr = res.get("Value").and_then(|x| x.get(symbol)).cloned().unwrap_or(Value::Null);
    parse_tq_board_bars(&arr)
}

/// 纯解析（单测钉死口径：指数/板块 TQ Volume 单位是手，不除 100；Amount 万元 → 元）。
fn parse_tq_board_bars(o: &Value) -> Result<Vec<KBar>, String> {
    let obj = o.as_object().ok_or_else(|| "TQ 日线结构异常".to_string())?;
    if obj.get("ErrorId").and_then(|x| x.as_str()).unwrap_or("0") != "0" {
        return Err("TQ 日线返回错误".into());
    }
    let dates: Vec<String> = obj.get("Date").and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    if dates.is_empty() {
        return Err("TQ 日线为空".into());
    }
    let at = |k: &str, i: usize, mul: f64| -> f64 {
        obj.get(k).and_then(|x| x.as_array()).and_then(|a| a.get(i))
            .and_then(|v| py_float(Some(v)).ok()).map(|v| v * mul).unwrap_or(0.0)
    };
    let mut bars = Vec::new();
    for (i, ds) in dates.iter().enumerate() {
        let s: String = ds.chars().filter(|c| *c != '-').collect();
        if s.len() < 8 {
            continue;
        }
        let close = at("Close", i, 1.0);
        if close <= 0.0 {
            continue;
        }
        // 指数/板块的 Volume 单位为手（股票 TQ 的 Volume 是股，已在 kline_tq_day 里 /100），
        // 这里直接用；Amount 万元 → 元。
        bars.push(KBar {
            date: format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]),
            open: at("Open", i, 1.0), close,
            high: at("High", i, 1.0), low: at("Low", i, 1.0),
            vol: at("Volume", i, 1.0), amount: at("Amount", i, 10000.0),
        });
    }
    if bars.is_empty() {
        return Err("TQ 日线为空".into());
    }
    bars.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(bars)
}

fn bars_stale(bars: &[KBar]) -> bool {
    let Some(last) = bars.last() else { return true };
    match ymd(&last.date) {
        Some(dt) => (chrono::Local::now().date_naive() - dt).num_days() > 15,
        None => true,
    }
}

fn refresh_tq_board(symbol: &str) {
    let now = Instant::now();
    {
        let mut g = BOARD_REFRESH.lock_ok();
        let m = g.get_or_insert_with(HashMap::new);
        if let Some(t) = m.get(symbol) {
            if now.duration_since(*t) < Duration::from_secs(600) {
                return;
            }
        }
        m.insert(symbol.to_string(), now);
    }
    // 新指数（932000/930050.CSI）客户端只留极短缓存，不 refresh 会一直读到旧日期
    let _ = tq::post("refresh_kline", &json!({"stock_list": [symbol], "period": "1d"}), 20);
}

fn tq_board_bars(symbol: &str, period: &str, lmt: usize, refresh: bool)
    -> Result<(Vec<KBar>, bool), String> {
    if !tq::available() {
        return Err("TQ 不可用".into());
    }
    let want = if lmt > 0 { lmt } else { 300 };
    let mul = match period { "week" => 6, "month" => 22, _ => 1 };
    let count = (want * mul + 40).min(1500);
    let mut daily = fetch_tq_board(symbol, count)?;
    let mut stale = bars_stale(&daily);
    if refresh && stale {
        refresh_tq_board(symbol);
        if let Ok(v) = fetch_tq_board(symbol, count) {
            daily = v;
            stale = bars_stale(&daily);
        }
    }
    let mut out = if period == "day" {
        daily
    } else {
        crate::market_data::synth_wm_from_daily(&daily, period)
    };
    if want > 0 && out.len() > want {
        out = out[out.len() - want..].to_vec();
    }
    Ok((out, stale))
}

fn tencent_board_bars(symbol: &str, period: &str, lmt: usize) -> Result<Vec<KBar>, String> {
    let n = if lmt > 0 { lmt } else { 300 };
    if period.starts_with("min") {
        kline::kline_tencent_min_symbol(symbol, period, n)
    } else {
        kline::kline_tencent_symbol(symbol, period, n).map(|(_, b)| b)
    }
}

fn board_error(code: &str, name: &str, period: &str, min_period: bool, msg: String) -> Value {
    json!({"code": code, "name": name, "period": period, "bars": [],
           "board": true, "min_period": min_period, "source": "", "error": msg})
}

/// 指数/板块 K 线（中心图）：指数走 本地 → TQ → 腾讯，板块走 本地 → TQ → 本地旧末根。
/// 分钟线仅指数且有腾讯代码时可用（TQ 指数分钟接口为空）。
pub fn board_kline(code: &str, period: &str, lmt: usize, offline: bool) -> Value {
    let code = code.trim();
    let period = period.trim().to_lowercase();
    let min_period = period.starts_with("min");
    if !matches!(period.as_str(), "day" | "week" | "month"
        | "min1" | "min5" | "min15" | "min30" | "min60")
    {
        return board_error(code, "", &period, min_period, format!("不支持的周期: {period}"));
    }
    let lmt = if lmt > 0 {
        lmt
    } else {
        match period.as_str() {
            "day" => 250, "week" => 200, "month" => 160, "min1" => 1200, "min5" => 800,
            _ => 480,
        }
    };
    // 上限约束：TQ 路径本就 ≤1500，本地/腾讯与缓存键同步钳住，防异常 lmt 撑大缓存
    let lmt = lmt.min(1500);
    let Some(board) = resolve_board(code) else {
        return board_error(code, "", &period, min_period, "未找到该指数/板块代码".into());
    };
    let (name, resolved_code, tq_symbol, tencent_symbol, local_market) = match &board {
        Board::Index(d) => (
            d.name.to_string(), d.code.to_string(), d.tq.to_string(),
            d.tencent.map(|s| s.to_string()),
            if d.market.is_empty() { None } else { Some(d.market.to_string()) },
        ),
        Board::Sector { code, name } => (
            name.clone(), code.clone(), format!("{code}.SH"), None, Some("sh".to_string()),
        ),
    };

    let cache_key = format!("{resolved_code}|{period}|{lmt}|{offline}");
    let ttl = if min_period {
        BOARD_TTL_MIN
    } else if period == "day" {
        BOARD_TTL_DAY
    } else {
        BOARD_TTL_WM
    };
    {
        let g = BOARD_CACHE.lock_ok();
        if let Some(m) = g.as_ref() {
            if let Some((ts, v)) = m.get(&cache_key) {
                if ts.elapsed() < ttl {
                    return v.clone();
                }
            }
        }
    }

    let mut errs: Vec<String> = Vec::new();
    let mut bars: Vec<KBar> = Vec::new();
    let mut source = "";
    let mut stale = false;

    if min_period {
        match tencent_symbol.as_deref() {
            Some(sym) if !offline => match tencent_board_bars(sym, &period, lmt) {
                Ok(b) => { bars = b; source = "tencent"; }
                Err(e) => errs.push(format!("腾讯: {e}")),
            },
            Some(_) => errs.push("离线模式不拉取分钟K线".into()),
            None => errs.push(if matches!(board, Board::Sector { .. }) {
                "板块分钟K线暂不可用".into()
            } else {
                "该指数分钟K线暂不可用".into()
            }),
        }
    } else {
        if let Some(mkt) = local_market.as_deref() {
            match local_board_bars(mkt, &resolved_code, &period, lmt, true) {
                Ok(b) => { bars = b; source = "local"; }
                Err(e) => errs.push(format!("本地: {e}")),
            }
        }
        if bars.is_empty() && !offline {
            match tq_board_bars(&tq_symbol, &period, lmt, true) {
                Ok((b, st)) => { bars = b; source = "tq"; stale = st; }
                Err(e) => errs.push(format!("TQ: {e}")),
            }
        }
        if bars.is_empty() && !offline {
            if let Some(sym) = tencent_symbol.as_deref() {
                match tencent_board_bars(sym, &period, lmt) {
                    Ok(b) => { bars = b; source = "tencent"; stale = bars_stale(&bars); }
                    Err(e) => errs.push(format!("腾讯: {e}")),
                }
            }
        }
        // 盘中本地 .day 尚未写入当日：退回上一交易日末根，并明确标记 cache_stale
        if bars.is_empty() {
            if let Some(mkt) = local_market.as_deref() {
                if let Ok(b) = local_board_bars(mkt, &resolved_code, &period, lmt, false) {
                    bars = b;
                    source = "local";
                    stale = true;
                }
            }
        }
    }

    if bars.is_empty() {
        let msg = if errs.is_empty() { "暂无可用K线数据".to_string() } else { errs.join("；") };
        return board_error(&resolved_code, &name, &period, min_period, msg);
    }

    let data_source = match source {
        "local" => "本地DFZQ",
        "tq" => "TQ-Local",
        "tencent" => "腾讯",
        _ => "",
    };
    let bars_v: Vec<Value> = bars.iter().map(|b| b.to_value()).collect();
    // 蜡烛形态引擎（与 /api/kline 同一套 rec27 规则，指数/板块同样适用）
    let candle_result = if crate::consts::candle_engine() == "legacy" {
        json!({"engine": "legacy", "signals": Vec::<Value>::new()})
    } else {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let last10 = bars.last().map(|b| b.date.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let provisional_last = zg_core::calendar::market_open_now(None) && last10 == today;
        let patterns = crate::consts::candle_patterns();
        let ps = if patterns == "all" { None } else { Some(patterns.as_str()) };
        crate::candles::analyze_candles(&bars_v, &period, provisional_last, ps)
    };
    let mut m = Map::new();
    m.insert("code".into(), Value::String(resolved_code));
    m.insert("name".into(), Value::String(name));
    m.insert("period".into(), Value::String(period));
    m.insert("market".into(), Value::String(
        local_market.clone().unwrap_or_else(|| "sh".into())));
    m.insert("bars".into(), Value::Array(bars_v));
    m.insert("source".into(), Value::String(source.into()));
    m.insert("board".into(), Value::Bool(true));
    m.insert("min_period".into(), Value::Bool(min_period));
    m.insert("data_source".into(), Value::String(data_source.into()));
    m.insert("candle_engine".into(),
             candle_result.get("engine").cloned().unwrap_or_else(|| json!("unavailable")));
    m.insert("candle_pattern_set".into(),
             Value::String(crate::consts::candle_patterns()));
    m.insert("candle_signals".into(),
             candle_result.get("signals").cloned().unwrap_or_else(|| json!([])));
    m.insert("cache_date".into(), Value::String(
        bars.last().map(|b| b.date.clone()).unwrap_or_default()));
    if stale {
        m.insert("cache_stale".into(), Value::Bool(true));
    }
    let value = Value::Object(m);
    {
        let mut g = BOARD_CACHE.lock_ok();
        let m = g.get_or_insert_with(HashMap::new);
        m.insert(cache_key, (Instant::now(), value.clone()));
        if m.len() > BOARD_CACHE_MAX {
            let mut oldest: Vec<(String, Instant)> =
                m.iter().map(|(k, (ts, _))| (k.clone(), *ts)).collect();
            oldest.sort_by_key(|(_, ts)| *ts);
            for (k, _) in oldest.iter().take(m.len() - BOARD_CACHE_MAX) {
                m.remove(k);
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defs_cover_requested_order() {
        let codes: Vec<&str> = INDICES.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["000001", "399001", "399006", "000688", "000300",
                           "000905", "000852", "932000", "000016", "930050", "000510"]);
        assert_eq!(INDICES[0].market, "sh"); // 000001 是上证指数，不是平安银行
        let csi = INDICES.iter().find(|d| d.code == "932000").unwrap();
        assert_eq!(csi.tq, "932000.CSI");
        assert_eq!(csi.tencent, None);
    }

    #[test]
    fn members_only_for_broad_indices() {
        let with: Vec<(&str, Option<&str>)> = INDICES.iter()
            .map(|d| (d.code, d.members_type)).collect();
        for (code, ty) in [("000300", "1"), ("000905", "3"), ("000852", "7"),
                           ("932000", "13"), ("000016", "2"), ("930050", "5"),
                           ("000510", "6")] {
            assert_eq!(with.iter().find(|(c, _)| *c == code).unwrap().1, Some(ty), "{code}");
        }
        for code in ["000001", "399001", "399006", "000688"] {
            assert_eq!(with.iter().find(|(c, _)| *c == code).unwrap().1, None, "{code}");
        }
    }

    #[test]
    fn secid_and_scope_filter() {
        assert_eq!(em_secid("600519"), "1.600519");
        assert_eq!(em_secid("688981"), "1.688981");
        assert_eq!(em_secid("000001"), "0.000001");
        assert!(out_of_scope("920001"));
        assert!(out_of_scope("830799"));
        assert!(out_of_scope("430047"));
        assert!(!out_of_scope("600519"));
        assert!(!out_of_scope("000001"));
        assert!(!out_of_scope("300750"));
    }

    #[test]
    fn adjacent_rejects_cache_gap() {
        assert!(adjacent("20261027", "20261028"));
        assert!(adjacent("20260930", "20261009")); // 长假
        assert!(!adjacent("20260921", "20261028")); // 5 周断档
    }

    #[test]
    fn board_resolution_is_unambiguous() {
        assert!(matches!(resolve_board("000001"), Some(Board::Index(d)) if d.name == "上证指数"));
        assert!(matches!(resolve_board("000300"), Some(Board::Index(d)) if d.name == "沪深300"));
        assert!(resolve_board("600519").is_none());
    }

    #[test]
    fn sector_catalog_maps_ref_and_code() {
        let j = json!({"nodes": [
            {"code": "881333", "name": "元器件", "ref": "X4004"},
            {"code": "881121", "name": "水产饲料", "ref": "X200502"}
        ]});
        let m = parse_sectors(&j);
        assert_eq!(m.get("X4004").unwrap().0, "881333");
        assert_eq!(m.get("881333").unwrap().1, "元器件");
        assert_eq!(m.get("X200502").unwrap().0, "881121");
    }

    #[test]
    fn board_rejects_unknown_period_offline() {
        let v = board_kline("000300", "year", 10, true);
        assert!(v["error"].as_str().unwrap().contains("不支持的周期"));
    }

    #[test]
    fn tq_board_volume_stays_in_lots() {
        // 指数/板块 TQ Volume 单位是手（股票才是股），解析层不得除 100；Amount 万元 → 元
        let o = json!({"ErrorId": "0",
            "Date": ["2026-10-27", "2026-10-28"],
            "Open": [1000.0, 1010.0], "High": [1020.0, 1030.0],
            "Low": [990.0, 1000.0], "Close": [1010.0, 1025.0],
            "Volume": [123456.0, 234567.0], "Amount": [100.0, 200.0]});
        let bars = parse_tq_board_bars(&o).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].date, "2026-10-28");
        assert_eq!(bars[1].vol, 234567.0);
        assert_eq!(bars[1].amount, 2_000_000.0);
        assert!(parse_tq_board_bars(&json!({"ErrorId": "1", "Date": []})).is_err());
        assert!(parse_tq_board_bars(&Value::Null).is_err());
    }

    #[test]
    fn pct_rounding_and_partial() {
        let q = Quote { price: 3676.92, pre_close: Some(3648.30), open: None, high: None,
                        low: None, date: "2026-10-28".into(), time: None, source: "tq" };
        let it = item_json(&INDICES[0], Some(&q));
        assert_eq!(it["pct"], json!(0.78));
        let part = Quote { pre_close: None, ..q };
        let it2 = item_json(&INDICES[0], Some(&part));
        assert!(it2["pct"].is_null());
        assert_eq!(it2["price"], json!(3676.92));
        assert_eq!(it2["error"], "昨收不可用");
    }
}
