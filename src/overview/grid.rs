//! 板块网格与成分加载：行业目录 → 每板块日线序列（`load_catalog` / `compute_ctx`）。
//! 算法口径注释留在根文件与各函数上，随代码原样迁移、未改一字。

use super::*;
use super::crowd::{build_comps, ir_series};

fn mom_at(closes: &[Option<f64>], i: usize, back: usize) -> Option<f64> {
    if i < back {
        return None;
    }
    match (closes[i]?, closes[i - back]?) {
        (c, p) if p > 0.0 => Some((c / p - 1.0) * 100.0),
        _ => None,
    }
}

/// catalog → (128 个二级节点, 按二级 ref 分组的全量成员)
fn load_catalog() -> Result<(Vec<Node>, Groups, usize, usize, String), String> {
    let p = catalog_path();
    let doc = read_json(&p).ok_or_else(|| format!("读不到行业目录 {}", p.display()))?;
    let updated = doc.get("updated").and_then(|x| x.as_str()).unwrap_or("").to_string();

    let mut nodes: Vec<Node> = Vec::new();
    let list = doc.get("nodes").and_then(|x| x.as_array())
        .ok_or_else(|| "行业目录缺少 nodes".to_string())?;
    for n in list {
        let ref_ = n.get("ref").and_then(|x| x.as_str()).unwrap_or("");
        if ref_.len() != 5 {
            continue;
        }
        let code = n.get("code").and_then(|x| x.as_str()).unwrap_or("");
        if !code.starts_with("881") {
            continue;
        }
        let name = n.get("name").and_then(|x| x.as_str()).unwrap_or("");
        nodes.push(Node {
            code: code.to_string(),
            name: name.to_string(),
            ref_: ref_.to_string(),
        });
    }
    nodes.sort_by(|a, b| a.code.cmp(&b.code));
    if nodes.is_empty() {
        return Err("行业目录里没有 ref 为 5 位的二级节点".to_string());
    }

    let mut groups: Groups = HashMap::new();
    let mut n_members = 0usize;
    let mut n_bj = 0usize;
    for m in doc.get("members").and_then(|x| x.as_array()).map(|x| x.as_slice()).unwrap_or(&[]) {
        let code = match m.get("code").and_then(|x| x.as_str()) {
            Some(c) => c,
            None => continue,
        };
        let market = match m.get("market").and_then(|x| x.as_u64()) {
            Some(0) => "sz",
            Some(1) => "sh",
            Some(2) => "bj",
            _ => continue,
        };
        let ind = match m.get("industry").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let key = match ind.get(..5) {
            Some(k) => k,
            None => continue,
        };
        groups.entry(key.to_string()).or_default().push((market.to_string(), code.to_string()));
        n_members += 1;
        if market == "bj" {
            n_bj += 1;
        }
    }
    Ok((nodes, groups, n_members, n_bj, updated))
}

