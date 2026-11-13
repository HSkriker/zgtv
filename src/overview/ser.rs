//! 界面载荷组装（JSON 字段在这里定形）与回放 CSV（对账脚本的独立口径）。

use super::*;
use super::crowd::{crowd_comp_of, crowd_parts};
use super::grid::compute_ctx;
use super::rps::breadth;
use serde_json::json;

pub(super) fn build_payload() -> Result<Value, String> {
    let ctx = compute_ctx()?;
    let n = ctx.n;
    let as_of = ctx.axis[n - 1].clone();
    // compute_ctx 已要求基准轴 ≥ FULL_HIST(140) 根，故 t ≥ 139，回看 20 日无需再判下界
    let t = n - 1;
    let mut rows = Vec::new();
    for (i, s) in ctx.secs.iter().enumerate() {
        let closes = &ctx.series[i].c;
        let amts = &ctx.series[i].a;

        let parts = crowd_parts(&ctx.comps[i], t);
        let crowd = parts[0];     // 与旧 crowd_pct 同口径（成交额占比分位）
        let vol_crowd = parts[1]; // 与旧 crowd_vol_pct 同含义（振幅分位，改走对齐轴）
        let crowd_comp = crowd_comp_of(&parts);
        let rk6 = ctx.ranks[i][t];
        let rk20 = ctx.ranks[i][t - 20];
        let rkc = ctx.ranksc[i][t];
        let rkc20 = ctx.ranksc[i][t - 20];
        let rk_ir = ctx.ranks_ir[i][t];
        let rk_ir20 = ctx.ranks_ir[i][t - 20];
        let rs_comp = ctx.rs_comp(i, t);
        let rs90 = rs90_of(rs_comp);

        let amt_x = match amts[t] {
            Some(a) => {
                let prev: Vec<f64> =
                    (t.saturating_sub(20)..t).filter_map(|k| amts[k]).collect();
                if prev.len() >= 15 {
                    Some(a / (prev.iter().sum::<f64>() / prev.len() as f64))
                } else {
                    None
                }
            }
            None => None,
        };

        // 相对基准超额（百分点）与连续跑赢天数
        let rs20 = rs_at(closes, &ctx.bench_c, t, 20);
        let rs63 = rs_at(closes, &ctx.bench_c, t, 63);
        let rs252 = rs_at(closes, &ctx.bench_c, t, 252);
        // RS252 的 20 日位移（百分点）：双窗模式下作加速度外圈，口径同 rank6_d20（同为 t 与 t−20 之差）
        let rs252_20 = rs_at(closes, &ctx.bench_c, t - 20, 252);
        let mut streak = 0usize;
        let mut k = t;
        while k > 0 && k > t - 60 {
            let (cc, cp) = match (closes[k], closes[k - 1]) {
                (Some(c), Some(p)) if p > 0.0 => (c, p),
                _ => break,
            };
            let (bc, bp) = match (ctx.bench_c[k], ctx.bench_c[k - 1]) {
                (Some(c), Some(p)) if p > 0.0 => (c, p),
                _ => break,
            };
            if cc / cp > bc / bp {
                streak += 1;
                k -= 1;
            } else {
                break;
            }
        }

        let hn = s.closes.len();
        let w = hn.min(250);
        let hi250 = s.highs[hn - w..].iter().fold(f64::MIN, |a, b| a.max(*b));
        let lo250 = s.lows[hn - w..].iter().fold(f64::MAX, |a, b| a.min(*b));

        // 散点轨迹：最近 TRAJ_DAYS 个交易日的逐日坐标（拥挤分量当日回算，point-in-time）
        let traj_quad: Vec<Value> = ((t + 1).saturating_sub(TRAJ_DAYS)..=t)
            .map(|k| {
                let pk = crowd_parts(&ctx.comps[i], k);
                let c = pk[0];
                let cc = crowd_comp_of(&pk);
                let a = rs_at(closes, &ctx.bench_c, k, 63);
                let b = rs_at(closes, &ctx.bench_c, k, 252);
                let (kk, q, ir) = (ctx.ranks[i][k], ctx.ranksc[i][k], ctx.ranks_ir[i][k]);
                if c.is_none() && cc.is_none() && kk.is_none() && q.is_none()
                    && a.is_none() && b.is_none() && ir.is_none()
                {
                    return Value::Null;
                }
                json!({
                    "c": c.map(r1).unwrap_or(Value::Null),
                    "cc": cc.map(r1).unwrap_or(Value::Null),
                    "k": kk.map(r1).unwrap_or(Value::Null),
                    "q": q.map(r1).unwrap_or(Value::Null),
                    "ir": ir.map(r1).unwrap_or(Value::Null),
                    "a": a.map(r1).unwrap_or(Value::Null),
                    "b": b.map(r1).unwrap_or(Value::Null),
                })
            })
            .collect();

        // 自身序列派生的动量/距高低点：未到 as_of 一律置空（对齐「算不出的指标留空」声明）
        let own_mom = |back: usize| {
            if s.fresh {
                mom(&s.closes, back).map(r2).unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        };

        rows.push(json!({
            "name": s.name,
            "code": s.code,
            "ref": s.ref_,
            "hist_bars": hn,
            "day_ret": own_mom(1),
            "mom": {
                "1M": own_mom(21),
                "3M": own_mom(63),
                "6M": own_mom(126),
                "9M": own_mom(189),
                "12M": own_mom(252),
            },
            "mom_rank6": rk6.map(r2).unwrap_or(Value::Null),
            "rank6_d20": d20(rk6, rk20).map(r2).unwrap_or(Value::Null),
            "mom_rank_c": rkc.map(r2).unwrap_or(Value::Null),
            "rankc_d20": d20(rkc, rkc20).map(r2).unwrap_or(Value::Null),
            "mom_rank_ir": rk_ir.map(r2).unwrap_or(Value::Null),
            "mom_rank_ir_d20": d20(rk_ir, rk_ir20).map(r2).unwrap_or(Value::Null),
            "rs20": rs20.map(r2).unwrap_or(Value::Null),
            "rs63": rs63.map(r2).unwrap_or(Value::Null),
            "rs252": rs252.map(r2).unwrap_or(Value::Null),
            "rs252_d20": d20(rs252, rs252_20).map(r2).unwrap_or(Value::Null),
            "rs_comp": rs_comp.map(r2).unwrap_or(Value::Null),
            "rs90": rs90,
            "streak_rel": if closes[t].is_none() { Value::Null } else { Value::from(streak as i64) },
            "amt_x": amt_x.map(r2).unwrap_or(Value::Null),
            "off_hi250": if s.fresh && w >= 60 && hn > 0 && hi250 > 0.0 {
                r2((s.closes[hn - 1] / hi250 - 1.0) * 100.0)
            } else { Value::Null },
            "off_lo250": if s.fresh && w >= 60 && hn > 0 && lo250 > 0.0 {
                r2((s.closes[hn - 1] / lo250 - 1.0) * 100.0)
            } else { Value::Null },
            "crowd_pct": crowd.map(r2).unwrap_or(Value::Null),
            "crowd_vol_pct": vol_crowd.map(r2).unwrap_or(Value::Null),
            "crowd_beta_pct": parts[2].map(r2).unwrap_or(Value::Null),
            "crowd_corr_pct": parts[3].map(r2).unwrap_or(Value::Null),
            "crowd_bias_pct": parts[4].map(r2).unwrap_or(Value::Null),
            "crowd_comp": crowd_comp.map(r2).unwrap_or(Value::Null),
            "traj_quad": traj_quad,
            "label": label_of(rk6, crowd),
            "label_comp": label_of(rk6, crowd_comp),
        }));
    }

    let (bre, _files, used) = breadth(&ctx.groups, &as_of);
    for row in rows.iter_mut() {
        let key = row.get("ref").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let b = bre.get(&key).cloned().unwrap_or_else(|| json!({
            "above_ma20_pct": Value::Null,
            "above_ma60_pct": Value::Null,
            "nh60_pct": Value::Null,
            "rps20_pct": Value::Null,
            "rps63_pct": Value::Null,
            "n_rps20": 0,
            "n_rps63": 0,
            "n_quoted": 0,
            "n_total": 0,
        }));
        if let Some(o) = row.as_object_mut() {
            o.insert("breadth".into(), b);
        }
    }

    let session = crate::live::session_today();
    let ref_day = crate::live::reference_day();
    let stale = session || as_of.as_str() < ref_day.as_str();
    // 交易日历取自基准 000300 这一个文件：它比行业侧末根旧时，as_of 退一天、所有
    // 「自身序列派生」的列（今日 / 1M~12M / 距高低点）按 fresh 门禁整体置空——
    // 数值没有错，但界面看起来像坏了，所以把原因算出来交给界面说。
    let bench_lag = ctx
        .ind_last
        .as_deref()
        .map(|d| d > as_of.as_str())
        .unwrap_or(false);
    let stale_reason = {
        let mut rs: Vec<String> = Vec::new();
        if session { rs.push("盘中：本地日线只到上一交易日".to_string()); }
        if bench_lag {
            rs.push(format!(
                "基准 000300 末根 {} 落后行业众数末根 {}，动量/距高低点整列按新鲜度门禁置空",
                as_of, ctx.ind_last.clone().unwrap_or_default()
            ));
        }
        if stale && !session && !bench_lag {
            rs.push(format!("本地日线未更新到最近交易日 {}", ref_day));
        }
        rs.join("；")
    };
    Ok(json!({
        "ok": true,
        "as_of": as_of,
        "session": session,
        "stale": stale,
        "stale_reason": stale_reason,
        "bench_lag": bench_lag,
        "coverage": { "as_of": ctx.cov[t], "full": ctx.cov_full },
        "source": {
            "sectors": format!("{} 行业", ctx.secs.len()),
            "breadth": format!("{} 只（含北交所）", ctx.n_members),
            "bench": "000300（本地日线）",
            "catalog": format!(
                "zg modules/market/industry_catalog.json（{} 个二级行业 / {} 只成员，updated {}）",
                ctx.nodes.len(), ctx.n_members, ctx.cat_updated
            ),
        },
        // 屏幕上一行说完；完整口径全在 note_long（悬停显示）
        "note": "缺数据留空；拥挤度=五分量等权分位合成（量价类，无估值/资金维度）",
        "note_long": format!(
            "本地重算口径：128 个通达信研究行业二级全量入表（{} 个有日线 / {} 个缺 / {} 个历史不足 {} 根，算不出的指标留空）；合成拥挤 crowd_comp=五分量各自 ≤5 年历史分位后等权均值（c1 成交额占 128 行业比重 / c2 20 日均振幅 / c3 60 日滚动 β 对 000300 / c4 20 日 corr(日涨跌,成交额环比) / c5 乖离率 c/MA20−1，可用分量 <3 为空；参照国盛/华泰/中金/兴业研报的多分量等权合成，全部量价类，无估值/资金维度）；c1 的分母是当日有 bar 的行业成交额之和，覆盖掉到满覆盖（{} 家）的 9/10 以下时该日份额不可比、整日置空，末根覆盖 {} 家；crowd_pct=单分量 c1（旧口径）；IR=252 日对 000300 日均超额÷超额标准差（信息比率）的截面百分位 mom_rank_ir；合成分 RkC=0.4×3M+0.2×6M+0.2×9M+0.2×12M 的截面百分位（IBD 权重）；双窗 RS=相对 000300 的 63/252 日超额（百分点）；rs_comp=rs20/63/252 截面百分位等权合成，≥90（与界面显示一致，按整数分位判定）为主线标记 rs90（国盛 RS≥90 双重验证口径）；广度按 catalog 全量成员（含北交所 {} 只），当日有效样本 {} 只，MA20/60 与新高只统计当日有本地日线的成员；label=Rk6×单分量拥挤（旧口径，过渡保留），label_comp=Rk6×合成拥挤（新口径；两者阈值均按界面显示整数分位取整）；散点四模式（轮动/合成/IR/双窗）+ 20 日轨迹开关，只画两轴都可算的行（其余行仍在下方表格，不代表被剔除）；外圈=当前纵轴指标的 20 日位移（rank6_d20/rankc_d20/mom_rank_ir_d20/rs252_d20，四口径同 t−t20 差值）",
            ctx.n_full, ctx.n_no_file, ctx.n_short, FULL_HIST, ctx.cov_full, ctx.cov[t], ctx.n_bj, used
        ),
        "missing": ctx.missing,
        "industries": rows,
    }))
}

fn csv_num(v: Option<f64>, dp: usize) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x:.dp$}"),
        _ => String::new(),
    }
}

