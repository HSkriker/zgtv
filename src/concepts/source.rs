//! 个股概念题材的**在线源与编排**部分：MAC 连接、单股补漏、全量刷新、启动刷新。
//!
//! 与 `cache.rs`（本地缓存文件/快照/负缓存）分开的理由：这里只关心「怎么取、什么时候刷」。
//!
//! 口径与拆分前完全一致（纯搬家）：父模块 `concepts.rs` 的文档注释是总纲。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::tdxmac::{
    self, MacBoard, MacConn, BOARD_CATEGORY_DQ, BOARD_CATEGORY_FG, BOARD_CATEGORY_GN,
    BOARD_TYPE_CONCEPT, BOARD_TYPE_INDUSTRY, BOARD_TYPE_REGION, BOARD_TYPE_STYLE,
};

use super::cache::{
    cache_key, fresh_this_week, is_excluded, local_stamp, negative_hit, negative_put, now_secs,
    read_cache_file, snapshot, snapshot_from_value, split_tags, stale, write_cache, write_locked,
    BoardTag, Quality, StockTags, FILE_LOCK,
};

/// 全量刷新失败后，隔多久允许再试一次。
const REBUILD_BACKOFF_SECS: u64 = 600;

/// 单股实时查询连接的复用窗口（秒）：小于响应读超时（8s），
/// 保证取到的连接至少还有一整个读窗口的寿命。
const CONN_REUSE_SECS: u64 = 5;

/// 板块清单单页上限（服务端限制）。
const BOARD_PAGE: u16 = 150;

/// 成分股单页上限（服务端限制）。
const MEMBER_PAGE: u16 = 80;


/// 一次刷新的板块类别：MAC 分类码 → `0x1218` 的 wire type。
///
/// 行业（wire type 12）**不在这里**：tv 的 L2 行业走本地 `industry_catalog.json`
/// （由通达信 `tdxhy.cfg` 生成，更新及时），这里只补「题材」侧的三类。
const CATEGORIES: [(u16, u16); 3] = [
    (BOARD_CATEGORY_GN, BOARD_TYPE_CONCEPT),
    (BOARD_CATEGORY_DQ, BOARD_TYPE_REGION),
    (BOARD_CATEGORY_FG, BOARD_TYPE_STYLE),
];


static REBUILDING: AtomicBool = AtomicBool::new(false);

/// 上一次「真的尝试过」全量刷新的时刻（Unix 秒，未取到时钟时为 0）。
/// 单股补漏触发的补救据此退避，避免 MAC 半故障时每只股票都重试 50 秒。
static REBUILD_ATTEMPT_AT: AtomicU64 = AtomicU64::new(0);

/// 单股实时查询连接的复用槽（一次 HTTP 请求只发一条 `0x1218`，串行就错，必须独占取用）。
static LIVE_CONN: OnceLock<Mutex<Option<(Instant, MacConn)>>> = OnceLock::new();


fn live_conn_slot() -> &'static Mutex<Option<(Instant, MacConn)>> {
    LIVE_CONN.get_or_init(|| Mutex::new(None))
}


/// 取一条可复用的 MAC 连接（`CONN_REUSE_SECS` 内的旧连接直接用）。
///
/// 一次 HTTP 请求只发一条 `0x1218`，串行即可错开，因此连接独占取用：
/// 谁 take 到谁负责放回。取回时机由调用方（`live_belong`）决定——失败时**不放回**，
/// 免得把一个已经帧错位的连接交给下一个请求。
fn take_live_conn() -> Result<MacConn, String> {
    let mut slot = live_conn_slot()
        .lock()
        .map_err(|_| "MAC 连接槽已损坏".to_string())?;
    if let Some((at, conn)) = slot.take() {
        if at.elapsed() < Duration::from_secs(CONN_REUSE_SECS) {
            return Ok(conn);
        }
    }
    MacConn::connect_any(Duration::from_secs(8))
}


