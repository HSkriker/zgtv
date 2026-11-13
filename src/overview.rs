mod crowd;
mod grid;
mod rps;
mod ser;

use ser::build_payload;
pub use ser::replay_csv;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use serde_json::{json, Value};
use zg_core::sync::MutexExt;
use crate::paths;

const HIST: usize = 1300;      // 每板块参与计算的最近日线根数

const CROWD_WIN: usize = 1250; // 拥挤分位窗口（约 5 年）

const FULL_HIST: usize = 140;  // 视为「历史充足」的根数（不足仍入表，仅诊断）

const TRAJ_DAYS: usize = 20;   // 散点轨迹回看交易日数

const COV_TOL_NUM: usize = 9;  // c1 分母覆盖率下限 = 9/10 × 满覆盖日（见 build_comps）

const COV_TOL_DEN: usize = 10;

const COMP_MIN: usize = 3;     // crowd_comp 至少需要的可用分量数（防单分量伪装合成）

const AMP_WIN: usize = 20;     // c2 平均振幅窗口

const BETA_WIN: usize = 60;    // c3 滚动 β 窗口（对 000300）

const CORR_WIN: usize = 20;    // c4 量价相关性窗口

const MA_WIN: usize = 20;      // c5 乖离率 MA 窗口

const IR_WIN: usize = 252;     // 信息比率窗口（国盛趋势度口径）

const RPS_WIN_M: usize = 20;   // 成分股 RPS 广度月窗

const RPS_WIN_Q: usize = 63;   // 成分股 RPS 广度季窗（主窗，对应「按季度周期看板块强弱」）

const RPS_HI: f64 = 90.0;      // 强股阈值：全 A 截面分位 ≥90（与 rs90 主线口径一致）

/// 二级 ref → [(市场, 代码)]，市场为 sh/sz/bj
type Groups = HashMap<String, Vec<(String, String)>>;

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn r2(v: f64) -> Value {
    if !v.is_finite() {
        return Value::Null;
    }
    Value::from(round2(v))
}

fn read_json(p: &std::path::Path) -> Option<Value> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

fn catalog_path() -> std::path::PathBuf {
    paths::tv_root().join("modules").join("market").join("industry_catalog.json")
}

/// 通达信研究行业二级节点（catalog 中 ref 为 5 位的节点，共 128 个）
struct Node {
    code: String, // 881xxx
    name: String,
    ref_: String, // Xnnnn
}

/// 单板块自身序列（动量/高低点用）
struct Sec {
    name: String,
    code: String,
    ref_: String,
    closes: Vec<f64>,
    highs: Vec<f64>,
    lows: Vec<f64>,
    /// 自身日线是否到达 as_of（缺最近几日的行业，自身序列派生的指标整体置空，
    /// 避免同行混排「截至旧日期」的数值——排名/RS 走对齐轴无此问题）
    fresh: bool,
}

/// 按基准轴逐日对齐的单板块序列——缺该日数据为 None
struct Series {
    c: Vec<Option<f64>>,
    h: Vec<Option<f64>>,
    l: Vec<Option<f64>>,
    a: Vec<Option<f64>>,
}

/// 拥挤度五分量的原始值序列（轴对齐）。全部量价类，无估值/资金维度：
/// c1 成交额占比 · c2 20日均振幅 · c3 60日滚动β(对000300) · c4 20日corr(日涨跌,成交额环比) · c5 乖离率 c/MA20−1
/// 分位在读取时按 pct_at 现算（各自 ≤CROWD_WIN 日自身历史，point-in-time 无 lookahead）
struct CrowdComps {
    share: Vec<Option<f64>>,
    amp20: Vec<Option<f64>>,
    beta60: Vec<Option<f64>>,
    corr20: Vec<Option<f64>>,
    bias20: Vec<Option<f64>>,
}

fn mom(closes: &[f64], back: usize) -> Option<f64> {
    let n = closes.len();
    if n <= back || closes[n - 1 - back] <= 0.0 {
        return None;
    }
    Some((closes[n - 1] / closes[n - 1 - back] - 1.0) * 100.0)
}

/// 阈值分类按界面显示精度（整数分位）取整：先落到序列化精度（2 位），
/// 再按前端 toFixed(0) 的整数显示取整，避免显示 30 / 90 但 label/rs90 判成另一类。
fn pct0(v: f64) -> f64 {
    round2(v).round()
}

