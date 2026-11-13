use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use zg_api::http::Req;
use tv::{api, cli, concepts, heat, live, overview, paths, router, scan, PORT};

fn main() {
    // 必须在任何 zg_core 调用之前设置：paths::get() 只求值一次。
    // 用运行期解析出来的根（ZG_ROOT 优先，否则按可执行文件布局向上找 modules/+data/），
    // 不能用 env!("CARGO_MANIFEST_DIR")——那会把编译机路径烧进二进制，
    // 分发包拷到别的机器上就找不到 catalog/静态资源/symbols。
    let root = paths::tv_root();
    std::env::set_var("ZG_ROOT", root.to_string_lossy().as_ref());
    println!("[tv] 数据根目录: {}", root.display());

    // 一次性任务入口（--refresh-concepts / --fetch-stocks / --gen-symbols /
    // --build-catalog / --replay-overview）：命中即跑完退出，详见 cli.rs。
    cli::try_run_one_shot();

    // 本地缓存自愈：热度/缠论扫描缓存过期（新日线同步后）时后台自动重算，不阻塞服务
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if heat::stale() {
            println!("[tv] 热度缓存过期，启动本地重算…");
            let _ = heat::rebuild();
        }
        if scan::stale() {
            println!("[tv] 缠论扫描缓存过期，启动本地重扫…（全市场约 1-6 分钟）");
            let _ = scan::rebuild();
        }
        // 总览缓存预热：stamp 未变时近乎免费（直接读盘上缓存）；过期则在此后台重建，
        // 避免日线同步后的第一个 /api/sector-overview 请求在线程内阻塞数十秒
        let t0 = std::time::Instant::now();
        match overview::payload(false) {
            Ok(_) => println!("[tv] 总览缓存预热完成（{} ms）", t0.elapsed().as_millis()),
            Err(e) => eprintln!("[tv] 总览缓存预热失败：{e}"),
        }
    });

    // TQ 首次调用有 ~6s 冷启动：后台预热，不阻塞 8790 端口起来。
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(500));
        live::warmup();
    });

    // 概念题材缓存（MAC 在线源）：每周第一次启动刷一次（缺失/退化/早于本周一才后台全量刷新），
    // 同一周内重启不再出网，不阻塞服务。
    // 离线模式（TV_LIVE=0）不联网，直接用本地快照 / block_gn.dat 兜底。
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        concepts::auto_refresh_on_start();
    });

    let handler: zg_api::server::Handler = Arc::new(|req: &Req| {
        let t0 = std::time::Instant::now();
        let resp = router::dispatch(req);
        api::logged(req, resp, t0.elapsed())
    });
    if let Err(e) = zg_api::server::serve(PORT, handler, Arc::new(AtomicBool::new(false))) {
        eprintln!("[tv] 启动失败: {e}");
        std::process::exit(1);
    }
}
