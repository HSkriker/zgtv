//! 行业热度与情报 —— 对齐 `modules/market/industry_heat.py` + `industry_intel.py`。
//!
//! 网络工作在线程外执行，陈旧数据保持可见（与 Python 一致）。
//! 调用方须持有 `Arc<Self>`（`request`/`refresh` 用 `self: &Arc<Self>` 接收者，
//! 后台线程持有 Arc 克隆，保证对象生命周期安全）。

use zg_core::sync::MutexExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Datelike;
use regex::Regex;
use serde_json::{json, Map, Value};

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|x| x.is_finite()),
        _ => None,
    }
}

/// 未报价行业/个股恒排最后；并列按 code 稳定排序。
pub fn rank(rows: Vec<Value>, order: &str) -> Vec<Value> {
    let mut rows = rows;
    rows.sort_by(|a, b| {
        let ka = rank_key(a, order);
        let kb = rank_key(b, order);
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

fn rank_key(r: &Value, order: &str) -> (bool, f64, String) {
    let heat = r.get("heat").and_then(|h| h.as_f64());
    let sign = if order == "desc" { -1.0 } else { 1.0 };
    (
        heat.is_none(),
        heat.unwrap_or(0.0) * sign,
        r.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string(),
    )
}

// ---------------------------------------------------------------- 行业热度

/// 后台刷新闸门：无论正常结束还是 panic，Drop 都复位 running，避免永久「刷新中」。
struct RunningGuard<'a>(&'a Mutex<bool>);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock_ok() = false;
    }
}

type HeatProvider = dyn Fn(&[Value]) -> Result<(Vec<Value>, String), String> + Send + Sync;

pub struct IndustryHeat {
    catalog: Value,
    nodes: HashMap<String, Value>,
    members: HashMap<String, Value>,
    provider: Arc<HeatProvider>,
    cache_path: String,
    running: Mutex<bool>,
    error: Mutex<String>,
    catalog_error: Mutex<String>,
    attempt: Mutex<f64>,
    snapshot: Mutex<Value>,
}