fn label_of(rk6: Option<f64>, crowd: Option<f64>) -> &'static str {
    let (r, c) = match (rk6, crowd) {
        (Some(r), Some(c)) => (pct0(r), pct0(c)),
        _ => return "数据不足",
    };
    if r >= 70.0 {
        if c >= 70.0 { "共振占优_高拥挤" } else { "共振占优_尚不拥挤" }
    } else if r <= 30.0 {
        if c <= 30.0 { "底部酝酿_低拥挤" }
        else if c >= 70.0 { "底部高拥挤_回避" }
        else { "底部观察" }
    } else {
        "中性观察"
    }
}

fn rs90_of(rs_comp: Option<f64>) -> bool {
    rs_comp.map(|v| pct0(v) >= 90.0).unwrap_or(false)
}

/// 20 个交易日的位移：t 与 t−20 之差。任一端缺失即为 None（不留 0 假装有值）。
/// 所有加速度字段共用这一个口径：rank6_d20 / rankc_d20 / mom_rank_ir_d20 / rs252_d20。
fn d20(cur: Option<f64>, prev: Option<f64>) -> Option<f64> {
    match (cur, prev) {
        (Some(a), Some(b)) => Some(a - b),
        _ => None,
    }
}

fn r1(v: f64) -> Value {
    if !v.is_finite() {
        return Value::Null;
    }
    Value::from((v * 10.0).round() / 10.0)
}

/// 相对基准超额（百分点）：c/cp − b/bp，回看 back 根（对齐轴）。
fn rs_at(closes: &[Option<f64>], bench: &[Option<f64>], i: usize, back: usize) -> Option<f64> {
    if i < back {
        return None;
    }
    match (closes[i]?, closes[i - back]?, bench[i]?, bench[i - back]?) {
        (c, cp, b, bp) if cp > 0.0 && bp > 0.0 => Some((c / cp - b / bp) * 100.0),
        _ => None,
    }
}

/// 全部逐日网格的计算上下文：payload 与 replay_csv 共用同一份代码路径，
/// 保证回放口径与线上口径逐字一致
struct Ctx {
    nodes: Vec<Node>,
    groups: Groups,
    n_members: usize,
    n_bj: usize,
    cat_updated: String,
    axis: Vec<String>,
    n: usize,
    secs: Vec<Sec>,
    series: Vec<Series>,
    bench_c: Vec<Option<f64>>,
    comps: Vec<CrowdComps>,
    /// 该日有 bar 的行业家数（c1 分母覆盖率）与轴上满覆盖日的家数
    cov: Vec<usize>,
    cov_full: usize,
    /// 行业侧末根的众数日期：比基准轴末根新 ⇒ 基准掉队，全表 fresh 门禁会误杀
    ind_last: Option<String>,
    ranks: Vec<Vec<Option<f64>>>,    // Rk6 截面百分位
    ranksc: Vec<Vec<Option<f64>>>,   // RkC（IBD 权重）截面百分位
    ranks_ir: Vec<Vec<Option<f64>>>, // IR 截面百分位
    rs20r: Vec<Vec<Option<f64>>>,
    rs63r: Vec<Vec<Option<f64>>>,
    rs252r: Vec<Vec<Option<f64>>>,
    missing: Vec<Value>,
    n_full: usize,
    n_no_file: usize,
    n_short: usize,
}

impl Ctx {
    /// RS 合成分：rs20/63/252 三个截面百分位等权平均（国盛 RS 双重验证口径），任一缺为 null
    fn rs_comp(&self, i: usize, t: usize) -> Option<f64> {
        match (self.rs20r[i][t], self.rs63r[i][t], self.rs252r[i][t]) {
            (Some(a), Some(b), Some(c)) => Some((a + b + c) / 3.0),
            _ => None,
        }
    }
}

