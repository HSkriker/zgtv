//! 拥挤度五分量合成：c1 成交额份额、c2 平均振幅、c3 滚动 β、c4 量价相关、c5 乖离率，
//! 以及 252 日信息比率 IR。分位窗口与覆盖率门禁的口径注释在各函数上，原样迁移。

use super::*;

fn percentile_of(hist: &[f64], cur: f64) -> Option<f64> {
    if hist.len() < 250 {
        return None;
    }
    let below = hist.iter().filter(|x| **x < cur).count();
    Some(below as f64 / hist.len() as f64 * 100.0)
}

/// 第 k 天的历史分位（窗口 ≤win 含当日、≥250 有效样本）；旧 day_crowd 的一般化，
/// 窗口锚定在 k（point-in-time），回放逐日回算与末日语义一致
fn pct_at(v: &[Option<f64>], k: usize, win: usize) -> Option<f64> {
    let cur = v.get(k).copied().flatten()?;
    let from = k.saturating_sub(win);
    let hist: Vec<f64> = v[from..=k].iter().filter_map(|x| *x).collect();
    percentile_of(&hist, cur)
}

/// 对齐轴上的日收益率（缺任一日为 None）
fn day_ret(c: &[Option<f64>], k: usize) -> Option<f64> {
    if k == 0 {
        return None;
    }
    match (c[k], c[k - 1]) {
        (Some(x), Some(p)) if p > 0.0 => Some(x / p - 1.0),
        _ => None,
    }
}

fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    let m = pairs.len() as f64;
    if m < 2.0 {
        return None;
    }
    let mx = pairs.iter().map(|p| p.0).sum::<f64>() / m;
    let my = pairs.iter().map(|p| p.1).sum::<f64>() / m;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in pairs {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    if sxx > 1e-12 && syy > 1e-12 {
        Some(sxy / (sxx.sqrt() * syy.sqrt()))
    } else {
        None
    }
}

/// OLS β = cov(x,y)/var(y)，x 为板块收益、y 为基准收益
fn ols_beta(pairs: &[(f64, f64)]) -> Option<f64> {
    let m = pairs.len() as f64;
    if m < 2.0 {
        return None;
    }
    let mx = pairs.iter().map(|p| p.0).sum::<f64>() / m;
    let my = pairs.iter().map(|p| p.1).sum::<f64>() / m;
    let (mut cov, mut var) = (0.0, 0.0);
    for (x, y) in pairs {
        cov += (x - mx) * (y - my);
        var += (y - my) * (y - my);
    }
    if var > 1e-12 {
        Some(cov / var)
    } else {
        None
    }
}

/// 五分量原始值序列（一次算全程；分位读取时现算）
/// cov：该日有 bar 的行业家数；cov_full：轴上满覆盖日的覆盖家数。
/// c1 的分母是「当日碰巧有数的那些行业成交额之和」，缺数行业不会出现在分母里，
/// 于是其余行业的份额被整体抬高。一天的覆盖掉到满覆盖的 9/10 以下（半边市场没下载/
/// 同步中断）时该日份额不可比，整日置空；掉一两家（实测 127/128）落在容差内、
/// 相对偏差不足一个行业份额，不必为此牺牲分位窗口的样本数。
pub(super) fn build_comps(s: &Series, bench: &[Option<f64>], total_amt: &[f64], cov: &[usize], cov_full: usize) -> CrowdComps {
    let n = s.c.len();
    let mut share = vec![None; n];
    let mut amp20 = vec![None; n];
    let mut beta60 = vec![None; n];
    let mut corr20 = vec![None; n];
    let mut bias20 = vec![None; n];
    for k in 0..n {
        // 当日无 bar 时所有分量一律 None：窗口允许 15/20、40/60 的缺失，
        // 否则会把「缺当日」的行业拿旧数据算成当日拥挤（旧版 vol_crowd_pct 的 s.fresh 门禁）。
        if s.c[k].is_none() {
            continue;
        }
        let cov_ok = cov_full == 0 || cov[k] * COV_TOL_DEN >= cov_full * COV_TOL_NUM;
        if cov_ok {
            if let (Some(a), tot) = (s.a[k], total_amt[k]) {
                if tot > 0.0 && a > 0.0 {
                    share[k] = Some(a / tot);
                }
            }
        }
        if k + 1 >= AMP_WIN {
            let vals: Vec<f64> = (k + 1 - AMP_WIN..=k)
                .filter_map(|j| match (s.h[j], s.l[j], s.c[j]) {
                    (Some(h), Some(l), Some(c)) if c > 0.0 => Some((h - l) / c),
                    _ => None,
                })
                .collect();
            if vals.len() >= AMP_WIN * 3 / 4 {
                amp20[k] = Some(vals.iter().sum::<f64>() / vals.len() as f64);
            }
        }
        if k + 1 >= MA_WIN {
            if let Some(ck) = s.c[k] {
                let vals: Vec<f64> = (k + 1 - MA_WIN..=k).filter_map(|j| s.c[j]).collect();
                if vals.len() >= MA_WIN * 3 / 4 {
                    let ma = vals.iter().sum::<f64>() / vals.len() as f64;
                    if ma > 0.0 {
                        bias20[k] = Some(ck / ma - 1.0);
                    }
                }
            }
        }
        if k + 1 >= BETA_WIN {
            let pairs: Vec<(f64, f64)> = (k + 1 - BETA_WIN..=k)
                .filter_map(|j| Some((day_ret(&s.c, j)?, day_ret(bench, j)?)))
                .collect();
            if pairs.len() >= BETA_WIN * 2 / 3 {
                beta60[k] = ols_beta(&pairs);
            }
        }
        if k + 1 >= CORR_WIN {
            let pairs: Vec<(f64, f64)> = (k + 1 - CORR_WIN..=k)
                .filter_map(|j| {
                    if j == 0 {
                        return None;
                    }
                    let da = match (s.a[j], s.a[j - 1]) {
                        (Some(x), Some(p)) if p > 0.0 => Some(x / p - 1.0),
                        _ => None,
                    };
                    Some((day_ret(&s.c, j)?, da?))
                })
                .collect();
            if pairs.len() >= CORR_WIN * 3 / 4 {
                corr20[k] = pearson(&pairs);
            }
        }
    }
    CrowdComps { share, amp20, beta60, corr20, bias20 }
}

