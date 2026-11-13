//! 日线合并：本地与远端同一天只能留一根，且「谁赢」由调用点显式指定。
//!
//! 这里原来有两份实现（个股 live 本地优先、板块 board 远端优先），
//! 语义不同却没写进签名。抽出来后：
//! - 合并策略是枚举参数，调用点必须写明；
//! - 日期归一只有一份（远端 `20261104` 与本地 `2026-11-04` 必须落同键）。

use std::collections::BTreeMap;

use crate::bars::Agg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergePolicy {
    /// 同日以本地为准；远端零成交占位 bar 丢弃。
    /// 个股日线：停牌时 TQ 会给一根 vol=0 的假十字星，进图会顶高末根日期。
    LocalWins,
    /// 同日以远端为准；不丢远端零成交 bar。
    /// 板块/指数保持既有行为（要改这条另开单，并先补对表测试）。
    RemoteWins,
}

/// 日期归一：`2026-11-04` / `20261104` / `2026-11-04 15:00` → `2026-11-04`。
///
/// 远端各源的日期格式不统一，而代码里有两处**字典序**比较（远端新鲜度判定、
/// `day_series` 的 `as_of == remote_last`）：`'20261103' >= '2026-11-04'` 判成**真**
/// —— 陈旧的一天会被当成新鲜的。归一后再比，格式差异不再影响新鲜度与合并。
pub fn norm_date(s: &str) -> String {
    let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.len() >= 8 {
        format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
    } else {
        s.trim().to_string()
    }
}

pub fn merge_days(local: Vec<Agg>, remote: Vec<Agg>, policy: MergePolicy) -> Vec<Agg> {
    let mut m: BTreeMap<String, Agg> = BTreeMap::new();
    // 键统一归一：远端若用 '20261104' 而本地用 '2026-11-04'，不归一会变成**两根同一天**
    let put = |m: &mut BTreeMap<String, Agg>, mut b: Agg| {
        b.date = norm_date(&b.date);
        m.insert(b.date.clone(), b);
    };
    match policy {
        MergePolicy::LocalWins => {
            for b in remote {
                // 远端的**零成交占位 bar**（停牌/当日无成交，TQ 会给一根
                // open=high=low=close 的空柱，vol=0）不是真实交易 → 不进图。
                if b.vol <= 0.0 {
                    continue;
                }
                put(&mut m, b);
            }
            for b in local {
                put(&mut m, b); // 本地后放 → 同日以本地为准
            }
        }
        MergePolicy::RemoteWins => {
            for b in local {
                put(&mut m, b);
            }
            for b in remote {
                put(&mut m, b); // 远端后放 → 同日以远端为准
            }
        }
    }
    m.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: &str, c: f64) -> Agg {
        Agg { date: d.to_string(), open: c, high: c, low: c, close: c, vol: 1.0, amount: 1.0 }
    }

    /// **合并必须本地优先**：同日以本地为准，远端只补本地没有的日期。
    #[test]
    fn merge_days_prefers_local_on_overlap() {
        let local = vec![day("2026-11-02", 10.0), day("2026-11-03", 11.0)];
        let remote = vec![
            day("2026-11-02", 10.05),
            day("2026-11-03", 11.06),
            day("2026-11-04", 12.0),
        ];
        let merged = merge_days(local, remote, MergePolicy::LocalWins);
        assert_eq!(merged.len(), 3);
        let m: std::collections::HashMap<&str, f64> =
            merged.iter().map(|b| (b.date.as_str(), b.close)).collect();
        assert_eq!(m["2026-11-02"], 10.0, "重叠日必须用本地值");
        assert_eq!(m["2026-11-03"], 11.0, "重叠日必须用本地值");
        assert_eq!(m["2026-11-04"], 12.0, "本地没有的日期由远端补上");
    }

    /// 日期键必须归一：远端 '20261104' 与本地 '2026-11-04' 不归一会变成**两根同一天**。
    #[test]
    fn merge_days_normalizes_date_keys() {
        let local = vec![day("2026-11-04", 10.0)];
        let remote = vec![day("20261104", 10.5)];
        let merged = merge_days(local, remote, MergePolicy::LocalWins);
        assert_eq!(merged.len(), 1, "同一天只能留一根");
        assert_eq!(merged[0].close, 10.0, "归一后同日以本地为准");
        assert_eq!(merged[0].date, "2026-11-04");
    }

    /// 远端的**零成交占位 bar**（停牌/当日无成交）不是真实交易，不进图。
    #[test]
    fn merge_days_drops_remote_zero_volume_bars() {
        let mut local = day("2026-11-02", 10.0);
        local.vol = 100.0;
        let mut placeholder = day("2026-11-04", 10.0);
        placeholder.vol = 0.0;
        let merged = merge_days(vec![local], vec![placeholder], MergePolicy::LocalWins);
        assert_eq!(merged.len(), 1, "零成交占位柱不该进图");
        assert_eq!(merged[0].date, "2026-11-02");
    }

    /// 板块/指数是**远端优先**，且保持既有行为（零成交 bar 不丢）。
    /// 这条测试是为了防止有人把 LocalWins 的规则顺手套到 board 上。
    #[test]
    fn merge_days_remote_wins_keeps_board_behavior() {
        let local = vec![day("2026-11-02", 10.0)];
        let remote = vec![day("2026-11-02", 10.05), day("2026-11-03", 0.0)];
        let merged = merge_days(local, remote, MergePolicy::RemoteWins);
        assert_eq!(merged.len(), 2);
        let m: std::collections::HashMap<&str, f64> =
            merged.iter().map(|b| (b.date.as_str(), b.close)).collect();
        assert_eq!(m["2026-11-02"], 10.05, "板块同日以远端为准");
        assert!(m.contains_key("2026-11-03"), "板块不丢零成交 bar（既有行为）");
    }

    /// RemoteWins 的输出日期**恒为归一后的 `YYYY-MM-DD`**（TV-33 起的行为契约）。
    /// 旧 board 版 merge 原样透传远端日期（`20261104` 会直接进响应）；归一是合并
    /// 正确性的前提（不归一会两根同一天），顺带把输出格式也钉死——board-kline 的
    /// date 字段消费方（前端字典序比较/显示）可以依赖这个形态。
    #[test]
    fn merge_days_remote_wins_normalizes_output_dates() {
        let local = vec![day("2026-11-02", 10.0)];
        let remote = vec![day("20261104", 10.5)];
        let merged = merge_days(local, remote, MergePolicy::RemoteWins);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].date, "2026-11-04", "远端紧凑日期必须归一后输出");
    }
}
