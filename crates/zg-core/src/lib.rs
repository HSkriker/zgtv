//! zg-core —— `modules/zgcore` 的 Rust 移植（P1：数据层，逐值等价）。
//!
//! 分层：
//!   paths/jsonio/locks/net/calendar   基础设施
//!   reader/resample/adjust/events/kcache/api  行情数据层（.day/.lc1/复权/重采样/缓存）
//!
//! 约束：本 crate **不依赖 czsc**（缠论在 P2 的 zg-chan）。

pub mod adjust;
pub mod api;
pub mod calendar;
pub mod error;
pub mod events;
pub mod gbbq;
pub mod jsonio;
pub mod kcache;
pub mod locks;
pub mod model;
pub mod net;
pub mod paths;
pub mod reader;
pub mod resample;
pub mod sync;

pub use error::{py_repr, Result, ZgError};
pub use model::Bar;

/// 带用户可读文案的数据层错误（对齐 Python `zgcore.BridgeError`）。
pub type BridgeError = ZgError;

/// 日志一律走 stderr —— 桥的 stdout 是 JSON-lines 协议通道。
pub fn log(msg: &str) {
    eprintln!("[zgcore] {}", msg.trim_end_matches('\n'));
}

pub const VERSION: &str = "0.1.0";