/// 历史回放出口：最近 days 个交易日（含当日）逐日输出全行业指标 + 前向 20/60 日收益（CSV）。
/// 与 payload 共用 compute_ctx 网格，口径逐字一致；给门禁分析（output/scatter-optim）用。
pub fn replay_csv(out: &std::path::Path, days: usize) -> Result<String, String> {
    let ctx = compute_ctx()?;
    let n = ctx.n;
    let days = days.clamp(1, n);
    let t_end = n - 1;
    let t0 = t_end + 1 - days;
    let mut buf = String::from(
        // crowd_pct 与 c1 逐字同值（同一 parts[0]）：v5 之前的回放只有 crowd_pct，
        // output/scatter-optim 的 analyze.py 按列名读，保留这列只为让旧脚本仍可复跑。
        "date,code,name,rk6,rkc,rk_ir,c1,c2,c3,c4,c5,crowd_comp,crowd_pct,rs_comp,rs90,label,label_comp,rank6_d20,rankc_d20,mom_rank_ir_d20,rs252_d20,fwd20,fwd60\n",
    );
    let mut rows = 0usize;
    for t in t0..=t_end {
        let date = &ctx.axis[t];
        let fwd = |i: usize, h: usize| -> Option<f64> {
            let j = t + h;
            if j > t_end {
                return None;
            }
            match (ctx.series[i].c[t], ctx.series[i].c[j]) {
                (Some(a), Some(b)) if a > 0.0 => Some((b / a - 1.0) * 100.0),
                _ => None,
            }
        };
        for (i, s) in ctx.secs.iter().enumerate() {
            let parts = crowd_parts(&ctx.comps[i], t);
            let cc = crowd_comp_of(&parts);
            let rk6 = ctx.ranks[i][t];
            let rs_comp = ctx.rs_comp(i, t);
            let rs90 = rs90_of(rs_comp);
            let label = label_of(rk6, parts[0]);
            let label_comp = label_of(rk6, cc);
            // 散点外圈的四条 20 日位移：界面上的画环率/加粗率（panels.js OV_ACCEL 阈值）
            // 拿这四列即可在全历史区间复核，不必再解 payload JSON
            let back20 = |g: &Vec<Vec<Option<f64>>>| if t >= 20 { g[i][t - 20] } else { None };
            let rs252 = rs_at(&ctx.series[i].c, &ctx.bench_c, t, 252);
            let rs252_prev = if t >= 20 { rs_at(&ctx.series[i].c, &ctx.bench_c, t - 20, 252) } else { None };
            buf.push_str(&format!(
                "{},{},\"{}\",{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                date,
                s.code,
                s.name,
                csv_num(rk6, 2),
                csv_num(ctx.ranksc[i][t], 2),
                csv_num(ctx.ranks_ir[i][t], 2),
                csv_num(parts[0], 2),
                csv_num(parts[1], 2),
                csv_num(parts[2], 2),
                csv_num(parts[3], 2),
                csv_num(parts[4], 2),
                csv_num(cc, 2),
                csv_num(parts[0], 2),
                csv_num(rs_comp, 2),
                if rs90 { "1" } else { "0" },
                label,
                label_comp,
                csv_num(d20(rk6, back20(&ctx.ranks)), 2),
                csv_num(d20(ctx.ranksc[i][t], back20(&ctx.ranksc)), 2),
                csv_num(d20(ctx.ranks_ir[i][t], back20(&ctx.ranks_ir)), 2),
                csv_num(d20(rs252, rs252_prev), 2),
                csv_num(fwd(i, 20), 3),
                csv_num(fwd(i, 60), 3),
            ));
            rows += 1;
        }
    }
    if let Some(p) = out.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::write(out, buf.as_bytes()).map_err(|e| format!("写回放 CSV 失败: {e}"))?;
    Ok(format!(
        "{} 行（{} 个交易日 × {} 行业，as_of {}）→ {}",
        rows,
        t_end - t0 + 1,
        ctx.secs.len(),
        ctx.axis[t_end],
        out.display()
    ))
}
