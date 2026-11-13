//! 个股概念题材的**本地状态**部分：缓存文件读写、快照、周判据、负缓存、标签拆分。
//!
//! 与 `source.rs`（MAC 连接/补漏/全量刷新）分开的理由：这里的逻辑不碰网络，
//! 可以离线单测；改缓存格式的人不必在一堆 socket 代码里找目标函数。
//!
//! 口径与拆分前完全一致（纯搬家）：父模块 `concepts.rs` 的文档注释是总纲。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use chrono::{Datelike, TimeZone};

use crate::paths;
use crate::tdxmac::{BOARD_TYPE_CONCEPT, BOARD_TYPE_REGION, BOARD_TYPE_STYLE};

/// 负缓存有效期（秒）：非个股代码问 `0x1218` 恒返回 0 行，不缓存的话每次请求都要重开一条 TCP。
const NEGATIVE_TTL_SECS: u64 = 600;

/// 负缓存条数上限（超出整体清空，防无界增长）。
const NEGATIVE_MAX: usize = 4096;

/// 一个板块标签（股票 → 板块）。
#[derive(Clone, Debug, PartialEq)]
pub struct BoardTag {
    /// 3 地区 / 4 概念 / 5 风格。
    pub board_type: u16,
    pub code: String,
    pub name: String,
    /// 这条归属的抓取时刻（Unix 秒）。全量刷新的条目等于根 `ts`；
    /// 单股按需补漏写入的条目只影响这一只股票，**不动根 `ts`**。
    pub fetched: f64,
}


/// 一次全量刷新的解码质量指标（随缓存一起落盘，供门禁事后审计）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quality {
    /// 有效成分股行数。
    pub rows: usize,
    /// 被解码守卫拒绝的行数。
    pub rejected: usize,
    /// 0 只成分股的板块数。
    pub empty_boards: usize,
}


/// 缓存快照。
pub struct Snapshot {
    /// 股票代码（6 位裸码）→ 板块列表。
    pub map: HashMap<String, Vec<BoardTag>>,
    /// 数据来源（`mac@主机`）。
    pub source: String,
    /// 抓取时间（本地可读格式）。
    pub fetched_at: String,
    /// 抓取时刻（Unix 秒）：**只有全量刷新会改它**。
    pub ts: f64,
    /// 板块总数。
    pub boards: usize,
    /// 覆盖股票数。
    pub stocks: usize,
    /// 解码质量。
    pub quality: Quality,
    /// 缓存文件 mtime（秒）：文件被替换后内存快照作废重载。
    pub mtime: u64,
}


/// 个股标签（按类型分好）。
pub struct StockTags {
    /// 概念题材（type=4）。
    pub concepts: Vec<String>,
    /// 地区板块（type=3）。
    pub regions: Vec<String>,
    /// 风格板块（type=5）。
    pub styles: Vec<String>,
    /// 数据来源。
    pub source: String,
    /// 抓取时间。
    pub fetched_at: String,
}


static SNAPSHOT: RwLock<Option<Arc<Snapshot>>> = RwLock::new(None);

/// 缓存文件的读-改-写锁：既保护文件，也保护「取旧快照 → 合并 → 落盘」这段事务。
pub(super) static FILE_LOCK: Mutex<()> = Mutex::new(());

/// 负缓存：代码 → 判定时刻。装的是「`0x1218` 返回 0 行」的代码（指数/板块/ETF），
/// 它们不进缓存文件（会让 `stocks` 统计虚增），但也不该每次都出网。
static NEGATIVE: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn cache_path() -> std::path::PathBuf {
    paths::tv_root().join("data").join("tdx").join("concepts_cache.json")
}


pub(super) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}


/// 本自然周周一 00:00（本地时区）的 Unix 秒；时钟异常或该时刻不存在/不唯一时 `None`。
fn week_start_secs() -> Option<f64> {
    let now = chrono::Local::now();
    let monday = (now.date_naive()
        - chrono::Duration::days(now.weekday().num_days_from_monday() as i64))
        .and_hms_opt(0, 0, 0)?;
    Some(chrono::Local.from_local_datetime(&monday).single()?.timestamp() as f64)
}