impl IndustryHeat {
    pub fn new(catalog_path: &str, cache_path: &str, provider: Arc<HeatProvider>) -> Self {
        // 行业目录缺失/损坏不能让整个行业面板永久 500：降级成「目录不可用」的空目录，
        // 由 request/request_all 回错误文案。
        let (catalog, catalog_error) = match std::fs::read_to_string(catalog_path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => (v, String::new()),
                Err(e) => (
                    json!({"nodes": [], "members": []}),
                    format!("行业目录解析失败: {e}"),
                ),
            },
            Err(e) => (
                json!({"nodes": [], "members": []}),
                format!("行业目录读取失败: {e}"),
            ),
        };
        let nodes = index_nodes(&catalog);
        let members = index_members(&catalog);
        let mut snapshot = json!({"updated": Value::Null, "ts": 0, "source": "", "quotes": {}});
        if let Ok(text) = std::fs::read_to_string(cache_path) {
            if let Ok(cached) = serde_json::from_str::<Value>(&text) {
                if cached.get("quotes").and_then(|q| q.as_object()).is_some() {
                    snapshot = cached;
                }
            }
        }
        Self {
            catalog,
            nodes,
            members,
            provider,
            cache_path: cache_path.to_string(),
            running: Mutex::new(false),
            error: Mutex::new(String::new()),
            catalog_error: Mutex::new(catalog_error),
            attempt: Mutex::new(0.0),
            snapshot: Mutex::new(snapshot),
        }
    }

    fn refresh_inner(&self) {
        let rows_source = (self.provider)(&self.members.values().cloned().collect::<Vec<_>>());
        match rows_source {
            Ok((rows, source)) => {
                let mut quotes = Map::new();
                for row in &rows {
                    let code = row.get("code").and_then(|c| c.as_str())
                        .or_else(|| row.get("f12").and_then(|c| c.as_str()))
                        .unwrap_or("")
                        .to_string();
                    if !self.members.contains_key(&code) {
                        continue;
                    }
                    let price = number(row.get("price").or_else(|| row.get("f2")));
                    let pct = number(row.get("pct").or_else(|| row.get("f3")));
                    let amount = number(row.get("amount").or_else(|| row.get("f6")));
                    let vol = number(row.get("vol").or_else(|| row.get("f5")));
                    let suspended = row.get("suspended").and_then(|s| s.as_bool()).unwrap_or(false);
                    // 停牌/无成交不得稀释热度分母
                    if price.is_none() || price.unwrap() <= 0.0 || pct.is_none() || suspended
                        || (vol.is_some() && vol.unwrap() <= 0.0) {
                        continue;
                    }
                    let name = row.get("name").and_then(|n| n.as_str())
                        .or_else(|| row.get("f14").and_then(|n| n.as_str()))
                        .filter(|n| !n.is_empty())
                        .unwrap_or(&code)
                        .to_string();
                    let mut q = Map::new();
                    q.insert("code".into(), Value::String(code.clone()));
                    q.insert("name".into(), Value::String(name));
                    q.insert("price".into(), Value::from(price.unwrap()));
                    q.insert("heat".into(), Value::from(pct.unwrap()));
                    q.insert("amount".into(), amount.map(Value::from).unwrap_or(Value::Null));
                    quotes.insert(code, Value::Object(q));
                }
                if quotes.is_empty() {
                    *self.error.lock_ok() = "行情源未返回有效成分股，保留上次数据".into();
                } else {
                    let snap = json!({
                        "updated": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        "ts": now(),
                        "source": source,
                        "quotes": Value::Object(quotes),
                    });
                    *self.snapshot.lock_ok() = snap.clone();
                    *self.error.lock_ok() = String::new();
                    // 原子落盘：tmp + rename（失败静默，同 Python OSError pass）
                    if let Some(dir) = std::path::Path::new(&self.cache_path).parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let tmp = zg_core::jsonio::tmp_name(&self.cache_path);
                    if let Ok(text) = serde_json::to_string(&snap) {
                        if std::fs::write(&tmp, text).is_ok() {
                            let _ = std::fs::rename(&tmp, &self.cache_path);
                        }
                    }
                }
            }
            Err(e) => {
                *self.error.lock_ok() = e;
            }
        }
        *self.running.lock_ok() = false;
    }

    pub fn request(self: &Arc<Self>, parent: &str, order: &str, offline: bool) -> Value {
        let cat_err = self.catalog_error.lock_ok().clone();
        if !cat_err.is_empty() {
            return json!({"error": cat_err, "rows": [], "parent": parent, "level": ""});
        }
        let order = if order == "asc" || order == "desc" { order } else { "desc" };
        let other = parent.ends_with(":other");
        let key = if other { &parent[..parent.len() - 6] } else { parent };
        if !parent.is_empty() && !self.nodes.contains_key(key) {
            return json!({"error": "行业代码不存在", "rows": []});
        }
        self.ensure_refresh(offline);
        let (snap, running, error) = {
            let s = self.snapshot.lock_ok();
            (s.clone(), *self.running.lock_ok(), self.error.lock_ok().clone())
        };
        let quotes = snap.get("quotes").cloned().unwrap_or_else(|| json!({}));
        let stocks = |prefix: &str, exact: bool| -> Vec<Value> {
            self.stock_rows(&quotes, prefix, exact)
        };
        let group = |code: &str, name: &str, next_level: &str, exact: bool| -> Value {
            self.group_row(&quotes, code, name, next_level, exact)
        };
        let (level, mut rows) = if key.is_empty() {
            // 研究行业二级一律直接下钻到个股：根级不再产出 'children'
            let mut groups = Vec::new();
            for (c, n) in &self.nodes {
                if c.len() == 5 {
                    let name = n.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    groups.push(group(c, &name, "stocks", false));
                }
            }
            ("industries".to_string(), groups)
        } else {
            let mut ss = stocks(key, other);
            for r in ss.iter_mut() {
                if let Some(o) = r.as_object_mut() {
                    o.insert("next".into(), Value::String("chart".into()));
                }
            }
            ("stocks".to_string(), ss)
        };
        rows = rank(rows, order);
        let valid: Vec<&Value> = rows.iter()
            .filter(|r| r.get("heat").and_then(|h| h.as_f64()).is_some())
            .collect();
        let (highest, lowest) = if valid.is_empty() {
            (Value::Null, Value::Null)
        } else {
            let heat_of = |r: &&Value| r.get("heat").and_then(|h| h.as_f64()).unwrap_or(0.0);
            let hi = valid.iter().max_by(|a, b| heat_of(a).partial_cmp(&heat_of(b))
                .unwrap_or(std::cmp::Ordering::Equal)).cloned().cloned();
            let lo = valid.iter().min_by(|a, b| heat_of(a).partial_cmp(&heat_of(b))
                .unwrap_or(std::cmp::Ordering::Equal)).cloned().cloned();
            (hi.unwrap_or(Value::Null), lo.unwrap_or(Value::Null))
        };
        let snap_ts = snap.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
        let mut out = Map::new();
        out.insert("parent".into(), Value::String(parent.to_string()));
        out.insert("name".into(), Value::String(
            self.nodes.get(key).and_then(|n| n.get("name")).and_then(|x| x.as_str())
                .unwrap_or("二级行业").to_string()));
        out.insert("level".into(), Value::String(level));
        out.insert("order".into(), Value::String(order.to_string()));
        out.insert("rows".into(), Value::Array(rows));
        out.insert("highest".into(), highest);
        out.insert("lowest".into(), lowest);
        out.insert("updated".into(), snap.get("updated").cloned().unwrap_or(Value::Null));
        out.insert("source".into(), snap.get("source").cloned().unwrap_or(Value::Null));
        out.insert("loading".into(), Value::Bool(running));
        out.insert("stale".into(), Value::Bool(now() - snap_ts >= 180.0));
        out.insert("error".into(), Value::String(error));
        out.insert("offline".into(), Value::Bool(offline));
        out.insert("catalog_updated".into(), self.catalog.get("updated").cloned().unwrap_or(Value::Null));
        out.insert("metric".into(), Value::String(
            "热度=有效成分股涨跌幅等权平均；个股热度=自身涨跌幅。缺失/停牌不计入平均。".into()));
        Value::Object(out)
    }

    /// 90 秒快照过期判定与后台刷新闸门（`request`/`request_all` 共用）。
    fn ensure_refresh(self: &Arc<Self>, offline: bool) {
        let now_t = now();
        // 锁序固定为 snapshot → running → attempt，与 request/request_all 保持一致，避免 ABBA
        let snap_ts = self.snapshot.lock_ok().get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
        let mut running = self.running.lock_ok();
        let mut attempt = self.attempt.lock_ok();
        if !offline && !*running && now_t - snap_ts >= 90.0 && now_t - *attempt >= 30.0 {
            *running = true;
            *attempt = now_t;
            let this = Arc::clone(self);
            std::thread::spawn(move || {
                let _g = RunningGuard(&this.running);
                this.refresh_inner();
            });
        }
    }

    /// 某行业前缀下的成分股行情行（原 `request` 内联闭包，逐字抽出）。
    fn stock_rows(&self, quotes: &Value, prefix: &str, exact: bool) -> Vec<Value> {
        let mut out = Vec::new();
        for (code, m) in &self.members {
            let ind = m.get("industry").and_then(|x| x.as_str()).unwrap_or("");
            let hit = if exact { ind == prefix } else { ind.starts_with(prefix) };
            if !hit {
                continue;
            }
            match quotes.get(code).cloned() {
                Some(q) => out.push(q),
                None => {
                    let name = m.get("name").and_then(|n| n.as_str())
                        .filter(|n| !n.is_empty())
                        .unwrap_or(code)
                        .to_string();
                    out.push(json!({"code": code, "name": name,
                                    "price": Value::Null, "heat": Value::Null,
                                    "amount": Value::Null}));
                }
            }
        }
        out
    }

    /// 行业聚合行（原 `request` 内联闭包，逐字抽出）。
    fn group_row(&self, quotes: &Value, code: &str, name: &str,
                 next_level: &str, exact: bool) -> Value {
        let ss = self.stock_rows(quotes, code, exact);
        let valid: Vec<&Value> = ss.iter()
            .filter(|q| q.get("heat").and_then(|h| h.as_f64()).is_some())
            .collect();
        let amounts: Vec<f64> = valid.iter()
            .filter_map(|q| number(q.get("amount")))
            .collect();
        let heat = if valid.is_empty() {
            None
        } else {
            Some(zg_core::model::py_round(
                valid.iter().map(|q| q.get("heat").and_then(|h| h.as_f64()).unwrap_or(0.0)).sum::<f64>()
                    / valid.len() as f64, 4))
        };
        let up_ratio = if valid.is_empty() {
            None
        } else {
            Some(zg_core::model::py_round(
                valid.iter().filter(|q| q.get("heat").and_then(|h| h.as_f64()).unwrap_or(0.0) > 0.0)
                    .count() as f64 / valid.len() as f64 * 100.0, 2))
        };
        let mut m = Map::new();
        m.insert("code".into(), Value::String(format!("{code}{}", if exact { ":other" } else { "" })));
        m.insert("name".into(), Value::String(name.to_string()));
        m.insert("next".into(), Value::String(next_level.to_string()));
        m.insert("total".into(), Value::from(ss.len()));
        m.insert("quoted".into(), Value::from(valid.len()));
        m.insert("heat".into(), heat.map(Value::from).unwrap_or(Value::Null));
        m.insert("up_ratio".into(), up_ratio.map(Value::from).unwrap_or(Value::Null));
        m.insert("amount".into(), {
            if amounts.is_empty() { Value::Null } else { Value::from(amounts.iter().sum::<f64>()) }
        });
        Value::Object(m)
    }

    /// 侧栏预载：一次返回 128 个二级行业 + 每个行业的全部成分股行情。
    ///
    /// 老接口 `/api/industry-heat?parent=` 保持原样（前端仍可用）；本接口只是把
    /// 逐行业下钻的网络往返合并成一次，页面打开即可全量预热，点击行业零请求。
    pub fn request_all(self: &Arc<Self>, order: &str, offline: bool) -> Value {
        let cat_err = self.catalog_error.lock_ok().clone();
        if !cat_err.is_empty() {
            return json!({"error": cat_err, "rows": [], "industries": [], "stocks": {}});
        }
        let order = if order == "asc" || order == "desc" { order } else { "desc" };
        self.ensure_refresh(offline);
        let (snap, running, error) = {
            let s = self.snapshot.lock_ok();
            (s.clone(), *self.running.lock_ok(), self.error.lock_ok().clone())
        };
        let quotes = snap.get("quotes").cloned().unwrap_or_else(|| json!({}));
        let mut industries: Vec<Value> = Vec::new();
        let mut stocks = Map::new();
        let mut count = 0usize;
        for (code, node) in &self.nodes {
            if code.len() != 5 {
                continue;
            }
            let name = node.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            industries.push(self.group_row(&quotes, code, &name, "stocks", false));
            let mut rows = self.stock_rows(&quotes, code, false);
            for r in rows.iter_mut() {
                if let Some(o) = r.as_object_mut() {
                    o.insert("next".into(), Value::String("chart".into()));
                }
            }
            count += rows.len();
            stocks.insert(code.clone(), Value::Array(rank(rows, order)));
        }
        let industries = rank(industries, order);
        let (highest, lowest) = extremes(&industries);
        let snap_ts = snap.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
        json!({
            "ok": true,
            "parent": "",
            "name": "二级行业",
            "level": "industries",
            "order": order,
            "industries": industries,
            "stocks": Value::Object(stocks),
            "count": count,
            "highest": highest,
            "lowest": lowest,
            "updated": snap.get("updated").cloned().unwrap_or(Value::Null),
            "source": snap.get("source").cloned().unwrap_or(Value::Null),
            "loading": running,
            "stale": now() - snap_ts >= 180.0,
            "error": error,
            "offline": offline,
            "catalog_updated": self.catalog.get("updated").cloned().unwrap_or(Value::Null),
            "metric": "热度=有效成分股涨跌幅等权平均；个股热度=自身涨跌幅。缺失/停牌不计入平均。",
        })
    }
}

