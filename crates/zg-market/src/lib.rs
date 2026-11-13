//! zg-market：行情层（P3）。
//!
//! 移植源：`modules/zgapp/{kline_sources,market_data,mac_source,quotes}.py` +
//! `modules/market/{tq_local,tdx_backup,industry_heat,industry_intel,fetch_boards}.py`。
//! 移植纪律同 P1/P2：分支顺序、比较运算、round 位置照抄 Python，不许「顺手优化」。

pub mod candles;
pub mod canon;
pub mod consts;
pub mod indices;
pub mod industry;
pub mod kbar;
pub mod kbuild;
pub mod kline;
pub mod live;
pub mod mac;
pub mod market_data;
pub mod net_stub;
pub mod quotes;
pub mod stock_list;
pub mod tdx_backup;
pub mod ths;
pub mod tq;
