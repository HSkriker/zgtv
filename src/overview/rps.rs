//! 成分股 RPS 广度（全 A 截面分位 ≥90 的板块内占比，双窗 20/63）与新高/均线广度统计。

use super::*;
use serde_json::json;

/// 广度只需末 70 根：MA60 / 60 日新高 / mom63 回看 63 根，多留 7 根缓冲。
const RPS_TAIL: usize = 70;

/// 广度单只成分股记录：第一遍读盘产出（mom20/mom63），pct20/pct63 由 assign_rps_pcts 回填
struct BreadthRec {
    key: String, // 所属板块 ref
    bj: bool,    // 北交所成员：MA/NH 口径照旧，但不进 RPS 截面
    ma20: bool,
    ma60: bool,
    nh: bool,
    mom20: Option<f64>,
    mom63: Option<f64>,
    pct20: Option<f64>,
    pct63: Option<f64>,
}

/// 截面百分位（与 rank_series 同约定：below/valid×100，并列值同名次）
fn rps_percentiles(vals: &[f64]) -> Vec<f64> {
    let n = vals.len() as f64;
    vals.iter()
        .map(|v| vals.iter().filter(|x| **x < *v).count() as f64 / n * 100.0)
        .collect()
}

/// 双窗全 A 截面分位：北交所不进截面（2026-09-25 用户裁定「北交所一律不管」）；
/// 历史不足 64 根的次新股只进月窗（季窗 mom63 为 None）
fn assign_rps_pcts(recs: &mut [BreadthRec]) {
    let idx: Vec<usize> = (0..recs.len()).filter(|&i| !recs[i].bj).collect();
    let i20: Vec<usize> = idx.iter().copied().filter(|&i| recs[i].mom20.is_some()).collect();
    let v20: Vec<f64> = i20.iter().map(|&i| recs[i].mom20.unwrap()).collect();
    for (j, p) in rps_percentiles(&v20).into_iter().enumerate() {
        recs[i20[j]].pct20 = Some(p);
    }
    let i63: Vec<usize> = idx.iter().copied().filter(|&i| recs[i].mom63.is_some()).collect();
    let v63: Vec<f64> = i63.iter().map(|&i| recs[i].mom63.unwrap()).collect();
    for (j, p) in rps_percentiles(&v63).into_iter().enumerate() {
        recs[i63[j]].pct63 = Some(p);
    }
}

/// 按板块聚合：MA/新高占比分母 = 当日有效样本（含北交所，口径不变）；
/// RPS≥90 占比分母 = 该窗有有效分位的家数（n_rps20 / n_rps63，可不同）。
/// ≥90 判定按显示精度 pct0（与 rs90 的 v9 口径一致，避免界面显示 90 却不计入）
fn breadth_rows(recs: &[BreadthRec], totals: &HashMap<String, usize>) -> (HashMap<String, Value>, usize) {
    let mut out = HashMap::new();
    let mut used = 0usize;
    for (key, &total) in totals {
        let rs: Vec<&BreadthRec> = recs.iter().filter(|r| &r.key == key).collect();
        let quoted = rs.len();
        used += quoted;
        let above20 = rs.iter().filter(|r| r.ma20).count();
        let above60 = rs.iter().filter(|r| r.ma60).count();
        let nh = rs.iter().filter(|r| r.nh).count();
        let g20: Vec<f64> = rs.iter().filter_map(|r| r.pct20).collect();
        let g63: Vec<f64> = rs.iter().filter_map(|r| r.pct63).collect();
        let hit = |g: &[f64]| g.iter().filter(|p| pct0(**p) >= RPS_HI).count();
        let share = |k: usize, n: usize| {
            if n > 0 { r2(k as f64 / n as f64 * 100.0) } else { Value::Null }
        };
        out.insert(key.clone(), json!({
            "above_ma20_pct": share(above20, quoted),
            "above_ma60_pct": share(above60, quoted),
            "nh60_pct": share(nh, quoted),
            "rps20_pct": share(hit(&g20), g20.len()),
            "rps63_pct": share(hit(&g63), g63.len()),
            "n_rps20": g20.len(),
            "n_rps63": g63.len(),
            "n_quoted": quoted,
            "n_total": total,
        }));
    }
    (out, used)
}

