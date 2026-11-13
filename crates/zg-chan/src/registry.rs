//! czsc 1.0.2 注册表视图 —— 对应 `signals_official.py::_build_registry()`。
//!
//! Python 侧走 `czsc._native.{list_all_signals,get_signal_template}`；Rust 侧走
//! `czsc_signals::registry::{list_all_signals,SIGNAL_REGISTRY,TRADER_SIGNAL_REGISTRY}`。
//! `param_keys` 由 `param_template` 里的 `{key}` 反解（排除 `freq`），**不手抄** ——
//! 上游模板一改，键白名单自动跟着变（`signals_official.py:62-74`）。

use std::collections::BTreeMap;
use std::sync::LazyLock;

use czsc_signals::registry::list_all_signals;

/// 一条注册表记录（字段与 `RegisteredSignalInfo` 同序同名）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryRow {
    pub name: String,
    pub param_template: String,
    pub category: String,
    pub namespace: String,
    /// `{key}` 反解结果（已排除 `freq`），升序去重。
    pub param_keys: Vec<String>,
}

/// 全量注册表（246 = 222 kline + 24 trader），按 name 升序。
pub static REGISTRY: LazyLock<BTreeMap<String, RegistryRow>> = LazyLock::new(build_registry);

fn build_registry() -> BTreeMap<String, RegistryRow> {
    let mut out = BTreeMap::new();
    for row in list_all_signals(true, true) {
        let param_keys = template_keys(&row.param_template);
        out.insert(
            row.name.clone(),
            RegistryRow {
                name: row.name,
                param_template: row.param_template,
                category: row.category,
                namespace: row.namespace,
                param_keys,
            },
        );
    }
    out
}

/// 模板 → 参数键集合（Python: `set(re.findall(r"\{(\w+)\}", tpl)) - {"freq"}`）。
///
/// `\w` 在 Python 里是 Unicode 单词字符；模板里实际只有 ASCII 标识符，
/// 这里按 `alphanumeric() || '_'` 判定，语义等价且不引入 regex 依赖。
pub fn template_keys(template: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let key = &after[..end];
        if !key.is_empty()
            && key.chars().all(|c| c == '_' || c.is_alphanumeric())
            && key != "freq"
            && !keys.iter().any(|k| k == key)
        {
            keys.push(key.to_string());
        }
        rest = &after[end + 1..];
    }
    keys.sort();
    keys
}

/// 注册表不可用原因。Rust 侧构建是静态的、无失败路径，恒为 `None`；
/// 保留该常量是为了让 `official::check_spec` 的分支与 Python 逐字对齐。
pub const REGISTRY_ERROR: Option<&str> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_keys_basic() {
        assert_eq!(
            template_keys("{freq}_D{di}N{n}M{m}TH{th}_ADTMV230603"),
            vec!["di", "m", "n", "th"]
        );
        assert_eq!(
            template_keys("{freq}_D{di}MA{ma_type}#{timeperiod}_第二买卖点V240524"),
            vec!["di", "ma_type", "timeperiod"]
        );
        assert!(template_keys("").is_empty());
        assert!(template_keys("{freq}").is_empty());
        assert!(template_keys("{freq}_{").is_empty());
    }

    #[test]
    fn registry_nonempty_and_split() {
        let n = REGISTRY.len();
        let kline = REGISTRY.values().filter(|r| r.category == "kline").count();
        let trader = REGISTRY.values().filter(|r| r.category == "trader").count();
        assert!(n > 0, "注册表为空：inventory 未链接，检查 czsc-signals 是否被直接引用");
        assert_eq!((kline, trader), (222, 24));
        assert_eq!(n, 246);
    }
}
