//! `zgbridge.analyze` 的 Rust 移植：协议载荷组装（`_assemble_payload`）。
//!
//! 计算顺序照抄 Python（三者共享同一份 `valid_zs_list`）：
//!   zss = valid_zs_list(c)
//!   bs  = buy_sell_summary(c, zss)
//!   zs  = zhongshu_summary(c, zss)
//!   dv  = divergence_summary(c, zss, trend=zs.trend)
//!
//! 字段裁剪也照抄：`bis` 只留 5 键、`zs` 只留 6 键、`bs` 留 15 键，
//! `sources` 八项固定。**不要**顺手把内部结构整块序列化出去 —— 前端按这些
//! 键名取值，多一个少一个都算协议破坏。

use std::time::Instant;

use czsc_core::analyze::CZSC;
use czsc_core::objects::bar::RawBar;
use serde_json::{json, Map, Value};

use crate::common::{dir_name, fmt_dt, py_round};
use crate::divergence::{divergence_summary, DIV_SOURCE};
use crate::points::{buy_sell_summary, point_dt_str, ACTION_SOURCE, POINTS_SOURCE};
use crate::service::{build_czsc, czsc_from_inline, InlineBar};
use crate::zhongshu::{valid_zs_list, STATE_SOURCE, TREND_SOURCE, ZS_SOURCE};

/// `czsc.__version__`（P0 冻结 1.0.2）。
pub const CZSC_VERSION: &str = "1.0.2";
/// `sources.method`：L2/L3 全是参考口径。
pub const METHOD_SOURCE_REF: &str = "reference";

fn r3v(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(py_round(x, 3)),
        None => Value::Null,
    }
}

/// `_assemble_payload(c, bars, freq, cd, mkt, t0)`。
pub fn assemble(
    c: &CZSC,
    bars: &[RawBar],
    freq: &str,
    cd: &str,
    mkt: &str,
    elapsed_ms: i64,
) -> Value {
    let intraday = freq.to_uppercase().starts_with('F');

    let zss = valid_zs_list(c, None);
    let bs = buy_sell_summary(c, Some(&zss));
    let zs = crate::zhongshu::zhongshu_summary(c, Some(&zss));
    let dv = divergence_summary(c, Some(&zss), Some(&zs.trend));

    let bis: Vec<Value> = c
        .bi_list
        .iter()
        .map(|b| {
            json!({
                "sdt": fmt_dt(&b.start_dt(), intraday),
                "edt": fmt_dt(&b.end_dt(), intraday),
                "sd": dir_name(b.direction),
                "high": py_round(b.get_high(), 3),
                "low": py_round(b.get_low(), 3),
            })
        })
        .collect();

    let zss_out: Vec<Value> = zs
        .zhongshu_list
        .iter()
        .map(|z| {
            json!({
                "sdt": fmt_dt(&z.sdt, intraday),
                "edt": fmt_dt(&z.edt, intraday),
                "zd": py_round(z.zd, 3),
                "zg": py_round(z.zg, 3),
                "bis": z.bi_count as i64,
                "state": z.state,
            })
        })
        .collect();

    let points: Vec<Value> = bs
        .points
        .iter()
        .map(|p| {
            json!({
                "point_type": p.point_type,
                "direction": p.direction,
                "operate": p.operate,
                "price": r3v(p.price),
                "ref_price": r3v(p.ref_price),
                "ref_price_basis": p.ref_price_basis,
                "dt": point_dt_str(p.dt, intraday),
                "confirm_dt": point_dt_str(p.confirm_dt, intraday),
                "signal_name": p.signal_name,
                "in_finished_bis": p.in_finished_bis,
                "method_label": p.method_label,
                "qualification": Value::Object(p.qualification.clone()),
                "stop_ref": Value::Object(p.stop_ref.clone()),
                "level": p.level,
                "source_method": p.source_method,
            })
        })
        .collect();

    let last = bars.last();
    let bars_obj = json!({
        "n": bars.len() as i64,
        "last": match last {
            Some(b) => json!({"dt": fmt_dt(&b.dt, intraday), "close": py_round(b.close, 3)}),
            None => Value::Null,
        },
    });

    let mut sources = Map::new();
    sources.insert("bis".into(), json!(format!("czsc{CZSC_VERSION}")));
    sources.insert("zs".into(), json!(ZS_SOURCE));
    sources.insert("state".into(), json!(STATE_SOURCE));
    sources.insert("trend".into(), json!(TREND_SOURCE));
    sources.insert("divergence".into(), json!(DIV_SOURCE));
    sources.insert("points".into(), json!(POINTS_SOURCE));
    sources.insert("action_text".into(), json!(ACTION_SOURCE));
    sources.insert("method".into(), json!(METHOD_SOURCE_REF));

    json!({
        "ok": true,
        "ver": CZSC_VERSION,
        "code": cd,
        "market": mkt,
        "freq": freq.to_uppercase(),
        "elapsed_ms": elapsed_ms,
        "bars": bars_obj,
        "bis": bis,
        "zs": zss_out,
        "bs": points,
        "trend": zs.trend,
        "divergence": dv,
        "sources": Value::Object(sources),
    })
}

/// `build_payload(code, freq, market)`：code → 协议载荷（本地 .day 取数）。
pub fn build_payload(code: &str, freq: &str, market: Option<&str>) -> Result<Value, String> {
    let t0 = Instant::now();
    let (mkt, cd) = zg_core::reader::resolve(code, market).map_err(|e| e.to_string())?;
    let (c, bars) = build_czsc(&cd, freq, Some(&mkt))?;
    let ms = t0.elapsed().as_millis() as i64;
    Ok(assemble(&c, &bars, freq, &cd, &mkt, ms))
}

/// `build_payload_from_bars`：内联 bars → 协议载荷（不取数、不复权、不重采样）。
pub fn build_payload_from_bars(
    code: &str,
    freq: &str,
    bars: &[InlineBar],
    market: Option<&str>,
) -> Result<Value, String> {
    let t0 = Instant::now();
    let (c, raw, cd, mkt) = czsc_from_inline(code, freq, bars, market)?;
    let ms = t0.elapsed().as_millis() as i64;
    Ok(assemble(&c, &raw, freq, &cd, &mkt, ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::MIN_BARS;

    #[test]
    fn inline_too_few_bars_message() {
        let bars: Vec<InlineBar> = (0..10)
            .map(|i| InlineBar {
                date: format!("2026-01-{:02}", i + 1),
                open: 20.0,
                close: 20.0,
                high: 20.0,
                low: 20.0,
                vol: 1.0,
                amount: 1.0,
            })
            .collect();
        let err = build_payload_from_bars("600519", "D", &bars, None).unwrap_err();
        assert_eq!(err, format!("内联数据不足{MIN_BARS}根: 10"));
    }

    #[test]
    fn r3_rounding() {
        assert_eq!(r3v(Some(94.80000000000001)), json!(94.8));
        assert_eq!(r3v(None), Value::Null);
    }

}
