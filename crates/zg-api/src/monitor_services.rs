//! 8787 数据服务 —— 对齐 `monitor_app/services/{limitup,etf}.py`。
//! commodity（期货联动）功能按用户裁定（2026-10-27）下线，不再实现。

use zg_core::sync::MutexExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::monitor_ths as ths;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------- 涨停观察

const LU_TTL: f64 = 30.0;
const LU_TTL_LADDER: f64 = 600.0;
const LU_MAX_PAGES: usize = 5;

struct LuState {
    ts: f64,
    data: Option<Value>,
    ladder: Option<Value>,
    ladder_ts: f64,
    /// 最近一次抓取尝试的结束时刻（成功或失败都写）。
    /// 等待方据此判断「在途那次已结束」，不必空转到 30s 超时。
    attempt_ts: f64,
    /// 最近一次抓取失败的原因（成功时清空）。
    error: Option<String>,
}

static LU: Mutex<LuState> = Mutex::new(LuState {
    ts: 0.0,
    data: None,
    ladder: None,
    ladder_ts: 0.0,
    attempt_ts: 0.0,
    error: None,
});

/// 单飞标志：同一时刻只允许一个请求抓同花顺，其余复用缓存。
static LU_BUSY: AtomicBool = AtomicBool::new(false);

struct LuBusyGuard;

impl Drop for LuBusyGuard {
    fn drop(&mut self) {
        LU_BUSY.store(false, Ordering::SeqCst);
    }
}

fn lock_lu() -> std::sync::MutexGuard<'static, LuState> {
    LU.lock().unwrap_or_else(|e| e.into_inner())
}

/// `_lu_paged`：special-data 池全量分页（page/size=200）。
fn lu_paged(path: &str, extra: &[(&str, String)]) -> Result<Value, String> {
    let mut items: Vec<Value> = Vec::new();
    let mut page = 1usize;
    let mut pages = 1usize;
    let mut ts: Value = Value::Null;
    let mut total: Option<i64> = None;
    while page <= pages.min(LU_MAX_PAGES) {
        let mut params: Vec<(&str, String)> = extra.to_vec();
        params.push(("page", page.to_string()));
        params.push(("size", "200".to_string()));
        let d = ths::ths_get(path, &params, 20, 3, 1)?;
        let pg = d.get("pagination").cloned().unwrap_or(json!({}));
        pages = pg.get("pages").and_then(|p| p.as_i64()).unwrap_or(1).max(1) as usize;
        if let Some(t) = d.get("timestamp") {
            ts = t.clone();
        }
        if total.is_none() {
            total = pg.get("total").and_then(|t| t.as_i64());
        }
        if let Some(arr) = d.get("item").and_then(|x| x.as_array()) {
            items.extend(arr.iter().cloned());
        }
        if page >= pages {
            break;
        }
        page += 1;
    }
    Ok(json!({
        "timestamp": ts,
        "total": total.unwrap_or(items.len() as i64),
        "items": items,
    }))
}

/// 单个池子失败时降级为空池：保留错误信息，不拖垮整个 /api/limitup。
fn degraded_pool(path: &str, err: &str) -> Value {
    json!({
        "timestamp": Value::Null,
        "total": 0,
        "items": [],
        "degraded": true,
        "error": format!("{path}: {}", err.chars().take(120).collect::<String>()),
    })
}

/// `lu_payload`：涨停观察数据包（30s TTL；ladder 600s TTL）。
pub fn limitup_payload(force: bool) -> Result<Value, String> {
    // 涨停池/连板天梯整体依赖同花顺；未配置 key 时明确降级，不报错刷屏
    if !zg_market::ths::enabled() {
        return Ok(json!({
            "fetched_at": Value::Null, "live": false,
            "pool": {"total": 0}, "ladder": {},
            "error": "未配置同花顺 API key，涨停监控不可用（配置后自动恢复）",
            "src": "disabled"
        }));
    }
    let t = now();
    {
        let st = lock_lu();
        if !force {
            if let Some(d) = &st.data {
                if t - st.ts < LU_TTL {
                    return Ok(d.clone());
                }
            }
            // 别的请求正在抓：先给旧数据，避免并发重复打同花顺
            if LU_BUSY.load(Ordering::SeqCst) {
                if let Some(d) = &st.data {
                    return Ok(d.clone());
                }
            }
        }
    }
    if LU_BUSY.swap(true, Ordering::SeqCst) {
        // force 撞上正在进行的抓取：等它结束再取（最多 30s）
        for _ in 0..300 {
            std::thread::sleep(Duration::from_millis(100));
            let st = lock_lu();
            // 在途那次抓取已结束（成功或失败）：立刻回结果，不空转到超时
            if st.attempt_ts > t {
                if st.ts > t {
                    if let Some(d) = &st.data {
                        return Ok(d.clone());
                    }
                }
                return Err(st.error.clone().unwrap_or_else(|| "涨停数据刷新失败".into()));
            }
        }
        return Err("涨停数据刷新超时（另一请求仍在抓取）".into());
    }
    let _busy = LuBusyGuard;
    match fetch_lu(t) {
        Ok(payload) => Ok(payload),
        Err(e) => {
            // 失败路径也要落 attempt_ts + error：否则等待方无从得知「已失败」
            let mut st = lock_lu();
            st.attempt_ts = now();
            st.error = Some(e.clone());
            Err(e)
        }
    }
}

