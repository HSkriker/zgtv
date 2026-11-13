//! 除权事件源 —— 与 `zgcore/bars/events.py` 等价：共享 tdx_xdxr 缓存优先，
//! 陈旧/缺失回落本地 gbbq。
//!
//! 铁律（工单 §16）：
//!   - 共享缓存是全项目唯一除权事件源；缓存陈旧 = 新公告事件缺席 ⇒ 假跳空；
//!   - 新鲜度判据：(fetched_at + 8h).date() >= 末根K线日期（fetched_at 是 UTC）；
//!   - 两份都拿不到时绝不静默输出未复权价：gbbq 载入失败直接抛 BridgeError。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, ZgError};
use crate::gbbq;
use crate::paths;
use crate::reader::{lru_put, LruMap};
use crate::sync::MutexExt;

pub const XDXR_MEM_MAX: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct XdxrEvent {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub category: i32,
    pub fenhong: f64,
    pub songzhuangu: f64,
    pub peigu: f64,
    pub peigujia: f64,
}

impl XdxrEvent {
    pub fn date_key(&self) -> i32 {
        self.year * 10000 + self.month * 100 + self.day
    }
}

fn log(msg: &str) {
    eprintln!("[zgcore] {msg}");
}

/// 逐股事件日志限流：全市场扫描一次 5000+ 只，每只一行会把日志刷爆
/// （实测 4.5 分钟 42MB，且同步控制台输出会拖慢扫描）。
/// 默认只打前 20 条 + 一条折叠提示；`ZG_XDXR_LOG=1` 恢复全量。
fn log_xdxr(msg: &str) {
    static N: AtomicUsize = AtomicUsize::new(0);
    if std::env::var("ZG_XDXR_LOG").map(|v| v == "1").unwrap_or(false) {
        log(msg);
        return;
    }
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n < 20 {
        log(msg);
    } else if n == 20 {
        log("（其余逐股除权缓存日志已折叠；设 ZG_XDXR_LOG=1 可全量输出）");
    }
}

/// 供 adjust 等模块使用的限流告警出口（全市场扫描时同样折叠）。
pub(crate) fn log_warn(msg: &str) {
    log_xdxr(msg);
}

// ---------------------------------------------------------------------------
// gbbq 全量
// ---------------------------------------------------------------------------

type GbbqMap = HashMap<(String, String), Vec<XdxrEvent>>;

fn gbbq_state() -> &'static Mutex<Option<GbbqMap>> {
    static ST: OnceLock<Mutex<Option<GbbqMap>>> = OnceLock::new();
    ST.get_or_init(|| Mutex::new(None))
}

/// gbbq 载入统计（对应 Python `_GBBQ_STATS`）。
#[derive(Clone, Debug, Default)]
pub struct GbbqStats {
    /// 文件里的全部记录数（含非 category==1）
    pub records: usize,
    /// category==1 的记录数
    pub cat1: usize,
    /// 去重后的股票数
    pub codes: usize,
    /// 解析耗时（ms）
    pub load_ms: u64,
    /// "parse" / "pickle"（Rust 侧恒 "parse"，Python 侧字段本身是易变量）
    pub source: &'static str,
}

fn gbbq_stats_state() -> &'static Mutex<Option<GbbqStats>> {
    static ST: OnceLock<Mutex<Option<GbbqStats>>> = OnceLock::new();
    ST.get_or_init(|| Mutex::new(None))
}

pub fn gbbq_stats_struct() -> GbbqStats {
    let _ = ensure_gbbq(false);
    gbbq_stats_state()
        .lock_ok()
        .clone()
        .unwrap_or_default()
}

/// 全量解析 gbbq（只取 category==1）→ {(market, code): [event...]}。
/// ⚠️ 返回的是整表副本（约 5.9MB）；只要「确保已载入」时用 `ensure_gbbq`。
pub fn load_gbbq(force: bool) -> Result<GbbqMap> {
    ensure_gbbq(force)?;
    Ok(gbbq_state()
        .lock_ok()
        .as_ref()
        .cloned()
        .unwrap_or_default())
}