/// 热度最高/最低（跳过缺失值；空集返回 Null）。
fn extremes(rows: &[Value]) -> (Value, Value) {
    let valid: Vec<&Value> = rows.iter()
        .filter(|r| r.get("heat").and_then(|h| h.as_f64()).is_some())
        .collect();
    if valid.is_empty() {
        return (Value::Null, Value::Null);
    }
    let heat_of = |r: &&Value| r.get("heat").and_then(|h| h.as_f64()).unwrap_or(0.0);
    let hi = valid.iter().max_by(|a, b| heat_of(a).partial_cmp(&heat_of(b))
        .unwrap_or(std::cmp::Ordering::Equal)).cloned().cloned();
    let lo = valid.iter().min_by(|a, b| heat_of(a).partial_cmp(&heat_of(b))
        .unwrap_or(std::cmp::Ordering::Equal)).cloned().cloned();
    (hi.unwrap_or(Value::Null), lo.unwrap_or(Value::Null))
}

fn index_nodes(catalog: &Value) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    if let Some(nodes) = catalog.get("nodes").and_then(|x| x.as_array()) {
        for n in nodes {
            if let Some(r) = n.get("ref").and_then(|x| x.as_str()) {
                m.insert(r.to_string(), n.clone());
            }
        }
    }
    m
}

fn index_members(catalog: &Value) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    if let Some(members) = catalog.get("members").and_then(|x| x.as_array()) {
        for n in members {
            if let Some(c) = n.get("code").and_then(|x| x.as_str()) {
                m.insert(c.to_string(), n.clone());
            }
        }
    }
    m
}

// ---------------------------------------------------------------- 行业情报

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 默认抓取器（对齐 Python `IndustryIntel._fetch`：UA + Accept-Language，超时 12s）。
pub fn default_fetcher(
    url: &str,
    data: Option<&[u8]>,
    extra_headers: Option<&[(&str, &str)]>,
) -> Result<String, String> {
    let mut headers: Vec<(&str, &str)> = vec![
        ("User-Agent", UA),
        ("Accept-Language", "zh-CN,zh;q=0.9"),
    ];
    if let Some(extra) = extra_headers {
        headers.extend_from_slice(extra);
    }
    let raw = if let Some(body) = data {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(12))
            .redirects(5)
            .build();
        let mut req = agent.post(url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        req.send_bytes(body).map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?
    } else {
        zg_core::net::read_url(url, Some(&headers), 12, 0, None, None)
            .map(|b| String::from_utf8_lossy(&b).to_string())?
    };
    Ok(raw)
}

type Fetcher = dyn Fn(&str, Option<&[u8]>, Option<&[(&str, &str)]>) -> Result<String, String>
    + Send + Sync;

pub const FEEDS: &[(&str, &str, &str, u64)] = &[
    ("miit", "工业和信息化部", "政策", 900),
    ("stcn", "证券时报", "快讯", 180),
];

pub struct IndustryIntel {
    nodes: HashMap<String, Value>,
    members: HashMap<String, Value>,
    cache_path: String,
    fetcher: Arc<Fetcher>,
    running: Mutex<bool>,
    announcement_running: Mutex<HashSet<String>>,
    items: Mutex<Vec<Value>>,
    sources: Mutex<HashMap<String, Value>>,
    announcements: Mutex<HashMap<String, Value>>,
}