/// 真正发起同花顺请求（网络全部在锁外）。
fn fetch_lu(t: f64) -> Result<Value, String> {
    // 网络请求全部在锁外进行，锁只用于读写缓存
    let lad = {
        let cached = {
            let st = lock_lu();
            if st.ladder.is_some() && t - st.ladder_ts <= LU_TTL_LADDER {
                Some(st.ladder.clone().unwrap_or(json!({})))
            } else {
                None
            }
        };
        match cached {
            Some(v) => v,
            None => {
                let v = ths::ths_get("/api/a-share/special-data/limit-up-ladder", &[], 25, 3, 1)?;
                let mut st = lock_lu();
                st.ladder = Some(v.clone());
                st.ladder_ts = t;
                v
            }
        }
    };
    let pool = lu_paged(
        "/api/a-share/special-data/limit-up-pool",
        &[("sort_field", "continue_day_cnt".into()), ("sort_dir", "desc".into())],
    )?;
    let brk = lu_paged(
        "/api/a-share/special-data/limit-break-pool",
        &[("sort_field", "open_times".into()), ("sort_dir", "desc".into())],
    )
    .unwrap_or_else(|e| degraded_pool("limit-break-pool", &e));
    let down = lu_paged(
        "/api/a-share/special-data/limit-down-pool",
        &[("sort_field", "last_limit_time".into()), ("sort_dir", "desc".into())],
    )
    .unwrap_or_else(|e| degraded_pool("limit-down-pool", &e));
    let payload = json!({
        "fetched_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "live": true,
        "pool": pool, "ladder": lad, "brk": brk, "down": down,
    });
    {
        let mut st = lock_lu();
        st.data = Some(payload.clone());
        st.ts = now();
        st.attempt_ts = st.ts;
        st.error = None;
    }
    Ok(payload)
}

// ---------------------------------------------------------------- ETF 状态（轮转线程 2026-10-09 起不启动）

/// `ETF_CACHE` 初始结构（Python 轮转线程未启动 → 恒为空缓存）。
struct EtfCache {
    ts: i64,
    list: Vec<Value>,
    quotes: Map<String, Value>,
    done: i64,
    total: i64,
    running: bool,
    error: Option<String>,
    cursor: i64,
    rate: f64,
    elite: i64,
    fail_n: i64,
    fail_ok: i64,
    fail_msg: String,
    fail_t: f64,
}

static ETF: OnceLock<Mutex<EtfCache>> = OnceLock::new();

fn etf() -> &'static Mutex<EtfCache> {
    ETF.get_or_init(|| Mutex::new(EtfCache {
    ts: 0,
    list: Vec::new(),
    quotes: Map::new(),
    done: 0,
    total: 0,
    running: false,
    error: None,
    cursor: 0,
    rate: 0.0,
    elite: 0,
    fail_n: 0,
    fail_ok: 0,
        fail_msg: String::new(),
        fail_t: 0.0,
    }))
}

const ETF_TIER1: i64 = 40;
const ETF_SLICE: i64 = 200;

/// `/api/etf`（Python `api.py::api_etf` 逐字段）。
pub fn etf_payload() -> Value {
    let e = etf().lock_ok();
    let t = now();
    let n_all = if e.total != 0 { e.total } else { e.list.len() as i64 };
    let covered = e.quotes.len() as i64;
    let fresh = e.quotes
        .values()
        .filter(|v| t - v.get("t").and_then(|x| x.as_f64()).unwrap_or(0.0) <= 1800.0)
        .count() as i64;
    json!({
        "ts": e.ts,
        "list": e.list,
        "quotes": Value::Object(e.quotes.clone()),
        "done": e.done,
        "total": n_all,
        "running": e.running,
        "error": e.error.clone().map(Value::String).unwrap_or(Value::Null),
        "age": if e.ts != 0 { Value::from(((t - e.ts as f64) * 10.0).round() / 10.0) } else { Value::Null },
        "cursor": e.cursor,
        "slice": ETF_TIER1,
        "tier1": if e.elite != 0 { e.elite } else { ETF_TIER1 },
        "covered": covered,
        "fresh": fresh,
        "rate": if e.rate != 0.0 { Value::from(e.rate) } else { Value::from(0) },
        "na": 0,
        "cycle_min": Value::Null,
    })
}

/// ETF 状态块（`/api/ths_debug` 内嵌）。
pub fn etf_debug_block() -> Value {
    let e = etf().lock_ok();
    json!({
        "covered": e.quotes.len(),
        "done": e.done,
        "total": e.total,
        "cursor": e.cursor,
        "running": e.running,
        "error": e.error.clone().map(Value::String).unwrap_or(Value::Null),
        "slice": ETF_SLICE,
        "fail_total": e.fail_n,
        "ok_total": e.fail_ok,
        "last_fail": e.fail_msg,
        "last_fail_age": if e.fail_t > 0.0 { Value::from(((now() - e.fail_t) * 10.0).round() / 10.0) } else { Value::Null },
    })
}

// ---------------------------------------------------------------- ths_debug

/// `/api/ths_debug`：限速器状态 + ETF 状态块 + 实时探测（510300.SH 快照）。
pub fn ths_debug_payload() -> Value {
    let mut st = ths::debug_state();
    if let Some(o) = st.as_object_mut() {
        o.insert("etf".into(), etf_debug_block());
    }
    let t0 = now();
    let live = match ths::ths_get(
        "/api/fund/market/snapshot",
        &[("thscode", "510300.SH".to_string())],
        8, 1, 1,
    ) {
        Ok(_) => json!({"ok": true, "ms": ((now() - t0) * 1000.0).round() as i64}),
        Err(e) => json!({
            "ok": false,
            "ms": ((now() - t0) * 1000.0).round() as i64,
            "err": e.chars().take(120).collect::<String>(),
        }),
    };
    if let Some(o) = st.as_object_mut() {
        o.insert("live".into(), live);
    }
    st
}