fn put_live_conn(conn: MacConn) {
    if let Ok(mut slot) = live_conn_slot().lock() {
        *slot = Some((Instant::now(), conn));
    }
}


/// 单股实时查（`0x1218`）。返回 (板块标签, 实际主机, 服务端原始行数)。
///
/// 原始行数是「这是不是一只个股」的判据：指数/板块代码问 `0x1218` 会**成功返回 0 行**，
/// 若把这种空结果写进缓存，`stocks` 统计会虚增（实测 000300 就被算成一只股票）。
fn live_belong(key: &str) -> Result<(Vec<BoardTag>, String, usize), String> {
    let (mkt, cd) = zg_core::reader::resolve(key, None)
        .map_err(|e| format!("代码解析失败: {e}"))?;
    let market = tdxmac::market_code(&mkt);
    let mut conn = take_live_conn()?;
    let host = conn.host().to_string();
    // 失败时**不放回连接槽**：读失败意味着帧可能错位，交给下一个请求只会连坐
    let boards = conn.belong_board(market, &cd)?;
    put_live_conn(conn);
    let raw = boards.len();
    let fetched = now_secs();
    let tags = boards
        .into_iter()
        .filter(|b| match b.board_type {
            BOARD_TYPE_CONCEPT | BOARD_TYPE_REGION | BOARD_TYPE_STYLE => true,
            // 行业（12）不走这条路：tv 的 L2 行业来自本地 industry_catalog.json
            BOARD_TYPE_INDUSTRY => false,
            _ => false,
        })
        // 剔除口径必须与 `rebuild_inner`（板块清单那里 retain 过一次）一致：
        // 只在前台显示时剔，会让两条写盘路径产出形状不同的缓存，对表必挂。
        .filter(|b| !is_excluded(&b.name))
        .map(|b| BoardTag {
            board_type: b.board_type,
            code: b.code,
            name: b.name,
            fetched,
        })
        .collect();
    Ok((tags, host, raw))
}


/// 个股板块标签：先查缓存，缺这只股票再实时问 MAC（并写回缓存）。
///
/// 缓存里**存在该代码但标签为空**也算命中（真没有归属概念的股票不重复打网络）。
/// 命中即返回：整周用同一份缓存，读路径不出网（每周刷一次由启动检查负责）。
pub fn for_stock(code: &str) -> Result<StockTags, String> {
    let key = cache_key(code.trim());
    if key.is_empty() {
        return Err("代码为空".to_string());
    }
    let snap = snapshot();
    if let Some(s) = snap.as_ref() {
        if let Some(tags) = s.map.get(&key) {
            return Ok(split_tags(tags, &s.source, &s.fetched_at));
        }
    }
    // 缓存没有这只 → 实时问一次。
    // 离线模式必须在这里拦住：`TV_LIVE=0` 的语义是「完全不出网」，
    // 单股按需查询若不判开关，会静默在离线模式下联网（实测会绕过整个开关）。
    if !crate::live::enabled() {
        return Err("实时链路已关闭（TV_LIVE=0 / TV_OFFLINE=1）".to_string());
    }
    // 非个股代码（指数/板块/ETF）走内存负缓存，不进缓存文件也不每次出网
    if negative_hit(&key) {
        return Ok(StockTags {
            concepts: Vec::new(),
            regions: Vec::new(),
            styles: Vec::new(),
            source: snap.map(|s| s.source.clone()).unwrap_or_else(|| "mac".to_string()),
            fetched_at: String::new(),
        });
    }
    let (tags, host, raw) = live_belong(&key)?;
    if raw == 0 {
        negative_put(&key);
        return Ok(StockTags {
            concepts: Vec::new(),
            regions: Vec::new(),
            styles: Vec::new(),
            source: format!("mac@{host}"),
            fetched_at: local_stamp(now_secs()),
        });
    }
    let source = format!("mac@{host}");
    if let Err(e) = merge_entry(&key, &tags) {
        // 走到这里通常是「全量刷新已在进行/退避中」：本次没落盘不影响正确性，
        // 下一次全量刷新会把这只股票一起带上，响应本身已经是 MAC 的实时数据。
        eprintln!("[tv] 概念补漏未落盘（{e}），等下一次全量刷新带上");
    }
    let at = local_stamp(now_secs());
    Ok(split_tags(&tags, &source, &at))
}


