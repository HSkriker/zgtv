//! zg-api：8786 主站（P4）。对齐 `server.py` + `modules/zgapp/*` 的服务层。
//!
//! tv 侧使用说明（2026-11-07 架构工单 2 裁决）：本内嵌副本在 tv 二进制里
//! **只使用 `http` / `server` / `staticfiles` 三个模块**（tv 自带路由，见
//! `src/router.rs`，不调用本 crate 的 `run()`）。其余模块（`aux_svc`、`consts`、
//! `live`、`monitor*`、`routes`、`state` 及 `run()`）是 zg 8786/8787 服务层的
//! 编译陪跑，tv 运行期不可达。
//!
//! 之所以不删：上游 `gs/zg/zg-rs` 仍在活跃维护（2026-11-03 有全库审查修复），
//! 保持与上游文件级一一对应，将来回搬修复时可直接 diff/整文件覆盖；删掉的
//! 模块反而会让同步通道断掉。确认彻底不再对照后，再按工单裁剪。

pub mod aux_svc;
pub mod consts;
pub mod http;
pub mod live;
pub mod monitor;
pub mod monitor_flow;
pub mod monitor_fund;
pub mod monitor_live;
pub mod monitor_sentiment;
pub mod monitor_services;
pub mod monitor_ths;
pub mod monitor_yuanhang;
pub mod routes;
pub mod server;
pub mod state;
pub mod staticfiles;

use zg_core::sync::MutexExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use routes::App;
use state::WebState;
use serde_json::Value;

/// 启动 8786：状态装载 → （非离线：预热线程）→ HTTP 服务。
/// `no_browser` 语义保留（桌面环境打开浏览器；服务端环境无操作）。
pub fn run(port: u16, offline: bool, no_browser: bool) -> Result<(), String> {
    let state = Arc::new(WebState::new(port, offline, no_browser));
    state.load_state();
    if !offline {
        start_prewarm(Arc::clone(&state));
        start_market_prewarm(Arc::clone(&state));
    }
    let shutdown = Arc::new(AtomicBool::new(false));

    // 8787 行情监控：随主站自动拉起（同进程线程；对外行为与 Python「子进程」一致，
    // 但不再有残留进程 —— 端口被占时只告警并跳过，不拖累主站）。
    // 与 Python 一致：`--offline` 不拉起行情监控子进程。
    if !offline {
        let sh = Arc::clone(&shutdown);
        let flow_port: u16 = std::env::var("FLOW_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);
        std::thread::Builder::new()
            .name("flow-8787".into())
            .spawn(move || {
                let mapp = Arc::new(monitor::MonitorApp { shutdown: Arc::clone(&sh) });
                let handler: server::Handler =
                    Arc::new(move |req| monitor::dispatch(&mapp, req));
                match server::serve(flow_port, handler, sh) {
                    Ok(()) => eprintln!("行情监控已停止"),
                    Err(e) => eprintln!("行情监控启动失败: {e}（主站不受影响）"),
                }
            })
            .ok();
    }

    eprintln!("==========================================================");
    eprintln!("  市场提醒机器人已启动（Rust zg-api）");
    eprintln!("  打开浏览器访问: http://127.0.0.1:{port}");
    eprintln!("  关闭本窗口或按 Ctrl+C 停止服务");
    eprintln!("==========================================================");
    // 对齐 bootstrap.py:77 —— 绑定成功后打开默认浏览器（--no-browser 跳过）。
    // 用 /api/shared/status 确认端口上确实是本服务，避免端口被占时打开别的站点。
    if !no_browser {
        let p = port;
        std::thread::Builder::new()
            .name("open-browser".into())
            .spawn(move || {
                for _ in 0..60 {
                    if is_running(p) {
                        open_browser(&format!("http://127.0.0.1:{p}"));
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
            .ok();
    }
    let app = Arc::new(App {
        state,
        shutdown: Arc::clone(&shutdown),
    });
    let handler: server::Handler = Arc::new(move |req| routes::dispatch(&app, req));
    server::serve(port, handler, shutdown)
}

/// 探测端口上是否已是本服务（`/api/shared/status` 返回 name=整合工作台）。
pub fn is_running(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut s) = TcpStream::connect(&addr) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
    let req = format!("GET /api/shared/status HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).contains("整合工作台")
}

/// 用系统默认浏览器打开 URL；失败静默（是否调用由 `--no-browser` 决定）。
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn start_prewarm(state: Arc<WebState>) {
    // `bootstrap._prewarm_local_gbbq`：gbbq 预热（失败仅告警；Rust 走 xdxr 缓存/事件链）
    std::thread::spawn(|| {
        let (mkt, cd) = match zg_core::reader::resolve("600519", None) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[local] gbbq 预热失败(请求时将按降级链处理): {e}");
                return;
            }
        };
        if let Err(e) = zg_core::adjust::adjusted_day(&mkt, &cd) {
            eprintln!("[local] gbbq 预热失败(请求时将按降级链处理): {e}");
        }
    });
    // `bootstrap.market_warmup`：TQ 预热
    std::thread::spawn(zg_market::tq::warmup);
    // 宽基成分股目录：载入磁盘缓存 + 后台校对一次（列表有变才回写）
    zg_market::indices::start_members_cache();
    // 8787 lifespan：flow 0s / live 6s / fund 18s（错峰启动，避免冷启动并发 TLS）
    std::thread::Builder::new()
        .name("monitor-flow".into())
        .spawn(monitor_flow::flow_live_loop)
        .ok();
    // 8786 盘中实时巡检（TQ→同花顺→腾讯）+ 预警判定
    let st_live = Arc::clone(&state);
    std::thread::Builder::new()
        .name("live-loop".into())
        .spawn(move || live::live_loop(st_live))
        .ok();
    // 启动时用本地 gbbq 同步共享 xdxr 缓存（离线、秒级；gbbq 未变则跳过）
    std::thread::Builder::new()
        .name("xdxr-sync".into())
        .spawn(|| match zg_core::events::sync_cache_from_gbbq(false) {
            Ok(v) if v.get("skipped").and_then(|x| x.as_bool()).unwrap_or(false) => {}
            Ok(v) => eprintln!(
                "[xdxr-sync] {}",
                serde_json::to_string(&v).unwrap_or_default()
            ),
            Err(e) => eprintln!("[xdxr-sync] 失败: {e}"),
        })
        .ok();
    // 股票列表刷新（同花顺→东财→Ashare→腾讯→新浪；失败 10s 后重试一次）
    let st_list = Arc::clone(&state);
    std::thread::Builder::new()
        .name("bg-list".into())
        .spawn(move || aux_svc::bg_list(st_list))
        .ok();
    std::thread::Builder::new()
        .name("monitor-live".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(6));
            monitor_live::live_loop();
        })
        .ok();
    std::thread::Builder::new()
        .name("monitor-fund".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(18));
            monitor_fund::fund_loop();
        })
        .ok();
    let _ = state;
    // 说明：K 缓存的定时刷新循环仍为 P4 后续项（当前由 `/api/*-k?action=build`
    // 手动触发或 `zg kcache build` 离线重建）；列表刷新与 live 循环已在本函数启动。
}

