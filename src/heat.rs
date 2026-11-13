use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use zg_market::industry::IndustryHeat;

use crate::paths;

type Provider = dyn Fn(&[Value]) -> Result<(Vec<Value>, String), String> + Send + Sync;

// RwLock 而非 OnceLock：本地重算落盘后要整体换新实例（新实例构造时重读缓存文件）
static HEAT: RwLock<Option<Arc<IndustryHeat>>> = RwLock::new(None);
static REBUILDING: AtomicBool = AtomicBool::new(false);

fn catalog_path() -> std::path::PathBuf {
    paths::tv_root().join("modules").join("market").join("industry_catalog.json")
}

fn cache_path() -> std::path::PathBuf {
    paths::tv_root().join("data").join("market").join("industry_heat_cache.json")
}

fn make_instance() -> Arc<IndustryHeat> {
    let provider: Arc<Provider> = Arc::new(|_| Err("tv 离线：不抓取行业行情".to_string()));
    Arc::new(IndustryHeat::new(
        &catalog_path().to_string_lossy(),
        &cache_path().to_string_lossy(),
        provider,
    ))
}

fn instance() -> Arc<IndustryHeat> {
    if let Some(h) = HEAT.read().ok().and_then(|g| g.clone()) {
        return h;
    }
    let mut w = HEAT.write().expect("heat rwlock");
    if let Some(h) = w.clone() {
        return h;
    }
    let h = make_instance();
    *w = Some(h.clone());
    h
}

pub fn payload(order: &str) -> Value {
    // 横截面热度仍用本地日线重算的结果（不逐股拉实时）；盘中明确标 stale。
    let mut v = instance().request_all(order, true);
    if let Some(o) = v.as_object_mut() {
        let data_as_of = crate::bars::market_last_date().unwrap_or_default();
        let reference = crate::live::reference_day();
        // **不要继承上游的 `stale`**：上游 `crates/zg-market/src/industry.rs:395` 的语义是
        // 「内存快照超过 180 秒未刷新」（`now() - snap_ts >= 180.0`），而 tv 的 provider 是
        // **离线**的（`make_instance` 里写死 "tv 离线：不抓取行业行情"），`ensure_refresh(offline=true)`
        // 的刷新分支要求 `!offline` ⇒ 快照永不刷新，`snap_ts` 停在重算那一刻。
        // 结果：重算完 180 秒后上游那个标志**必然**为 true，被原样继承的话
        // 行业热度面板会常显「旧快照」——数据明明是当天的，标记却是假的
        // （提示常亮 = 提示不可信 = 提示没有意义；与主图徽标同一类病）。
        //
        // tv 侧按自己的数据口径重算：
        //   ① 盘中一律 stale（横截面不逐股拉实时，明说）
        //   ② 本地日线落后最近交易日 → stale
        //   ③ 缓存比本地日线旧（服务运行中手动 sync 过）→ stale
        let cache_mt = cache_mtime();
        let axis_mt = axis_mtime();
        let cache_behind = cache_mt > 0.0 && axis_mt > cache_mt;
        o.insert("session".into(), serde_json::json!(crate::live::session_today()));
        o.insert("as_of".into(), serde_json::json!(data_as_of));
        o.insert(
            "stale".into(),
            serde_json::json!(crate::live::session_today()
                || data_as_of.as_str() < reference.as_str()
                || cache_behind),
        );
        // 快照年龄另出字段：信息不丢，但不再冒充「数据陈旧」
        let age = if cache_mt > 0.0 {
            (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(cache_mt)
                - cache_mt)
                .max(0.0)
        } else {
            0.0
        };
        o.insert("snapshot_age_sec".into(), serde_json::json!(age.round() as i64));
    }
    v
}