/// 用本地 gbbq 同步共享 xdxr 缓存（离线）。
///
/// 只补 gbbq 中缓存缺失的 category==1 事件，并按 gbbq 的 mtime 推进
/// `fetched_at`；缓存比 gbbq 新时不动。带 `.gbbq_sync.json` 标记，gbbq
/// 未变化时直接跳过（`force` 可忽略标记）。
pub fn sync_cache_from_gbbq(force: bool) -> Result<Value> {
    let p = paths::get();
    if !p.gbbq_file.exists() {
        return Err(ZgError::bridge(format!(
            "gbbq 不可用: 文件不存在 {}",
            p.gbbq_file.display()
        )));
    }
    let gbbq_mtime = std::fs::metadata(&p.gbbq_file)
        .and_then(|m| m.modified())
        .map_err(|e| ZgError::bridge(format!("gbbq mtime 读取失败: {e}")))?;
    let gbbq_unix = gbbq_mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let gbbq_dt = DateTime::<Utc>::from(gbbq_mtime);
    let gbbq_iso = gbbq_dt.to_rfc3339();
    let gbbq_naive = gbbq_dt.naive_utc();

    let cache_dir = &p.xdxr_cache_dir;
    std::fs::create_dir_all(cache_dir).ok();
    let marker = cache_dir.join(".gbbq_sync.json");
    if !force {
        if let Ok(txt) = std::fs::read_to_string(&marker) {
            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                if v.get("gbbq_mtime").and_then(|x| x.as_u64()) == Some(gbbq_unix) {
                    return Ok(serde_json::json!({
                        "ok": true, "skipped": true, "reason": "gbbq unchanged",
                        "gbbq_mtime": gbbq_iso,
                    }));
                }
            }
        }
    }

    let table = load_gbbq(false)?;
    let now_iso = Utc::now().to_rfc3339();
    let mut files = 0usize;
    let mut created = 0usize;
    let mut added = 0usize;
    let mut skipped_codes = 0usize;

    for ((market, code), evs) in table.iter() {
        if market != "sh" && market != "sz" {
            skipped_codes += 1;
            continue;
        }
        // 6 位码跨市场可能重名（sh000001 指数 vs sz000001 平安），按前缀只认归属市场
        if crate::reader::market_of_prefix(code) != Some(market.as_str()) {
            skipped_codes += 1;
            continue;
        }
        if crate::reader::is_index(market, code) {
            skipped_codes += 1;
            continue;
        }

        let path = cache_dir.join(format!("{code}.json"));
        let existing_txt = std::fs::read_to_string(&path).ok();
        let mut doc = existing_txt
            .as_deref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .unwrap_or_else(|| serde_json::json!({"code": code, "events": []}));
        let fetched = doc
            .get("fetched_at")
            .and_then(|v| v.as_str())
            .and_then(parse_iso_naive);
        if let Some(f) = fetched {
            // 缓存抓取时间不早于 gbbq 修改时间：缓存本身更新，无需覆盖
            if f >= gbbq_naive {
                continue;
            }
        }

        let mut arr: Vec<Value> = doc
            .get("events")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut have: std::collections::HashSet<i32> = std::collections::HashSet::new();
        for e in &arr {
            if e.get("category").and_then(|v| v.as_i64()) != Some(1) {
                continue;
            }
            let y = e.get("year").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let m = e.get("month").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let d = e.get("day").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            have.insert(y * 10000 + m * 100 + d);
        }
        let mut n_add = 0usize;
        for e in evs {
            if have.contains(&e.date_key()) {
                continue;
            }
            arr.push(serde_json::json!({
                "year": e.year, "month": e.month, "day": e.day,
                "category": 1, "name": "除权除息",
                "fenhong": e.fenhong, "peigujia": e.peigujia,
                "songzhuangu": e.songzhuangu, "peigu": e.peigu,
                "suogu": Value::Null, "panqianliutong": Value::Null,
                "panhouliutong": Value::Null, "qianzongguben": Value::Null,
                "houzongguben": Value::Null, "fenshu": Value::Null,
                "xingquanjia": Value::Null,
            }));
            have.insert(e.date_key());
            n_add += 1;
        }
        // 缺失文件且 gbbq 无事件：不建空表（空表会被读取端当作抓取不完整）
        if existing_txt.is_none() && arr.is_empty() {
            continue;
        }
        arr.sort_by_key(|e| {
            (
                e.get("year").and_then(|v| v.as_i64()).unwrap_or(0),
                e.get("month").and_then(|v| v.as_i64()).unwrap_or(0),
                e.get("day").and_then(|v| v.as_i64()).unwrap_or(0),
                e.get("category").and_then(|v| v.as_i64()).unwrap_or(0),
            )
        });
        let n_cat1 = arr
            .iter()
            .filter(|e| e.get("category").and_then(|v| v.as_i64()) == Some(1))
            .count();
        let Some(obj) = doc.as_object_mut() else { continue };
        obj.insert("code".into(), Value::String(code.clone()));
        obj.insert("fetched_at".into(), Value::String(now_iso.clone()));
        obj.insert("n_events".into(), Value::from(arr.len() as u64));
        obj.insert("n_cat1".into(), Value::from(n_cat1 as u64));
        obj.insert("events".into(), Value::Array(arr));
        obj.insert("synced_from".into(), Value::String("gbbq".into()));
        obj.insert("gbbq_mtime".into(), Value::String(gbbq_iso.clone()));

        let tmp = cache_dir.join(format!(".{code}.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string(&doc).unwrap_or_default()).is_err() {
            continue;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        files += 1;
        if existing_txt.is_none() {
            created += 1;
        }
        added += n_add;
    }

    let stats = serde_json::json!({
        "ok": true, "skipped": false,
        "gbbq_mtime": gbbq_iso,
        "files_updated": files,
        "files_created": created,
        "events_added": added,
        "codes_skipped": skipped_codes,
        "synced_at": now_iso,
    });
    let _ = std::fs::write(
        &marker,
        serde_json::to_string(&serde_json::json!({
            "gbbq_mtime": gbbq_unix,
            "gbbq_mtime_iso": gbbq_iso,
            "synced_at": now_iso,
            "files_updated": files,
            "events_added": added,
        }))
        .unwrap_or_default(),
    );
    Ok(stats)
}

/// 确保 gbbq 已载入缓存（不复制整表）。
pub fn ensure_gbbq(force: bool) -> Result<()> {
    {
        let g = gbbq_state().lock_ok();
        if let Some(m) = g.as_ref() {
            if !force {
                let _ = m;
                return Ok(());
            }
        }
    }
    let p = paths::get();
    if !p.gbbq_file.exists() {
        return Err(ZgError::bridge(format!(
            "gbbq 不可用: 文件不存在 {}",
            p.gbbq_file.display()
        )));
    }
    if !p.vendor_dir.is_dir() {
        return Err(ZgError::bridge(format!(
            "gbbq 不可用: vendor 目录不存在 {}",
            p.vendor_dir.display()
        )));
    }
    let t0 = std::time::Instant::now();
    let recs = gbbq::read_gbbq(&p.gbbq_file)?;
    let mut ev: GbbqMap = HashMap::new();
    let mut n_cat1 = 0usize;
    for r in &recs {
        if r.category != 1 {
            continue;
        }
        n_cat1 += 1;
        let mkt = match r.market {
            1 => "sh",
            0 => "sz",
            _ => "bj",
        };
        let d = r.datetime;
        ev.entry((mkt.to_string(), r.code.clone())).or_default().push(XdxrEvent {
            year: (d / 10000) as i32,
            month: (d / 100 % 100) as i32,
            day: (d % 100) as i32,
            category: 1,
            fenhong: r.hongli_panqianliutong as f64,
            songzhuangu: r.songgu_qianzongguben as f64,
            peigu: r.peigu_houzongguben as f64,
            peigujia: r.peigujia_qianzongguben as f64,
        });
    }
    for v in ev.values_mut() {
        v.sort_by_key(|e| (e.year, e.month, e.day));
    }
    log(&format!(
        "gbbq 载入完成 records={} cat1={} codes={} ({}ms)",
        recs.len(),
        n_cat1,
        ev.len(),
        t0.elapsed().as_millis()
    ));
    let stats = GbbqStats {
        records: recs.len(),
        cat1: n_cat1,
        codes: ev.len(),
        load_ms: t0.elapsed().as_millis() as u64,
        source: "parse",
    };
    *gbbq_stats_state().lock_ok() = Some(stats);
    *gbbq_state().lock_ok() = Some(ev);
    Ok(())
}

/// Python `gbbq_stats()`：`{records, cat1, codes, load_ms, source}`（键序同 Python）。
pub fn gbbq_stats() -> serde_json::Map<String, Value> {
    let st = gbbq_stats_struct();
    let mut m = serde_json::Map::new();
    m.insert("records".into(), Value::from(st.records as u64));
    m.insert("cat1".into(), Value::from(st.cat1 as u64));
    m.insert("codes".into(), Value::from(st.codes as u64));
    m.insert("load_ms".into(), Value::from(st.load_ms));
    m.insert("source".into(), Value::from(st.source));
    m
}

// ---------------------------------------------------------------------------
// 共享缓存
// ---------------------------------------------------------------------------

/// 返回 `(cat1 事件, fetched_at, 原始事件表是否完整)`。
/// 第三个值用于区分「确实无除权史」与「抓取不完整」：文件里 events 非空即完整，
/// 空表但 n_events>0 说明写入被截断，必须回落 gbbq 复核。
fn cache_events(code: &str) -> Option<(Vec<XdxrEvent>, String, bool)> {
    let path = paths::get().xdxr_cache_dir.join(format!("{code}.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log(&format!("{code} 共享除权缓存读取失败（回落 gbbq）: {e}"));
            return None;
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            log(&format!("{code} 共享除权缓存读取失败（回落 gbbq）: {e}"));
            return None;
        }
    };
    let fetched_at = doc
        .get("fetched_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let raw = match doc.get("events").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Some((Vec::new(), fetched_at, false)),
    };
    // 事件表非空即视为完整抓取（0 条 cat1 = 确实无除权史）。
    // 空表才可能是写入被截断（n_events>0），此时必须回落 gbbq 复核。
    let complete = !raw.is_empty();
    let mut ev = Vec::new();
    for e in raw {
        let cat = e.get("category").and_then(as_int_or_zero).unwrap_or(0);
        if cat != 1 {
            continue;
        }
        let int = |k: &str| -> Option<i32> {
            e.get(k).and_then(|v| {
                v.as_i64()
                    .map(|x| x as i32)
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
        };
        let (Some(year), Some(month), Some(day)) = (int("year"), int("month"), int("day")) else {
            log(&format!("{code} 共享除权缓存读取失败（回落 gbbq）: 事件缺少年月日"));
            return None;
        };
        let flt = |k: &str| -> f64 {
            e.get(k)
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                })
                .unwrap_or(0.0)
        };
        ev.push(XdxrEvent {
            year,
            month,
            day,
            category: 1,
            fenhong: flt("fenhong"),
            songzhuangu: flt("songzhuangu"),
            peigu: flt("peigu"),
            peigujia: flt("peigujia"),
        });
    }
    ev.sort_by_key(|e| (e.year, e.month, e.day));
    Some((ev, fetched_at, complete))
}

fn as_int_or_zero(v: &Value) -> Option<i32> {
    v.as_i64()
        .map(|x| x as i32)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// (fetched_at + 8h).date() < 末根日期 ⇒ 陈旧；解析失败 fail-closed。
pub fn cache_stale(fetched_at: &str, anchor: Option<&str>) -> bool {
    let anchor = match anchor {
        Some(a) if !a.is_empty() => a,
        _ => return true,
    };
    if fetched_at.is_empty() {
        return true;
    }
    let fa = fetched_at.trim().replace('Z', "");
    let fa = fa.strip_suffix("+00:00").unwrap_or(&fa).to_string();

    let got: Option<NaiveDateTime> = if !fa.is_empty() && fa.chars().all(|c| c.is_ascii_digit()) {
        fa.parse::<i64>()
            .ok()
            .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0))
            .map(|d| d.naive_utc())
    } else {
        parse_iso_naive(&fa)
    };
    let last = match NaiveDate::parse_from_str(&anchor.chars().take(10).collect::<String>(), "%Y-%m-%d")
    {
        Ok(d) => d,
        Err(_) => return true,
    };
    match got {
        Some(g) => (g + Duration::hours(8)).date() < last,
        None => {
            log(&format!("fetched_at 解析失败({fetched_at:?})，按陈旧处理"));
            true
        }
    }
}