/// 第 k 天五分量分位 [c1..c5]
pub(super) fn crowd_parts(comps: &CrowdComps, k: usize) -> [Option<f64>; 5] {
    [
        pct_at(&comps.share, k, CROWD_WIN),
        pct_at(&comps.amp20, k, CROWD_WIN),
        pct_at(&comps.beta60, k, CROWD_WIN),
        pct_at(&comps.corr20, k, CROWD_WIN),
        pct_at(&comps.bias20, k, CROWD_WIN),
    ]
}

/// 合成拥挤：可用分量等权均值；<COMP_MIN 个可用为 null（研报共识：多分量等权分位合成，
/// 国盛=换手/波动/β 比率，华泰=动量/流动性/波动/量价相关，中金/兴业=多维分位等权）
pub(super) fn crowd_comp_of(parts: &[Option<f64>; 5]) -> Option<f64> {
    let vals: Vec<f64> = parts.iter().filter_map(|x| *x).collect();
    if vals.len() < COMP_MIN {
        return None;
    }
    Some(vals.iter().sum::<f64>() / vals.len() as f64)
}

/// 信息比率序列：IR_WIN 日对基准的日均超额 ÷ 超额标准差（国盛趋势度口径，比原始动量稳）
pub(super) fn ir_series(c: &[Option<f64>], bench: &[Option<f64>]) -> Vec<Option<f64>> {
    let n = c.len();
    let mut out = vec![None; n];
    for k in IR_WIN..n {
        let ex: Vec<f64> = (k + 1 - IR_WIN..=k)
            .filter_map(|j| Some(day_ret(c, j)? - day_ret(bench, j)?))
            .collect();
        if ex.len() < IR_WIN * 2 / 3 {
            continue;
        }
        let m = ex.len() as f64;
        let mu = ex.iter().sum::<f64>() / m;
        let var = ex.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / m;
        if var > 1e-16 {
            out[k] = Some(mu / var.sqrt());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_series(n: usize, value: f64) -> Series {
        Series {
            c: vec![Some(value); n],
            h: vec![Some(value * 1.1); n],
            l: vec![Some(value * 0.9); n],
            a: vec![Some(100.0); n],
        }
    }

    #[test]
    fn missing_current_bar_blocks_crowd_components() {
        let n = 300;
        let mut s = flat_series(n, 10.0);
        s.c[n - 1] = None;
        s.h[n - 1] = None;
        s.l[n - 1] = None;
        s.a[n - 1] = None;
        let bench = vec![Some(100.0); n];
        let total = vec![128.0 * 100.0; n];
        let cov = vec![128usize; n];
        let comps = build_comps(&s, &bench, &total, &cov, 128);

        assert!(comps.share[n - 1].is_none());
        assert!(comps.amp20[n - 1].is_none());
        assert!(comps.beta60[n - 1].is_none());
        assert!(comps.corr20[n - 1].is_none());
        assert!(comps.bias20[n - 1].is_none());
        assert!(crowd_comp_of(&crowd_parts(&comps, n - 1)).is_none());
        // 门禁只作用于缺当日 bar 的那一天，不能误杀前一日
        assert!(comps.amp20[n - 2].is_some());
    }

    #[test]
    fn c1_share_requires_coverage() {
        let n = 300;
        let s = flat_series(n, 10.0);
        let bench = vec![Some(100.0); n];
        // 末根只剩 100/128 家（同步中断、半边市场没落地）：分母不可比，c1 整日置空
        let cov = vec![100usize; n];
        let comps = build_comps(&s, &bench, &vec![100.0 * 100.0; n], &cov, 128);
        assert!(comps.share[n - 1].is_none(), "覆盖 100/128 低于 9/10 门槛");
        // 127/128 是实测常态缺口（近一年 174/260 天），偏差不足一个行业份额，
        // 不为它牺牲分位窗口的样本数
        let comps_ok = build_comps(&s, &bench, &vec![127.0 * 100.0; n], &vec![127usize; n], 128);
        assert!(comps_ok.share[n - 1].is_some(), "127/128 应照常出数");
        // 只有依赖分母的 c1 受门禁，其余分量不得连带置空
        assert!(comps.amp20[n - 1].is_some());
        assert!(comps.bias20[n - 1].is_some());
    }
}