fn cache_stamp() -> String {
    // v14：双信号叠加——v13 的基准文件指纹（抓 TDX 原地追加）之外，恢复 v12 的
    // lday 目录 mtime（抓 rsync/拷贝等「临时文件+改名」落地的单只回填：改名必更新
    // 目录 mtime，哪怕三个基准文件都没动）。两个信号只会让失效更敏感，不会更钝。
    // 剩余盲区：对非基准成分的**原地改写**（不经改名、基准也不动）——TDX 盘后
    // 同步整目录都写（含基准），实际到不了这条路径；真遇到就 force=1。
    // v13：失效信号从 lday 目录 mtime 改为「交易日 + 三市场基准文件指纹」。
    // 直读 DFZQ 后 TDX 往已有 .day 里追加，目录 mtime 不会变，旧指纹会让多日不重建。
    // 语义定义见 bars::market_stamp。
    // v12：广度新增双窗成分股 RPS 广度（rps20_pct/rps63_pct/n_rps20/n_rps63，北交所不进截面）
    // v11：删无人消费的 rank6_traj/rankc_traj/crowd_mix + c1 覆盖率门禁 + 陈旧原因字段
    // v10：补 IR 排名与 RS252 的 20 日位移（mom_rank_ir_d20 / rs252_d20）——
    //       散点加速度外圈统一为「纵轴指标的 20 日位移」，双窗模式不再无圈可画
    // v9：拥挤分量补当日 bar 门禁 + label/rs90 改按界面显示整数分位判定
    // v8：合成拥挤/IR/RS 合成口径上线 + rank 网格改全程逐日，旧缓存全部失效
    // v7：目录聚合指纹取代 v5/v6 的哨兵抽样——rsync 默认临时文件+改名落地，
    // 任一 .day 增/改都会更新所在 lday 目录的 mtime，单只补数据不再无感续命
    let lday = |m: &str| crate::cache::file_stamp(&zg_core::paths::get().vipdoc.join(m).join("lday"));
    format!(
        "v14|{}|{}|{}|{}|{}",
        crate::cache::file_stamp(&catalog_path()),
        crate::bars::market_stamp(),
        lday("sh"),
        lday("sz"),
        lday("bj"),
    )
}

/// 按数据指纹缓存到 target/cache/sector-overview.json；stamp 变了（当晚日线更新）自动重建。
///
/// 单飞：一次全量重建要读 5587 份日线（约 50 秒）。此前没有任何互斥——启动预热
/// （main.rs）与首个请求重叠、或连点界面上的「刷新」（force 会绕过前端的重入保护）
/// 都会并行各算一遍；而落盘是 `fs::write`（truncate+write），并发读方拿到半截 JSON
/// 会被当成「未命中」，于是再触发一次重建。bars/chan/ind 走 cache.rs 的认领机制，
/// 总览是文件缓存，这里补等价的一把重建锁 + 进程内 memo。
static MEMO: OnceLock<Mutex<Option<(String, Value)>>> = OnceLock::new();

static BUILD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

static BUILD_GEN: AtomicU64 = AtomicU64::new(0);

fn memo() -> &'static Mutex<Option<(String, Value)>> {
    MEMO.get_or_init(|| Mutex::new(None))
}

fn build_lock() -> &'static Mutex<()> {
    BUILD_LOCK.get_or_init(|| Mutex::new(()))
}

fn memo_hit(stamp: &str) -> Option<Value> {
    let g = memo().lock_ok();
    g.as_ref().filter(|(s, _)| s == stamp).map(|(_, p)| p.clone())
}

fn memo_put(stamp: &str, payload: &Value) {
    *memo().lock_ok() = Some((stamp.to_string(), payload.clone()));
}

/// 盘上缓存的 stamp 与本次一致时取 payload；否则视为未命中（旧口径自动失效）
fn disk_hit(file: &std::path::Path, stamp: &str) -> Option<Value> {
    let doc = read_json(file)?;
    if doc.get("stamp").and_then(|x| x.as_str()) == Some(stamp) {
        doc.get("payload").cloned()
    } else {
        None
    }
}

/// 先写同目录 .tmp 再 rename：读方只会看到完整文件
fn write_cache(dir: &std::path::Path, file: &std::path::Path, stamp: &str, payload: &Value) -> Result<(), String> {
    let doc = json!({
        "stamp": stamp,
        "built_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "payload": payload,
    });
    std::fs::create_dir_all(dir).map_err(|e| format!("建缓存目录失败: {e}"))?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&doc).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写临时缓存失败: {e}"))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("缓存改名失败: {e}"))
}

