//! read_day_tail 与 read_day 的逐值等价：末段、短文件、前复权尾部。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use zg_core::{api, paths};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .canonicalize()
        .expect("repo root")
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

fn day_cases(fixtures: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for mkt in ["sh", "sz"] {
        let dir = fixtures.join(mkt).join("lday");
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".day") else { continue };
            let Some(code) = stem.strip_prefix(mkt) else { continue };
            out.push((mkt, code.to_string()));
        }
    }
    out.sort();
    out
}

#[test]
fn tail_equals_full_tail() {
    let fixtures = setup();
    let cases = day_cases(&fixtures);
    if cases.is_empty() {
        eprintln!("SKIP day_tail_parity: 缺夹具 eq/fixtures/tdx（tools/restore-fixtures.sh）");
        return;
    }

    for (mkt, code) in cases {
        let full = api::read_day_bars(mkt, &code).expect("read_day");
        let mut ns = vec![1usize, 2, 3, 70, 140, 1300];
        ns.push(full.len());
        ns.push(full.len() + 500);
        for n in ns {
            let got = api::read_day_bars_tail(mkt, &code, n).expect("read_day_tail");
            let want = &full[full.len().saturating_sub(n.max(1))..];
            assert_eq!(got, want, "{mkt}{code} max_bars={n}");
        }
    }
}

#[test]
fn tail_qfq_equals_full_qfq_tail() {
    let fixtures = setup();
    let cases = day_cases(&fixtures);
    if cases.is_empty() {
        eprintln!("SKIP day_tail_parity: 缺夹具");
        return;
    }

    for (mkt, code) in cases {
        let mut full = api::read_day_bars(mkt, &code).expect("read_day");
        api::apply_qfq_bars(&mut full, mkt, &code).expect("qfq(full)");
        for n in [70usize, 140] {
            let mut tail = api::read_day_bars_tail(mkt, &code, n).expect("tail");
            api::apply_qfq_bars(&mut tail, mkt, &code).expect("qfq(tail)");
            let want = &full[full.len() - tail.len()..];
            assert_eq!(tail, want, "{mkt}{code} qfq max_bars={n}");
        }
    }
}