fn parse_iso_naive(s: &str) -> Option<NaiveDateTime> {
    // 带时区偏移：先按 RFC3339 解析再转 UTC
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc).naive_utc());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0);
    }
    None
}

/// key = (market, code, anchor)：market 必须入键，否则 sh/sz 同 6 位码会串味（M25）。
type XdxrKey = (String, String, String);

fn xdxr_mem() -> &'static Mutex<LruMap<XdxrKey, Option<Vec<XdxrEvent>>>> {
    static MEM: OnceLock<Mutex<LruMap<XdxrKey, Option<Vec<XdxrEvent>>>>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(LruMap::new(XDXR_MEM_MAX)))
}

/// 事件来源。空事件表在两种来源下含义完全不同：
/// 共享缓存是全项目唯一权威事件源，其空表 = 「确实无除权史」；
/// gbbq 回落空表只是「本地这份也没记录」，不能据此断定无除权。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventSource {
    SharedCache,
    Gbbq,
}

/// 该 (市场, 代码) 的 category=1 事件（升序）；无除权史返回空表。
pub fn events_of(market: &str, code: &str, anchor: Option<&str>) -> Result<Vec<XdxrEvent>> {
    events_of_detailed(market, code, anchor).map(|(ev, _)| ev)
}

/// 同 `events_of`，额外返回事件来源（H4：区分权威空表与回落空表）。
pub fn events_of_detailed(
    market: &str,
    code: &str,
    anchor: Option<&str>,
) -> Result<(Vec<XdxrEvent>, EventSource)> {
    let code = code.to_string();
    let mkey: XdxrKey = (
        market.to_string(),
        code.clone(),
        anchor.unwrap_or("").to_string(),
    );
    {
        let mut mem = xdxr_mem().lock_ok();
        if let Some(Some(v)) = mem.get_cloned(&mkey) {
            // 只缓存共享缓存结果（见下方 lru_put 注释）
            return Ok((v, EventSource::SharedCache));
        }
    }

    let got = cache_events(&code);
    let value = match got {
        None => {
            log_xdxr(&format!("{market}{code} 共享缓存无事件文件，回落本地 gbbq"));
            None
        }
        Some((ev, fetched_at, complete)) => {
            if !complete {
                // events 为空但可能写过 n_events：无法区分「确实无事件」与「写入被截断」
                log_xdxr(&format!("{market}{code} 共享缓存事件表为空，回落本地 gbbq 复核"));
                None
            } else if ev.is_empty() {
                // 原始事件表非空但无 cat1：权威的「无分红送转」，直接返回空表
                log_xdxr(&format!("{market}{code} 共享缓存无 cat1 事件（无除权史）"));
                Some(ev)
            } else if anchor.is_none() {
                log_xdxr(&format!(
                    "{market}{code} 无 anchor（未传末根日），无法判缓存新鲜度，按旧行为直接用缓存"
                ));
                Some(ev)
            } else if cache_stale(&fetched_at, anchor) {
                log_xdxr(&format!(
                    "{market}{code} 共享缓存陈旧(fetched_at={fetched_at} 早于末根 {})，回落 gbbq",
                    anchor.unwrap_or("")
                ));
                None
            } else {
                Some(ev)
            }
        }
    };
    if let Some(v) = value {
        let mut mem = xdxr_mem().lock_ok();
        // 只缓存共享缓存的成功结果：回落 gbbq 的结果一旦缓存，缓存补数后仍会
        // 命中旧值直到 anchor 变化（M25）。
        lru_put(&mut mem, mkey, Some(v.clone()), None);
        return Ok((v, EventSource::SharedCache));
    }
    Ok((gbbq_get(market, &code)?, EventSource::Gbbq))
}