impl IndustryIntel {
    pub fn new(catalog_path: &str, cache_path: &str, fetcher: Arc<Fetcher>) -> Self {
        let text = std::fs::read_to_string(catalog_path).expect("行业目录读取失败");
        let catalog: Value = serde_json::from_str(&text).expect("行业目录解析失败");
        let nodes = index_nodes(&catalog);
        let members = index_members(&catalog);
        let this = Self {
            nodes,
            members,
            cache_path: cache_path.to_string(),
            fetcher,
            running: Mutex::new(false),
            announcement_running: Mutex::new(HashSet::new()),
            items: Mutex::new(Vec::new()),
            sources: Mutex::new(HashMap::new()),
            announcements: Mutex::new(HashMap::new()),
        };
        this.load();
        this
    }

    fn load(&self) {
        if let Ok(text) = std::fs::read_to_string(&self.cache_path) {
            if let Ok(cache) = serde_json::from_str::<Value>(&text) {
                if let Some(items) = cache.get("items").and_then(|x| x.as_array()) {
                    *self.items.lock_ok() = items.clone();
                }
                if let Some(sources) = cache.get("sources").and_then(|x| x.as_object()) {
                    *self.sources.lock_ok() =
                        sources.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
                if let Some(ann) = cache.get("announcements").and_then(|x| x.as_object()) {
                    *self.announcements.lock_ok() =
                        ann.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
            }
        }
    }

    fn save(&self) {
        let payload = json!({
            "items": Value::Array(self.items.lock_ok().clone()),
            "sources": Value::Object(self.sources.lock_ok().iter()
                .map(|(k, v)| (k.clone(), v.clone())).collect()),
            "announcements": Value::Object(self.announcements.lock_ok().iter()
                .map(|(k, v)| (k.clone(), v.clone())).collect()),
        });
        if let Some(dir) = std::path::Path::new(&self.cache_path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = zg_core::jsonio::tmp_name(&self.cache_path);
        if let Ok(text) = serde_json::to_string(&payload) {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &self.cache_path);
            }
        }
    }

    fn refresh_feed(&self, feed: &(&str, &str, &str, u64)) -> Result<(Vec<Value>, Value), String> {
        let (id, name, category, _ttl) = *feed;
        let url = feed_url(id);
        let raw = (self.fetcher)(&url, None, None)?;
        let rows = extract_headlines(&raw, &url, name, category, 80);
        if rows.is_empty() {
            return Err("页面未解析出有效标题".into());
        }
        // 仅保留近 180 天内带日期的标题，避免导航/归档链接冒充新鲜情报
        let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(180);
        let today = chrono::Local::now().date_naive();
        let mut recent = Vec::new();
        for row in rows {
            let published = row.get("published").and_then(|p| p.as_str())
                .and_then(parse_date);
            let ok = match published {
                Some(p) if p.len() >= 10 => {
                    let y: i32 = p[0..4].parse().unwrap_or(0);
                    let m: u32 = p[5..7].parse().unwrap_or(1);
                    let d: u32 = p[8..10].parse().unwrap_or(1);
                    match chrono::NaiveDate::from_ymd_opt(y, m, d) {
                        Some(dt) => cutoff <= dt && dt <= today,
                        None => false,
                    }
                }
                _ => false,
            };
            if ok {
                recent.push(row);
            }
        }
        let mut rows = recent;
        if id == "miit" {
            for row in rows.iter_mut() {
                let cat = if row.get("url").and_then(|u| u.as_str()).unwrap_or("").contains("/zwgk/zcwj/") {
                    "政策"
                } else {
                    "快讯"
                };
                row.as_object_mut().unwrap().insert("category".into(), Value::String(cat.into()));
            }
        }
        if rows.is_empty() {
            return Err("页面未解析到近180天内带日期的标题".into());
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        for row in rows.iter_mut() {
            if let Some(o) = row.as_object_mut() {
                o.insert("fetched_at".into(), Value::String(stamp.clone()));
            }
        }
        let state = json!({
            "name": name, "category": category, "url": url,
            "updated_at": stamp, "ts": now(), "status": "已更新", "error": "",
        });
        Ok((rows, state))
    }

    fn refresh_inner(self: &Arc<Self>, force: bool) {
        let current = self.sources.lock_ok().clone();
        let due: Vec<(&str, &str, &str, u64)> = FEEDS.iter()
            .filter(|f| {
                force || now() - current.get(f.0)
                    .and_then(|s| s.get("ts"))
                    .and_then(|t| t.as_f64())
                    .unwrap_or(0.0) >= f.3 as f64
            })
            .copied()
            .collect();
        if due.is_empty() {
            *self.running.lock_ok() = false;
            return;
        }
        // max_workers = min(4, len(due))；FEEDS 现仅 2 条，直接并发全部
        let mut handles = Vec::new();
        for feed in &due {
            let feed = *feed;
            let self_arc = Arc::clone(self);
            let handle = std::thread::spawn(move || {
                let r = self_arc.refresh_feed(&feed);
                (feed, r)
            });
            handles.push((feed, handle));
        }
        let mut results: HashMap<&str, (Option<Vec<Value>>, Value)> = HashMap::new();
        for (feed, h) in handles {
            let (feed, r) = match h.join() {
                Ok(v) => v,
                Err(_) => (feed, Err("线程崩溃".into())),
            };
            results.insert(feed.0, finish_feed(feed, r, &current));
        }
        {
            let mut items = self.items.lock_ok();
            let mut sources = self.sources.lock_ok();
            let keep: Vec<Value> = items.iter()
                .filter(|row| !due.iter().any(|f| {
                    row.get("source").and_then(|s| s.as_str()).unwrap_or("") == f.1
                }))
                .cloned()
                .collect();
            let mut keep = keep;
            for feed in &due {
                let (rows, state) = results.get(feed.0).unwrap();
                if rows.is_none() {
                    keep.extend(items.iter().filter(|row| {
                        row.get("source").and_then(|s| s.as_str()).unwrap_or("") == feed.1
                    }).cloned());
                } else {
                    keep.extend(rows.clone().unwrap());
                }
                sources.insert(feed.0.to_string(), state.clone());
            }
            let mut unique: HashMap<(String, String), Value> = HashMap::new();
            for row in keep {
                unique.insert(
                    (row.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                     row.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string()),
                    row,
                );
            }
            let mut all: Vec<Value> = unique.into_values().collect();
            // HashMap 迭代序不定：截断前必须按发布时间排（新在前），
            // 否则保留的是「哈希序的尾 600 条」，可能丢最新留最旧
            all.sort_by(|a, b| {
                let pa = a.get("published").and_then(|v| v.as_str()).unwrap_or("");
                let pb = b.get("published").and_then(|v| v.as_str()).unwrap_or("");
                pb.cmp(pa)
            });
            if all.len() > 600 {
                all.truncate(600);
            }
            *items = all;
        }
        *self.running.lock_ok() = false;
        self.save();
    }

    /// 启动后台刷新；已在刷新或无到期源时返回 false（对齐 Python `refresh`）。
    pub fn refresh(self: &Arc<Self>, force: bool) -> bool {
        let mut running = self.running.lock_ok();
        if *running {
            return false;
        }
        let current = self.sources.lock_ok().clone();
        let due = force || FEEDS.iter().any(|f| {
            now() - current.get(f.0)
                .and_then(|s| s.get("ts"))
                .and_then(|t| t.as_f64())
                .unwrap_or(0.0) >= f.3 as f64
        });
        if !due {
            return false;
        }
        *running = true;
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let _g = RunningGuard(&this.running);
            this.refresh_inner(force);
        });
        true
    }