/// 市场强弱侧栏预热：打开页面即可点，不等首次取数。
///
/// 只走既有降级链（TQ `get_market_data` count=2 / 东财 / 腾讯 / 同花顺），
/// **不碰** `get_market_snapshot`；顺序执行，避免冷启动并发打满。
fn start_market_prewarm(state: Arc<WebState>) {
    std::thread::Builder::new()
        .name("market-prewarm".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let t0 = std::time::Instant::now();
            // 行业快照：接口本身毫秒级返回磁盘快照，同时触发一次后台刷新
            let heat = aux_svc::industry_heat_all_request(&state, "desc", false);
            let n_ind = heat.get("industries").and_then(|v| v.as_array())
                .map(|a| a.len()).unwrap_or(0);
            let n_stk = heat.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!("[prewarm] 行业快照就绪：{n_ind} 个二级行业 / {n_stk} 只成分股");
            // 指数列表（冷启动 3~4s，之后走 10s 内存缓存）
            let snap = zg_market::indices::snapshot(false, false);
            let ok = snap.get("ok_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = snap.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!("[prewarm] 主要指数就绪：{ok}/{total}");
            // 7 个宽基成分股：服务端 SWR 缓存 + 落盘，点击时直接命中
            let mut done = 0usize;
            for code in ["000016", "000300", "000905", "000852", "932000", "930050", "000510"] {
                let v = zg_market::indices::constituents(code, false, false);
                if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                    done += 1;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            eprintln!("[prewarm] 指数成分股就绪 {done}/7，总用时 {:.1}s",
                      t0.elapsed().as_secs_f64());
            // 题材本地化：缓存已在 load_state 装载；覆盖率不足 90%（首次升级/长期未刷新）时
            // 自动引导一次增量补齐，之后日常刷新全靠 刷新题材.bat
            let n_cache = state.theme_cache.lock_ok().len();
            eprintln!("[prewarm] 题材本地缓存已就绪（内存 {n_cache}）");
            let mut universe = state.stock_list.lock_ok().len();
            if universe == 0 {
                // 预热点时股票列表可能还在后台刷新，回落 stocks.json 数一下
                let path = zg_core::paths::get().market_data.join("stocks.json");
                if let Ok(t) = std::fs::read_to_string(&path) {
                    if let Ok(list) = serde_json::from_str::<Vec<Value>>(t.trim_start_matches('\u{feff}')) {
                        universe = list.len();
                    }
                }
            }
            if universe > 0 && (n_cache as f64) < (universe as f64) * 0.9 {
                let r = aux_svc::theme_refresh_start(&state, false, 3, None);
                eprintln!("[prewarm] 题材覆盖率 {n_cache}/{universe} 不足 90%，自动引导后台补齐：{}",
                          r.get("msg").and_then(|v| v.as_str()).unwrap_or(""));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_and_flags() {
        assert!(consts::czsc_freq_of_period("day").is_some());
        assert!(!state::tdx_flag());
    }
}