/// 把单股补漏结果并进缓存文件。
///
/// 事务性：**读旧 → 合并 → 落盘**全在 FILE_LOCK 内。锁外合并会让两个并发补漏各基于
/// 同一份旧快照写全量文件，后写的把先写的整条覆盖掉（丢数据，要等下次全量刷新才回来）。
///
/// 落盘沿用**旧根 `ts`**（见 `write_locked`）：一次补漏不能把 6000 只股票冒充成刚抓的，
/// 更不能把下一次全量刷新整整数地推迟。缓存缺失/过期/退化时不能只补一只就冒充新鲜，
/// 所以那种情况要走全量刷新——但**不能在持锁时调它**（`write_cache` 也要拿这把锁，
/// `std::sync::Mutex` 不可重入，直接死锁），故锁内只产出「有没有写成」，补救留在锁外。
fn merge_entry(key: &str, tags: &[BoardTag]) -> Result<(), String> {
    let written = {
        let guard = FILE_LOCK
            .lock()
            .map_err(|_| "概念缓存锁已损坏".to_string())?;
        let out = match read_cache_file().as_ref().and_then(snapshot_from_value) {
            Some(s) if s.boards > 0 && fresh_this_week(s.ts) => {
                let mut map = s.map.clone();
                map.insert(key.to_string(), tags.to_vec());
                Some(write_locked(&map, &s.source, s.boards, s.ts, s.quality))
            }
            _ => None,
        };
        drop(guard);
        out
    };
    match written {
        Some(r) => r,
        // 缓存缺失/过期/退化：不能只补这一只就冒充全市场新鲜。请求线程手上已经有实时
        // 数据，所以这里**只点火、不等待**——同步跑完会把这次 HTTP 请求挂住 50 秒。
        None => {
            trigger_rebuild_with_backoff()?;
            Ok(())
        }
    }
}


/// 带退避的后台全量刷新：由补漏路径触发，600 秒内不重复尝试。
///
/// 不这么退避的话，MAC 半故障（能连上但板块命令出错）时每只股票都会重试一遍 50 秒的
/// 全量抓取，把请求线程池占满。`rebuild_logged` 会复位标志，失败也不会把状态卡住。
fn trigger_rebuild_with_backoff() -> Result<(), String> {
    let now = now_secs().max(0.0) as u64;
    let last = REBUILD_ATTEMPT_AT.load(Ordering::SeqCst);
    if last != 0 && now - last < REBUILD_BACKOFF_SECS {
        return Err(format!(
            "全量刷新距上次尝试不足 {} 秒，跳过",
            REBUILD_BACKOFF_SECS
        ));
    }
    // 已有刷新在跑时不当失败：那次跑完缓存就新了
    if claim_rebuild().is_err() {
        return Ok(());
    }
    spawn_claimed_rebuild()
}


/// 跑一次全量刷新并打日志（同步与后台线程共用，两处口径必须一致）。
///
/// 调用前必须已占住 `REBUILDING`，守卫在函数内复位。
fn rebuild_logged(t0: std::time::Instant) -> Result<Value, String> {
    REBUILD_ATTEMPT_AT.store(now_secs().max(0.0) as u64, Ordering::SeqCst);
    let _guard = RebuildGuard(&REBUILDING);
    let r = rebuild_inner();
    match &r {
        Ok(v) => println!(
            "[tv] 概念题材刷新完成：{} 个板块 / {} 只股票（{} ms）",
            v.get("boards").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("stocks").and_then(|x| x.as_u64()).unwrap_or(0),
            t0.elapsed().as_millis()
        ),
        Err(e) => println!("[tv] 概念题材刷新失败（{} ms）：{e}", t0.elapsed().as_millis()),
    }
    r
}