    /// 巨潮资讯个股公告（45 天窗口，sse/szse 按代码段路由）。门 1 夹具对表入口。
    pub fn fetch_announcements(&self, code: &str) -> Result<Vec<Value>, String> {
        let end = chrono::Local::now().date_naive();
        let start = end - chrono::Duration::days(45);
        let column = if code.starts_with("5") || code.starts_with("6") { "sse" } else { "szse" };
        let form = format!(
            "pageNum=1&pageSize=30&column={column}&tabName=fulltext&plate=&stock={code}\
&searchkey=&secid=&category=&trade=&seDate={}~{}&sortName=&sortType=&isHLtitle=true",
            start.format("%Y-%m-%d"), end.format("%Y-%m-%d")
        );
        let raw = (self.fetcher)(
            "https://www.cninfo.com.cn/new/hisAnnouncement/query",
            Some(form.as_bytes()),
            Some(&[
                ("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8"),
                ("Referer", "https://www.cninfo.com.cn/"),
                ("X-Requested-With", "XMLHttpRequest"),
            ]),
        )?;
        let payload: Value = serde_json::from_str(&raw).map_err(|e| format!("巨潮响应解析失败: {e}"))?;
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut rows = Vec::new();
        for row in (payload.get("announcements").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
            if row.get("secCode").and_then(|c| c.as_str()).unwrap_or("") != code {
                continue;
            }
            let title = clean_text(row.get("announcementTitle").and_then(|t| t.as_str()).unwrap_or(""));
            let when = row.get("announcementTime").and_then(|t| t.as_f64());
            let published = when.map(|w| {
                chrono::TimeZone::timestamp_millis_opt(&chrono::Local, w as i64).single()
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default()
            });
            let path = row.get("adjunctUrl").and_then(|u| u.as_str()).unwrap_or("");
            let mut m = Map::new();
            let id = row.get("announcementId").and_then(|i| i.as_i64())
                .map(|i| format!("cninfo:{i}"))
                .unwrap_or_else(|| {
                    // Python `hashlib.sha1(path.encode()).hexdigest()[:16]`
                    use sha1::Digest;
                    let mut ctx = sha1::Sha1::new();
                    ctx.update(path.as_bytes());
                    format!("cninfo:{}", &hex_of(&ctx.finalize())[..16])
                });
            m.insert("id".into(), Value::String(id));
            m.insert("source".into(), Value::String("巨潮资讯".into()));
            m.insert("category".into(), Value::String("公告".into()));
            m.insert("title".into(), Value::String(title));
            m.insert("url".into(), Value::String(urljoin("https://static.cninfo.com.cn/", path)));
            m.insert("published".into(), published.map(Value::String).unwrap_or(Value::Null));
            m.insert("fetched_at".into(), Value::String(stamp.clone()));
            m.insert("match_level".into(), Value::String("直接相关".into()));
            m.insert("match_reason".into(), Value::String("公告证券代码与当前个股一致".into()));
            rows.push(Value::Object(m));
        }
        Ok(rows)
    }

