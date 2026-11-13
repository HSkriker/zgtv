//! P1 门 1：逐值等价（fixtures 固定夹具，Python oracle 金标）。
//!
//! 金标由 `tools/verify/dump_core_golden.py` 生成，覆盖 read_day / chart_bars /
//! qfq_factor_by_day / resolve / read_min / resample_min / minute_bars / events_of
//! 以及 7 条错误文案。值比较为 f64 位级相等（JSON 往返本就精确），错误比较
//! `(异常类名, 文案)` 二元组。

use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::{json, Value};
use zg_core::error::ZgError;
use zg_core::{api, events, paths, reader};

fn repo_root() -> PathBuf {
    // zg 原件 crate 在 zg-rs/crates/ 下需三次上跳到仓根；tv 内嵌在 crates/ 下浅一层，两次即 tv 仓根
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .canonicalize()
        .expect("repo root")
}

/// 行情夹具与 bar 级金标不入公开仓（见 .gitignore）。缺失时整组跳过而不是变红：
/// 没有行情数据可校，本门无事可做；重建跑 tools/restore-fixtures.sh。
fn fixtures_ready() -> bool {
    let root = repo_root();
    root.join("eq/fixtures/tdx").is_dir()
        && root.join("eq/golden/rust/core_golden.json.gz").is_file()
}

macro_rules! need_fixtures {
    () => {
        if !fixtures_ready() {
            eprintln!("SKIP bars_parity: 行情夹具/金标缺失（夹具 tools/restore-fixtures.sh；core_golden.json.gz 需从录制机拷）");
            return;
        }
    };
}

fn setup() -> PathBuf {
    static INIT: OnceLock<PathBuf> = OnceLock::new();
    INIT.get_or_init(|| {
        let root = repo_root();
        let fixtures = root.join("eq/fixtures/tdx");
        let mut p = paths::Paths::from_env();
        p.vipdoc = fixtures.clone();
        p.xdxr_cache_dir = fixtures.join("xdxr");
        p.gbbq_file = fixtures.join("__missing_gbbq__");
        p.vendor_dir = root.join("modules/market/vendor");
        paths::set_for_test(p);
        fixtures
    })
    .clone()
}

fn golden() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        let path = repo_root().join("eq/golden/rust/core_golden.json.gz");
        let f = std::fs::File::open(&path).expect("open golden");
        let mut gz = flate2::read::GzDecoder::new(f);
        let mut text = String::new();
        gz.read_to_string(&mut text).expect("gunzip golden");
        serde_json::from_str(&text).expect("parse golden")
    })
}

fn enc<T: Serialize>(r: Result<T, ZgError>) -> Value {
    match r {
        Ok(v) => json!({"ok": v}),
        Err(e) => json!({"err": [e.kind(), e.message()]}),
    }
}

/// 错误文案内嵌夹具绝对路径，且 golden 录制仓根（/mnt/h/wsl/gs/zg）与对拍仓不同：
/// 两边都把 fixtures 前缀归一成 <fixtures> 后再比，只比相对结构与消息本体。
fn scrub(v: &Value, fixtures: &str) -> Value {
    match v {
        Value::String(s) => Value::String(
            s.replace(fixtures, "<fixtures>")
                .replace("/mnt/h/wsl/gs/zg/eq/fixtures/tdx", "<fixtures>"),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub(x, fixtures)).collect()),
        Value::Object(m) => Value::Object(
            m.iter().map(|(k, x)| (k.clone(), scrub(x, fixtures))).collect(),
        ),
        other => other.clone(),
    }
}

fn check(label: &str, want: &Value, got: Value) {
    let fixtures = format!(
        "{}/eq/fixtures/tdx",
        repo_root().to_string_lossy().replace("\\", "/")
    );
    let want = scrub(want, &fixtures);
    let got = scrub(&got, &fixtures);
    if want != got {
        let w = serde_json::to_string(&want).unwrap_or_default();
        let g = serde_json::to_string(&got).unwrap_or_default();
        let w_short: String = w.chars().take(600).collect();
        let g_short: String = g.chars().take(600).collect();
        panic!("{label} 不一致\n  golden: {w_short}\n  rust  : {g_short}");
    }
}

