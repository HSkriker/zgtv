//! tv 一次性任务入口：`--refresh-concepts` / `--fetch-stocks` / `--gen-symbols`
//! / `--build-catalog` / `--replay-overview`。
//!
//! 从 main.rs 搬出（2026-11-07 工单 1），行为逐字不变：命中指令则跑完即退出
//! （成功 0、失败 1），未命中直接返回，服务照常启动。

use crate::{catalog, concepts, overview, symbols};

pub fn try_run_one_shot() {
    // 一次性任务入口：`tv --refresh-concepts` 同步跑完概念全量刷新后退出。
    // 给运维/门禁用（HTTP 端点走异步，脚本里等不到确定的完成时刻）。
    if std::env::args().any(|a| a == "--refresh-concepts") {
        match concepts::rebuild() {
            Ok(v) => {
                println!("{v}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[tv] 概念刷新失败: {e}");
                std::process::exit(1);
            }
        }
    }

    // 一次性任务入口：`tv --fetch-stocks` 联网拉全市场股票列表 → data/market/stocks.json。
    // 新机器/新 clone 没有 stocks.json（不进 git）时用它初始化；失败只报错退出。
    if std::env::args().any(|a| a == "--fetch-stocks") {
        match symbols::fetch_stocks() {
            Ok(v) => {
                println!("[tv] {v}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[tv] 拉取股票列表失败: {e}");
                std::process::exit(1);
            }
        }
    }

    // 一次性任务入口：`tv --gen-symbols` 由 stocks.json 生成 symbols.json
    // （market + 拼音首字母/全拼），取代旧的 `python3 tools/gen_symbols.py`。
    if std::env::args().any(|a| a == "--gen-symbols") {
        match symbols::generate() {
            Ok(v) => {
                println!("[tv] {v}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[tv] 生成 symbols.json 失败: {e}");
                std::process::exit(1);
            }
        }
    }

    // 一次性任务入口：`tv --build-catalog --hq-cache <T0002/hq_cache> [--out ...] [--date ...]`
    // 取代旧的 `python3 tools/build_industry_catalog.py`（sync-data.sh 每次同步后调用）。
    if std::env::args().any(|a| a == "--build-catalog") {
        let usage = "用法: tv --build-catalog --hq-cache <T0002/hq_cache> \
                     [--out modules/market/industry_catalog.json] [--date YYYY-MM-DD]";
        let Some(hq) = arg_after("--hq-cache") else {
            eprintln!("[tv] 缺少 --hq-cache\n{usage}");
            std::process::exit(1);
        };
        let out = arg_after("--out").unwrap_or_else(|| {
            crate::paths::tv_root()
                .join("modules")
                .join("market")
                .join("industry_catalog.json")
                .to_string_lossy()
                .to_string()
        });
        let date = arg_after("--date")
            .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
        match catalog::build(std::path::Path::new(&hq), std::path::Path::new(&out), &date) {
            Ok(v) => {
                println!("{v}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[tv] 行业目录生成失败: {e}");
                std::process::exit(1);
            }
        }
    }

    // 一次性任务入口：`tv --replay-overview <out.csv> [days]` 输出板块总览历史回放
    // （逐日全行业指标 + 前向 20/60 日收益），与 /api/sector-overview 共用同一套网格代码
    if let Some(pos) = std::env::args().position(|a| a == "--replay-overview") {
        let args: Vec<String> = std::env::args().collect();
        let out = args.get(pos + 1).cloned().unwrap_or_default();
        if out.is_empty() {
            eprintln!("[tv] 用法: tv --replay-overview <out.csv> [days=750]");
            std::process::exit(1);
        }
        let days: usize = args.get(pos + 2).and_then(|v| v.parse().ok()).unwrap_or(750);
        match overview::replay_csv(std::path::Path::new(&out), days) {
            Ok(msg) => {
                println!("{msg}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[tv] 总览回放失败: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// 取 `--flag value` 里紧跟 flag 的值（一次性任务入口用；重复出现取第一处）。
fn arg_after(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