pub fn payload(force: bool) -> Result<Value, String> {
    let stamp = cache_stamp();
    let dir = paths::tv_root().join("target").join("cache");
    let file = dir.join("sector-overview.json");
    if !force {
        if let Some(p) = memo_hit(&stamp) {
            return Ok(p);
        }
        if let Some(p) = disk_hit(&file, &stamp) {
            memo_put(&stamp, &p);
            return Ok(p);
        }
    }
    let gen_before = BUILD_GEN.load(Ordering::SeqCst);
    let _hold = build_lock().lock_ok();
    // 排队期间已有人重建完（含并发的第二次 force）：同一 stamp 直接复用，不重算第二遍
    if BUILD_GEN.load(Ordering::SeqCst) != gen_before {
        if let Some(p) = disk_hit(&file, &stamp) {
            memo_put(&stamp, &p);
            return Ok(p);
        }
    }
    let payload = build_payload()?;
    memo_put(&stamp, &payload);
    BUILD_GEN.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = write_cache(&dir, &file, &stamp, &payload) {
        // 缓存只是加速：落盘失败（只读盘/满盘）不该让已经算好的数据报 500
        eprintln!("[overview] 缓存落盘失败（本次仍返回新算结果）: {e}");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_rs90_use_display_precision() {
        // 界面显示 27 / 30：应为筑底
        assert_eq!(label_of(Some(26.77), Some(30.15)), "底部酝酿_低拥挤");
        // 界面显示 70 / 30：占优尚不拥挤
        assert_eq!(label_of(Some(69.6), Some(30.4)), "共振占优_尚不拥挤");
        // 原始 69.495 先落 2 位=69.50，再按显示=70，必须与 toFixed(0) 一致
        assert_eq!(label_of(Some(96.85), Some(69.495)), "共振占优_高拥挤");
        // 界面显示 90：应点亮 RS90；显示 89：不点亮
        assert!(rs90_of(Some(89.53)));
        assert!(rs90_of(Some(89.495)));
        assert!(!rs90_of(Some(89.49)));
    }

    #[test]
    fn d20_is_t_minus_t20_and_missing_stays_none() {
        // 所有加速度字段共用的口径：t 与 t−20 之差，符号即方向
        assert_eq!(d20(Some(80.0), Some(65.0)), Some(15.0));
        assert_eq!(d20(Some(65.0), Some(80.0)), Some(-15.0));
        assert_eq!(d20(Some(80.0), Some(80.0)), Some(0.0));
        // 任一端缺失 → None（不补 0 假装有值）
        assert_eq!(d20(None, Some(65.0)), None);
        assert_eq!(d20(Some(80.0), None), None);
        assert_eq!(d20(None, None), None);
    }

    #[test]
    fn rs252_d20_sign_tracks_relative_outperformance() {
        // 基准恒定 100；行业前 280 根平走，最近 20 根冲到 130 → 相对超额在扩大
        let n = 301;
        let bench: Vec<Option<f64>> = vec![Some(100.0); n];
        let up: Vec<Option<f64>> = (0..n)
            .map(|i| Some(if i > 280 { 130.0 } else { 100.0 }))
            .collect();
        let cur = rs_at(&up, &bench, 300, 252).unwrap();   // 130/100 → +30 百分点
        let prev = rs_at(&up, &bench, 280, 252).unwrap();  // 100/100 → 0
        assert!((cur - 30.0).abs() < 1e-9, "cur={cur}");
        assert!((prev - 0.0).abs() < 1e-9, "prev={prev}");
        assert!(d20(Some(cur), Some(prev)).unwrap() > 0.0, "超额扩大 → 位移为正");

        // 反向：最近 20 根跌到 70 → 超额在收缩，位移为负
        let down: Vec<Option<f64>> = (0..n)
            .map(|i| Some(if i > 280 { 70.0 } else { 100.0 }))
            .collect();
        let cur2 = rs_at(&down, &bench, 300, 252).unwrap();
        let prev2 = rs_at(&down, &bench, 280, 252).unwrap();
        assert!(d20(Some(cur2), Some(prev2)).unwrap() < 0.0, "超额收缩 → 位移为负");

        // 历史不足：t−20 处凑不满 252 根回看 → 位移为空，不留 0 假装有值
        let short: Vec<Option<f64>> = vec![Some(100.0); 260];
        let b2: Vec<Option<f64>> = vec![Some(100.0); 260];
        assert!(rs_at(&short, &b2, 259, 252).is_some());
        assert!(rs_at(&short, &b2, 239, 252).is_none());
    }
}