    fn refresh_announcements(&self, code: &str) {
        let now_t = now();
        let cached = self.announcements.lock_ok().get(code).cloned();
        let value = match self.fetch_announcements(code) {
            Ok(rows) => json!({
                "ts": now_t,
                "updated_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                "rows": rows, "error": "",
            }),
            Err(e) => {
                let mut v = cached.unwrap_or_else(|| json!({"ts": 0, "updated_at": Value::Null,
                                                            "rows": [], "error": ""}));
                if let Some(o) = v.as_object_mut() {
                    o.insert("error".into(), Value::String(friendly_error(&e)));
                }
                v
            }
        };
        {
            let mut ann = self.announcements.lock_ok();
            ann.insert(code.to_string(), value);
            self.announcement_running.lock_ok().remove(code);
            if ann.len() > 200 {
                let oldest = ann.iter().min_by(|a, b| {
                    a.1.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0)
                        .partial_cmp(&b.1.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                }).map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    ann.remove(&k);
                }
            }
        }
        self.save();
    }

    pub fn announcements_for(self: &Arc<Self>, code: &str, force: bool) -> (Vec<Value>, String, bool) {
        let now_t = now();
        let (cached, running) = {
            let ann = self.announcements.lock_ok();
            let cached = ann.get(code).cloned().unwrap_or_else(|| json!({}));
            let ts = cached.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
            let due = force || now_t - ts >= 300.0;
            let mut running_set = self.announcement_running.lock_ok();
            if due && !running_set.contains(code) {
                running_set.insert(code.to_string());
                let this = Arc::clone(self);
                let code = code.to_string();
                std::thread::spawn(move || this.refresh_announcements(&code));
            }
            let running = running_set.contains(code);
            (cached, running)
        };
        let rows = cached.get("rows").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let error = cached.get("error").and_then(|e| e.as_str()).unwrap_or("").to_string();
        (rows, error, running)
    }

    pub fn identity(&self, code: &str, theme: Option<&Value>) -> Value {
        let member = self.members.get(code).cloned().unwrap_or_else(|| json!({}));
        let ref_ = member.get("industry").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let parent = if ref_.len() >= 5 { ref_[..5].to_string() } else { ref_.clone() };
        let mut names = Vec::new();
        for key in [parent.as_str(), ref_.as_str()] {
            let name = self.nodes.get(key).and_then(|n| n.get("name")).and_then(|x| x.as_str());
            if let Some(name) = name {
                if !name.is_empty() && !names.contains(&name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }
        let theme = theme.cloned().unwrap_or_else(|| json!({}));
        let member_name = member.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let mut terms: Vec<String> = vec![member_name.clone(), code.to_string()];
        for n in &names {
            terms.push(n.clone());
        }
        if let Some(v) = theme.get("industry").and_then(|x| x.as_str()) {
            terms.push(v.to_string());
        }
        if let Some(concepts) = theme.get("concepts").and_then(|x| x.as_array()) {
            for c in concepts {
                if let Some(s) = c.as_str() {
                    terms.push(s.to_string());
                }
            }
        }
        let mut seen = HashSet::new();
        terms.retain(|t| {
            let t = t.trim();
            t.chars().count() >= 2 && seen.insert(t.to_string())
        });
        let terms: Vec<String> = terms.into_iter().map(|t| t.trim().to_string()).collect();
        let name = if member_name.is_empty() { code.to_string() } else { member_name };
        json!({
            "code": code,
            "name": name,
            "industry_code": ref_,
            "industry_path": names,
            "theme_industry": theme.get("industry").and_then(|x| x.as_str()).unwrap_or(""),
            "concepts": theme.get("concepts").cloned().unwrap_or_else(|| json!([])),
            "terms": terms,
        })
    }

    pub fn request(self: &Arc<Self>, code: &str, theme: Option<&Value>, force: bool) -> Value {
        self.refresh(force);
        let identity = self.identity(code, theme);
        let (announcements, announcement_error, announcement_loading) =
            self.announcements_for(code, force);
        let (items, sources, running) = {
            let items = self.items.lock_ok().clone();
            let sources = self.sources.lock_ok().clone();
            let running = *self.running.lock_ok();
            (items, sources, running)
        };
        let mut matched = Vec::new();
        for row in &items {
            if let Some((level, reason)) = match_row(row, &identity) {
                let mut r = row.clone();
                if let Some(o) = r.as_object_mut() {
                    o.insert("match_level".into(), Value::String(level.into()));
                    o.insert("match_reason".into(), Value::String(reason));
                }
                matched.push(r);
            }
        }
        matched.extend(announcements);
        matched.sort_by(|a, b| {
            let ka = a.get("published").and_then(|p| p.as_str())
                .or_else(|| a.get("fetched_at").and_then(|f| f.as_str()))
                .unwrap_or("");
            let kb = b.get("published").and_then(|p| p.as_str())
                .or_else(|| b.get("fetched_at").and_then(|f| f.as_str()))
                .unwrap_or("");
            kb.cmp(ka)
        });
        let now_t = now();
        let mut source_rows = Vec::new();
        for (id, name, category, ttl) in FEEDS {
            let mut state = sources.get(*id).cloned().unwrap_or_else(|| json!({
                "name": name, "category": category, "url": feed_url(id),
                "status": "尚未刷新", "ts": 0, "updated_at": Value::Null, "error": "",
            }));
            if let Some(o) = state.as_object_mut() {
                let ts = o.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
                let stale = ts > 0.0 && now_t - ts > ((*ttl as f64) * 3.0).max(900.0);
                o.insert("stale".into(), Value::Bool(stale));
                o.remove("ts");
            }
            source_rows.push(state);
        }
        let mut matched = matched;
        if matched.len() > 80 {
            matched.truncate(80);
        }
        let mut counts = Map::new();
        for c in ["政策", "快讯", "公告"] {
            counts.insert(c.to_string(), Value::from(
                matched.iter().filter(|r| {
                    r.get("category").and_then(|x| x.as_str()).unwrap_or("") == c
                }).count()));
        }
        let mut iden = identity.clone();
        if let Some(o) = iden.as_object_mut() {
            o.remove("terms");
        }
        let mut out = Map::new();
        out.insert("identity".into(), iden);
        out.insert("items".into(), Value::Array(matched));
        out.insert("counts".into(), Value::Object(counts));
        out.insert("loading".into(), Value::Bool(running));
        out.insert("sources".into(), Value::Array(source_rows));
        out.insert("announcement_error".into(), Value::String(announcement_error));
        out.insert("announcement_loading".into(), Value::Bool(announcement_loading));
        out.insert("generated_at".into(), Value::String(
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()));
        out.insert("scope".into(), Value::String(
            "一期：官方政策、行业快讯标题、当前个股公告；按通达信行业/题材标题匹配".into()));
        Value::Object(out)
    }
}

fn feed_url(id: &str) -> String {
    match id {
        "miit" => "https://www.miit.gov.cn/zwgk/".to_string(),
        "stcn" => "https://www.stcn.com/article/index".to_string(),
        _ => String::new(),
    }
}

fn finish_feed(
    feed: (&str, &str, &str, u64),
    r: Result<(Vec<Value>, Value), String>,
    current: &HashMap<String, Value>,
) -> (Option<Vec<Value>>, Value) {
    match r {
        Ok((rows, state)) => (Some(rows), state),
        Err(e) => {
            let old = current.get(feed.0).cloned().unwrap_or_else(|| json!({}));
            (
                None,
                json!({
                    "name": feed.1, "category": feed.2, "url": feed_url(feed.0),
                    "updated_at": old.get("updated_at").cloned().unwrap_or(Value::Null),
                    "ts": old.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0),
                    "status": "更新失败", "error": friendly_error(&e),
                }),
            )
        }
    }
}

fn friendly_error(message: &str) -> String {
    if message.contains("WinError 10013") || message.contains("WinError 10051")
        || message.contains("WinError 10060") {
        return "当前环境无法连接外部网站；请检查 Windows 防火墙/代理，或稍后重试".into();
    }
    if message.contains("CERTIFICATE_VERIFY_FAILED")
        || message.to_lowercase().contains("certificate verify failed") {
        return "目标网站的 HTTPS 证书校验失败，已跳过该来源".into();
    }
    if message.to_lowercase().contains("timed out") || message.to_lowercase().contains("timeout") {
        return "连接超时，请稍后重试".into();
    }
    message.chars().take(180).collect()
}

// ---------------------------------------------------------------- 解析工具

static TAG_RE: OnceLock<Regex> = OnceLock::new();
static ANCHOR_RE: OnceLock<Regex> = OnceLock::new();
static DATE_RE: OnceLock<Regex> = OnceLock::new();
static SHORT_DATE_RE: OnceLock<Regex> = OnceLock::new();
static DIGITS_ONLY_RE: OnceLock<Regex> = OnceLock::new();

fn tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

fn anchor_re() -> &'static Regex {
    ANCHOR_RE.get_or_init(|| {
        Regex::new(r#"(?is)<a\b([^>]*)href=["']([^"']+)["']([^>]*)>(.*?)</a>"#).unwrap()
    })
}

fn date_re() -> &'static Regex {
    DATE_RE.get_or_init(|| {
        Regex::new(r"(20\d{2})[-/.年](\d{1,2})[-/.月](\d{1,2})(?:日)?(?:\s+(\d{1,2}):(\d{2}))?")
            .unwrap()
    })
}

fn short_date_re() -> &'static Regex {
    SHORT_DATE_RE.get_or_init(|| {
        Regex::new(r"(\d{1,2})[-/.月](\d{1,2})(?:日)?").unwrap()
    })
}

