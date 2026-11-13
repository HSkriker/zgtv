//! 实时取数层（方案 C）——所有网络调用集中在这里。
//!
//! 口径（与 zg 对齐，勿改）：
//! - 盘中（09:25–15:05 且今天是交易日）本地 `.day` 只有 T-1，跳过本地走网络；
//! - 收盘后本地优先，但本地末根必须 ≥ 最近交易日，否则切远端；
//! - 涨跌幅 = 前复权末两根收盘之比（TQ `dividend_type=front`）；
//! - TQ 禁调 `get_market_snapshot` / `get_scjy_value`，由 `zg_market::tq::post` 拒发。
//!
//! `TV_LIVE=0` 或 `TV_OFFLINE=1` 退回纯离线（对表/无网机器）。

use std::collections::{HashMap, HashSet};

mod policy;
mod source_local;
mod source_mac;

pub use policy::{day_series, minute_series};
pub(crate) use policy::remote_mergeable;

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::{json, Map, Value};
use zg_core::sync::MutexExt;
use crate::bars::{resample_qy, Agg, Row7};

/// 一段行情 + 来源/日期/新鲜度。所有图表接口都带这组元数据。
#[derive(Clone, Debug)]
pub struct Series {
    pub rows: Vec<Agg>,
    pub source: String,
    /// 来源的附加定位信息（如 MAC 实际连上的主机 IP）——对表脚本要连同一台才有可比性。
    pub detail: String,
    pub as_of: String,
    pub stale: bool,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn now_secs() -> u64 {
    now() as u64
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 实时链路总开关。`TV_LIVE=0` 或 `TV_OFFLINE=1` → 完全离线。
pub fn enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        if std::env::var("TV_OFFLINE").map(|v| v.trim() == "1").unwrap_or(false) {
            return false;
        }
        std::env::var("TV_LIVE").map(|v| v.trim() != "0").unwrap_or(true)
    })
}

/// 盘中（含午休）：09:25–15:05 且当天是交易日。与 zg `get_kline` 的 `session_today` 同口径。
pub fn session_today() -> bool {
    let now = chrono::Local::now();
    let hm = now.format("%H:%M").to_string();
    hm.as_str() >= "09:25"
        && hm.as_str() < "15:05"
        && zg_core::calendar::last_trading_date(Some(now)) == today()
}

pub fn market_open_now() -> bool {
    zg_core::calendar::market_open_now(None)
}

/// 新鲜度参照日：盘中看今天（本地 .day 天然 T-1）；其余时段看最近交易日。
pub fn reference_day() -> String {
    if session_today() {
        today()
    } else {
        zg_market::kline::local_reference_day()
    }
}

/// 缓存时间桶：盘中按 TTL；收盘后返回 None（仍走 mtime 指纹）。
///
/// 盘中一律 15s（午休 60s）—— 远端失败时会退回「本地 + stale」，这个坏结果
/// 不能被长时间缓存，否则一次 TQ 抖动会让整段行情在缓存 TTL 内一直陈旧。
/// 15s 也与前端轮询同频：每个 tick 最多一次远端调用，TQ 护栏（2s/3 连发）扛得住。
pub fn ttl_bucket(_period: &str) -> Option<u64> {
    if !enabled() || !session_today() {
        return None;
    }
    let secs = if market_open_now() { 15 } else { 60 };
    Some(now_secs() / secs)
}

fn is_qy(period: &str) -> bool {
    matches!(period, "quarter" | "year")
}

fn to_row7(rows: &[Agg]) -> Vec<Row7> {
    rows.iter()
        .map(|b| (b.date.clone(), b.open, b.high, b.low, b.close, b.vol, b.amount))
        .collect()
}

fn date_key(s: &str) -> Option<i32> {
    let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.len() >= 8 {
        d[..8].parse::<i32>().ok()
    } else {
        None
    }
}

/// 日期先后（a < b）：能解析成 YYYYMMDD 就按整数比，否则退回字符串比。
///
/// 字典序比较会把 `'20261103' < '2026-11-04'` 判成**假**（'-' < '1'），
/// 于是某源用 YYYYMMDD 格式时，陈旧的一天会被当成新鲜的。
fn date_lt(a: &str, b: &str) -> bool {
    match (date_key(a), date_key(b)) {
        (Some(x), Some(y)) => x < y,
        _ => a < b,
    }
}

/// 四舍五入到 2 位（A 股价格最小变动单位）。
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn quote_cache() -> &'static Mutex<HashMap<String, (f64, Value)>> {
    static C: OnceLock<Mutex<HashMap<String, (f64, Value)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn batch_cache() -> &'static Mutex<Option<(String, f64, Value)>> {
    static C: OnceLock<Mutex<Option<(String, f64, Value)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn py_num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// 补 code/source/pct/as_of；TQ 批量报价不带 pct，这里按前复权昨收算。