/// 缓存文件 mtime（写盘时刻 = 重算结束时刻）。
///
/// 用文件 mtime 而不是缓存 JSON 里的 `ts`：这里每次请求都要用，读 611KB 的 JSON
/// 去取一个时间戳太贵（`stale()` 那条路径是启动期一次性调用，不心疼）。
fn cache_mtime() -> f64 {
    std::fs::metadata(cache_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 基准指数 .day 的 mtime（本地日线最新落盘时刻）。
fn axis_mtime() -> f64 {
    std::fs::metadata(crate::bars::market_axis_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 过期判定：缓存 ts 早于 sh000001.day 的 mtime（同步新数据后必触发；周末无新文件不误触）
pub fn stale() -> bool {
    let mt = std::fs::metadata(crate::bars::market_axis_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let ts = std::fs::read_to_string(cache_path()).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|d| d.get("ts").and_then(|t| t.as_f64()))
        .unwrap_or(0.0);
    ts < mt
}

/// REBUILDING 的 RAII 守卫：rebuild_inner panic（栈展开）也会复位标志，重建端点不再永久 409
struct RebuildGuard(&'static std::sync::atomic::AtomicBool);
impl Drop for RebuildGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 异步触发重算（端点用）：已在跑时拒绝
pub fn start_rebuild() -> Result<(), String> {
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return Err("热度重算已在进行".to_string());
    }
    std::thread::spawn(|| {
        let t0 = std::time::Instant::now();
        let _g = RebuildGuard(&REBUILDING);
        let r = rebuild_inner();
        report(&r, t0);
    });
    Ok(())
}

/// 同步重算（启动线程与验证用）
pub fn rebuild() -> Result<Value, String> {
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return Err("热度重算已在进行".to_string());
    }
    let _g = RebuildGuard(&REBUILDING);
    let t0 = std::time::Instant::now();
    let r = rebuild_inner();
    report(&r, t0);
    r
}

fn report(r: &Result<Value, String>, t0: std::time::Instant) {
    let ms = t0.elapsed().as_millis();
    match &r {
        Ok(v) => println!("[tv] 热度缓存本地重算完成：{} 只行情（{ms} ms）",
            v.get("n_quotes").and_then(|x| x.as_u64()).unwrap_or(0)),
        Err(e) => println!("[tv] 热度缓存本地重算失败（{ms} ms）：{e}"),
    }
}

/// 单只成分股 → 一条 quote。停牌/退市（末根非全市场最新交易日）、不足两根、
/// 价格异常都返回 `None`（与串行版的 `continue` 等义）。
/// 抽成独立函数是为了能按股票并行扫描：除 `events_of` 内部的短锁外不碰共享状态。
fn quote_one(market: &str, code: &str, name: &str, last_date: &str, key: i32) -> Option<Value> {
    // 只需末两根（当日涨幅 + 除权参考昨收）：尾部读，避免全市场每遍 1.5GB
    let bars = zg_core::api::read_day_bars_tail(market, code, 2).ok()?;
    let last = bars.last()?;
    if last.date != last_date || bars.len() < 2 || last.close <= 0.0 || last.volume <= 0.0 {
        return None;
    }
    let prev = &bars[bars.len() - 2];
    if prev.close <= 0.0 {
        return None;
    }
    // 交易所口径涨跌幅：除权除息日的昨收要用除权参考价
    // （东财/行情源的 pct 即此口径；原始价直接相除会把除权误算成暴跌）
    let mut prev_close = prev.close;
    if let Ok(evs) = zg_core::events::events_of(market, code, None) {
        if let Some(e) = evs.iter().find(|e| e.category == 1 && e.date_key() == key) {
            // 交易所参考价四舍五入到分，与行情源的昨收口径一致
            prev_close = ((prev.close - e.fenhong / 10.0 + e.peigujia * e.peigu / 10.0)
                / (1.0 + e.songzhuangu / 10.0 + e.peigu / 10.0) * 100.0).round() / 100.0;
        }
    }
    if prev_close <= 0.0 {
        return None;
    }
    let pct = (last.close / prev_close - 1.0) * 100.0;
    Some(json!({
        "code": code, "name": name,
        "price": last.close, "heat": pct, "amount": last.amount,
    }))
}

/// vipdoc 每只成分股末两根 → price/pct/amount，写个股级 quotes 缓存。
/// 格式与 zg-market industry.rs refresh_inner 写盘逐字段一致（heat=pct；行业聚合在
/// 读取路径 group_row 做）。停牌/退市（末根非全市场最新交易日）不稀释分母。
fn rebuild_inner() -> Result<Value, String> {
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(catalog_path()).map_err(|e| format!("读行业目录失败: {e}"))?,
    ).map_err(|e| format!("行业目录解析失败: {e}"))?;
    let members = doc.get("members").and_then(|m| m.as_array())
        .ok_or("行业目录缺少 members")?;

    let last_date = crate::bars::market_last_date().ok_or("无本地日线基准（sh000001）")?;
    let key: i32 = last_date.chars().filter(|c| c.is_ascii_digit()).collect::<String>()
        .parse().unwrap_or(0);

    // 预扫成员：把 (目录序号, code, market, name) 提出来。序号用于并行结果回填——
    // serde_json 开了 preserve_order，quotes 的插入顺序就是响应里的字段顺序，
    // 必须与串行扫描逐字节一致。
    let mut list: Vec<(usize, &str, &str, &str)> = Vec::with_capacity(members.len());
    for (i, m) in members.iter().enumerate() {
        let Some(code) = m.get("code").and_then(|c| c.as_str()) else { continue };
        let market = match m.get("market").and_then(|x| x.as_u64()) {
            Some(0) => "sz",
            Some(1) => "sh",
            Some(2) => "bj",
            _ => continue,
        };
        let name = m.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or(code);
        list.push((i, code, market, name));
    }

    // 逐股耗时几乎全在两次小文件 open（vipdoc 尾部 + 共享除权缓存）的固定延迟，
    // 串行 5572 只 ≈ 32s。并行把 open 延迟叠起来：drvfs 实测 8 线程 6.2x、16 线程 7.7x，
    // 再往上收益趋平，故上限 16。
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .clamp(1, 16);
    let chunk = list.len().div_ceil(n_threads);
    let mut all: Vec<(usize, String, Value)> = Vec::with_capacity(list.len());
    if chunk > 0 {
        let last_date_ref: &str = &last_date;
        let batches = std::thread::scope(|s| {
            let handles: Vec<_> = list
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || {
                        let mut out = Vec::new();
                        for &(i, code, market, name) in part {
                            if let Some(v) = quote_one(market, code, name, last_date_ref, key) {
                                out.push((i, code.to_string(), v));
                            }
                        }
                        out
                    })
                })
                .collect::<Vec<_>>();
            let mut acc = Vec::with_capacity(handles.len());
            for h in handles {
                acc.push(h.join().map_err(|_| "热度扫描线程异常退出".to_string())?);
            }
            Ok::<_, String>(acc)
        })?;
        for mut b in batches {
            all.append(&mut b);
        }
    }
    // 各批按目录顺序 spawn/join 本就有序；显式排序把「输出顺序 = 目录顺序」写成契约
    all.sort_by_key(|x| x.0);
    let mut quotes = Map::new();
    for (_, code, v) in all {
        quotes.insert(code, v);
    }
    if quotes.is_empty() {
        return Err("本地日线未产出有效行情（旧缓存保持不动）".to_string());
    }
    let n = quotes.len();
    let snap = json!({
        "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "ts": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
        "source": "tv 本地日线重算",
        "quotes": Value::Object(quotes),
    });
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&path);
    std::fs::write(&tmp, serde_json::to_string(&snap).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写热度缓存失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落热度缓存失败: {e}"))?;
    if let Ok(mut w) = HEAT.write() {
        *w = Some(make_instance());
    }
    Ok(json!({"ok": true, "n_quotes": n}))
}
