//! `zhongshu.py` 的 Rust 移植：中枢识别与三态判断（官方几何 + 三笔下限）。

use chrono::{DateTime, Utc};
use czsc_core::analyze::utils::get_zs_seq;
use czsc_core::analyze::CZSC;
use czsc_core::objects::zs::ZS;
use czsc_utils::monotonicity::monotonicity;
use serde::{Deserialize, Serialize};

use crate::common::{dir_name, py_round};

/// 缠论「中枢需至少三段重叠」；官方 `is_valid()` 无笔数下限。
pub const MIN_ZS_BIS: usize = 3;
/// 来源标注（`/api/czsc` sources.zs 用）。
pub const ZS_SOURCE: &str = "czsc1.0.2+min3";
pub const STATE_SOURCE: &str = "reference";
pub const TREND_SOURCE: &str = "reference";

/// `c.zs_list` —— 官方 `CZSC.zs_list` getter = `get_zs_seq(&get_finished_bis())`。
pub fn zs_list(c: &CZSC) -> Vec<ZS> {
    get_zs_seq(&c.get_finished_bis())
}

/// 项目标准的中枢过滤：**官方几何 + 三笔下限**（`z.zg > z.zd` 是冗余保险）。
pub fn valid_zs_list(c: &CZSC, min_bis: Option<usize>) -> Vec<ZS> {
    let min_bis = min_bis.unwrap_or(MIN_ZS_BIS);
    zs_list(c)
        .into_iter()
        .filter(|z| z.bis.len() >= min_bis && z.zg > z.zd)
        .collect()
}

/// 中枢状态。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZSState {
    pub index: usize,
    pub zg: f64,
    pub zd: f64,
    pub zz: f64,
    pub gg: f64,
    pub dd: f64,
    pub sdt: DateTime<Utc>,
    pub edt: DateTime<Utc>,
    pub bi_count: usize,
    /// "延续" / "扩张" / "新生"
    pub state: String,
    pub sdir: String,
    pub edir: String,
}

/// 中枢三态（参考口径）：段数达 9 = 扩张；非末位 = 新生；末位 >3 笔 = 延续。
fn zs_state(zs: &ZS, is_last: bool) -> &'static str {
    let n = zs.bis.len();
    if n >= 9 {
        return "扩张";
    }
    if !is_last {
        return "新生";
    }
    if n > 3 {
        "延续"
    } else {
        "新生"
    }
}

/// 分析中枢三态（几何来自官方 `zs_list` + `MIN_ZS_BIS` 过滤）。
pub fn analyze_zhongshu(c: &CZSC, zss: Option<&[ZS]>) -> Vec<ZSState> {
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };
    let mut results = Vec::with_capacity(zss.len());
    let last = zss.len().saturating_sub(1);
    for (i, zs) in zss.iter().enumerate() {
        results.push(ZSState {
            index: i,
            zg: zs.zg,
            zd: zs.zd,
            zz: zs.zz,
            gg: zs.gg,
            dd: zs.dd,
            sdt: zs.sdt,
            edt: zs.edt,
            bi_count: zs.bis.len(),
            state: zs_state(zs, i == last).to_string(),
            sdir: dir_name(zs.sdir).to_string(),
            edir: dir_name(zs.edir).to_string(),
        });
    }
    results
}

/// 走势类型与趋势方向（参考口径：对全部相邻中枢对投票）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trend {
    pub trend_type: String,
    pub trend_direction: String,
    pub zs_count: usize,
    pub zg_monotonicity: Option<f64>,
}

pub fn classify_trend(c: &CZSC, zss: Option<&[ZS]>) -> Trend {
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };
    if zss.is_empty() {
        return Trend {
            trend_type: "未形成中枢".to_string(),
            trend_direction: "未知".to_string(),
            zs_count: 0,
            zg_monotonicity: None,
        };
    }
    if zss.len() == 1 {
        return Trend {
            trend_type: "盘整".to_string(),
            trend_direction: "盘整".to_string(),
            zs_count: 1,
            zg_monotonicity: None,
        };
    }

    let mut up = 0usize;
    let mut down = 0usize;
    for pair in zss.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if b.zd > a.zg {
            up += 1;
        } else if b.zg < a.zd {
            down += 1;
        }
    }
    let (direction, trend_type) = if up > 0 && up > down {
        ("上涨", "趋势")
    } else if down > 0 && down > up {
        ("下跌", "趋势")
    } else {
        ("盘整", "盘整")
    };

    let zgs: Vec<f64> = zss.iter().map(|z| z.zg).collect();
    let mono = py_round(monotonicity(&zgs), 4);

    Trend {
        trend_type: trend_type.to_string(),
        trend_direction: direction.to_string(),
        zs_count: zss.len(),
        zg_monotonicity: Some(mono),
    }
}

/// 中枢综合分析（供 worker /api/czsc 组装）。
#[derive(Clone, Debug, Serialize)]
pub struct ZhongshuSummary {
    pub zs_source: &'static str,
    pub state_source: &'static str,
    pub trend_source: &'static str,
    pub trend: Trend,
    pub zhongshu_list: Vec<ZSState>,
}

pub fn zhongshu_summary(c: &CZSC, zss: Option<&[ZS]>) -> ZhongshuSummary {
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };
    ZhongshuSummary {
        zs_source: ZS_SOURCE,
        state_source: STATE_SOURCE,
        trend_source: TREND_SOURCE,
        trend: classify_trend(c, Some(zss)),
        zhongshu_list: analyze_zhongshu(c, Some(zss)),
    }
}
