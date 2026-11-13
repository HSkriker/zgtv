//! MACD 与面积计算 —— `divergence._calc_macd_from_df` / `_segment_macd_area` 的 Rust 版。
//!
//! 数值等价要点（实测对标，见工单附录）：
//!   - pandas `ewm(span, adjust=False).mean()`：`y0 = x0`，
//!     `y_i = alpha*x_i + (1-alpha)*y_{i-1}`，`alpha = 2/(span+1)`；
//!   - `Series.abs().sum()`：numpy **成对求和**，不是朴素累加（1 ulp 会翻背驰结论），
//!     故复用 [`crate::common::pairwise_sum`]。

use chrono::{DateTime, Utc};

/// 与 `_calc_macd_from_df` 产出的 DataFrame 同列：dt/close/dif/dea/macd。
#[derive(Clone, Debug, Default)]
pub struct MacdFrame {
    pub dt: Vec<DateTime<Utc>>,
    pub close: Vec<f64>,
    pub dif: Vec<f64>,
    pub dea: Vec<f64>,
    pub macd: Vec<f64>,
}

impl MacdFrame {
    pub fn is_empty(&self) -> bool {
        self.dt.is_empty()
    }

    pub fn len(&self) -> usize {
        self.dt.len()
    }
}

fn ewm_span(xs: &[f64], span: usize) -> Vec<f64> {
    let alpha = 2.0 / (span as f64 + 1.0);
    let mut out = Vec::with_capacity(xs.len());
    if xs.is_empty() {
        return out;
    }
    out.push(xs[0]);
    for i in 1..xs.len() {
        let prev = out[i - 1];
        out.push(xs[i] * alpha + prev * (1.0 - alpha));
    }
    out
}

/// 收盘价序列 → MACD（dif/dea/macd 三列）。
pub fn calc_macd(close: &[f64]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let ema12 = ewm_span(close, 12);
    let ema26 = ewm_span(close, 26);
    let dif: Vec<f64> = ema12.iter().zip(&ema26).map(|(a, b)| a - b).collect();
    let dea = ewm_span(&dif, 9);
    let macd: Vec<f64> = dif.iter().zip(&dea).map(|(d, e)| 2.0 * (d - e)).collect();
    (dif, dea, macd)
}

/// `c.bars_raw` → MACD 表（等价 `c.bars_raw_df` + `_calc_macd_from_df`）。
pub fn frame_from_bars(bars: &[(DateTime<Utc>, f64)]) -> MacdFrame {
    let close: Vec<f64> = bars.iter().map(|(_, c)| *c).collect();
    let (dif, dea, macd) = calc_macd(&close);
    MacdFrame {
        dt: bars.iter().map(|(d, _)| *d).collect(),
        close,
        dif,
        dea,
        macd,
    }
}

/// `_segment_macd_area`：区间 [sdt, edt] 内 MACD 柱面积（绝对值求和，可选同号过滤）。
///
/// 返回 `None` = 区间内无数据（Python `seg.empty`）。
// Python `m[m > 0]` / `m[m < 0]`：NaN 比较恒 False ⇒ 被排除。
// clippy 会建议改成 `<=`/`>=`，那会把 NaN 放进来，破坏与 pandas 的等价性。
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn segment_macd_area(
    df: &MacdFrame,
    sdt: Option<DateTime<Utc>>,
    edt: Option<DateTime<Utc>>,
    direction: Option<&str>,
) -> Option<f64> {
    let (Some(s), Some(e)) = (sdt, edt) else {
        return None;
    };
    // 注意：Python 只在**区间本身为空**时返回 None；方向过滤后为空是合法的
    // 0.0（`Series([]).sum() == 0.0`）。这两者混同会让 MACD 轨凭空「不可得」。
    let mut n_rows = 0usize;
    let mut acc: Vec<f64> = Vec::new();
    for i in 0..df.len() {
        let dt = df.dt[i];
        if dt < s || dt > e {
            continue;
        }
        n_rows += 1;
        let m = df.macd[i];
        match direction {
            Some("Up") if !(m > 0.0) => continue,
            Some("Down") if !(m < 0.0) => continue,
            _ => {}
        }
        acc.push(m.abs());
    }
    if n_rows == 0 {
        return None;
    }
    Some(crate::common::pairwise_sum(&acc))
}

/// `_macd_at(df, col, dt)`：读 dt 当根（含）之前的最后一个指标值。
pub fn macd_at(df: &MacdFrame, col: &str, dt: Option<DateTime<Utc>>) -> Option<f64> {
    let dt = dt?;
    let idx = df.dt.iter().rposition(|d| *d <= dt)?;
    let v = match col {
        "dif" => df.dif[idx],
        "dea" => df.dea[idx],
        "macd" => df.macd[idx],
        "close" => df.close[idx],
        _ => return None,
    };
    if v.is_nan() {
        None
    } else {
        Some(v)
    }
}

/// `_zero_cross`：(sdt, edt] 内 DIF 是否由负转正（窗口前一根参与比较）。
pub fn zero_cross(
    df: &MacdFrame,
    sdt: Option<DateTime<Utc>>,
    edt: Option<DateTime<Utc>>,
) -> Option<bool> {
    let (s, e) = (sdt?, edt?);
    if s > e {
        return None;
    }
    // searchsorted(side="left") / (side="right")
    let i0 = df.dt.iter().position(|d| *d >= s)?;
    let i1 = df.dt.iter().rposition(|d| *d <= e)? + 1;
    if i1 <= i0 {
        return None;
    }
    let lo = i0.saturating_sub(1);
    let d = &df.dif[lo..i1];
    for w in d.windows(2) {
        if w[0] < 0.0 && w[1] >= 0.0 {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewm_matches_manual() {
        let x = [1.0, 2.0, 3.0];
        let y = ewm_span(&x, 9);
        let a = 2.0 / 10.0;
        assert_eq!(y[0], 1.0);
        assert_eq!(y[1], 2.0 * a + 1.0 * (1.0 - a));
        assert_eq!(y[2], 3.0 * a + y[1] * (1.0 - a));
    }

    #[test]
    fn empty_series() {
        let (dif, dea, macd) = calc_macd(&[]);
        assert!(dif.is_empty() && dea.is_empty() && macd.is_empty());
    }
}