/// 根 `ts` 是否落在本自然周内——「每周第一次启动刷一次」的全部判据。
/// 取不到本周一时按「不新鲜」处理：宁可多刷一次，别拿着上周数据冒充本周。
pub(super) fn fresh_this_week(ts: f64) -> bool {
    week_start_secs().is_some_and(|ws| ts >= ws)
}


/// 本地时间戳（`YYYY-MM-DD HH:MM:SS`）。
pub(super) fn local_stamp(ts: f64) -> String {
    let secs = ts as i64;
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}


/// 读缓存文件（原始 JSON）。
pub(super) fn read_cache_file() -> Option<Value> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}


/// 缓存文件 mtime（用于判断内存快照是否过期）。
fn cache_mtime() -> u64 {
    std::fs::metadata(cache_path())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}


/// 解析缓存文件 → 快照。
pub(super) fn snapshot_from_value(v: &Value) -> Option<Snapshot> {
    let ts = v.get("ts")?.as_f64()?;
    let map_obj = v.get("map")?.as_object()?;
    let mut map: HashMap<String, Vec<BoardTag>> = HashMap::with_capacity(map_obj.len());
    for (code, tags) in map_obj {
        let Some(arr) = tags.as_array() else { continue };
        let mut list = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(row) = item.as_array() else { continue };
            if row.len() < 3 {
                continue;
            }
            let board_type = row[0].as_u64().unwrap_or(0) as u16;
            let code = row[1].as_str().unwrap_or("").to_string();
            let name = row[2].as_str().unwrap_or("").to_string();
            // 第 4 位是这条归属的抓取时刻；TV-34 初版的 3 元素行没有它，按根 ts 处理
            let fetched = row.get(3).and_then(|x| x.as_f64()).unwrap_or(ts);
            if code.is_empty() || name.is_empty() {
                continue;
            }
            list.push(BoardTag { board_type, code, name, fetched });
        }
        map.insert(code.clone(), list);
    }
    let quality = v.get("quality").and_then(|q| {
        Some(Quality {
            rows: q.get("rows")?.as_u64()? as usize,
            rejected: q.get("rejected").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            empty_boards: q.get("empty_boards").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
        })
    });
    Some(Snapshot {
        source: v
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or("mac")
            .to_string(),
        fetched_at: v
            .get("fetched_at")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        ts,
        boards: v.get("boards").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
        stocks: map.len(),
        quality: quality.unwrap_or_default(),
        mtime: cache_mtime(),
        map,
    })
}


/// 取当前快照（按文件 mtime 缓存到内存）。
pub fn snapshot() -> Option<Arc<Snapshot>> {
    let mtime = cache_mtime();
    if let Ok(g) = SNAPSHOT.read() {
        if let Some(s) = g.as_ref() {
            if s.mtime == mtime {
                return Some(s.clone());
            }
        }
    }
    let loaded = read_cache_file()
        .as_ref()
        .and_then(snapshot_from_value)
        .map(|mut s| {
            // 用**读文件之前**取的 mtime：读的过程中文件被替换 → mtime 对不上 → 下次重载
            s.mtime = mtime;
            Arc::new(s)
        });
    if let Ok(mut w) = SNAPSHOT.write() {
        *w = loaded.clone();
    }
    loaded
}


/// 缓存是否过期（缺失也算过期）。
///
/// 过期 = 本周还没刷过：根 `ts` 早于本周一 00:00（见 `fresh_this_week`）。
///
/// `boards == 0` 同样算过期：那是**单股按需查询**写出的退化缓存
/// （`for_stock` 在无缓存时只写入查到的那一只），不能因此把全量刷新压后一整周。
pub fn stale() -> bool {
    match snapshot() {
        None => true,
        Some(s) => s.boards == 0 || !fresh_this_week(s.ts),
    }
}


/// 缓存统计（给 API / 门禁用）。
pub fn stats() -> Value {
    match snapshot() {
        None => json!({"ok": false, "exists": false, "stale": true}),
        Some(s) => {
            let rows = s.quality.rows;
            let rejected = s.quality.rejected;
            json!({
                "ok": true,
                "exists": true,
                "stale": s.boards == 0 || !fresh_this_week(s.ts),
                "ts": s.ts,
                "fetched_at": s.fetched_at,
                "source": s.source,
                "boards": s.boards,
                "stocks": s.stocks,
                // 解码质量（建这份缓存时算出的，随文件落盘，供门禁事后审计）
                "quality": {
                    "rows": rows,
                    "rejected": rejected,
                    "empty_boards": s.quality.empty_boards,
                    // 与 rebuild 的落盘判据同一口径：不可信行占比 > 5% 就该拒绝落盘
                    "rejected_pct": if rows + rejected > 0 {
                        rejected as f64 * 100.0 / (rows + rejected) as f64
                    } else { 0.0 },
                },
            })
        }
    }
}


