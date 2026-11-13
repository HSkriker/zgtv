//! 本地通达信 .day/.lc1 读取 + 市场归属判定（逐行对应 `zgcore/bars/reader.py`）。
//!
//! 格式（实测铁律，勿改）：
//!   .day 32B : <IIIIIfii date(int YYYYMMDD) O/H/L/C(×100) amount(f32 元) vol(i32 股) 保留
//!   .lc1 32B : <HHfffffIi date(u16=(year-2004)*2048+month*100+day) time(u16 当日分钟)
//!              O/H/L/C(f32) amount(f32 元) vol(i32 股) 保留
//!
//! 读取返回**未复权原始价**；复权只能在读全历史后做（见 adjust::apply_qfq）。

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::error::{py_repr, Result, ZgError};
use crate::model::Bar;
use crate::paths;
use crate::sync::MutexExt;

pub const MIN_BARS: usize = 60;

pub const MKT_SH: &str = "sh";
pub const MKT_SZ: &str = "sz";
pub const MKT_BJ: &str = "bj";

pub fn index_prefix(market: &str) -> &'static [&'static str] {
    match market {
        MKT_SH => &["000", "88", "999"],
        MKT_SZ => &["399"],
        _ => &[],
    }
}

pub const MIN_FREQ_MINUTES: &[(&str, u32)] = &[
    ("F1", 1),
    ("F5", 5),
    ("F15", 15),
    ("F30", 30),
    ("F60", 60),
];

/// 笔数上限：max_bi_num ≥ ceil(图窗根数 / min_bi_len)，改表会改中枢/买卖点窗口。
/// 2026-11-01 按 tv 图窗重算（min_bi_len=6）：日线图窗 2430 根需 ≥405、1 分图窗 28800 根需
/// ≥4800——旧表 50/200/140/80 系 zg 窄图窗的遗留，笔窗口只盖住最近一小段（日线仅 ~300 根有笔）。
/// 数值 = ceil(图窗/6) × ~1.15 余量。zg 侧若图窗不同需按其窗口另算（回同步事项）。
pub fn max_bi_num_for(freq: &str) -> usize {
    match freq.to_uppercase().as_str() {
        "D" | "W" | "M" => 500,
        "F1" => 5000,
        "F5" => 1000,
        "F15" => 400,
        "F30" => 200,
        "F60" => 100,
        _ => 500,
    }
}

pub fn min_freq_minutes(freq: &str) -> Option<u32> {
    let f = freq.to_uppercase();
    MIN_FREQ_MINUTES.iter().find(|(k, _)| *k == f).map(|(_, v)| *v)
}

// ---------------------------------------------------------------------------
// LRU：Python 用「dict 保序 + 命中 refresh」实现，这里用 HashMap + VecDeque 复刻。
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LruMap<K: Eq + Hash + Clone, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V> LruMap<K, V> {
    pub fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), order: VecDeque::new(), cap: cap.max(1) }
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).expect("pos");
            self.order.push_back(k);
        }
    }

    pub fn put(&mut self, key: K, value: V) {
        self.map.insert(key.clone(), value);
        self.touch(&key);
        if !self.order.contains(&key) {
            self.order.push_back(key);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.map.contains_key(key) {
            self.touch(key);
            self.map.get(key)
        } else {
            None
        }
    }

    pub fn get_cloned(&mut self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        self.get(key).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

/// Python `lru_put(store, key, value, max_size=None)` 的等价：写入并逐出最旧。
pub fn lru_put<K: Eq + Hash + Clone, V>(store: &mut LruMap<K, V>, key: K, value: V,
                                        max_size: Option<usize>) {
    if let Some(cap) = max_size {
        if cap != store.cap {
            store.cap = cap.max(1);
        }
    }
    store.put(key, value);
}

static MIN_MEM_MAX: OnceLock<usize> = OnceLock::new();

pub fn min_mem_max() -> usize {
    *MIN_MEM_MAX.get_or_init(paths::min_mem_max)
}

/// 分钟线进程内缓存条目：(mtime, bars)。
type MinMemEntry = (SystemTime, Arc<Vec<Bar>>);
/// 分钟线进程内缓存（key=(market, code)，按文件 mtime 判新鲜）。
type MinMemMap = LruMap<(String, String), MinMemEntry>;

fn min_mem() -> &'static Mutex<MinMemMap> {
    static MEM: OnceLock<Mutex<MinMemMap>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(LruMap::new(min_mem_max())))
}

// ---------------------------------------------------------------------------