fn digits_only_re() -> &'static Regex {
    DIGITS_ONLY_RE.get_or_init(|| Regex::new(r"^[\d\s:./-]+$").unwrap())
}

/// Python `html.unescape` 等价（HTML5 命名实体 + 数字实体）。
pub fn html_unescape(s: &str) -> String {
    html_escape::decode_html_entities(s).into_owned()
}

pub fn clean_text(value: &str) -> String {
    let stripped = tag_re().replace_all(value, " ").into_owned();
    let unescaped = html_unescape(&stripped);
    let compact: String = unescaped.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.trim().to_string()
}

/// 解析附近文本中的日期；无日期 → None。
pub fn parse_date(value: &str) -> Option<String> {
    if let Some(caps) = date_re().captures(value) {
        let y: i32 = caps.get(1)?.as_str().parse().ok()?;
        let m: u32 = caps.get(2)?.as_str().parse().ok()?;
        let d: u32 = caps.get(3)?.as_str().parse().ok()?;
        let hh = caps.get(4).and_then(|c| c.as_str().parse::<u32>().ok());
        let mm = caps.get(5).and_then(|c| c.as_str().parse::<u32>().ok());
        return format_date(y, m, d, hh, mm);
    }
    // Python `(?<!\d)...(?!\d)` 的前后边界用 regex crate 不支持，手动检查
    let short = short_date_re()
        .captures_iter(value)
        .find(|c| {
            let m = c.get(0).unwrap();
            let before = value[..m.start()].chars().next_back();
            let after = value[m.end()..].chars().next();
            !before.map(|c| c.is_ascii_digit()).unwrap_or(false)
                && !after.map(|c| c.is_ascii_digit()).unwrap_or(false)
        })?;
    let m: u32 = short.get(1)?.as_str().parse().ok()?;
    let d: u32 = short.get(2)?.as_str().parse().ok()?;
    let today = chrono::Local::now().date_naive();
    let mut year = today.year();
    let candidate = chrono::NaiveDate::from_ymd_opt(year, m, d)?;
    if candidate > today + chrono::Duration::days(30) {
        year -= 1;
    }
    format_date(year, m, d, None, None)
}

fn format_date(y: i32, m: u32, d: u32, hh: Option<u32>, mm: Option<u32>) -> Option<String> {
    chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    match (hh, mm) {
        (Some(h), Some(min)) => Some(format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}")),
        _ => Some(format!("{y:04}-{m:02}-{d:02}")),
    }
}

/// Python `urllib.parse.urljoin` 的常用子集（绝对/协议相对/根相对/目录相对）。
pub fn urljoin(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    if href.starts_with('/') {
        let scheme = base.split("://").next().unwrap_or("https");
        let host = base.split("://").nth(1).unwrap_or("")
            .split('/').next().unwrap_or("");
        return format!("{scheme}://{host}{href}");
    }
    let dir = base.rfind('/').map(|i| &base[..i]).unwrap_or(base);
    format!("{dir}/{href}")
}

/// 保守的通用标题提取器（对齐 Python `extract_headlines`）。
/// 注意：id 字段用确定性 fnv1a 替代 Python 进程随机 `hash()`（跨进程不稳定），
/// 门 1 对表时该字段按 volatile 剥离。
pub fn extract_headlines(raw: &str, base_url: &str, source: &str, category: &str, limit: usize)
    -> Vec<Value> {
    let skip: HashSet<&str> = ["首页", "更多", "新闻", "政策", "快讯", "登录", "注册", "返回顶部",
                               "下一页", "上一页"].iter().copied().collect();
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for cap in anchor_re().captures_iter(raw) {
        let title = clean_text(cap.get(4).map(|m| m.as_str()).unwrap_or(""));
        let title_chars = title.chars().count();
        if !(8..=140).contains(&title_chars) || skip.contains(title.as_str())
            || seen.contains(&title) {
            continue;
        }
        if title.matches('|').count() > 3 {
            continue;
        }
        if digits_only_re().is_match(&title) {
            continue;
        }
        let href = html_unescape(cap.get(2).map(|m| m.as_str()).unwrap_or(""));
        let href = urljoin(base_url, &href);
        if !href.starts_with("http://") && !href.starts_with("https://") {
            continue;
        }
        let m = cap.get(0).unwrap();
        let (start, end) = char_window(raw, m.start(), m.end(), 180);
        let nearby = clean_text(&raw[start..end]);
        let published = parse_date(&nearby);
        seen.insert(title.clone());
        let id = format!("{source}:{}", fnv1a(href.as_bytes()) % 1_000_000_000_000u64);
        rows.push(json!({
            "id": id, "source": source, "category": category,
            "title": title, "url": href, "published": published, "fetched_at": Value::Null,
        }));
        if rows.len() >= limit {
            break;
        }
    }
    rows
}

/// 以「字符」为单位向两侧扩展窗口（对齐 Python 字符串切片语义）。
fn char_window(raw: &str, m_start: usize, m_end: usize, span: usize) -> (usize, usize) {
    let mut start = m_start;
    for _ in 0..span {
        match raw[..start].char_indices().next_back() {
            Some((i, _)) => start = i,
            None => break,
        }
    }
    let mut end = m_end;
    for _ in 0..span {
        match raw[end..].chars().next() {
            Some(c) => end += c.len_utf8(),
            None => break,
        }
    }
    (start, end)
}