/// 广度：catalog 全量成员 → 本地 .day，算 MA20/MA60 上方与 60 日收盘新高占比，
/// 以及双窗成分股 RPS 广度（全 A 20/63 日截面分位 ≥90 的成分占比）。
/// 返回 (ref → 广度, 读到文件的成员数, 当日有效样本数)。北交所同样解析（仅不进 RPS 截面）。
pub(super) fn breadth(
    groups: &Groups,
    as_of: &str,
) -> (HashMap<String, Value>, usize, usize) {
    let mut recs: Vec<BreadthRec> = Vec::new();
    let mut totals: HashMap<String, usize> = HashMap::with_capacity(groups.len());
    let mut files = 0usize;
    for (key, members) in groups {
        totals.insert(key.clone(), members.len());
        for (market, code) in members {
            // 只需末 70 根（MA60 / 60 日新高 / mom63 回看 63）：尾部读，全市场 ~12MB
            let Ok(mut bars) = zg_core::api::read_day_bars_tail(market, code, RPS_TAIL) else { continue };
            // 前复权后再算 MA/新高/动量：原始价在除权日有假缺口，会把 MA 抬高、把 60 日新高比没
            // （与 heat.rs 的除权参考昨收同一口径问题）；复权失败与读取失败同样剔除
            if zg_core::api::apply_qfq_bars(&mut bars, market, code).is_err() {
                continue;
            }
            files += 1;
            // 有效样本：最新一根就是全市场 as_of 当日（停牌/退市剔除），且够 61 根
            let n = bars.len();
            if n < 61 || bars.last().map(|b| b.date.as_str()) != Some(as_of) {
                continue;
            }
            let tail = &bars[n - 60..];
            let close = tail[59].close;
            let ma20: f64 = tail[40..].iter().map(|b| b.close).sum::<f64>() / 20.0;
            let ma60: f64 = tail.iter().map(|b| b.close).sum::<f64>() / 60.0;
            let hi60 = tail.iter().fold(f64::MIN, |a, b| a.max(b.close));
            // 双窗动量：回看根收盘价 ≤0（极端前复权）视为无效
            let mom = |back: usize| -> Option<f64> {
                let p = bars[n - 1 - back].close;
                (p > 0.0).then(|| close / p - 1.0)
            };
            recs.push(BreadthRec {
                key: key.clone(),
                bj: market == "bj",
                ma20: close > ma20,
                ma60: close > ma60,
                nh: close >= hi60,
                mom20: mom(RPS_WIN_M),
                mom63: (n > RPS_WIN_Q).then(|| mom(RPS_WIN_Q)).flatten(),
                pct20: None,
                pct63: None,
            });
        }
    }
    assign_rps_pcts(&mut recs);
    let (out, used) = breadth_rows(&recs, &totals);
    (out, files, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rps_percentiles_follow_rank_series_convention() {
        // below/valid×100，并列同名次，最大者趋近 100 但到不了
        let p = rps_percentiles(&[1.0, 2.0, 2.0, 3.0]);
        assert_eq!(p, vec![0.0, 25.0, 25.0, 75.0]);
        assert!(rps_percentiles(&[]).is_empty());
    }

    #[test]
    fn rps_breadth_excludes_bj_and_short_history() {
        let rec = |key: &str, bj: bool, m20: Option<f64>, m63: Option<f64>| BreadthRec {
            key: key.to_string(),
            bj,
            ma20: false,
            ma60: false,
            nh: false,
            mom20: m20,
            mom63: m63,
            pct20: None,
            pct63: None,
        };
        // X001：10 只递增动量 + 1 只北交所强股；X002：1 只次新股（只有月窗）
        let mut recs: Vec<BreadthRec> = (0..10)
            .map(|i| {
                let m = Some(0.1 * (i as f64 + 1.0));
                rec("X001", false, m, m)
            })
            .collect();
        recs.push(rec("X001", true, Some(9.0), Some(9.0)));
        recs.push(rec("X002", false, Some(0.05), None));
        assign_rps_pcts(&mut recs);

        // 北交所不进截面：分位保持空
        assert!(recs[10].pct20.is_none() && recs[10].pct63.is_none());
        // 次新股：月窗有分位、季窗没有
        assert!(recs[11].pct20.is_some() && recs[11].pct63.is_none());
        // 月窗截面 11 个样本：榜首 10/11=90.9 → ≥90 点亮；第二名 9/11=81.8 → 不亮
        assert!(pct0(recs[9].pct20.unwrap()) >= RPS_HI);
        assert!(pct0(recs[8].pct20.unwrap()) < RPS_HI);
        // 季窗截面 10 个样本：榜首 9/10=90.0 恰好压线点亮（pct0 显示精度口径）
        assert_eq!(recs[9].pct63, Some(90.0));
        assert!(pct0(recs[9].pct63.unwrap()) >= RPS_HI);

        let mut totals = HashMap::new();
        totals.insert("X001".to_string(), 12usize);
        totals.insert("X002".to_string(), 1usize);
        let (out, used) = breadth_rows(&recs, &totals);
        assert_eq!(used, 12);
        let x1 = &out["X001"];
        assert_eq!(x1["n_quoted"], json!(11), "北交所照旧计入 MA/NH 分母");
        assert_eq!(x1["n_rps20"], json!(10), "北交所不进 RPS 截面");
        assert_eq!(x1["n_rps63"], json!(10));
        assert_eq!(x1["rps20_pct"], json!(10.0), "1/10 点亮");
        assert_eq!(x1["rps63_pct"], json!(10.0));
        let x2 = &out["X002"];
        assert_eq!(x2["n_rps20"], json!(1));
        assert_eq!(x2["n_rps63"], json!(0));
        assert!(x2["rps63_pct"].is_null(), "分母 0 → Null 不是 0");
    }
}