/// 占位 `REBUILDING`：已被占用时返回 `Err`，绝不复位别人的标志。
fn claim_rebuild() -> Result<(), String> {
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return Err("概念刷新已在进行".to_string());
    }
    Ok(())
}


/// 全量刷新：板块清单 → 逐板块成分股 → 反向映射 → 落盘。
///
/// 约 460 个板块、900 次请求，实测 40~60 秒。
pub fn rebuild() -> Result<Value, String> {
    live_allowed()?;
    claim_rebuild()?;
    rebuild_logged(std::time::Instant::now())
}


/// 异步触发全量刷新（端点用）：已在跑时拒绝。
pub fn start_rebuild() -> Result<(), String> {
    live_allowed()?;
    claim_rebuild()?;
    spawn_claimed_rebuild()
}


/// 离线开关下拒绝**任何**会出网的刷新入口。
///
/// `TV_LIVE=0` 的语义是「这个进程一个 MAC 请求都不发」。单股查询已在 `for_stock`
/// 挡住，但 `--refresh-concepts` 与 `/api/concepts-rebuild` 是另外两条独立入口：
/// 只挡其中一条等于开关仍然可被绕过（运维脚本或页面上的刷新按钮都能触发 50 秒全量出网）。
/// 两个公开入口都在这里挡，而不是只在 `rebuild_inner` 挡：`start_rebuild` 走后台线程，
/// 拒得太晚调用方已经拿到 202，离线时端点必须当场报错而不是把失败写进日志。
fn live_allowed() -> Result<(), String> {
    if crate::live::enabled() {
        return Ok(());
    }
    Err("实时链路已关闭（TV_LIVE=0 / TV_OFFLINE=1），不做全量刷新".to_string())
}


/// 后台跑一次已占位的全量刷新。
///
/// 建线程失败时 `thread::spawn` 会 panic，`REBUILDING` 就永久留在 true，
/// 之后每次刷新（含 `/api/concepts-rebuild`）永久 409 —— 用 `Builder` 接住这个 `Err`。
fn spawn_claimed_rebuild() -> Result<(), String> {
    let t0 = std::time::Instant::now();
    std::thread::Builder::new()
        .name("concepts-rebuild".into())
        .spawn(move || {
            let _ = rebuild_logged(t0);
        })
        .map_err(|e| {
            REBUILDING.store(false, Ordering::SeqCst);
            format!("概念刷新线程启动失败: {e}")
        })?;
    Ok(())
}


/// REBUILDING 的 RAII 守卫（panic 也会复位标志）。
struct RebuildGuard(&'static AtomicBool);

impl Drop for RebuildGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}


/// 成分股行是否可信：6 位数字代码 + 合法市场码 + 非空名称。
///
/// 这是**解码正确性的守卫**，不是数据清洗：板块码没换算、字段位图算错时，
/// 解析出来的代码会变成乱码或空串，这里能立刻拦住而不是把垃圾写进缓存。
fn valid_member(m: &crate::tdxmac::MacMember) -> bool {
    if m.code.len() != 6 || !m.code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if m.market > 2 {
        return false;
    }
    !m.name.trim().is_empty()
}


/// 网络类操作重试一次：MAC 是公网链路，全量刷新约 900 次请求，
/// 不该让一次抖动废掉 50 秒的工作。
///
/// 只用于「整页可重取」的操作（板块清单的一页、成分股的一页）：失败的那次没有把结果
/// 并进累加器，重取成功后只并一份，所以重试不会造成重复行。
fn with_retry<T, F>(what: &str, mut f: F) -> Result<T, String>
where
    F: FnMut() -> Result<T, String>,
{
    match f() {
        Ok(v) => Ok(v),
        Err(e1) => {
            eprintln!("[tv] {what} 失败（{e1}），重试一次");
            std::thread::sleep(Duration::from_millis(300));
            f()
        }
    }
}