/// 'sh600519' / '600519.SH' / '600519' → (market|None, '600519')。
pub fn strip_code(raw: &str) -> (Option<String>, String) {
    let s = raw.trim().to_uppercase().replace('.', "");
    for mkt in ["SH", "SZ", "BJ"] {
        if let Some(rest) = s.strip_prefix(mkt) {
            return (Some(mkt.to_lowercase()), rest.to_string());
        }
    }
    for mkt in ["SH", "SZ", "BJ"] {
        if let Some(rest) = s.strip_suffix(mkt) {
            return (Some(mkt.to_lowercase()), rest.to_string());
        }
    }
    (None, s)
}

pub fn day_path(market: &str, code: &str) -> PathBuf {
    paths::get().vipdoc.join(market).join("lday").join(format!("{market}{code}.day"))
}

pub fn min_path(market: &str, code: &str) -> PathBuf {
    paths::get().vipdoc.join(market).join("minline").join(format!("{market}{code}.lc1"))
}

/// 解析 (market, code)。000001 的沪深两种身份必须靠显式 market 或代码标签区分。
pub fn resolve(code: &str, market: Option<&str>) -> Result<(String, String)> {
    if let Some(m) = market {
        let m = m.trim().to_lowercase();
        if !matches!(m.as_str(), MKT_SH | MKT_SZ | MKT_BJ) {
            return Err(ZgError::bridge(format!(
                "未知市场: {}（只支持 sh/sz/bj）",
                py_repr(market.unwrap_or_default())
            )));
        }
        return Ok((m, strip_code(code).1));
    }
    let (tagged, cd) = strip_code(code);
    if let Some(t) = tagged {
        return Ok((t, cd));
    }
    let is_digits = cd.len() == 6 && cd.chars().all(|c| c.is_ascii_digit());
    if !is_digits {
        return Err(ZgError::bridge(format!(
            "代码格式非法: {}（应为 6 位数字，可带 sh/sz 前缀或后缀）",
            py_repr(code)
        )));
    }
    if cd.starts_with('4') || cd.starts_with("92") {
        return Ok((MKT_BJ.into(), cd));
    }
    if cd.starts_with("88") {
        return Ok((MKT_SH.into(), cd));
    }
    if cd.starts_with('8') {
        return Ok((MKT_BJ.into(), cd));
    }
    if cd.starts_with("999") {
        return Ok((MKT_SH.into(), cd));
    }
    if cd.starts_with('6') || cd.starts_with('9') || cd.starts_with('5') {
        return Ok((MKT_SH.into(), cd));
    }
    if cd.starts_with('3') || cd.starts_with('2') || cd.starts_with('1') {
        return Ok((MKT_SZ.into(), cd));
    }
    if cd.starts_with('0') {
        if day_path(MKT_SZ, &cd).exists() {
            return Ok((MKT_SZ.into(), cd));
        }
        if day_path(MKT_SH, &cd).exists() {
            return Ok((MKT_SH.into(), cd));
        }
        return Ok((MKT_SZ.into(), cd));
    }
    Err(ZgError::bridge(format!("无法判定市场: {}", py_repr(code))))
}

/// 纯前缀规则判定市场（不看本地文件）。
/// 与 `resolve` 的前缀分支同口径，但不做磁盘探测 —— 供 TQ/THS 后缀拼接这类
/// 需要纯函数确定性的场合用（920xxx→BJ、88xxx→SH、8xxxx→BJ、999xxx→SH）。
pub fn market_of_prefix(code: &str) -> Option<&'static str> {
    let cd = code.trim();
    if cd.len() != 6 || !cd.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if cd.starts_with('4') || cd.starts_with("92") {
        return Some(MKT_BJ);
    }
    if cd.starts_with("88") {
        return Some(MKT_SH);
    }
    if cd.starts_with('8') {
        return Some(MKT_BJ);
    }
    if cd.starts_with("6") || cd.starts_with('9') || cd.starts_with('5') {
        return Some(MKT_SH);
    }
    if cd.starts_with('3') || cd.starts_with('2') || cd.starts_with('1') || cd.starts_with('0') {
        return Some(MKT_SZ);
    }
    None
}

pub fn is_index(market: &str, code: &str) -> bool {
    index_prefix(market).iter().any(|p| code.starts_with(p))
}

