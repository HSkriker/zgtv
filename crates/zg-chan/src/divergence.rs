//! `divergence.py` 的 Rust 移植：背驰与盘整背驰力度比较（参考口径，description only）。

use czsc_core::analyze::CZSC;
use czsc_core::objects::bi::BI;
use czsc_core::objects::zs::ZS;
use serde::{Deserialize, Serialize};

use crate::common::{dir_name, py_sum};
use crate::macd::{macd_at, segment_macd_area, MacdFrame};
use crate::zhongshu::{classify_trend, valid_zs_list};

/// 来源标注（`/api/czsc` sources.divergence 用）。
pub const DIV_SOURCE: &str = "reference";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DivergenceResult {
    pub divergence_type: String,
    pub is_divergence: bool,
    pub level: String,
    pub macd_confirm: Option<bool>,
    pub detail: String,
    pub power_bc: Option<bool>,
    pub macd_bc: Option<bool>,
    pub tracks: String,
}

impl DivergenceResult {
    /// Python 侧「无背驰」的原因一律放在 `level` 字段，detail 留空
    /// （`DivergenceResult("无背驰", False, <level>, None)` 的位置参数）。
    fn no(reason: impl Into<String>) -> Self {
        Self {
            divergence_type: "无背驰".to_string(),
            is_divergence: false,
            level: reason.into(),
            macd_confirm: None,
            detail: String::new(),
            power_bc: None,
            macd_bc: None,
            tracks: String::new(),
        }
    }
}

fn txt(x: Option<bool>) -> &'static str {
    match x {
        Some(true) => "成立",
        Some(false) => "不成立",
        None => "不可得",
    }
}

/// 两轨各算并报，合取才记背驰。
fn two_tracks(power_bc: Option<bool>, macd_bc: Option<bool>) -> (bool, String) {
    if power_bc.is_none() && macd_bc.is_none() {
        return (false, "均不可得（数据不足）⇒ 不记背驰".to_string());
    }
    if power_bc.is_none() || macd_bc.is_none() {
        return (
            false,
            format!(
                "仅一轨可得（力度轨{}/MACD轨{}，另一轨数据不足）⇒ 不作背驰结论",
                txt(power_bc),
                txt(macd_bc)
            ),
        );
    }
    if power_bc == macd_bc {
        let v = power_bc.unwrap_or(false);
        return (
            v,
            format!(
                "一致（均{}）⇒ {}",
                txt(power_bc),
                if v { "记背驰" } else { "不记背驰" }
            ),
        );
    }
    (
        false,
        format!(
            "不一致（力度轨{}/MACD轨{}）⇒ 不记背驰",
            txt(power_bc),
            txt(macd_bc)
        ),
    )
}

/// 后/前比值，纯描述文本。prev<=0 时不给比值（避免除零与无意义大数）。
fn ratio_txt(last: f64, prev: f64) -> String {
    if prev > 0.0 {
        format!("后/前={:.3}", last / prev)
    } else {
        "前段力度为0，比值不适用".to_string()
    }
}

fn find_bi_index(bis: &[BI], dt: chrono::DateTime<chrono::Utc>, side: &str, reverse: bool) -> Option<usize> {
    let exact = |b: &BI| {
        if side == "start" {
            b.start_dt() == dt
        } else {
            b.end_dt() == dt
        }
    };
    let contains = |b: &BI| b.start_dt() <= dt && dt <= b.end_dt();

    let idxs: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..bis.len()).rev())
    } else {
        Box::new(0..bis.len())
    };
    for i in idxs {
        if exact(&bis[i]) {
            return Some(i);
        }
    }
    let idxs: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..bis.len()).rev())
    } else {
        Box::new(0..bis.len())
    };
    idxs.into_iter().find(|i| contains(&bis[*i]))
}