/// 全量刷新主体（不放 RAII 守卫的版本，给同步调用用）。
fn rebuild_inner() -> Result<Value, String> {
    let mut conn = MacConn::connect_any(Duration::from_secs(8))?;
    let host = conn.host().to_string();
    // 本次抓到的所有条目共用这个时刻：它就是根 ts，也是界面上的「抓取时间」
    let ts = now_secs();
    let mut map: HashMap<String, Vec<BoardTag>> = HashMap::new();
    let mut board_total = 0usize;
    let mut rejected = 0usize;
    let mut empty_boards = 0usize;

    for (category, board_type) in CATEGORIES {
        // 板块清单（分页）
        let mut boards: Vec<MacBoard> = Vec::new();
        let mut start = 0u32;
        loop {
            let (page, total) =
                with_retry("板块清单", || conn.board_list(category, start, BOARD_PAGE))?;
            if page.is_empty() {
                break;
            }
            let got = page.len() as u32;
            boards.extend(page);
            start += got;
            if start >= total as u32 {
                break;
            }
        }
        // 剔除指数成分块（通达信88）后再统计：它整块被剔会让空板块计数误伤
        boards.retain(|b| !is_excluded(&b.name));
        board_total += boards.len();

        // 逐板块取成分股（分页），反向建「股票 → 板块」
        for b in boards.iter() {
            let proto = tdxmac::board_protocol_code(&b.code);
            let mut offset = 0u32;
            let mut board_rows = 0usize;
            loop {
                let (members, total) = with_retry(&format!("成分股 {}", b.code), || {
                    conn.board_members(proto, offset, MEMBER_PAGE)
                })?;
                if members.is_empty() {
                    break;
                }
                let got = members.len() as u32;
                for m in members {
                    if !valid_member(&m) {
                        rejected += 1;
                        continue;
                    }
                    board_rows += 1;
                    let entry = map.entry(m.code.clone()).or_default();
                    if !entry.iter().any(|t| t.code == b.code) {
                        entry.push(BoardTag {
                            board_type,
                            code: b.code.clone(),
                            name: b.name.clone(),
                            fetched: ts,
                        });
                    }
                }
                offset += got;
                if offset >= total {
                    break;
                }
            }
            if board_rows == 0 {
                empty_boards += 1;
            }
        }
    }

    if map.is_empty() {
        return Err("刷新结果为空（拒绝落盘，避免污染缓存）".to_string());
    }
    // 解码守卫：成分股行大量不可信 = 板块码换算/字段布局错了，必须报错而不是落盘半垃圾。
    // 空板块（查了但 0 只成分股）本身可能是真状态（新板块），但占比过高同样是解码错。
    let rows: usize = map.values().map(|v| v.len()).sum();
    if rejected > 0 && rejected * 20 > rows + rejected {
        return Err(format!(
            "成分股解码异常：{rejected} 行不可信 / {} 行有效（拒绝落盘）",
            rows
        ));
    }
    if empty_boards * 2 > board_total {
        return Err(format!(
            "空板块过多：{empty_boards}/{board_total}（板块码换算或分页逻辑有误，拒绝落盘）"
        ));
    }
    let source = format!("mac@{host}");
    let quality = Quality { rows, rejected, empty_boards };
    write_cache(&map, &source, board_total, ts, quality)?;
    Ok(json!({
        "ok": true,
        "source": source,
        "boards": board_total,
        "stocks": map.len(),
        "rows": rows,
        "rejected": rejected,
        "empty_boards": empty_boards,
    }))
}


/// 启动时调用：本周还没刷过（缺失/退化/根 ts 早于本周一）就在后台刷新（不阻塞启动）。
///
/// 「每周第一次启动刷一次」就落在这一句：同一周内第二次启动时 `stale()` 为假，直接返回。
pub fn auto_refresh_on_start() {
    if !crate::live::enabled() {
        return;
    }
    if !stale() {
        return;
    }
    if let Err(e) = start_rebuild() {
        eprintln!("[tv] 概念题材后台刷新未启动：{e}");
    }
}

