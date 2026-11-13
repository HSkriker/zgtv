//! K线/报价取数编排 —— 对齐 `modules/zgapp/market_data.py` + `cache_store.py` 读路径。
//!
//! 优先级（用户 2026-10-30 裁定）：
//!   盘中日K：TQ → 同花顺 → 东财 → 腾讯 → tdx-master → pytdx（本地 .day 仅 T-1，盘中跳过）
//!   收盘后日K：本地 zgcore → TQ → 同花顺 → 东财 → 腾讯 → datahub → tdx-master → pytdx
//!   周/月：收盘后本地 → TQ → 周K缓存(MAC) → 东财 → 腾讯 → tdx-master → pytdx
//!   → 周/月日线合成 → 日K缓存兜底。
//!   分钟：本地 → TQ → 分钟缓存 → tdx-master → 腾讯。
//! ⚠️ 用户 2026-10-27 裁定：**easy_tdx MAC 不做 Rust 化**，保留为 Python 侧兜底
//! （`vendor/easy_tdx` 删除计划取消）；`mac.rs` 仅保留为门 2 对表工具（`zg mac`），
//! 不进入本链。周K缓存分支沿用 Python 的 "mac" 来源标签（历史口径）。
//! vol 统一「手」（1 手 = 100 股），amount 元。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::Mutex;

use chrono::Datelike;
use serde_json::{json, Map, Value};

use crate::consts;
use crate::kbar::KBar;
use crate::kline::{
    kline_datahub, kline_em, kline_local_dwm, kline_tdx_master, kline_tencent,
    kline_tencent_min, kline_ths, kline_tq_day, kline_tq_min, ths_name_map,
    _src_fail, _src_ok, _src_open,
};
// ---------------------------------------------------------------- 状态

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KlineKey {
    pub code: String,
    pub period: String,
    pub lmt: usize,
    pub end: Option<String>,
}

/// 进程内状态（对齐 `modules/zgapp/state.py` 的 K 线相关字段）。
pub struct MarketState {
    pub weekly_cache: Mutex<Option<zg_core::kcache::KCacheStore>>,
    pub weekly_names: Mutex<HashMap<String, String>>,
    pub weekly_date: Mutex<String>,
    pub weekly_ready: Mutex<bool>,
    pub min_cache: Mutex<HashMap<String, zg_core::kcache::KCacheStore>>,
    pub min_names: Mutex<HashMap<String, String>>,
    pub min_date: Mutex<String>,
    pub min_marks: Mutex<HashMap<String, String>>,
    pub min_ready: Mutex<bool>,
    pub daily_cache: Mutex<Option<zg_core::kcache::KCacheStore>>,
    pub daily_date: Mutex<String>,
    pub kline_cache: Mutex<HashMap<KlineKey, (f64, Value)>>,
}

