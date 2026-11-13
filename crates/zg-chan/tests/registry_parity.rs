//! P2-0 自检：Rust 注册表 vs czsc 1.0.2 Python 金标，逐字段相同。
//!
//! 金标 `eq/golden/rust/signals_prod102.json` 由 P0-4 从 Python
//! `czsc._native.list_all_signals() + get_signal_template()` 导出（246 条）。
//! 本测试同时锁住 `official::SPECS` 的钉参 —— 与 `parity_params.json` 的 project 段
//! 逐键逐值相同（该文件就是 P0.5 探针喂给 Rust 的那一份）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use zg_chan::official;
use zg_chan::registry::REGISTRY;

fn repo_root() -> PathBuf {
    // zg 原件 crate 在 zg-rs/crates/ 下需三次上跳到仓根；tv 内嵌在 crates/ 下浅一层，两次即 tv 仓根
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .canonicalize()
        .expect("repo root")
}

fn load(rel: &str) -> Value {
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn registry_matches_python_golden() {
    let golden = load("eq/golden/rust/signals_prod102.json");
    let rows = golden["signals"].as_array().expect("signals");
    assert_eq!(golden["count"].as_u64().unwrap() as usize, rows.len());
    assert_eq!(REGISTRY.len(), rows.len(), "注册表数量不一致");

    let mut by_cat: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        let name = row["name"].as_str().unwrap();
        let got = REGISTRY
            .get(name)
            .unwrap_or_else(|| panic!("Rust 注册表缺 {name}"));
        assert_eq!(got.param_template, row["param_template"].as_str().unwrap(), "{name} param_template");
        assert_eq!(got.category, row["category"].as_str().unwrap(), "{name} category");
        assert_eq!(got.namespace, row["namespace"].as_str().unwrap(), "{name} namespace");
        *by_cat.entry(got.category.as_str()).or_default() += 1;
    }
    assert_eq!(by_cat["kline"], 222);
    assert_eq!(by_cat["trader"], 24);
    assert_eq!(
        golden["by_category"]["kline"].as_u64().unwrap() as usize,
        by_cat["kline"]
    );
    assert_eq!(
        golden["by_category"]["trader"].as_u64().unwrap() as usize,
        by_cat["trader"]
    );
}

#[test]
fn specs_params_match_parity_params() {
    let doc = load("eq/golden/rust/parity_params.json");
    let project = doc["project"].as_object().expect("project");
    assert_eq!(official::SPECS.len(), project.len(), "SPECS 与 project 参数表数量不一致");

    for spec in official::SPECS {
        let want = project
            .get(spec.name)
            .unwrap_or_else(|| panic!("parity_params 缺 {}", spec.name));
        let got = serde_json::to_value(spec.param_dict()).unwrap();
        let got = json!(got);
        assert_eq!(&got, want, "{} 钉参与 parity_params 不一致", spec.name);
    }
    assert!(official::selfcheck().is_empty());
}