pub(super) fn compute_ctx() -> Result<Ctx, String> {
    let (nodes, groups, n_members, n_bj, cat_updated) = load_catalog()?;

    // 基准 000300 提供交易日历（轴）：只需最近 HIST 根
    let bench_bars = zg_core::api::read_day_bars_tail("sh", "000300", HIST)
        .map_err(|e| format!("读基准 000300 日线失败: {e}"))?;
    if bench_bars.len() < FULL_HIST {
        return Err("基准 000300 本地日线不足".to_string());
    }
    let bench_bars = if bench_bars.len() > HIST {
        &bench_bars[bench_bars.len() - HIST..]
    } else {
        &bench_bars[..]
    };
    let axis: Vec<String> = bench_bars.iter().map(|b| b.date.clone()).collect();
    let n = axis.len();
    let as_of = axis[n - 1].clone();
    let axis_pos: HashMap<&str, usize> =
        axis.iter().enumerate().map(|(i, d)| (d.as_str(), i)).collect();
    let bench_c: Vec<Option<f64>> = bench_bars.iter().map(|b| Some(b.close)).collect();

    // 128 个二级行业全部入表：缺日线 / 历史短也保留，算不出的指标返回 null
    let mut secs: Vec<Sec> = Vec::with_capacity(nodes.len());
    let mut series: Vec<Series> = Vec::with_capacity(nodes.len());
    let mut missing: Vec<Value> = Vec::new();
    let mut last_dates: HashMap<String, usize> = HashMap::new();
    let (mut n_full, mut n_no_file, mut n_short) = (0usize, 0usize, 0usize);
    for nd in &nodes {
        // 板块自身也只需最近 HIST 根
        let bars = zg_core::api::read_day_bars_tail("sh", &nd.code, HIST);
        let own_bars: &[zg_core::model::Bar] = match &bars {
            Ok(b) if !b.is_empty() => {
                if b.len() < FULL_HIST {
                    n_short += 1;
                    missing.push(json!({
                        "code": nd.code, "name": nd.name,
                        "reason": format!("历史仅 {} 根", b.len()),
                    }));
                } else {
                    n_full += 1;
                }
                if b.len() > HIST { &b[b.len() - HIST..] } else { &b[..] }
            }
            _ => {
                n_no_file += 1;
                missing.push(json!({
                    "code": nd.code, "name": nd.name, "reason": "无本地日线",
                }));
                &[]
            }
        };
        let s = Sec {
            name: nd.name.clone(),
            code: nd.code.clone(),
            ref_: nd.ref_.clone(),
            closes: own_bars.iter().map(|b| b.close).collect(),
            highs: own_bars.iter().map(|b| b.high).collect(),
            lows: own_bars.iter().map(|b| b.low).collect(),
            fresh: own_bars.last().map(|b| b.date.as_str()) == Some(as_of.as_str()),
        };
        if let Some(last) = own_bars.last().map(|b| b.date.clone()) {
            *last_dates.entry(last).or_default() += 1;
        }
        let mut sr = Series {
            c: vec![None; n],
            h: vec![None; n],
            l: vec![None; n],
            a: vec![None; n],
        };
        for b in own_bars {
            if let Some(&ai) = axis_pos.get(b.date.as_str()) {
                sr.c[ai] = Some(b.close);
                sr.h[ai] = Some(b.high);
                sr.l[ai] = Some(b.low);
                sr.a[ai] = Some(b.amount);
            }
        }
        series.push(sr);
        secs.push(s);
    }

    let (total_amt, cov): (Vec<f64>, Vec<usize>) = (0..n)
        .map(|k| {
            let rows: Vec<f64> = series.iter().filter_map(|s| s.a[k]).collect();
            (rows.iter().sum::<f64>(), rows.len())
        })
        .collect();
    let cov_full = *cov.iter().max().unwrap_or(&0);
    // 众数末根（并列取较晚的一天）：只用于诊断「基准轴掉队」，不参与任何数值口径
    let ind_last = last_dates
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(d, _)| d);
    let comps: Vec<CrowdComps> = series
        .iter()
        .map(|s| build_comps(s, &bench_c, &total_amt, &cov, cov_full))
        .collect();
    let irs: Vec<Vec<Option<f64>>> =
        series.iter().map(|s| ir_series(&s.c, &bench_c)).collect();

    // 每日截面百分位排名（全程逐日网格：payload 取末位与轨迹，replay 扫历史区间）：
    // Rk6 用 6M 单窗，RkC 用 IBD 权重合成分 comp = 0.4×3M + 0.2×6M + 0.2×9M + 0.2×12M
    // （3 月双倍，四窗任一缺则该日无值），IR 用预算好的信息比率序列
    let rank_series = |start: usize,
                       val_at: &dyn Fn(usize, usize) -> Option<f64>|
     -> Vec<Vec<Option<f64>>> {
        let mut out = vec![vec![None; n]; series.len()];
        // 时间 × 行业双下标网格按设计保留数值下标（截断窗口 + 截面排名），closure 化更难读
        #[allow(clippy::needless_range_loop)]
        for t in start..n {
            let vals: Vec<Option<f64>> = (0..series.len()).map(|i| val_at(i, t)).collect();
            let valid: Vec<f64> = vals.iter().filter_map(|x| *x).collect();
            if valid.len() < 10 {
                continue;
            }
            for (i, v) in vals.iter().enumerate() {
                if let Some(v) = v {
                    let below = valid.iter().filter(|x| **x < *v).count();
                    out[i][t] = Some(below as f64 / valid.len() as f64 * 100.0);
                }
            }
        }
        out
    };
    let ranks = rank_series(126, &|i, t| mom_at(&series[i].c, t, 126));
    let ranksc = rank_series(252, &|i, t| {
        match (
            mom_at(&series[i].c, t, 63),
            mom_at(&series[i].c, t, 126),
            mom_at(&series[i].c, t, 189),
            mom_at(&series[i].c, t, 252),
        ) {
            (Some(a), Some(b), Some(c), Some(d)) => Some(0.4 * a + 0.2 * b + 0.2 * c + 0.2 * d),
            _ => None,
        }
    });
    let ranks_ir = rank_series(IR_WIN, &|i, t| irs[i][t]);
    let rs20r = rank_series(20, &|i, t| rs_at(&series[i].c, &bench_c, t, 20));
    let rs63r = rank_series(63, &|i, t| rs_at(&series[i].c, &bench_c, t, 63));
    let rs252r = rank_series(252, &|i, t| rs_at(&series[i].c, &bench_c, t, 252));

    Ok(Ctx {
        nodes,
        groups,
        n_members,
        n_bj,
        cat_updated,
        axis,
        n,
        secs,
        series,
        bench_c,
        comps,
        cov,
        cov_full,
        ind_last,
        ranks,
        ranksc,
        ranks_ir,
        rs20r,
        rs63r,
        rs252r,
        missing,
        n_full,
        n_no_file,
        n_short,
    })
}
