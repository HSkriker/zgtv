//! 8786 进程内状态 —— 对齐 `modules/zgapp/state.py` + `bootstrap.load_state`。

use zg_core::sync::MutexExt;
use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use zg_market::market_data::MarketState;

pub struct WebState {
    pub port: u16,
    pub offline: bool,
    pub no_browser: bool,

    pub stock_list: Mutex<Vec<Value>>,
    pub list_time: Mutex<Option<String>>,
    pub data_source: Mutex<Option<String>>,
    /// 列表刷新的原始行（东财 clist 形状）与时间戳，90s 内存缓存
    pub raw_rows: Mutex<Vec<Value>>,
    pub raw_time: Mutex<f64>,
    pub list_lock: Mutex<()>,

    pub live: Mutex<Value>,
    pub watch_extra: Mutex<Vec<String>>,

    pub alerts: Mutex<HashMap<String, Value>>,
    pub alert_log: Mutex<Vec<Value>>,
    pub alert_load_ok: Mutex<bool>,
    /// 上次 alerts.json 落盘时间（秒）；0 = 从未落盘
    pub alert_flush: Mutex<f64>,

    /// 题材本地缓存：code → {industry, area, concepts, fetched_at, fetched_at_txt}；
    /// 点击路径只读这里（零网络），刷新一律走 /api/theme-refresh 后台任务
    pub theme_cache: Mutex<HashMap<String, Value>>,
    /// 题材后台刷新进度（/api/theme-refresh GET 的响应体）
    pub theme_prog: Mutex<Value>,
    pub theme_refreshing: std::sync::atomic::AtomicBool,
    pub czq_cache: Mutex<HashMap<String, (f64, Value)>>,
    pub czsc_scan_state: Mutex<Value>,
    pub czsc_scan_running: Mutex<bool>,

    /// 周/分/日 K 缓存 + kline 内存缓存（复用 zg-market 的编排状态）
    pub market: MarketState,
    pub weekly_build: Mutex<Value>,
    pub min_build: Mutex<Value>,
    pub daily_build: Mutex<Value>,
    pub daily_date: Mutex<String>,
    /// TDX_MASTER 兜底状态（对齐 tdx_backup.status；真实现由 zg-market 提供 available 判定）
    pub tdx_backup_enabled: bool,
}

impl WebState {
    pub fn new(port: u16, offline: bool, no_browser: bool) -> Self {
        Self {
            port,
            offline,
            no_browser,
            stock_list: Mutex::new(Vec::new()),
            list_time: Mutex::new(None),
            data_source: Mutex::new(None),
            raw_rows: Mutex::new(Vec::new()),
            raw_time: Mutex::new(0.0),
            list_lock: Mutex::new(()),
            live: Mutex::new(json!({
                "stocks": {}, "updated": Value::Null, "fetch_ms": Value::Null,
                "cycle_ms": Value::Null, "quote_age": Value::Null, "source": Value::Null,
                "watch_n": 0, "tdx": tdx_flag(), "cycle": zg_market::consts::LIVE_CYCLE,
            })),
            watch_extra: Mutex::new(Vec::new()),
            alerts: Mutex::new(HashMap::new()),
            alert_log: Mutex::new(Vec::new()),
            alert_load_ok: Mutex::new(true),
            alert_flush: Mutex::new(0.0),
            theme_cache: Mutex::new(HashMap::new()),
            theme_prog: Mutex::new(json!({"running": false})),
            theme_refreshing: std::sync::atomic::AtomicBool::new(false),
            czq_cache: Mutex::new(HashMap::new()),
            czsc_scan_state: Mutex::new(json!({"running": false, "started": "", "total": 0, "error": ""})),
            czsc_scan_running: Mutex::new(false),
            market: MarketState::new(),
            weekly_build: Mutex::new(json!({"running": false, "total": 0, "done": 0,
                                            "date": "", "attempted": false, "src": "", "secs": 0.0})),
            min_build: Mutex::new(json!({"running": false, "total": 0, "done": 0, "date": "",
                                          "attempted": false, "src": "", "secs": 0.0, "period": ""})),
            daily_build: Mutex::new(json!({"running": false, "total": 0, "done": 0,
                                            "date": "", "attempted": false})),
            daily_date: Mutex::new(String::new()),
            tdx_backup_enabled: tdx_backup_available(),
        }
    }

    /// `bootstrap.load_state`：offline → stocks.json；非 offline → 带 mtime 的 list_time。
    pub fn load_state(&self) {
        // 题材缓存开机即装（点击路径零网络，不等预热线程）
        let _ = crate::aux_svc::load_theme_disk(self);
        let paths = zg_core::paths::get();
        let stocks_file = paths.market_data.join("stocks.json");
        if self.offline {
            let list: Vec<Value> = std::fs::read_to_string(&stocks_file)
                .map_err(|_| ())
                .and_then(|t| {
                    // Python read_json(..., encoding="utf-8-sig")
                    let t = t.trim_start_matches('\u{feff}');
                    serde_json::from_str::<Vec<Value>>(t).map_err(|_| ())
                })
                .unwrap_or_default();
            *self.stock_list.lock_ok() = list;
        } else {
            if let Ok(t) = std::fs::read_to_string(&stocks_file) {
                if let Ok(list) = serde_json::from_str::<Vec<Value>>(&t) {
                    *self.stock_list.lock_ok() = list;
                    if let Ok(meta) = std::fs::metadata(&stocks_file) {
                        if let Ok(mtime) = meta.modified() {
                            let dt: chrono::DateTime<chrono::Local> = mtime.into();
                            *self.list_time.lock_ok() =
                                Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
                        }
                    }
                }
            }
        }
        crate::aux_svc::load_alerts(self);
        zg_market::market_data::load_daily_cache(&self.market);
        // WebState.daily_date 与缓存日期同源（pct_base / daily-k 状态用）
        let daily_date = self.market.daily_date.lock_ok().clone();
        *self.daily_date.lock_ok() = daily_date;
        zg_market::market_data::load_weekly_cache(&self.market);
        zg_market::market_data::load_min_cache(&self.market);
    }
}

/// Python `config.HAS_TDX` = `pytdx 可导入`。生产/测试环境均无 pytdx（P5 后也不会有），
/// 该历史 API 字段恒 false（工单 P4 备注：语义为「pydx 可用性」，非本地 tdx 能力）。
pub fn tdx_flag() -> bool {
    false
}

/// Python `TDXMasterBackup.available`：env 开关 + `modules/market/bin/tdx-bridge.exe` 存在。
fn tdx_backup_available() -> bool {
    let env_ok = std::env::var("TDX_MASTER_BACKUP")
        .map(|v| !matches!(v.to_lowercase().as_str(), "0" | "false" | "off"))
        .unwrap_or(true);
    if !env_ok {
        return false;
    }
    zg_core::paths::get()
        .root
        .join("modules")
        .join("market")
        .join("bin")
        .join("tdx-bridge.exe")
        .is_file()
}

/// 简单 set 语义（Python `set`）：add + 超过上限时 pop 任意元素。
pub fn watch_add(set: &Mutex<Vec<String>>, code: &str) -> usize {
    let mut g = set.lock_ok();
    if !g.iter().any(|c| c == code) {
        g.push(code.to_string());
    }
    if g.len() > 60 {
        g.pop();
    }
    g.len()
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