/// 落盘（自己取 FILE_LOCK）。
///
/// `ts` 必须由调用方显式给出，不给默认「现在」：全量刷新要盖的是**抓取开始**那一刻
/// （数据是爬完 460 个板块才落盘的，用写盘时刻会让整份缓存凭空多活几十秒，还会和
/// 条自带的时间戳对不上）；单股补漏盖的是旧根 ts（见 `write_locked`）。
pub(super) fn write_cache(
    map: &HashMap<String, Vec<BoardTag>>,
    source: &str,
    boards: usize,
    ts: f64,
    quality: Quality,
) -> Result<(), String> {
    let _guard = FILE_LOCK
        .lock()
        .map_err(|_| "概念缓存锁已损坏".to_string())?;
    write_locked(map, source, boards, ts, quality)
}


/// 落盘（调用方已持 FILE_LOCK）：`ts` 由调用方决定，**单股补漏必须传旧根 ts**。
///
/// 根 `ts` 是全量刷新的时间戳，也是 `stale()` 唯一的判据。单股补漏若把它刷成当前时刻，
/// 等于让 6000 只股票的数据冒充刚抓的、并把下一次全量刷新整整数地推迟；而界面上
/// 「抓取时间」也随之失真。补漏那条自己的时刻记在条目的第 4 位。
pub(super) fn write_locked(
    map: &HashMap<String, Vec<BoardTag>>,
    source: &str,
    boards: usize,
    ts: f64,
    quality: Quality,
) -> Result<(), String> {
    let mut root = Map::new();
    root.insert("ts".into(), json!(ts));
    root.insert("fetched_at".into(), json!(local_stamp(ts)));
    root.insert("source".into(), json!(source));
    root.insert("boards".into(), json!(boards));
    root.insert(
        "quality".into(),
        json!({
            "rows": quality.rows,
            "rejected": quality.rejected,
            "empty_boards": quality.empty_boards,
        }),
    );
    let mut m = Map::new();
    for (code, tags) in map {
        let rows: Vec<Value> = tags
            .iter()
            .map(|t| json!([t.board_type, t.code, t.name, t.fetched]))
            .collect();
        m.insert(code.clone(), Value::Array(rows));
    }
    root.insert("map".into(), Value::Object(m));

    let path = cache_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&Value::Object(root)).map_err(|e| format!("序列化失败: {e}"))?)
        .map_err(|e| format!("写临时文件失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落盘失败: {e}"))?;
    // 内存快照立即失效，下次读时重载
    if let Ok(mut w) = SNAPSHOT.write() {
        *w = None;
    }
    Ok(())
}


/// 缓存键：6 位裸码（`sh600519` / `600519.SH` / `SH600519` → `600519`）。
///
/// 必须归一化：`rebuild` 落盘的键恒为裸码，若不归一化，带前/后缀的请求会**必然 miss**，
/// 于是每次出网，并把 `sh600519` 这种非规范键写进缓存文件——同一只股票存两份、
/// `stocks` 统计虚增、此后每个该形式的新代码都重复这个代价。
pub(super) fn cache_key(code: &str) -> String {
    zg_core::reader::strip_code(code).1
}


/// 代码是否「不是个股」（负缓存判定，带 TTL，不落盘）。
pub(super) fn negative_hit(key: &str) -> bool {
    let mut guard = match NEGATIVE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let map = guard.get_or_insert_with(Default::default);
    match map.get(key) {
        Some(at) if at.elapsed() < Duration::from_secs(NEGATIVE_TTL_SECS) => true,
        Some(_) => {
            map.remove(key);
            false
        }
        None => false,
    }
}


/// 记「这只查了，服务端返回 0 行」（指数/板块/ETF 恒如此）。
pub(super) fn negative_put(key: &str) {
    if let Ok(mut guard) = NEGATIVE.lock() {
        let map = guard.get_or_insert_with(Default::default);
        if map.len() >= NEGATIVE_MAX {
            map.clear();
        }
        map.insert(key.to_string(), Instant::now());
    }
}