/// 判断当前走势的背驰情况。
pub fn analyze_divergence(
    c: &CZSC,
    zss: Option<&[ZS]>,
    trend: Option<&crate::zhongshu::Trend>,
) -> DivergenceResult {
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };
    let bis: &[BI] = &c.bi_list;
    if zss.is_empty() || bis.len() < 4 {
        return DivergenceResult::no("无中枢或笔不足");
    }

    let bars: Vec<(chrono::DateTime<chrono::Utc>, f64)> =
        c.bars_raw.iter().map(|b| (b.dt, b.close)).collect();
    let df = crate::macd::frame_from_bars(&bars);

    let bi_powers: Vec<f64> = bis.iter().map(|b| b.get_power()).collect();

    // 情况1：趋势背驰（投票只回答"是不是趋势"，方向标签跟被比较窗口走）
    if zss.len() >= 2 {
        let tr_owned;
        let tr = match trend {
            Some(t) => t,
            None => {
                tr_owned = classify_trend(c, None);
                &tr_owned
            }
        };
        let z1 = &zss[zss.len() - 2];
        let z2 = &zss[zss.len() - 1];
        let last_pair_dir = if z2.zd > z1.zg {
            Some("上涨")
        } else if z2.zg < z1.zd {
            Some("下跌")
        } else {
            None
        };

        if tr.trend_type == "趋势" {
            if last_pair_dir != Some(tr.trend_direction.as_str()) {
                return DivergenceResult::no(format!(
                    "{}趋势已终结（末对中枢方向{}），不作趋势背驰比较",
                    tr.trend_direction,
                    last_pair_dir.unwrap_or("重叠")
                ));
            }

            let trend_up = tr.trend_direction == "上涨";
            let i1_start = find_bi_index(bis, z1.sdt, "start", false);
            let i1_end = find_bi_index(bis, z1.edt, "end", true);
            let i2_end = find_bi_index(bis, z2.edt, "end", true);

            if let (Some(i1s), Some(i1e), Some(i2e)) = (i1_start, i1_end, i2_end) {
                if i1s < i1e && i1e < i2e {
                    let n_prev = i1e - i1s + 1;
                    let n_last = i2e - i1e;
                    let power_prev = py_sum(&bi_powers[i1s..=i1e]) / (n_prev.max(1) as f64);
                    let power_last = py_sum(&bi_powers[i1e + 1..=i2e]) / (n_last.max(1) as f64);

                    let dname = if trend_up { "Up" } else { "Down" };
                    let macd_prev = segment_macd_area(&df, Some(z1.sdt), Some(z1.edt), Some(dname));
                    let macd_last = segment_macd_area(&df, Some(z1.edt), Some(z2.edt), Some(dname));

                    let power_bc = if power_prev > 0.0 {
                        Some(power_last < power_prev)
                    } else {
                        None
                    };
                    let macd_bc = match (macd_prev, macd_last) {
                        (Some(a), Some(b)) => Some(b < a),
                        _ => None,
                    };
                    let (verdict, tracks) = two_tracks(power_bc, macd_bc);
                    let direction = if trend_up { "上涨" } else { "下跌" };
                    let txt = format!(
                        "前段平均力度={:.2}/笔({}笔), 后段平均力度={:.2}/笔({}笔)；{}",
                        power_prev,
                        n_prev,
                        power_last,
                        n_last,
                        ratio_txt(power_last, power_prev)
                    );
                    return DivergenceResult {
                        divergence_type: if verdict { "趋势背驰" } else { "无背驰" }.to_string(),
                        is_divergence: verdict,
                        level: format!("{direction}趋势末段"),
                        macd_confirm: macd_bc,
                        detail: format!("两轨{tracks}。{txt}"),
                        power_bc,
                        macd_bc,
                        tracks,
                    };
                }
            }

            return DivergenceResult::no(format!(
                "{}趋势末段窗口定位失败",
                tr.trend_direction
            ));
        }
    }

    // 情况2：盘整背驰
    if let Some(zs) = zss.last() {
        if zs.bis.len() >= 3 {
            let bis_in_zs = &zs.bis;
            let b1 = &bis_in_zs[bis_in_zs.len() - 3];
            let b2 = &bis_in_zs[bis_in_zs.len() - 1];
            // 结构保证隔笔同向；Python 侧是 assert（永不触发），此处不重建分支。
            let p1 = b1.get_power();
            let p2 = b2.get_power();

            let dname = dir_name(b1.direction);
            let m1 = segment_macd_area(&df, Some(b1.start_dt()), Some(b1.end_dt()), Some(dname));
            let m2 = segment_macd_area(&df, Some(b2.start_dt()), Some(b2.end_dt()), Some(dname));

            let power_bc = if p1 > 0.0 { Some(p2 < p1) } else { None };
            let macd_bc = match (m1, m2) {
                (Some(a), Some(b)) => Some(b < a),
                _ => None,
            };
            let (verdict, tracks) = two_tracks(power_bc, macd_bc);
            let txt = format!(
                "前段力度={:.2}, 后段力度={:.2}；{}",
                p1,
                p2,
                ratio_txt(p2, p1)
            );
            return DivergenceResult {
                divergence_type: if verdict { "盘整背驰" } else { "无背驰" }.to_string(),
                is_divergence: verdict,
                level: "中枢内同向笔比较".to_string(),
                macd_confirm: macd_bc,
                detail: format!("两轨{tracks}。{txt}"),
                power_bc,
                macd_bc,
                tracks,
            };
        }
    }

    DivergenceResult::no("未发现背驰信号")
}