fn decorate_quote(code: &str, v: Value, source: &str) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    m.entry("code".to_string()).or_insert_with(|| Value::String(code.to_string()));
    m.insert("source".into(), Value::String(source.to_string()));
    let price = py_num(m.get("price"));
    let pre = py_num(m.get("preClose"));
    let pct_missing = m.get("pct").map(|x| x.is_null()).unwrap_or(true);
    if pct_missing && price > 0.0 && pre > 0.0 {
        m.insert(
            "pct".into(),
            Value::from(zg_core::model::py_round((price / pre - 1.0) * 100.0, 2)),
        );
    }
    if m.get("time").map(|x| x.is_null()).unwrap_or(true) {
        m.insert(
            "time".into(),
            Value::String(chrono::Local::now().format("%H:%M:%S").to_string()),
        );
    }
    m.insert("as_of".into(), Value::String(today()));
    m.insert("stale".into(), Value::Bool(false));
    Value::Object(m)
}

/// 单码实时报价：批量 front 链优先（除权日 pct 正确），失败再走单票五源链。
pub fn quote(code: &str) -> Result<Value, String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("缺少 code 参数".to_string());
    }
    if !enabled() {
        return Err("实时行情已关闭（TV_LIVE=0 / TV_OFFLINE=1）".to_string());
    }
    let t = now();
    if let Some((ts, v)) = quote_cache().lock_ok().get(code) {
        if t - *ts < 3.0 {
            return Ok(v.clone());
        }
    }
    let (map, src) = zg_market::live::fetch_live(&[code.to_string()]);
    let out = match map.get(code) {
        Some(v) => decorate_quote(code, v.clone(), src.unwrap_or("tq")),
        None => {
            let q = zg_market::quotes::get_quote(code)?;
            let s = q.get("source").and_then(|x| x.as_str()).unwrap_or("quote").to_string();
            decorate_quote(code, q, &s)
        }
    };
    quote_cache().lock_ok().insert(code.to_string(), (t, out.clone()));
    Ok(out)
}

/// 批量实时报价（≤300 只）。3s 整体缓存；缺码再补单票链（最多 20 只，避免打爆）。
pub fn quotes(codes: &[String]) -> Value {
    if !enabled() {
        return json!({"ok": false, "error": "实时行情已关闭（TV_LIVE=0 / TV_OFFLINE=1）",
                      "source": "", "as_of": today(), "n": 0, "quotes": {}});
    }
    let mut uniq: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for c in codes {
        let c = c.trim().to_string();
        if c.is_empty() || !seen.insert(c.clone()) {
            continue;
        }
        uniq.push(c);
        if uniq.len() >= 300 {
            break;
        }
    }
    let key = uniq.join(",");
    let t = now();
    {
        let g = batch_cache().lock_ok();
        if let Some((k, ts, v)) = g.as_ref() {
            if *k == key && t - *ts < 3.0 {
                return v.clone();
            }
        }
    }
    if uniq.is_empty() {
        return json!({"ok": true, "source": "", "as_of": today(), "n": 0, "quotes": {}});
    }

    let mut out = Map::new();
    let mut source = "cache".to_string();
    let (map, src) = zg_market::live::fetch_live(&uniq);
    if let Some(s) = src {
        source = s.to_string();
    }
    for c in &uniq {
        if let Some(v) = map.get(c) {
            out.insert(c.clone(), decorate_quote(c, v.clone(), src.unwrap_or("tq")));
        }
    }
    let missing: Vec<String> = uniq
        .iter()
        .filter(|c| !out.contains_key(*c))
        .cloned()
        .collect();
    if !missing.is_empty() && missing.len() <= 20 {
        for c in &missing {
            if let Ok(q) = zg_market::quotes::get_quote(c) {
                let s = q.get("source").and_then(|x| x.as_str()).unwrap_or("quote").to_string();
                out.insert(c.clone(), decorate_quote(c, q, &s));
            }
        }
    }
    let v = json!({
        "ok": true,
        "source": source,
        "as_of": today(),
        "n": out.len(),
        "quotes": Value::Object(out),
    });
    *batch_cache().lock_ok() = Some((key, t, v.clone()));
    v
}

/// `/api/market-status`：前端徽标与排障用。
pub fn status() -> Value {
    json!({
        "ok": true,
        "live": enabled(),
        "session": session_today(),
        "market_open": market_open_now(),
        "today": today(),
        "reference": reference_day(),
        "local_last": crate::bars::market_last_date(),
        "tq": zg_market::tq::status(),
    })
}

/// 启动预热：TQ 首次调用 ~6s 冷启动，交给后台线程；TQ 不在也不影响启动。
pub fn warmup() {
    if !enabled() {
        println!("[tv] TV_LIVE=0 / TV_OFFLINE=1：实时链路关闭，仅本地数据");
        return;
    }
    println!("[tv] 实时链路开启，TQ 预热中…");
    zg_market::tq::warmup();
    let s = zg_market::tq::status();
    println!(
        "[tv] TQ 状态: ok={} rt={} live={} cooldown={}",
        s.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        s.get("rt").and_then(|x| x.as_bool()).unwrap_or(false),
        s.get("live").and_then(|x| x.as_bool()).unwrap_or(false),
        s.get("cooldown").and_then(|x| x.as_f64()).unwrap_or(0.0),
    );
}
