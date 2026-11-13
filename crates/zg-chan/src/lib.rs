//! zg-chan —— `modules/czsc_bridge` 的 Rust 移植（P2：缠论层）。
//!
//! 分层（与 Python 侧一一对应）：
//!   registry   czsc 1.0.2 注册表视图（category/param_template/namespace/param_keys）
//!   official   L1 官方判据入口（`signals_official.py`）：SPECS 表 + 参数钉死 + 词表解析
//!
//! 约束：本 crate 直接依赖 `czsc-* =1.0.2`（P0.5 已验证与 Python czsc 1.0.2 同源），
//! **不得**自建判据、不得"优化"上游语义；所有口径以 Python oracle 为准。

pub mod common;
pub mod divergence;
pub mod first_buy;
pub mod macd;
pub mod method;
pub mod official;
pub mod payload;
pub mod points;
pub mod probe;
pub mod protocol;
pub mod registry;
pub mod scan;
pub mod service;
pub mod zhongshu;

pub const VERSION: &str = "0.1.0";

/// 方法论版本号单一真相源（M13）：缓存新鲜度判定与响应字段都引用这里，
/// 避免 method / points / zg-api 三处硬编码单边升级后读到旧缓存。
pub const CZSC_METHOD_VER: i64 = 2;
