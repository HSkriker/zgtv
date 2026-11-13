//! 笔窗口因果性：同批日线 bars 分别以宽窗（当前表 max_bi_num_for("D")=500）与窄窗（50）
//! 跑 czsc 1.0.2 的 CZSC——窄窗保留的每一笔必须与宽窗末尾对应笔逐字段一致。
//! 这钉死「max_bi_num 裁剪只丢展示、不改保留窗口内的任何计算」——全量历史画缠论
//! 与 czsc 规则的符合性依据之一（另一依据是 registry_parity 对 Python 金标）。

use std::path::PathBuf;

use czsc_core::analyze::CZSC;
use zg_chan::service::{build_czsc, MIN_BI_LEN};

#[test]
fn bi_window_trims_display_only() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .canonicalize().expect("repo root");
    // 行情源（2026-11-06 起 tv 默认直读通达信目录，本地快照 data/vipdoc 已随改造删除）：
    // 优先仓库内 data/vipdoc 快照，其次环境变量 TV_TEST_VIPDOC 指定的通达信目录；
    // 两处都没有才跳过——公开 clone 不带行情，必须 SKIP 而不是红。
    let vipdoc = std::iter::once(root.join("data/vipdoc"))
        .chain(std::env::var("TV_TEST_VIPDOC").ok().map(PathBuf::from))
        .find(|p| p.join("sh").join("lday").join("sh600519.day").is_file());
    let Some(vipdoc) = vipdoc else {
        eprintln!(
            "SKIP 笔窗口因果性: 仓库内 data/vipdoc 与 TV_TEST_VIPDOC 都没有 sh600519.day"
        );
        return;
    };
    // 事件表（前复权必需）走仓库内的 data/tdx/gbbq；它和 vipdoc 是两条独立来源，
    // 装了通达信客户端的机器上 vipdoc 在、gbbq 不在（公开 clone 即如此，data/tdx 不入库），
    // 只判 vipdoc 会在这里 panic 而不是 SKIP。两样齐了才跑。
    let gbbq = root.join("data").join("tdx").join("gbbq");
    if !gbbq.is_file() {
        eprintln!(
            "SKIP 笔窗口因果性: 缺少 data/tdx/gbbq（除权事件表，公开 clone 不携带，需自备）"
        );
        return;
    }
    // 测试进程显式指向仓库根（本地 vipdoc/数据在 tv 树下）
    let mut paths = zg_core::paths::get();
    paths.root = root.clone();
    paths.data = root.join("data");
    paths.settings = paths.data.join("settings");
    paths.market_data = paths.data.join("market");
    // 行情/事件类路径与 run.sh 的导出同口径（否则会落到 paths.rs 的仓库内 data/ 默认值）
    paths.vipdoc = vipdoc;
    paths.gbbq_file = paths.data.join("tdx").join("gbbq");
    paths.xdxr_cache_dir = paths.data.join("tdx_xdxr");
    // xdxr 缓存陈旧时会回落 gbbq（读 vendor 目录），不跟着 root 一起指就找不到
    paths.vendor_dir = root.join("modules").join("market").join("vendor");
    zg_core::paths::set_for_test(paths);

    let (wide, raw) = build_czsc("600519", "D", Some("sh")).expect("日线全历史");
    assert!(raw.len() > 3000, "本地 600519 日线应远多于 3000 根，实际 {}", raw.len());
    assert!(wide.bi_list.len() >= 400, "宽窗（表值 500）笔数应 ≥400，实际 {}", wide.bi_list.len());

    let narrow = CZSC::new(raw, 50, MIN_BI_LEN);
    let n = narrow.bi_list.len();
    assert!((40..=50).contains(&n), "窄窗应保留 40~50 笔，实际 {n}");

    let tail = &wide.bi_list[wide.bi_list.len() - n..];
    for (i, (w, s)) in tail.iter().zip(narrow.bi_list.iter()).enumerate() {
        assert_eq!(
            format!("{w:?}"),
            format!("{s:?}"),
            "第 {i} 笔不一致：窗口裁剪不得改变保留窗口内的笔"
        );
    }
}
