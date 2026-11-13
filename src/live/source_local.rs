//! 本地通达信数据（vipdoc 日线 .day / 分钟 .lc1）读取：先前复权再出序列。

use super::*;

/// 本地日/周/月/季/年（不走网络）。`force_stale` 用于远端全失败后的兜底。
pub(super) fn local_day_series(code: &str, period: &str, lmt: usize, force_stale: bool)
    -> Result<Series, String>
{
    let (mkt, cd) = zg_core::reader::resolve(code, None).map_err(|e| e.to_string())?;
    let mut rows: Vec<Agg> = match period {
        "day" | "week" | "month" => zg_core::api::chart_bars(&cd, period, lmt, None, None)
            .map_err(|e| e.to_string())?
            .iter()
            .map(|b| Agg {
                date: b.date.clone(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                vol: b.vol,
                amount: b.amount,
            })
            .collect(),
        "quarter" | "year" => {
            let daily = zg_core::api::chart_bars(&cd, "day", 0, None, None)
                .map_err(|e| e.to_string())?;
            let t: Vec<Row7> = daily
                .iter()
                .map(|b| (b.date.clone(), b.open, b.high, b.low, b.close, b.vol, b.amount))
                .collect();
            let mut q = resample_qy(&t, period);
            if lmt > 0 && q.len() > lmt {
                q = q[q.len() - lmt..].to_vec();
            }
            q
        }
        other => return Err(format!("不支持的周期: {other}")),
    };
    if rows.is_empty() {
        return Err("本地日线为空".to_string());
    }
    let as_of = rows.last().map(|b| b.date.clone()).unwrap_or_default();
    let stale = force_stale || date_lt(&as_of, &reference_day());
    if lmt > 0 && rows.len() > lmt && !is_qy(period) {
        rows = rows[rows.len() - lmt..].to_vec();
    }
    let _ = mkt;
    Ok(Series { rows, source: "local".to_string(), detail: String::new(), as_of, stale })
}

/// 本地分钟：volume 原始为股，按个股 ÷100 转手；指数保持原值。
pub(super) fn local_min_to_agg(bars: &[zg_core::Bar], is_stock: bool) -> Vec<Agg> {
    bars.iter()
        .map(|b| Agg {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: if is_stock { b.volume / 100.0 } else { b.volume },
            amount: b.amount,
        })
        .collect()
}