// ---------------------------------------------------------------------------
// 读取
// ---------------------------------------------------------------------------

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn i32_at(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn f32_at(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// .day 定长 32B 行解析（`start` 为起始行号；尾部读取时 data 已按行对齐裁好，传 0）。
fn parse_day_rows(data: &[u8], start: usize) -> Vec<Bar> {
    let n = data.len() / 32;
    if start >= n {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n - start);
    for i in start..n {
        let p = &data[i * 32..i * 32 + 32];
        let d = u32_at(p, 0) as i32;
        if !(19900101..=20991231).contains(&d) {
            continue;
        }
        out.push(Bar {
            date: format!("{:04}-{:02}-{:02}", d / 10000, d / 100 % 100, d % 100),
            open: u32_at(p, 4) as f64 * 0.01,
            high: u32_at(p, 8) as f64 * 0.01,
            low: u32_at(p, 12) as f64 * 0.01,
            close: u32_at(p, 16) as f64 * 0.01,
            volume: i32_at(p, 24) as f64,
            amount: f32_at(p, 20) as f64,
        });
    }
    out
}

/// 读 .day 全历史 → Vec<Bar>（旧→新）。volume 股、amount 元、价格未复权。
pub fn read_day(market: &str, code: &str) -> Result<Vec<Bar>> {
    let path = day_path(market, code);
    if !path.exists() {
        return Err(ZgError::bridge(format!("本地日线数据不存在: {}", path.display())));
    }
    let data = std::fs::read(&path)?;
    if data.len() / 32 == 0 {
        return Err(ZgError::bridge(format!("本地日线数据为空: {}", path.display())));
    }
    let out = parse_day_rows(&data, 0);
    if out.is_empty() {
        return Err(ZgError::bridge(format!("本地日线数据无可用记录: {}", path.display())));
    }
    Ok(out)
}

/// 只读 .day 尾部 `max_bars` 根（全市场扫描用）。
///
/// 与 `read_day` 的末段逐值相等：行解析、非法日期过滤、错误文案完全共用。
/// 读盘用 seek 定位到尾部；读失败（盘中 TDX 正在追加/截断）退回整文件读，
/// 不猜半根。
pub fn read_day_tail(market: &str, code: &str, max_bars: usize) -> Result<Vec<Bar>> {
    use std::io::{Read, Seek, SeekFrom};

    let path = day_path(market, code);
    // 不先 exists() 再 open()：drvfs（/mnt/g）上每次 stat 要 ~2.9ms，比整读还贵，
    // 全市场扫 5572 只就是 16 秒。改由 open 的 NotFound 兜底，错误文案不变。
    let mut f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ZgError::bridge(format!("本地日线数据不存在: {}", path.display())));
        }
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let total = len / 32;
    if total == 0 {
        return Err(ZgError::bridge(format!("本地日线数据为空: {}", path.display())));
    }
    let want = max_bars.max(1).min(total);

    let mut data = vec![0u8; want * 32];
    let tail_ok = f
        .seek(SeekFrom::Start(((total - want) * 32) as u64))
        .is_ok()
        && f.read_exact(&mut data).is_ok();
    let out = if tail_ok {
        parse_day_rows(&data, 0)
    } else {
        // 盘中 TDX 可能正在写：元数据里的长度和实际读到的行数对不上时，整读兜底
        let all = std::fs::read(&path)?;
        let n = all.len() / 32;
        if n == 0 {
            return Err(ZgError::bridge(format!("本地日线数据为空: {}", path.display())));
        }
        parse_day_rows(&all, n.saturating_sub(want))
    };

    if out.is_empty() {
        return Err(ZgError::bridge(format!("本地日线数据无可用记录: {}", path.display())));
    }
    Ok(out)
}

/// 读 .lc1 全量 1 分钟（旧→新），按文件 mtime 缓存；返回缓存对象本体（Arc 共享）。
pub fn read_min(market: &str, code: &str) -> Result<Arc<Vec<Bar>>> {
    if market == MKT_BJ {
        return Err(ZgError::bridge("北交所暂不支持"));
    }
    let path = min_path(market, code);
    if !path.exists() {
        return Err(ZgError::bridge(format!("本地分钟数据不存在: {}", path.display())));
    }
    let mtime = std::fs::metadata(&path)?.modified()?;
    let key = (market.to_string(), code.to_string());
    {
        let mut mem = min_mem().lock_ok();
        if let Some((m, bars)) = mem.get_cloned(&key) {
            if m == mtime {
                return Ok(bars);
            }
        }
    }
    let data = std::fs::read(&path)?;
    let out = parse_lc1_rows(&data, 0);
    if out.is_empty() {
        return Err(ZgError::bridge(format!("本地分钟数据无可用记录: {}", path.display())));
    }
    let arc = Arc::new(out);
    let mut mem = min_mem().lock_ok();
    lru_put(&mut mem, key, (mtime, arc.clone()), None);
    Ok(arc)
}

