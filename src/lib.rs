//! tv 服务库：业务模块在这里注册；二进制入口只剩 src/main.rs 的启动编排。
//!
//! 为什么拆：集成测试（tests/api_contract.rs）要直接调 router::dispatch，
//! 而 bin crate 的私有模块对集成测试不可见。拆成 lib 后测试不用起服务、不占端口。

pub mod api;
pub mod bars;
pub mod board;
pub mod cache;
pub mod catalog;
pub mod chan;
pub mod cli;
pub mod concepts;
pub mod daymerge;
pub mod drawings;
pub mod heat;
pub mod ind;
pub mod indices;
pub mod live;
pub mod overview;
pub mod paths;
pub mod pinyin;
pub mod router;
pub mod scan;
pub mod static_files;
pub mod symbols;
pub mod tags;
pub mod tdxmac;

/// 监听端口。`router.rs` 的 `/api/health` 要读它（`crate::PORT`），
/// 所以它必须跟着模块一起进 lib；留在 `main.rs` 里会编译不过。
pub const PORT: u16 = 8790;