impl MarketState {
    pub fn new() -> Self {
        Self {
            weekly_cache: Mutex::new(None),
            weekly_names: Mutex::new(HashMap::new()),
            weekly_date: Mutex::new(String::new()),
            weekly_ready: Mutex::new(false),
            min_cache: Mutex::new(HashMap::new()),
            min_names: Mutex::new(HashMap::new()),
            min_date: Mutex::new(String::new()),
            min_marks: Mutex::new(HashMap::new()),
            min_ready: Mutex::new(false),
            daily_cache: Mutex::new(None),
            daily_date: Mutex::new(String::new()),
            kline_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MarketState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 周月合成

/// 日线合成周K/月K：week 按周一日期分组、month 按自然月聚合。
pub fn synth_wm_from_daily(bars: &[KBar], period: &str) -> Vec<KBar> {
    let mut out: Vec<KBar> = Vec::new();
    let mut key: Option<String> = None;
    let mut cur: Option<KBar> = None;
    for b in bars {
        let d: String = b.date.chars().take(10).collect();
        if d.len() < 10 {
            continue;
        }
        let k = if period == "week" {
            let y: i32 = d[0..4].parse().unwrap_or(0);
            let m: u32 = d[5..7].parse().unwrap_or(1);
            let day: u32 = d[8..10].parse().unwrap_or(1);
            match chrono::NaiveDate::from_ymd_opt(y, m, day) {
                Some(dt) => (dt - chrono::Duration::days(dt.weekday().num_days_from_monday() as i64))
                    .format("%Y-%m-%d").to_string(),
                None => continue,
            }
        } else {
            d[..7].to_string()
        };
        if key.as_deref() != Some(k.as_str()) {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            key = Some(k);
            cur = Some(KBar {
                date: d.clone(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                vol: b.vol,
                amount: b.amount,
            });
        } else if let Some(c) = cur.as_mut() {
            c.date = d;
            c.high = c.high.max(b.high);
            c.low = c.low.min(b.low);
            c.close = b.close;
            c.vol += b.vol;
            c.amount += b.amount;
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// 周/月K合成主路径：datahub 拉足量日线 → 聚合；失败再退日K持久化缓存。
pub fn get_wm_via_synth(state: &MarketState, code: &str, period: &str, want: usize)
    -> Option<Vec<Value>> {
    let per = if period == "week" { 5 } else { 21 };
    let dbars = kline_datahub(code, (want * per + 60).min(1500)).ok();
    if let Some((_, dbars)) = dbars {
        let wbars = synth_wm_from_daily(&dbars, period);
        if !wbars.is_empty() {
            return Some(tail_values(&wbars, want));
        }
    }
    let dbars = get_daily_k_bars(state, code);
    if let Some(dbars) = dbars {
        let parsed: Vec<KBar> = dbars.iter().filter_map(KBar::from_value).collect();
        let wbars = synth_wm_from_daily(&parsed, period);
        if !wbars.is_empty() {
            return Some(tail_values(&wbars, want));
        }
    }
    None
}

fn tail_values(bars: &[KBar], want: usize) -> Vec<Value> {
    let mut v: Vec<Value> = bars.iter().map(KBar::to_value_ohlc).collect();
    if want > 0 && v.len() > want {
        v = v[v.len() - want..].to_vec();
    }
    v
}

// ---------------------------------------------------------------- pytdx（最后兜底）

/// pytdx 网络兜底。生产环境（czsc102）未安装 pytdx → Python 侧 `_TdxHq_API is None`，
/// `kline_pytdx` 恒抛 "tdx 未连接"；Rust 侧保持同一行为（行为等价，非协议移植）。
/// 若后续需要真实兜底，可移植 vendored easy_tdx 的 0x010C/0x052D 标准协议。
fn kline_pytdx(_code: &str, _period: &str, _count: usize)
    -> Result<(Option<String>, Vec<KBar>), String> {
    Err("tdx 未连接".into())
}

// ---------------------------------------------------------------- 日/周/月编排

#[allow(clippy::too_many_arguments)]
pub fn get_kline(
    state: &MarketState,
    code: &str,
    klt: i64,
    lmt: usize,
    beg: Option<&str>,
    end: Option<&str>,
    _prefer_etdx: bool,
    skip_wk: bool,
    mut src: Option<&mut Vec<String>>,
) -> Result<(Option<String>, Vec<KBar>), String> {
    let period = match klt {
        101 => "day",
        102 => "week",
        103 => "month",
        _ => "day",
    };
    // 交易日 09:25–15:05（含午休）都算“盘中”：本地 .day 只有 T-1，不能进图
    let now = chrono::Local::now();
    let hm = now.format("%H:%M").to_string();
    let today = now.format("%Y-%m-%d").to_string();
    let session_today = hm.as_str() >= "09:25"
        && hm.as_str() < "15:05"
        && zg_core::calendar::last_trading_date(Some(now)) == today;
    // 盘中查历史窗口（end 早于今天）仍应走本地：本地数据是完整的历史
    let end_historical = end
        .map(|e| {
            let e10: String = e.chars().take(10).collect::<String>().replace('-', "");
            !e10.is_empty() && e10 < today.replace('-', "")
        })
        .unwrap_or(false);
    if (!session_today || end_historical) && consts::kline_local_on() {
        match kline_local_dwm(code, period, lmt, beg, end) {
            Ok((name, bars)) if !bars.is_empty() => {
                if let Some(s) = src.as_mut() {
                    s.push("local".into());
                }
                return Ok((name, bars));
            }
            _ => {}
        }
    }
    match kline_tq_day(code, period, lmt, beg) {
        Ok((name, bars)) if !bars.is_empty() => {
            if let Some(s) = src.as_mut() {
                s.push("tq".into());
            }
            return Ok((name, bars));
        }
        _ => {}
    }
    // 同花顺：历史日K开盘后含当天 bar；只服务日线
    if period == "day" && _src_open("ths") && crate::ths::enabled() {
        match kline_ths(code, lmt, beg) {
            Ok((name, bars)) if !bars.is_empty() => {
                if let Some(s) = src.as_mut() {
                    s.push("ths".into());
                }
                _src_ok("ths");
                return Ok((name, bars));
            }
            _ => _src_fail("ths"),
        }
    }
    if period == "week" && !skip_wk && beg.is_none() {
        if let Some(wbars) = get_weekly_bars(state, code, lmt) {
            if let Some(s) = src.as_mut() {
                s.push("mac".into());
            }
            // Python `state.weekly_names.get(code) or ths_name_map().get(code)`：空串回退
            let name = state.weekly_names.lock_ok().get(code).cloned()
                .filter(|n| !n.is_empty())
                .or_else(|| ths_name_map().get(code).cloned());
            let bars: Vec<KBar> = wbars.iter().filter_map(KBar::from_value).collect();
            return Ok((name, bars));
        }
    }
    if _src_open("em") {
        if let Ok(v) = kline_em(code, klt, lmt, beg) {
            if let Some(s) = src.as_mut() {
                s.push("em".into());
            }
            return Ok(v);
        }
    }
    if _src_open("tx") {
        let tcount = if beg.is_none() { lmt } else { 800 };
        match kline_tencent(code, period, tcount) {
            Ok((name, mut bars)) => {
                if let Some(b) = beg {
                    let bstr = format!("{}-{}-{}", &b[0..4], &b[4..6], &b[6..8]);
                    bars.retain(|x| x.date >= bstr);
                }
                if !bars.is_empty() {
                    if let Some(s) = src.as_mut() {
                        s.push("tx".into());
                    }
                    _src_ok("tx");
                    return Ok((name, bars));
                }
            }
            _ => _src_fail("tx"),
        }
    }
    if period == "day" && beg.is_none() && !session_today {
        if let Ok(v) = kline_datahub(code, lmt) {
            if let Some(s) = src.as_mut() {
                s.push("datahub".into());
            }
            return Ok(v);
        }
    }
    {
        let tcount = if beg.is_none() { lmt } else { 800 };
        if let Ok((name, bars)) = kline_tdx_master(code, period, tcount, beg) {
            if let Some(s) = src.as_mut() {
                s.push("tdx".into());
            }
            return Ok((name, bars));
        }
    }
    {
        let tcount = if beg.is_none() { lmt } else { 800 };
        let mut bars: Vec<KBar> = match kline_pytdx(code, period, tcount) {
            Ok((_, bars)) => bars,
            Err(_) => Vec::new(),
        };
        if let Some(b) = beg {
            let bstr = format!("{}-{}-{}", &b[0..4], &b[4..6], &b[6..8]);
            bars.retain(|x| x.date >= bstr);
        }
        if bars.is_empty() && matches!(period, "week" | "month") {
            let wbars = get_wm_via_synth(state, code, period, if beg.is_none() { lmt } else { 800 });
            if let Some(wbars) = wbars {
                let mut wbars = wbars;
                if let Some(b) = beg {
                    let bstr = format!("{}-{}-{}", &b[0..4], &b[4..6], &b[6..8]);
                    wbars.retain(|x| x.as_str().map(|s| s >= bstr.as_str()).unwrap_or(false));
                }
                return Ok((None, wbars.iter().filter_map(KBar::from_value).collect()));
            }
        }
        if bars.is_empty() && period == "day" {
            if let Some(mut cached) = get_daily_k_bars(state, code) {
                if let Some(b) = beg {
                    let bstr = format!("{}-{}-{}", &b[0..4], &b[4..6], &b[6..8]);
                    cached.retain(|x| x.get("date").and_then(|v| v.as_str()).unwrap_or("").cmp(bstr.as_str()).is_ge());
                }
                if lmt > 0 && cached.len() > lmt && beg.is_none() {
                    cached = cached[cached.len() - lmt..].to_vec();
                }
                if !cached.is_empty() {
                    return Ok((ths_name_map().get(code).cloned(),
                               cached.iter().filter_map(KBar::from_value).collect()));
                }
            }
        }
        if !bars.is_empty() {
            return Ok((None, bars));
        }
    }
    Err("所有K线数据源均失败".into())
}

// ---------------------------------------------------------------- 分钟编排

/// 取日期前 10 个字节（YYYY-MM-DD）；短串或非 ASCII 边界时原样返回，避免 `[..10]` 切片 panic
fn d10(s: &str) -> &str {
    if s.len() >= 10 && s.is_char_boundary(10) { &s[..10] } else { s }
}

fn local_min_fresh(bars: &[KBar]) -> bool {
    let last = bars.last().map(|b| b.date.as_str()).unwrap_or("");
    if zg_core::calendar::market_open_now(None) {
        let cutoff = (chrono::Local::now() - chrono::Duration::minutes(15))
            .format("%Y-%m-%d %H:%M").to_string();
        return last >= cutoff.as_str();
    }
    d10(last).cmp(crate::kline::local_reference_day().as_str()).is_ge()
}

#[allow(clippy::too_many_arguments)]
pub fn get_kline_min(
    state: &MarketState,
    code: &str,
    period: &str,
    lmt: usize,
    end: Option<&str>,
) -> Result<(Option<String>, Vec<Value>, String), String> {
    let want = {
        let d = consts::MINUTE_LMT.iter().find(|(p, _)| *p == period).map(|(_, v)| *v).unwrap_or(480);
        if lmt > 0 { lmt } else { d }
    };
    if consts::min_local_on() {
        match zg_core::reader::resolve(code, None) {
            Ok((mkt, cd)) if mkt != zg_core::reader::MKT_BJ => {
                let freq = consts::MINUTE_FREQ.iter().find(|(p, _)| *p == period)
                    .map(|(_, v)| *v).unwrap_or("F30");
                match zg_core::api::minute_bars(&mkt, &cd, freq, want) {
                    Ok((lbars, meta)) => {
                        let kbars: Vec<KBar> = lbars.iter().map(|b| KBar {
                            date: b.date.clone(),
                            open: zg_core::model::py_round(b.open, 3),
                            high: zg_core::model::py_round(b.high, 3),
                            low: zg_core::model::py_round(b.low, 3),
                            close: zg_core::model::py_round(b.close, 3),
                            vol: b.volume / 100.0,
                            amount: b.amount,
                        }).collect();
                        if local_min_fresh(&kbars) {
                            let mut bars: Vec<Value> = kbars.iter().map(|b| b.to_value()).collect();
                            if let Some(e) = end {
                                bars.retain(|b| d10(b.get("date").and_then(|d| d.as_str()).unwrap_or("")).cmp(e).is_le());
                            }
                            if !bars.is_empty() {
                                return Ok((ths_name_map().get(code).cloned(), bars, "local".into()));
                            }
                        }
                        eprintln!("[min-local] {code} {period} 陈旧 last={} → 换源", meta.last);
                    }
                    Err(e) => {
                        let es = e.to_string();
                        if !es.contains("北交所") {
                            eprintln!("[min-local] {code} {period} 不可用: {es}");
                        }
                    }
                }
            }
            // 北交所：Python 仅当异常文本含「北交所」时不打印 → 静默跳过本地源
            Ok(_) => {}
            Err(e) => {
                let es = e.to_string();
                if !es.contains("北交所") {
                    eprintln!("[min-local] {code} {period} 不可用: {es}");
                }
            }
        }
    }
    if let Ok((name, bars)) = kline_tq_min(code, period, want) {
        let mut bars: Vec<Value> = bars.iter().map(|b| b.to_value()).collect();
        if let Some(e) = end {
            bars.retain(|b| d10(b.get("date").and_then(|d| d.as_str()).unwrap_or("")).cmp(e).is_le());
        }
        if !bars.is_empty() {
            return Ok((name, bars, "tq".into()));
        }
    }
    if let Some(cbars) = get_min_bars(state, code, period, want) {
        let name = state.min_names.lock_ok().get(code).cloned()
            .or_else(|| ths_name_map().get(code).cloned());
        let mut bars = cbars;
        if let Some(e) = end {
            bars.retain(|b| d10(b.get("date").and_then(|d| d.as_str()).unwrap_or("")).cmp(e).is_le());
        }
        if !bars.is_empty() {
            return Ok((name, bars, "cache".into()));
        }
    }
    if let Ok((name, bars)) = kline_tdx_master(code, period, want, None) {
        let mut bars: Vec<Value> = bars.iter().map(|b| b.to_value()).collect();
        if let Some(e) = end {
            bars.retain(|b| d10(b.get("date").and_then(|d| d.as_str()).unwrap_or("")).cmp(e).is_le());
        }
        if !bars.is_empty() {
            return Ok((name, bars, "tdx".into()));
        }
    }
    match kline_tencent_min(code, period, want) {
        Ok((name, bars)) => {
            let mut bars: Vec<Value> = bars.iter().map(|b| b.to_value()).collect();
            if let Some(e) = end {
                bars.retain(|b| d10(b.get("date").and_then(|d| d.as_str()).unwrap_or("")).cmp(e).is_le());
            }
            Ok((name, bars, "tencent".into()))
        }
        Err(_) => Ok((ths_name_map().get(code).cloned(), Vec::new(), String::new())),
    }
}

// ---------------------------------------------------------------- 对外 api_kline

pub fn api_kline(
    state: &MarketState,
    code: &str,
    period: &str,
    lmt: usize,
    end: Option<&str>,
) -> Result<Value, String> {
    let key = KlineKey { code: code.to_string(), period: period.to_string(), lmt, end: end.map(|s| s.to_string()) };
    let ttl = if period.starts_with("min") {
        ttl_secs(15, 300)
    } else {
        ttl_secs(if period == "day" { 45 } else { 300 }, 3600)
    };
    {
        let cache = state.kline_cache.lock_ok();
        if let Some(ent) = cache.get(&key) {
            if now() - ent.0 < ttl as f64 {
                return Ok(ent.1.clone());
            }
        }
    }
    let requested_lmt = lmt;
    let mut lmt = lmt;
    let klt = match period {
        "day" => 101,
        "week" => 102,
        "month" => 103,
        _ => 101,
    };
    if end.is_some() && period == "day" {
        lmt = lmt.max(400);
    }
    let (name, mut bars_v, source) = if period.starts_with("min") {
        if consts::is_bj_code(code) {
            return Err("北交所暂不支持分钟级数据".into());
        }
        get_kline_min(state, code, period, lmt, end)?
    } else {
        // Python：get_kline 不传 beg（end 过滤在下方统一做）
        let mut src: Vec<String> = Vec::new();
        let (n, b) = get_kline(state, code, klt, lmt, None, end, true, false, Some(&mut src))?;
        let source = src.first().cloned().unwrap_or_default();
        let ohlc_order = source == "local";
        let vals: Vec<Value> = b.iter().map(|x| {
            if ohlc_order { x.to_value_ohlc() } else { x.to_value() }
        }).collect();
        (n, vals, source)
    };
    if let Some(e) = end {
        bars_v.retain(|x| d10(x.get("date").and_then(|v| v.as_str()).unwrap_or("")).cmp(e).is_le());
        // 远端兜底只按尾部窗口取数：end 早于窗口时明确报错，不再静默返回空图
        if bars_v.is_empty() {
            return Err(format!("end={e} 早于可用数据窗口或区间内无K线"));
        }
        // end 时上面把 lmt 抬到 400 是给远端兜底留窗口；这里按调用方原始 lmt 截尾，恢复 lmt 语义
        if requested_lmt > 0 && bars_v.len() > requested_lmt {
            bars_v = bars_v[bars_v.len() - requested_lmt..].to_vec();
        }
    }
    // candle 引擎：TA-Lib 官方 Rust 移植（ta-lib crate）+ 规则层（candles.rs）
    let candle_result = if consts::candle_engine() == "legacy" {
        json!({"engine": "legacy", "signals": Vec::<Value>::new()})
    } else if !bars_v.is_empty() {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let last10 = bars_v.last()
            .and_then(|b| b.get("date")).and_then(|d| d.as_str())
            .map(|s| s.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let provisional_last = zg_core::calendar::market_open_now(None) && last10 == today;
        let patterns = consts::candle_patterns();
        let ps = if patterns == "all" { None } else { Some(patterns.as_str()) };
        crate::candles::analyze_candles(&bars_v, period, provisional_last, ps)
    } else {
        json!({"engine": "unavailable", "signals": Vec::<Value>::new()})
    };
    let bars_empty = bars_v.is_empty();
    let last_date = bars_v.last()
        .and_then(|b| b.get("date")).and_then(|d| d.as_str())
        .map(|s| s[..10.min(s.len())].to_string())
        .unwrap_or_default();
    let mut data = Map::new();
    data.insert("code".into(), Value::String(code.to_string()));
    data.insert("name".into(), name.map(Value::String).unwrap_or(Value::Null));
    data.insert("period".into(), Value::String(period.to_string()));
    data.insert("bars".into(), Value::Array(bars_v));
    data.insert("source".into(), Value::String(source.clone()));
    data.insert("min_period".into(), Value::Bool(period.starts_with("min")));
    data.insert("candle_engine".into(),
                candle_result.get("engine").cloned().unwrap_or_else(|| json!("unavailable")));
    data.insert("candle_pattern_set".into(), Value::String(consts::candle_patterns()));
    data.insert("candle_signals".into(),
                candle_result.get("signals").cloned().unwrap_or_else(|| json!([])));
    if period == "day" && !bars_empty {
        let src_label = match source.as_str() {
            "local" => "本地DFZQ",
            "tq" => "TQ-Local",
            "ths" => "同花顺",
            "em" => "东财",
            "tx" => "腾讯",
            "datahub" => "Datahub",
            "mac" => "MAC",
            "tdx" => "tdx-master",
            _ => "实时行情",
        };
        data.insert("data_source".into(), Value::String(src_label.to_string()));
        data.insert("cache_date".into(), Value::String(last_date));
    }
    let data = Value::Object(data);
    cache_kline_api(state, code, period, requested_lmt, &data, end);
    Ok(data)
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Python `cache_store.ttl(base_intraday, base_closed)`。
pub fn ttl_secs(base_intraday: u64, base_closed: u64) -> u64 {
    if zg_core::calendar::market_open_now(None) { base_intraday } else { base_closed }
}

/// `cache_store.local_data_date`：本地日线末根日期（sh000001.day 最后 32B），
/// 失败回落日历推算。
pub fn local_data_date() -> String {
    let fp = zg_core::paths::get()
        .vipdoc
        .join("sh")
        .join("lday")
        .join("sh000001.day");
    if let Ok(bytes) = std::fs::read(&fp) {
        if bytes.len() >= 32 {
            let raw = &bytes[bytes.len() - 32..];
            let d = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            if d >= 19900101 {
                return format!("{:04}-{:02}-{:02}", d / 10000, d / 100 % 100, d % 100);
            }
        }
    }
    zg_core::calendar::last_trading_date(None)
}

// ---------------------------------------------------------------- cache_store 读路径

/// Value 层的分钟规范化：只改 `date`（13:00 → 11:30 / 去重），**保留数值原类型**
/// （kcache dicts 的 vol/amount 是整数，Python 输出整数；经 KBar 转换会变成 float）。
pub fn normalize_minute_values(bars: &[Value], period: &str) -> Vec<Value> {
    if !period.starts_with("min") {
        return bars.to_vec();
    }
    let times: std::collections::HashSet<String> = bars
        .iter()
        .filter_map(|b| b.get("date").and_then(|d| d.as_str()).map(|s| s.chars().take(16).collect()))
        .collect();
    let previous = if period == "min30" { "11:00" } else { "10:30" };
    let mut out: Vec<Value> = Vec::new();
    for b in bars {
        let stamp = b.get("date").and_then(|d| d.as_str()).unwrap_or("").to_string();
        let mut b = b.clone();
        if stamp.len() >= 16 && &stamp[11..16] == "13:00" {
            let corrected = format!("{}11:30", &stamp[..11]);
            if times.contains(&corrected) {
                continue;
            }
            if times.contains(&format!("{}{}", &stamp[..11], previous)) {
                if let Some(o) = b.as_object_mut() {
                    o.insert("date".into(), Value::String(format!("{}{}", corrected, &stamp[16..])));
                }
            }
        }
        out.push(b);
    }
    out.sort_by(|a, b| {
        let da = a.get("date").and_then(|d| d.as_str()).unwrap_or("");
        let db = b.get("date").and_then(|d| d.as_str()).unwrap_or("");
        da.cmp(db)
    });
    out
}

pub fn normalize_minute_bars(bars: &[KBar], period: &str) -> Vec<KBar> {
    if !period.starts_with("min") {
        return bars.to_vec();
    }
    let times: std::collections::HashSet<String> =
        bars.iter().map(|b| b.date.chars().take(16).collect()).collect();
    let previous = if period == "min30" { "11:00" } else { "10:30" };
    let mut out: Vec<KBar> = Vec::new();
    for b in bars {
        let stamp = &b.date;
        let mut b = b.clone();
        if stamp.len() > 15 && &stamp[11..16] == "13:00" {
            let corrected = format!("{}11:30", &stamp[..11]);
            if times.contains(&corrected) {
                continue;
            }
            if times.contains(&format!("{}{}", &stamp[..11], previous)) {
                b.date = format!("{}{}", corrected, &stamp[16..]);
            }
        }
        out.push(b);
    }
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

pub fn load_daily_cache(state: &MarketState) -> bool {
    let data = zg_core::paths::get().market_data;
    let prefix = data.join("daily_k_cache").to_string_lossy().to_string();
    let st = zg_core::kcache::KCacheStore::load(&prefix, "day", consts::DAILY_K_LMT);
    match st {
        Some(st) => {
            let date = st.date.clone();
            let n = st.len();
            *state.daily_date.lock_ok() = date.clone();
            *state.daily_cache.lock_ok() = Some(st);
            eprintln!("[daily_k] 已从本地加载日K缓存: {n} 只, 日期={date}");
            true
        }
        None => {
            eprintln!("[daily_k] 尚无本地日K缓存，可在页面点“构建缓存”重建");
            false
        }
    }
}

pub fn load_weekly_cache(state: &MarketState) -> bool {
    let data = zg_core::paths::get().market_data;
    let prefix = data.join("weekly_k_cache").to_string_lossy().to_string();
    let st = zg_core::kcache::KCacheStore::load(&prefix, "day", consts::WEEKLY_K_LMT);
    match st {
        Some(st) => {
            let mut names = HashMap::new();
            for (k, v) in &st.names {
                names.insert(k.clone(), v.clone());
            }
            *state.weekly_date.lock_ok() = st.date.clone();
            *state.weekly_cache.lock_ok() = Some(st);
            *state.weekly_names.lock_ok() = names;
            *state.weekly_ready.lock_ok() = true;
            let n = state.weekly_cache.lock_ok().as_ref().map(|s| s.len()).unwrap_or(0);
            let date = state.weekly_date.lock_ok().clone();
            eprintln!("[weekly_k] 已从本地加载周K缓存: {n} 只, 日期={date}");
            true
        }
        None => {
            eprintln!("[weekly_k] 尚无本地周K缓存，将在后台用 easy_tdx 构建");
            false
        }
    }
}

pub fn load_min_cache(state: &MarketState) -> bool {
    let data = zg_core::paths::get().market_data;
    let mut stores: HashMap<String, zg_core::kcache::KCacheStore> = HashMap::new();
    for p in consts::MIN_K_PERIODS {
        let lmt = consts::MIN_K_LMT.iter().find(|(pp, _)| *pp == *p).map(|(_, v)| *v).unwrap_or(480);
        let prefix = data.join(format!("{p}_k_cache")).to_string_lossy().to_string();
        if let Some(st) = zg_core::kcache::KCacheStore::load(&prefix, "min", lmt) {
            stores.insert(p.to_string(), st);
        }
    }
    if stores.is_empty() {
        eprintln!("[min_k] 尚无本地分钟K缓存，将在后台用 easy_tdx 构建");
        return false;
    }
    let mut names = HashMap::new();
    for st in stores.values() {
        for (k, v) in &st.names {
            names.insert(k.clone(), v.clone());
        }
    }
    let date = stores.values().map(|s| s.date.clone()).max().unwrap_or_default();
    *state.min_cache.lock_ok() = stores;
    *state.min_names.lock_ok() = names;
    *state.min_date.lock_ok() = date;
    *state.min_ready.lock_ok() = true;
    // Python `_load_minute_marks`
    let marks_path = zg_core::paths::get().market_data.join("min_k_marks.json");
    if let Ok(text) = std::fs::read_to_string(marks_path) {
        if let Ok(Value::Object(mk)) = serde_json::from_str::<Value>(&text) {
            if !mk.is_empty() {
                let mut mm = state.min_marks.lock_ok();
                for (k, v) in mk {
                    if let Some(s) = v.as_str() {
                        mm.insert(k, s.to_string());
                    }
                }
            }
        }
    }
    eprintln!("[min_k] 已从本地加载分钟K缓存: 日期={}", *state.min_date.lock_ok());
    true
}

pub fn get_daily_k_bars(state: &MarketState, code: &str) -> Option<Vec<Value>> {
    let guard = state.daily_cache.lock_ok();
    let store = guard.as_ref()?;
    store.dicts(code)
}

pub fn get_weekly_bars(state: &MarketState, code: &str, lmt: usize) -> Option<Vec<Value>> {
    if !*state.weekly_ready.lock_ok() {
        return None;
    }
    let guard = state.weekly_cache.lock_ok();
    let store = guard.as_ref()?;
    let mut bars = store.dicts(code)?;
    if bars.is_empty() {
        return None;
    }
    if lmt > 0 && bars.len() > lmt {
        bars = bars[bars.len() - lmt..].to_vec();
    }
    Some(bars)
}

pub fn get_min_bars(state: &MarketState, code: &str, period: &str, lmt: usize) -> Option<Vec<Value>> {
    if !*state.min_ready.lock_ok() {
        return None;
    }
    let guard = state.min_cache.lock_ok();
    let store = guard.get(period)?;
    let mut bars = store.dicts(code)?;
    if bars.is_empty() {
        return None;
    }
    if lmt > 0 && bars.len() > lmt {
        bars = bars[bars.len() - lmt..].to_vec();
    }
    Some(normalize_minute_values(&bars, period))
}

pub fn cache_kline_api(state: &MarketState, code: &str, period: &str, lmt: usize,
                       data: &Value, end: Option<&str>) {
    let mut cache = state.kline_cache.lock_ok();
    let key = KlineKey {
        code: code.to_string(),
        period: period.to_string(),
        lmt,
        end: end.map(|s| s.to_string()),
    };
    cache.insert(key, (now(), data.clone()));
    if cache.len() > 500 {
        if let Some(k) = cache.iter().min_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.clone()) {
            cache.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalize_1300_to_1130() {
        // min30：13:00 尾根在 11:00 根存在时改标 11:30（对齐 Python 三段逻辑）
        let bars = vec![
            KBar { date: "2026-10-27 11:00".into(), open: 1.0, high: 1.0, low: 1.0, close: 1.0, vol: 1.0, amount: 1.0 },
            KBar { date: "2026-10-27 13:00".into(), open: 2.0, high: 2.0, low: 2.0, close: 2.0, vol: 2.0, amount: 2.0 },
            KBar { date: "2026-10-27 13:30".into(), open: 3.0, high: 3.0, low: 3.0, close: 3.0, vol: 3.0, amount: 3.0 },
        ];
        let out = normalize_minute_bars(&bars, "min30");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].date, "2026-10-27 11:00");
        assert_eq!(out[1].date, "2026-10-27 11:30");
        assert_eq!(out[2].date, "2026-10-27 13:30");
    }

    #[test]
    fn normalize_drops_duplicate_1300() {
        let bars = vec![
            KBar { date: "2026-10-27 11:30".into(), open: 1.0, high: 1.0, low: 1.0, close: 1.0, vol: 1.0, amount: 1.0 },
            KBar { date: "2026-10-27 13:00".into(), open: 2.0, high: 2.0, low: 2.0, close: 2.0, vol: 2.0, amount: 2.0 },
        ];
        let out = normalize_minute_bars(&bars, "min30");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].date, "2026-10-27 11:30");
    }

    #[test]
    fn kline_cache_evicts_over_500() {
        let state = MarketState::new();
        for i in 0..510 {
            let code = format!("{:06}", i);
            let key = KlineKey { code: code.clone(), period: "day".into(), lmt: 300, end: None };
            let data = json!({"code": code});
            cache_kline_api(&state, &code, "day", 300, &data, None);
            assert!(state.kline_cache.lock_ok().contains_key(&key));
        }
        assert!(state.kline_cache.lock_ok().len() <= 500);
    }

    #[test]
    fn pytdx_stub_raises() {
        assert!(kline_pytdx("600519", "day", 100).is_err());
    }

    #[test]
    fn synth_wm_from_daily_grouping() {
        let bars = vec![
            KBar { date: "2026-10-27".into(), open: 1.0, high: 3.0, low: 1.0, close: 2.0, vol: 100.0, amount: 1000.0 },
            KBar { date: "2026-10-28".into(), open: 2.0, high: 5.0, low: 2.0, close: 4.0, vol: 200.0, amount: 2000.0 },
            KBar { date: "2026-11-02".into(), open: 4.0, high: 6.0, low: 3.0, close: 5.0, vol: 300.0, amount: 3000.0 },
        ];
        let w = synth_wm_from_daily(&bars, "week");
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].date, "2026-10-28");
        assert_eq!(w[0].high, 5.0);
        assert_eq!(w[0].vol, 300.0);
        let m = synth_wm_from_daily(&bars, "month");
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].date, "2026-11-02");
    }
}
