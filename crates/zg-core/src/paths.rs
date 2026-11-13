//! 路径/环境变量/端口 —— 与 Python `zgcore/paths.py` 同名同默认值。
//! 例外：三个行情路径（vipdoc / gbbq / xdxr_cache）的内置默认改成仓库内 `data/` 下，
//! 不再写死某台机器的通达信安装盘符；直读客户端目录时由 `run.sh` 用环境变量覆盖。
//!
//! Python 在 import 时求值；本 crate 在首次 `paths::get()` 时求值，
//! 之后运行期改环境变量不生效（与 Python 语义一致）。测试用 `set_for_test` 覆盖。

use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(Clone, Debug)]
pub struct Paths {
    /// 项目根：`ZG_ROOT` 优先；否则二进制所在目录（部署布局 = 仓库根）
    pub root: PathBuf,
    pub data: PathBuf,
    pub settings: PathBuf,
    pub market_data: PathBuf,
    /// 通达信 vipdoc（{sh,sz,bj}/lday/*.day、minline/*.lc1）
    pub vipdoc: PathBuf,
    /// 除权除息事件文件（通达信 T0002）
    pub gbbq_file: PathBuf,
    /// 全项目唯一除权事件源（tdxrs 抓取缓存）
    pub xdxr_cache_dir: PathBuf,
    /// vendored easy_tdx（gbbq 读取器）
    pub vendor_dir: PathBuf,
    /// gbbq 解析结果快照（Python pickle；Rust 不读不写，仅保留路径常量）
    pub gbbq_pkl: PathBuf,

    pub port_bot: u16,
    pub port_workbench: u16,
    pub port_flow: u16,
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => default,
    }
}

fn exe_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Paths {
    pub fn from_env() -> Self {
        let root = match std::env::var("ZG_ROOT") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => exe_root(),
        };
        let data = root.join("data");
        Self {
            settings: data.join("settings"),
            market_data: data.join("market"),
            // 三个行情路径的内置默认都落在仓库内 data/ 下（中性、可搬）：
            //   vipdoc   → data/vipdoc        （copy 模式产物）
            //   gbbq     → data/tdx/gbbq      （tools/sync-data.sh 拷入）
            //   xdxr     → data/tdx_xdxr      （tdxrs 抓取缓存）
            // 直读通达信目录时由 run.sh 导出 ZG_VIPDOC/ZG_GBBQ/ZG_XDXR_CACHE 覆盖，
            // 手跑二进制就自己设这三项（README「环境变量」节有说明）。
            vipdoc: env_path("ZG_VIPDOC", data.join("vipdoc")),
            gbbq_file: env_path("ZG_GBBQ", data.join("tdx").join("gbbq")),
            xdxr_cache_dir: env_path("ZG_XDXR_CACHE", data.join("tdx_xdxr")),
            vendor_dir: root.join("modules").join("market").join("vendor"),
            gbbq_pkl: data.join("gbbq_idx.pkl"),
            data,
            root,
            port_bot: env_u16("BOT_PORT", 8686),
            port_workbench: 8786,
            port_flow: 8787,
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

static STATE: RwLock<Option<Paths>> = RwLock::new(None);

/// 取全局路径（首次调用时按环境变量求值）。
pub fn get() -> Paths {
    {
        let g = STATE.read().expect("paths lock");
        if let Some(p) = g.as_ref() {
            return p.clone();
        }
    }
    let mut g = STATE.write().expect("paths lock");
    if g.is_none() {
        *g = Some(Paths::from_env());
    }
    g.clone().expect("paths initialized")
}

/// 测试/嵌入场景覆盖全局路径（生产代码不调用）。
pub fn set_for_test(p: Paths) {
    *STATE.write().expect("paths lock") = Some(p);
}

/// 重置为「下次按环境变量重新求值」。
pub fn reset() {
    *STATE.write().expect("paths lock") = None;
}

/// 分钟 bars 进程内 LRU 上限（只数只数，不数根数）。
pub fn min_mem_max() -> usize {
    std::env::var("ZG_MIN_MEM_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_defaults() {
        let p = Paths::from_env();
        assert_eq!(p.data, p.root.join("data"));
        assert_eq!(p.port_workbench, 8786);
        assert_eq!(p.port_flow, 8787);
        assert!(p.vendor_dir.ends_with("modules/market/vendor"));
    }
}
