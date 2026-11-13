//! candle 引擎金标对表：用金标里同一批 bars 跑 Rust 引擎，signals 逐值比较。
//! 金标来自 `eq/golden/wsl/20261027`（Python 服务 + talib 0.8.1 录制）。

use std::path::PathBuf;

use serde_json::Value;
use zg_market::canon;
use zg_market::candles;

fn golden_dir() -> PathBuf {
    // zg 原件 crate 在 zg-rs/crates/ 下需三次上跳到仓根；tv 内嵌在 crates/ 下浅一层，两次即 tv 仓根
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("eq").join("golden").join("wsl").join("20261027").join("api")
}

/// bar 级金标随行情数据一起不入公开仓（见 .gitignore）：整个金标目录不在即无事可校，
/// 跳过而不是变红。目录在但缺单个文件属真异常，交给下面的 read 直接 panic。
fn golden_present() -> bool {
    golden_dir().is_dir()
}

fn check(golden_file: &str) {
    if !golden_present() {
        eprintln!("SKIP candle 金标: {} 缺失（bar 级金标不入库，需从录制机拷贝）", golden_dir().display());
        return;
    }
    let path = golden_dir().join(golden_file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取金标失败 {}: {e}", path.display()));
    let doc: Value = serde_json::from_str(&text).expect("金标 JSON");
    let body = doc.get("body").expect("金标 body");
    let bars = body.get("bars").and_then(|b| b.as_array()).expect("金标 bars");
    let golden_signals = body.get("candle_signals").cloned().unwrap_or_else(|| serde_json::json!([]));
    let period = match golden_file {
        f if f.contains("min30") => "min30",
        f if f.contains("min60") => "min60",
        _ => "day",
    };
    // 金标为盘后录制（market_open=false）→ provisional_last=false
    let result = candles::analyze_candles(bars, period, false, Some("rec27"));
    assert_eq!(result["engine"], "talib");
    let actual = result.get("signals").cloned().unwrap_or_else(|| serde_json::json!([]));
    let a = canon::dumps(&actual);
    let g = canon::dumps(&golden_signals);
    if a != g {
        let fmt = |v: &Value| -> Vec<String> {
            v.as_array().map(|arr| arr.iter().map(|s| format!(
                "#{} {} dir={} status={}",
                s.get("index").and_then(|x| x.as_i64()).unwrap_or(-1),
                s.get("fn").and_then(|x| x.as_str()).unwrap_or(""),
                s.get("direction").and_then(|x| x.as_i64()).unwrap_or(-9),
                s.get("status").and_then(|x| x.as_str()).unwrap_or(""),
            )).collect()).unwrap_or_default()
        };
        eprintln!("golden: {:#?}", fmt(&golden_signals));
        eprintln!("actual: {:#?}", fmt(&actual));
    }
    assert_eq!(a, g, "金标 {golden_file} 的 candle_signals 不一致（actual {} 条 / golden {} 条）",
               actual.as_array().map(|x| x.len()).unwrap_or(0),
               golden_signals.as_array().map(|x| x.len()).unwrap_or(0));
}

#[test]
fn min30_signals_match_golden() {
    check("8786.kline.min30.600519.json");
}

#[test]
fn min60_signals_match_golden() {
    check("8786.kline.min60.000001.json");
}

/// 20 根的短序列覆盖不到 3 根确认/10 根失效链的深度，补一条 100 根 min30。
#[test]
fn min30_lmt100_signals_match_golden() {
    check("8786.kline.min30.600519.lmt100.json");
}