/// 测试辅助：清空分钟缓存（模拟新进程）。
pub fn clear_min_cache() {
    min_mem().lock_ok().clear();
}

/// lc1 定长 32B 行解析（`start` 为起始行号，用于尾部读取）。
fn parse_lc1_rows(data: &[u8], start: usize) -> Vec<Bar> {
    let n = data.len() / 32;
    if start >= n {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n - start);
    for i in start..n {
        let p = &data[i * 32..i * 32 + 32];
        let d = u16::from_le_bytes([p[0], p[1]]) as i32;
        let t = u16::from_le_bytes([p[2], p[3]]) as i32;
        let r = d % 2048;
        out.push(Bar {
            date: format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                2004 + d / 2048,
                r / 100,
                r % 100,
                t / 60,
                t % 60
            ),
            open: f32_at(p, 4) as f64,
            high: f32_at(p, 8) as f64,
            low: f32_at(p, 12) as f64,
            close: f32_at(p, 16) as f64,
            volume: i32_at(p, 24) as f64,
            amount: f32_at(p, 20) as f64,
        });
    }
    out
}

/// 只解析 lc1 尾部 `max_bars` 根（构建分钟缓存用）。
///
/// 全市场 `minline/` 约 11GB，而 480 根 30 分K 只需 1.4 万根 1 分K（≈460KB）。
/// 这里仍整文件读入（单只最大约 1.2MB），但只解析尾部，避免构造上百万个 Bar；
/// 尾部截断不影响结果：重采样只保留末尾 `lmt` 个完整桶，起始处那半个桶会被丢弃。
pub fn read_min_tail(market: &str, code: &str, max_bars: usize) -> Result<Vec<Bar>> {
    if market == MKT_BJ {
        return Err(ZgError::bridge("北交所暂不支持"));
    }
    let path = min_path(market, code);
    if !path.exists() {
        return Err(ZgError::bridge(format!("本地分钟数据不存在: {}", path.display())));
    }
    let data = std::fs::read(&path)?;
    let total = data.len() / 32;
    if total == 0 {
        return Err(ZgError::bridge(format!("本地分钟数据为空: {}", path.display())));
    }
    let start = total.saturating_sub(max_bars.max(1));
    let out = parse_lc1_rows(&data, start);
    if out.is_empty() {
        return Err(ZgError::bridge(format!("本地分钟数据无可用记录: {}", path.display())));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_and_resolve_rules() {
        assert_eq!(strip_code("sh600519"), (Some("sh".into()), "600519".into()));
        assert_eq!(strip_code("600519.SH"), (Some("sh".into()), "600519".into()));
        assert_eq!(strip_code("600519"), (None, "600519".into()));

        assert_eq!(resolve("600519", None).unwrap(), ("sh".into(), "600519".into()));
        assert_eq!(resolve("000858", None).unwrap(), ("sz".into(), "000858".into()));
        assert_eq!(resolve("399001", None).unwrap(), ("sz".into(), "399001".into()));
        assert_eq!(resolve("880301", None).unwrap(), ("sh".into(), "880301".into()));
        assert_eq!(resolve("830799", None).unwrap(), ("bj".into(), "830799".into()));
        assert_eq!(resolve("999999", None).unwrap(), ("sh".into(), "999999".into()));

        let e = resolve("ABCDEF", None).unwrap_err();
        assert_eq!(e.kind(), "BridgeError");
        assert!(e.message().starts_with("代码格式非法: 'ABCDEF'"));
        let e = resolve("600519", Some("us")).unwrap_err();
        assert_eq!(e.message(), "未知市场: 'us'（只支持 sh/sz/bj）");
    }

    #[test]
    fn max_bi_num_table() {
        // 2026-11-01 按 tv 图窗重算（见 max_bi_num_for 注释）：数值与不变量都钉住
        assert_eq!(max_bi_num_for("D"), 500);
        assert_eq!(max_bi_num_for("W"), 500);
        assert_eq!(max_bi_num_for("M"), 500);
        assert_eq!(max_bi_num_for("f1"), 5000);
        assert_eq!(max_bi_num_for("F5"), 1000);
        assert_eq!(max_bi_num_for("F15"), 400);
        assert_eq!(max_bi_num_for("F30"), 200);
        assert_eq!(max_bi_num_for("F60"), 100);
        assert_eq!(max_bi_num_for("X"), 500);
        // 不变量：≥ ceil(图窗根数 / min_bi_len)
        assert!(max_bi_num_for("D") * 6 >= 2430);
        assert!(max_bi_num_for("F1") * 6 >= 28800);
    }
}