/// 背驰综合分析（供 pipeline 步骤6 使用）。S5 起只作描述。
#[derive(Clone, Debug, Serialize)]
pub struct DivergenceSummary {
    pub div_source: &'static str,
    pub divergence_type: String,
    pub is_divergence: bool,
    pub level: String,
    pub macd_confirm: Option<bool>,
    pub detail: String,
    pub power_bc: Option<bool>,
    pub macd_bc: Option<bool>,
    pub tracks: String,
    pub role: &'static str,
}

pub fn divergence_summary(
    c: &CZSC,
    zss: Option<&[ZS]>,
    trend: Option<&crate::zhongshu::Trend>,
) -> DivergenceSummary {
    let r = analyze_divergence(c, zss, trend);
    DivergenceSummary {
        div_source: DIV_SOURCE,
        divergence_type: r.divergence_type,
        is_divergence: r.is_divergence,
        level: r.level,
        macd_confirm: r.macd_confirm,
        detail: r.detail,
        power_bc: r.power_bc,
        macd_bc: r.macd_bc,
        tracks: r.tracks,
        role: "evidence_only",
    }
}

/// 反向定位用的辅助（供 `method` 层按端点取笔索引）。
pub fn bi_index_of(bis: &[BI], dt: chrono::DateTime<chrono::Utc>, side: &str, reverse: bool) -> Option<usize> {
    find_bi_index(bis, dt, side, reverse)
}

/// `macd_at` 的转发（避免 calling 处重复 import）。
pub fn macd_value(df: &MacdFrame, col: &str, dt: Option<chrono::DateTime<chrono::Utc>>) -> Option<f64> {
    macd_at(df, col, dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_text() {
        assert_eq!(two_tracks(None, None).1, "均不可得（数据不足）⇒ 不记背驰");
        assert_eq!(
            two_tracks(Some(true), None).1,
            "仅一轨可得（力度轨成立/MACD轨不可得，另一轨数据不足）⇒ 不作背驰结论"
        );
        assert_eq!(two_tracks(Some(true), Some(true)), (true, "一致（均成立）⇒ 记背驰".to_string()));
        assert_eq!(
            two_tracks(Some(true), Some(false)).1,
            "不一致（力度轨成立/MACD轨不成立）⇒ 不记背驰"
        );
    }

    #[test]
    fn ratio() {
        assert_eq!(ratio_txt(1.0, 2.0), "后/前=0.500");
        assert_eq!(ratio_txt(1.0, 0.0), "前段力度为0，比值不适用");
    }
}