/// 确保 gbbq 已载入（不克隆整表），再取单只股票的事件。
pub fn gbbq_get(market: &str, code: &str) -> Result<Vec<XdxrEvent>> {
    {
        let g = gbbq_state().lock_ok();
        if g.is_none() {
            drop(g);
            ensure_gbbq(true)?;
        }
    }
    let g = gbbq_state().lock_ok();
    Ok(g.as_ref()
        .and_then(|m| m.get(&(market.to_string(), code.to_string())).cloned())
        .unwrap_or_default())
}

/// 测试辅助：清空事件缓存与 gbbq 全局态。
pub fn clear_caches() {
    xdxr_mem().lock_ok().clear();
    *gbbq_state().lock_ok() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_cases_match_python() {
        let cases = [
            ("2026-10-23T01:00:00Z", Some("2026-10-23"), false),
            ("2026-10-23T12:00:00+00:00", Some("2026-10-23"), false),
            ("2026-10-20T00:00:00Z", Some("2026-10-23"), true),
            ("1792748475", Some("2026-10-23"), false),
            ("", Some("2026-10-23"), true),
            ("bad", Some("2026-10-23"), true),
            ("2026-10-23T01:00:00", None, true),
        ];
        for (fa, anchor, want) in cases {
            assert_eq!(cache_stale(fa, anchor), want, "fa={fa:?} anchor={anchor:?}");
        }
    }
}