/// 按类型拆分标签，并按条目各自的抓取时刻给出「这批数据是什么时候的」。
pub(super) fn split_tags(tags: &[BoardTag], source: &str, fallback_at: &str) -> StockTags {
    let mut concepts = Vec::new();
    let mut regions = Vec::new();
    let mut styles = Vec::new();
    for t in tags {
        if is_excluded(&t.name) {
            continue;
        }
        let bucket = match t.board_type {
            BOARD_TYPE_CONCEPT => &mut concepts,
            BOARD_TYPE_REGION => &mut regions,
            BOARD_TYPE_STYLE => &mut styles,
            _ => continue,
        };
        if !bucket.contains(&t.name) {
            bucket.push(t.name.clone());
        }
    }
    // 「抓取时间」取这批条目里最晚的一条：全量刷新的股票各条目同值（根 ts），
    // 被单股补漏刷新过的股票则显示补漏时刻——显示根 ts 会把这只股票的新数据说成旧的，
    // 而显示一个不存在的时刻更糟。
    let fetched_at = match tags.iter().map(|t| t.fetched).fold(0.0f64, f64::max) {
        max if max > 0.0 => local_stamp(max),
        _ => fallback_at.to_string(),
    };
    StockTags {
        concepts,
        regions,
        styles,
        source: source.to_string(),
        fetched_at,
    }
}


/// 非题材板块（通达信88 是自编指数成分，不是题材），与 `tags.rs` 的旧路径口径一致。
/// 不剔的话界面上会出现「概念：通达信88」这种噪声。
const EXCLUDED_NAMES: &[&str] = &["通达信88"];