/// FNV-1a 64（替代 Python 进程随机 hash，仅用于 id 字段）
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Python `IndustryIntel.match`。
pub fn match_row(row: &Value, identity: &Value) -> Option<(&'static str, String)> {
    let title = row.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let name = identity.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let code = identity.get("code").and_then(|c| c.as_str()).unwrap_or("");
    if name != code && title.contains(name) {
        return Some(("直接相关", "标题提到当前公司".into()));
    }
    if let Some(path) = identity.get("industry_path").and_then(|p| p.as_array()) {
        for n in path.iter().rev() {
            let n = n.as_str().unwrap_or("");
            if !n.is_empty() && title.contains(n) {
                return Some(("直接相关", format!("标题命中通达信行业“{n}”")));
            }
        }
    }
    if let Some(theme) = identity.get("theme_industry").and_then(|t| t.as_str()) {
        if !theme.is_empty() && title.contains(theme) {
            return Some(("直接相关", format!("标题命中个股行业“{theme}”")));
        }
    }
    if let Some(concepts) = identity.get("concepts").and_then(|c| c.as_array()) {
        for concept in concepts {
            let concept = concept.as_str().unwrap_or("");
            if concept.chars().count() >= 3 && title.contains(concept) {
                return Some(("题材相关", format!("标题命中个股题材“{concept}”")));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_all_groups_members_by_second_level_industry() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("industry_catalog.json");
        std::fs::write(&catalog, r#"{
            "updated": "2026-10-01",
            "nodes": [
                {"name": "煤炭", "code": "881001", "type": 12, "sub_type": 1, "ref": "X10"},
                {"name": "煤炭开采", "code": "881002", "type": 12, "sub_type": 1, "ref": "X1001"},
                {"name": "动力煤", "code": "881003", "type": 12, "sub_type": 1, "ref": "X100101"}
            ],
            "members": [
                {"code": "600001", "market": 1, "industry": "X100101", "name": "甲"},
                {"code": "600002", "market": 1, "industry": "X100101", "name": "乙"}
            ]
        }"#).unwrap();
        let cache = dir.path().join("industry_heat_cache.json");
        std::fs::write(&cache, r#"{
            "updated": "2026-10-01 15:00:00", "ts": 9999999999, "source": "test",
            "quotes": {
                "600001": {"code": "600001", "name": "甲", "price": 10.0, "heat": 2.5, "amount": 1000.0},
                "600002": {"code": "600002", "name": "乙", "price": 20.0, "heat": -1.5, "amount": 2000.0}
            }
        }"#).unwrap();
        let provider: Arc<HeatProvider> = Arc::new(|_| Ok((Vec::new(), String::new())));
        let h = Arc::new(IndustryHeat::new(
            catalog.to_str().unwrap(), cache.to_str().unwrap(), provider));
        let all = h.request_all("desc", true);
        let rows = all["industries"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "只有 5 位 ref 才是二级行业");
        assert_eq!(rows[0]["code"], "X1001");
        assert_eq!(rows[0]["total"], 2);
        assert_eq!(rows[0]["quoted"], 2);
        assert_eq!(rows[0]["heat"], 0.5);
        assert_eq!(rows[0]["up_ratio"], 50.0);
        assert_eq!(all["count"], 2);
        let stocks = all["stocks"]["X1001"].as_array().unwrap();
        assert_eq!(stocks.len(), 2);
        assert_eq!(stocks[0]["code"], "600001", "降序：+2.5% 在前");
        assert_eq!(stocks[0]["next"], "chart");
        let asc = h.request_all("asc", true);
        assert_eq!(asc["stocks"]["X1001"][0]["code"], "600002");
    }

    #[test]
    fn clean_text_strips_tags_and_entities() {
        assert_eq!(clean_text("<a href=\"x\">贵州&lt;茅台&gt; &amp; 五粮液</a>"),
                   "贵州<茅台> & 五粮液");
        assert_eq!(clean_text("<p>  多  余   空格  </p>"), "多 余 空格");
    }

    #[test]
    fn parse_date_full_and_short() {
        assert_eq!(parse_date("2026-10-27 14:30"), Some("2026-10-27 14:30".into()));
        assert_eq!(parse_date("2026年10月27日"), Some("2026-10-27".into()));
        assert_eq!(parse_date("2026/10/27"), Some("2026-10-27".into()));
        let today = chrono::Local::now().date_naive();
        let s = format!("{}-{}", today.month(), today.day());
        let p = parse_date(&s).unwrap();
        assert!(p.starts_with(&today.format("%Y-%m").to_string()));
    }

    #[test]
    fn urljoin_variants() {
        assert_eq!(urljoin("https://a.com/x/y", "https://b.com/z"), "https://b.com/z");
        assert_eq!(urljoin("https://a.com/x", "//b.com/z"), "https://b.com/z");
        assert_eq!(urljoin("https://a.com/x", "/z"), "https://a.com/z");
        assert_eq!(urljoin("https://a.com/x/", "z"), "https://a.com/x/z");
    }

    #[test]
    fn extract_headlines_minimal() {
        let raw = r#"<html><a href="/news/1">一条足够长的新闻标题示例</a>
<a href="https://b.com/2">2026-10-27 另一条足够长的标题</a></html>"#;
        let rows = extract_headlines(raw, "https://miit.gov.cn/zwgk/", "工业和信息化部", "政策", 80);
        assert!(!rows.is_empty());
        assert_eq!(rows[0]["source"], "工业和信息化部");
        assert!(rows[0]["title"].as_str().unwrap().len() >= 8);
        assert!(rows[0]["url"].as_str().unwrap().starts_with("https://"));
    }

    #[test]
    fn match_row_levels() {
        let identity = json!({
            "code": "600519", "name": "贵州茅台",
            "industry_path": ["酿酒行业", "白酒"],
            "theme_industry": "白酒", "concepts": ["白酒概念", "消费概念"],
        });
        let row = json!({"title": "贵州茅台发布公告"});
        assert_eq!(match_row(&row, &identity).unwrap().0, "直接相关");
        let row = json!({"title": "白酒行业迎来旺季"});
        assert_eq!(match_row(&row, &identity).unwrap().0, "直接相关");
        // "白酒" 在 industry_path 里 → 直接相关（reversed 路径先命中）
        let row = json!({"title": "白酒概念板块走强"});
        assert_eq!(match_row(&row, &identity).unwrap().0, "直接相关");
        // 概念长度按字符数：2 字概念 Python len>=3 也不匹配，用 4 字概念验证题材命中
        let row = json!({"title": "消费概念板块迎来机会"});
        assert_eq!(match_row(&row, &identity).unwrap().0, "题材相关");
        let row = json!({"title": "消费板块迎来机会"});
        assert!(match_row(&row, &identity).is_none());
        let row = json!({"title": "无关的标题内容"});
        assert!(match_row(&row, &identity).is_none());
    }

    #[test]
    fn rank_orders_and_puts_none_last() {
        let rows = vec![
            json!({"code": "a", "heat": 1.0}),
            json!({"code": "b", "heat": Value::Null}),
            json!({"code": "c", "heat": 3.0}),
            json!({"code": "d", "heat": 2.0}),
        ];
        let desc = rank(rows.clone(), "desc");
        assert_eq!(desc[0]["code"], "c");
        assert_eq!(desc[1]["code"], "d");
        assert_eq!(desc[2]["code"], "a");
        assert_eq!(desc[3]["code"], "b");
        let asc = rank(rows, "asc");
        assert_eq!(asc[0]["code"], "a");
        assert_eq!(asc[3]["code"], "b");
    }

    #[test]
    fn heat_request_offline_empty() {
        // 无 catalog 时不构造对象；此处只验证 rank 稳定性语义
        let rows = vec![
            json!({"code": "x", "heat": 1.0}),
            json!({"code": "y", "heat": 1.0}),
        ];
        let r = rank(rows, "desc");
        assert_eq!(r[0]["code"], "x");
        assert_eq!(r[1]["code"], "y");
    }
}