#[test]
fn read_day_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["read_day"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        check(
            &format!("read_day {mkt}{code}"),
            &c["result"],
            enc(api::read_day_bars(mkt, code)),
        );
    }
}

#[test]
fn qfq_factors_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["factors"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        check(
            &format!("factors {mkt}{code}"),
            &c["result"],
            enc(zg_core::adjust::qfq_factor_by_day(mkt, code)),
        );
    }
}

#[test]
fn chart_bars_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["chart"].as_array().unwrap() {
        let code = c["code"].as_str().unwrap();
        let period = c["period"].as_str().unwrap();
        let lmt = c["lmt"].as_u64().unwrap_or(0) as usize;
        let beg = c["beg"].as_str();
        let end = c["end"].as_str();
        let label = format!("chart {code} {period:?} lmt={lmt} beg={beg:?} end={end:?}");
        check(&label, &c["result"], enc(api::chart_bars(code, period, lmt, beg, end)));
    }
}

#[test]
fn resolve_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["resolve"].as_array().unwrap() {
        let code = c["code"].as_str().unwrap();
        let market = c["market"].as_str();
        check(
            &format!("resolve {code} {market:?}"),
            &c["result"],
            enc(reader::resolve(code, market).map(|(m, c)| json!([m, c]))),
        );
    }
    for c in golden()["cases"]["is_index"].as_array().unwrap() {
        let mk = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        check(
            &format!("is_index {mk}{code}"),
            &c["result"],
            json!({"ok": reader::is_index(mk, code)}),
        );
    }
}

#[test]
fn error_messages_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["errors"].as_array().unwrap() {
        let fn_name = c["fn"].as_str().unwrap();
        let a: Vec<&str> = c["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let got = match fn_name {
            "chart" => match c["kw"].as_object() {
                Some(kw) => enc(api::chart_bars(
                    a[0],
                    a[1],
                    0,
                    kw.get("beg").and_then(|v| v.as_str()),
                    kw.get("end").and_then(|v| v.as_str()),
                )),
                None => enc(api::chart_bars(a[0], a[1], 0, None, None)),
            },
            "minute" => {
                let count = c["args"].get(3).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                enc(api::minute_bars(a[0], a[1], a[2], count)
                    .map(|(b, m)| json!([b, m])))
            }
            "read_min" => enc(api::read_min_bars(a[0], a[1]).map(|v| (*v).clone())),
            "read_day" => enc(api::read_day_bars(a[0], a[1])),
            other => panic!("unknown fn {other}"),
        };
        check(&format!("error case {fn_name} {a:?}"), &c["result"], got);
    }
}

#[test]
fn read_min_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["read_min"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        check(
            &format!("read_min {mkt}{code}"),
            &c["result"],
            enc(api::read_min_bars(mkt, code).map(|v| (*v).clone())),
        );
    }
}

#[test]
fn resample_min_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["resample_min"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        let minutes = c["minutes"].as_u64().unwrap() as u32;
        let raw = reader::read_min(mkt, code).map(|v| (*v).clone());
        let got = match raw {
            Ok(bars) => json!({"ok": api::resample_minutes(&bars, minutes)}),
            Err(e) => json!({"err": [e.kind(), e.message()]}),
        };
        check(&format!("resample {mkt}{code} {minutes}m"), &c["result"], got);
    }
}

#[test]
fn minute_bars_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["minute"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        let freq = c["freq"].as_str().unwrap();
        let count = c["count"].as_u64().unwrap_or(0) as usize;
        check(
            &format!("minute {mkt}{code} {freq} count={count}"),
            &c["result"],
            enc(api::minute_bars(mkt, code, freq, count).map(|(b, m)| json!([b, m]))),
        );
    }
}

#[test]
fn events_parity() {
    need_fixtures!();
    setup();
    for c in golden()["cases"]["events"].as_array().unwrap() {
        let mkt = c["market"].as_str().unwrap();
        let code = c["code"].as_str().unwrap();
        let anchor = c["anchor"].as_str();
        check(
            &format!("events {mkt}{code} {anchor:?}"),
            &c["result"],
            enc(events::events_of(mkt, code, anchor)),
        );
    }
}