/// 这个板块名是否应剔除。
pub(super) fn is_excluded(name: &str) -> bool {
    EXCLUDED_NAMES.contains(&name)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 刷新判据是日历周：本周一 00:00 之前抓的一律过期，之后的一律新鲜
    /// （「每周第一次启动刷一次」的全部逻辑就这一条）。
    #[test]
    fn freshness_is_the_calendar_week() {
        let ws = week_start_secs().expect("本地时钟可用");
        let now = now_secs();
        assert!(ws <= now && now - ws < 7.0 * 86_400.0, "本周一 00:00 应落在过去 7 天内");
        assert!(!fresh_this_week(ws - 1.0), "上周抓的必须判过期（本周首次启动才刷）");
        assert!(fresh_this_week(ws), "本周一 00:00 整抓的算本周");
        assert!(!fresh_this_week(0.0), "没有根 ts 的缓存判过期");
        assert!(!fresh_this_week(now - 7.5 * 86_400.0), "上周同时刻抓的判过期");
    }

    #[test]
    fn splits_tags_by_type() {
        let tags = vec![
            BoardTag { board_type: BOARD_TYPE_CONCEPT, code: "880564".into(), name: "白酒概念".into(), fetched: 0.0 },
            BoardTag { board_type: BOARD_TYPE_REGION, code: "880229".into(), name: "贵州板块".into(), fetched: 0.0 },
            BoardTag { board_type: BOARD_TYPE_STYLE, code: "880801".into(), name: "基金重仓".into(), fetched: 0.0 },
            BoardTag { board_type: 12, code: "881218".into(), name: "汽车零部件".into(), fetched: 0.0 },
        ];
        let t = split_tags(&tags, "mac@x", "2026-11-02 00:00:00");
        assert_eq!(t.concepts, vec!["白酒概念"]);
        assert_eq!(t.regions, vec!["贵州板块"]);
        assert_eq!(t.styles, vec!["基金重仓"]);
        // 行业（12）不走这条路径（行业由本地 catalog 提供）
        assert!(!t.concepts.iter().any(|c| c == "汽车零部件"));
        // 条目没时间戳时退回快照的抓取时间，不能显示成 1970
        assert_eq!(t.fetched_at, "2026-11-02 00:00:00");
    }

    /// 单股补漏过的股票：「抓取时间」取最晚那条条目，而不是全量刷新的根 ts。
    #[test]
    fn split_tags_reports_latest_entry_stamp() {
        let tags = vec![
            BoardTag { board_type: BOARD_TYPE_CONCEPT, code: "880564".into(), name: "白酒概念".into(), fetched: 1_700_000_000.0 },
            BoardTag { board_type: BOARD_TYPE_REGION, code: "880229".into(), name: "贵州板块".into(), fetched: 1_800_000_000.0 },
        ];
        let t = split_tags(&tags, "mac@x", "根时间");
        assert_eq!(t.fetched_at, local_stamp(1_800_000_000.0));
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut root = Map::new();
        root.insert("ts".into(), json!(1.0));
        root.insert("fetched_at".into(), json!("2026-11-02 00:00:00"));
        root.insert("source".into(), json!("mac@test"));
        root.insert("boards".into(), json!(1));
        root.insert("quality".into(), json!({"rows": 7, "rejected": 2, "empty_boards": 1}));
        let mut m = Map::new();
        m.insert("600519".into(), json!([[4, "880564", "白酒概念", 5.0]]));
        root.insert("map".into(), Value::Object(m));
        let snap = snapshot_from_value(&Value::Object(root)).expect("parse");
        assert_eq!(snap.map.len(), 1);
        assert_eq!(snap.source, "mac@test");
        assert_eq!(snap.stocks, 1);
        assert_eq!(snap.map.get("600519").unwrap()[0].name, "白酒概念");
        assert_eq!(snap.map.get("600519").unwrap()[0].board_type, BOARD_TYPE_CONCEPT);
        // 条目自带的抓取时刻必须活着（界面「抓取时间」靠它）
        assert_eq!(snap.map.get("600519").unwrap()[0].fetched, 5.0);
        // 解码质量随文件走，门禁才能事后审计这份缓存是怎么建出来的
        assert_eq!(snap.quality.rows, 7);
        assert_eq!(snap.quality.rejected, 2);
        assert_eq!(snap.quality.empty_boards, 1);
    }

    /// TV-34 初版的 3 元素行（无条目时间戳）要能读，且按根 ts 处理——
    /// 否则升级格式后老缓存会被解析成空标签。
    #[test]
    fn snapshot_accepts_legacy_rows_without_stamp() {
        let v = json!({
            "ts": 1234.5,
            "source": "mac@test",
            "boards": 1,
            "map": {"600519": [[4, "880564", "白酒概念"]]},
        });
        let snap = snapshot_from_value(&v).expect("parse");
        assert_eq!(snap.map.get("600519").unwrap()[0].fetched, 1234.5);
        assert_eq!(snap.quality, Quality::default());
    }

    /// 指数成分块（通达信88）即便混进了缓存也不能上界面。
    #[test]
    fn split_tags_drops_index_constituent_block() {
        let tags = vec![
            BoardTag { board_type: BOARD_TYPE_CONCEPT, code: "880915".into(), name: "通达信88".into(), fetched: 0.0 },
            BoardTag { board_type: BOARD_TYPE_CONCEPT, code: "880564".into(), name: "白酒概念".into(), fetched: 0.0 },
        ];
        let t = split_tags(&tags, "mac@x", "2026-11-02 00:00:00");
        assert_eq!(t.concepts, vec!["白酒概念"]);
    }

    /// 缓存键必须归一化：`rebuild` 落盘的键恒为 6 位裸码，带前/后缀的请求若不归一
    /// 就会必然 miss（每次出网）并把非规范键写进文件（同一只股票存两份、stocks 虚增）。
    #[test]
    fn cache_key_normalizes_market_prefix_and_suffix() {
        assert_eq!(cache_key("600519"), "600519");
        assert_eq!(cache_key("sh600519"), "600519");
        assert_eq!(cache_key("SH600519"), "600519");
        assert_eq!(cache_key("600519.SH"), "600519");
        assert_eq!(cache_key(" sz000338 "), "000338");
        // 归一化后的键才能命中快照
        let v = json!({"ts": 1.0, "boards": 1, "map": {"600519": [[4, "880564", "白酒概念"]]}});
        let snap = snapshot_from_value(&v).expect("parse");
        assert!(snap.map.contains_key(&cache_key("sh600519")));
    }

    /// 空缓存文件必须解析失败而不是给出空快照（防「静默降级为空」）。
    #[test]
    fn snapshot_rejects_missing_map() {
        let v = json!({"ts": 1.0, "source": "mac"});
        assert!(snapshot_from_value(&v).is_none());
    }
}

