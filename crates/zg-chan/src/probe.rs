//! 信号全量探针（P2 门 1）—— `tools/verify/probe_signal_parity.py` 的 Rust 等价实现。
//!
//! 输入 = `eq/golden/rust/parity_input.json`（9 个 case × 全量注册名 × 2 轮），
//! 输出 = 与 `signals_probe_prod102.json` 同构的 `{count, rows}`，供
//! `tools/verify/compare_probe.py` 逐条 diff。
//!
//! 两轮语义（不可混淆）：
//!   default  空参数表 —— 等价 Python `call_signal(name, c, None)`
//!   project  `official::SPECS` 钉死参数 —— 只跑项目已接入的 17 个信号
//!
//! 与 Python 的对齐点：`CZSC(bars_raw, max_bi_num=60)`、每次调用新建 `TaCache`、
//! 只取 `sigs[0]`、异常文案按 `{类名}: {消息}` 二元组。

use std::collections::HashMap;

use czsc_core::analyze::CZSC;
use czsc_core::objects::bar::RawBar;
use czsc_signals::registry::{SIGNAL_REGISTRY, TRADER_SIGNAL_REGISTRY};
use czsc_signals::types::TaCache;
use serde_json::{json, Map, Value};

use crate::official::SPECS;

/// 与 `signals_official.py:365` / `probe_signal_parity.py:31` 一致，勿改。
pub const MAX_BI_NUM: usize = 60;
/// Python `CZSC(min_bi_len=6)` 默认值。
pub const MIN_BI_LEN: usize = 6;

pub const DEFAULT_ROUND: &str = "default";
pub const PROJECT_ROUND: &str = "project";

/// 跑全量探针。`params_doc` 可选：给了就校验磁盘上的 project 参数与 `SPECS` 逐值相同
/// （防止探针与运行时口径悄悄分叉）。
pub fn run(input: &Value, params_doc: Option<&Value>) -> Result<Value, String> {
    if let Some(doc) = params_doc {
        verify_project_params(doc)?;
    }
    for key in ["CZSC_MAX_BI_NUM", "CZSC_MIN_BI_LEN", "czsc_max_bi_num", "czsc_min_bi_len"] {
        if std::env::var(key).is_ok() {
            return Err(format!(
                "环境变量 {key} 已设置，会改变 max_bi_num/min_bi_len 语义，拒绝运行"
            ));
        }
    }

    let names: Vec<String> = input["names"]
        .as_array()
        .ok_or("input.names 缺失")?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    let project: HashMap<&str, HashMap<String, Value>> =
        SPECS.iter().map(|s| (s.name, s.param_dict())).collect();
    let empty: HashMap<String, Value> = HashMap::new();

    let mut rows: Vec<Value> = Vec::new();
    for case in input["cases"].as_array().ok_or("input.cases 缺失")? {
        let code = case["code"].as_str().ok_or("case.code 缺失")?;
        let freq = case["freq"].as_str().ok_or("case.freq 缺失")?;
        let bars: Vec<RawBar> = case["bars"]
            .as_array()
            .ok_or("case.bars 缺失")?
            .iter()
            .map(|b| serde_json::from_value::<RawBar>(b.clone()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        // 生产链路 RawBar.id 恒 0（format_standard_kline 不传 id），输入里照抄，勿重编。
        let czsc = CZSC::new(bars, MAX_BI_NUM, MIN_BI_LEN);

        for round in [DEFAULT_ROUND, PROJECT_ROUND] {
            for name in &names {
                let pmap: &HashMap<String, Value> = if round == PROJECT_ROUND {
                    match project.get(name.as_str()) {
                        Some(p) => p,
                        None => continue, // 项目未接入的信号不跑 project 轮
                    }
                } else {
                    &empty
                };
                let (hit, count, error) = dispatch(name, &czsc, pmap);
                rows.push(json!({
                    "name": name,
                    "code": code,
                    "freq": freq,
                    "round": round,
                    "hit": hit,
                    "count": count,
                    "error": error,
                }));
            }
        }
    }

    rows.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v["name"].as_str().unwrap_or_default().to_string(),
                v["code"].as_str().unwrap_or_default().to_string(),
                v["freq"].as_str().unwrap_or_default().to_string(),
                v["round"].as_str().unwrap_or_default().to_string(),
            )
        };
        key(a).cmp(&key(b))
    });

    Ok(json!({"czsc": crate::official::SIGNALS_SOURCE, "count": rows.len(), "rows": rows}))
}

/// 返回 `(hit, count, error)`，语义与 `probe_signal_parity.py::one` 一致。
fn dispatch(
    name: &str,
    czsc: &CZSC,
    params: &HashMap<String, Value>,
) -> (Option<Value>, usize, Option<String>) {
    if TRADER_SIGNAL_REGISTRY.contains_key(name) && !SIGNAL_REGISTRY.contains_key(name) {
        return (
            None,
            0,
            Some(format!(
                "TypeError: {name} is a trader-state signal; dispatch via CzscTrader.update_signals"
            )),
        );
    }
    let Some(meta) = SIGNAL_REGISTRY.get(name) else {
        return (None, 0, Some(format!("KeyError: unknown signal: {name}")));
    };
    // Python 侧每次 call_signal 都新建 TaCache；两边都从空缓存开始，逐信号独立。
    let mut cache = TaCache::new();
    // 单信号 panic 隔离：Python 侧异常被捕获成 error 字段，Rust 侧不能因此
    // 让整轮探针（全 case × 全信号）崩掉 —— 那样一份对比结果都拿不到。
    let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (meta.func)(czsc, params, &mut cache)
    }));
    match called {
        Ok(sigs) => {
            let hit = sigs.first().map(|s| json!({"key": s.key(), "value": s.value()}));
            (hit, sigs.len(), None)
        }
        Err(payload) => (
            None,
            0,
            Some(format!(
                "PanicException: {}",
                crate::official::panic_text(payload.as_ref())
            )),
        ),
    }
}

fn verify_project_params(doc: &Value) -> Result<(), String> {
    let Some(project) = doc.get("project").and_then(|v| v.as_object()) else {
        return Ok(()); // 老文件可能没有 project 段，跳过
    };
    let ours: Map<String, Value> = SPECS
        .iter()
        .map(|s| (s.name.to_string(), Value::Object(s.param_dict().into_iter().collect())))
        .collect();
    if ours.len() != project.len() {
        return Err(format!(
            "parity_params.project 有 {} 个信号，SPECS 有 {} 个",
            project.len(),
            ours.len()
        ));
    }
    for (name, want) in &ours {
        let got = project
            .get(name)
            .ok_or_else(|| format!("parity_params.project 缺 {name}"))?;
        if got != want {
            return Err(format!(
                "{name} 钉参与 parity_params 不一致：SPECS={} 文件={}",
                want, got
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_guard_then_empty_input() {
        // 环境变量是进程级的，本测试独占（不要拆成两个测试 —— 并行会互相干扰）
        std::env::set_var("CZSC_MAX_BI_NUM", "50");
        let err = run(&json!({"names": [], "cases": []}), None).unwrap_err();
        std::env::remove_var("CZSC_MAX_BI_NUM");
        assert!(err.contains("CZSC_MAX_BI_NUM"), "{err}");

        let out = run(&json!({"names": [], "cases": []}), None).unwrap();
        assert_eq!(out["count"], 0);
        assert_eq!(out["rows"].as_array().unwrap().len(), 0);
    }
}
