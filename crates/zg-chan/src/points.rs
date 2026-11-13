//! `points.py` 的 Rust 移植：三类买卖点点位补齐。
//!
//! 判据主权在 czsc 1.0.2 官方信号（[`crate::official`]）；本层只从末笔终点分型
//! 补出点位价/日期/止损，并按 19%/47% 的实测口径报告「门内/门外」。

use chrono::{DateTime, Utc};
use czsc_core::analyze::CZSC;
use czsc_core::objects::zs::ZS;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::{fmt_dt, py_round};
use crate::divergence::{analyze_divergence, DivergenceResult, DivergenceSummary};
use crate::official::{self, SignalHit, BUY, SELL};
use crate::zhongshu::{classify_trend, valid_zs_list, Trend, ZSState};

/// 来源标注（`/api/czsc` sources.points 用）。
pub const POINTS_SOURCE: &str = official::SIGNALS_SOURCE;
pub const ACTION_SOURCE: &str = "reference";
/// L2 方法论版本（与 `method::METHOD_VER` 同一来源，M13）。
pub const L2_METHOD_VER: i64 = crate::CZSC_METHOD_VER;

/// 可成交参考价开关（`THINKING_REF_PRICE=0` 关闭；Python 在 import 时读取）。
pub static REF_PRICE_ENABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var("THINKING_REF_PRICE").unwrap_or_else(|_| "1".into()) != "0");

fn ref_price(c: &CZSC, cdt: Option<DateTime<Utc>>) -> (Option<f64>, &'static str) {
    if !*REF_PRICE_ENABLED {
        return (None, "");
    }
    let Some(cdt) = cdt else {
        return (None, "");
    };
    for b in c.bars_raw.iter().rev() {
        if b.dt == cdt {
            return (Some(b.close), "confirm_bar_close");
        }
    }
    if let Some(last) = c.bars_raw.last() {
        return (Some(last.close), "fallback_last_bar");
    }
    (None, "")
}

/// 买卖点。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuySellPoint {
    pub point_type: String,
    pub direction: String,
    pub operate: String,
    pub price: Option<f64>,
    pub dt: Option<DateTime<Utc>>,
    pub description: String,
    pub confirm_dt: Option<DateTime<Utc>>,
    pub anchor_bi: Option<i64>,
    pub signal_name: String,
    pub source: String,
    pub in_finished_bis: bool,
    pub method_label: String,
    pub qualification: Map<String, Value>,
    pub stop_ref: Map<String, Value>,
    pub level: String,
    pub source_method: String,
    pub ref_price: Option<f64>,
    pub ref_price_basis: String,
}

/// 笔的「可知时刻」= 终点分型的确认根那根 K（不能用 `bi.edt`：那是枢轴根）。
pub fn bi_confirm_dt(c: &CZSC, bi: &czsc_core::objects::bi::BI) -> Option<DateTime<Utc>> {
    if bi.fx_b.elements.len() >= 3 {
        bi.fx_b.elements.last().map(|nb| nb.dt)
    } else {
        c.bars_raw.last().map(|b| b.dt)
    }
}

/// 末笔此刻是否在 `c.finished_bis` 里（门内 ≠ 不可撤销，见 Python 长注释）。
pub fn last_bi_in_finished(c: &CZSC) -> bool {
    c.get_finished_bis().len() == c.bi_list.len()
}

/// 官方主判据 → engine 买卖点。
const OFFICIAL_PRIMARY: [(&str, &str, &str); 4] = [
    (
        "cxt_first_buy_V221126",
        "第一类",
        "bc_price ∧ (bc_volume ∨ bc_length)，三量取自关键笔链；无中枢、无 MACD",
    ),
    (
        "cxt_first_sell_V221126",
        "第一类",
        "同一买的镜像：首末同向 ∧ max_high==首笔高 ∧ min_low==末笔低",
    ),
    (
        "cxt_second_bs_V240524",
        "第二类",
        "纯笔结构：末笔 length≥7 ∧ 此前 length≥7 的笔中 ≥2 根的终点分型区间与末笔重叠；无背驰",
    ),
    (
        "cxt_third_bs_V230319",
        "第三类",
        "中枢由末5笔现场构造（zd=max(b1.low,b3.low), zg=min(b1.high,b3.high)），\
         b5 离开后不破中枢沿；MA(34) 只进 v2，不进 v1 判据",
    ),
];

fn action_text(point_type: &str, direction: &str) -> &'static str {
    match (point_type, direction) {
        ("第一类", "买") => "笔力度衰竭后的向下转折，Operate=LO(开多)；止损设于该低点下方",
        ("第一类", "卖") => "笔力度衰竭后的向上转折，Operate=LE(平多)",
        ("第二类", "买") => "向下回调的确认点，Operate=LO(开多)；回踩确认加仓",
        ("第二类", "卖") => "向上反抽的确认点，Operate=LE(平多)；反抽减仓",
        ("第三类", "买") => "向上离开中枢后回试不破 ZG，Operate=LO(开多)",
        ("第三类", "卖") => "向下离开中枢后回抽不破 ZD，Operate=LE(平多)",
        _ => "",
    }
}

/// 识别三类买卖点。判据主权在 czsc 官方信号，engine 只补点位。
pub fn identify_buy_sell_points(
    c: &CZSC,
    _divergence: Option<&DivergenceResult>,
    hits: Option<&[SignalHit]>,
) -> Vec<BuySellPoint> {
    let owned;
    let hits: &[SignalHit] = match hits {
        Some(h) => h,
        None => {
            owned = official::hits(c, None);
            &owned
        }
    };
    let bis = &c.bi_list;
    if bis.is_empty() {
        return Vec::new();
    }
    let last_bi = &bis[bis.len() - 1];
    let cdt = bi_confirm_dt(c, last_bi);
    let in_fin = last_bi_in_finished(c);
    let (ref_p, ref_basis) = ref_price(c, cdt);

    let mut points: Vec<BuySellPoint> = Vec::new();
    for (name, point_type, criteria) in OFFICIAL_PRIMARY {
        let Some(h) = hits.iter().find(|h| h.name == name && h.fired) else {
            continue;
        };
        if h.direction != BUY && h.direction != SELL {
            continue;
        }
        let direction = if h.direction == BUY { "买" } else { "卖" };
        let operate = if direction == "买" { "开多" } else { "平多" };
        let turn_price = last_bi.fx_b.fx;
        // Python: `f"（{h.anchor_bi}笔窗口）" if h.anchor_bi else ""`（0 视为无）
        let window = match h.anchor_bi {
            Some(n) if n != 0 => format!("（{n}笔窗口）"),
            _ => String::new(),
        };
        points.push(BuySellPoint {
            point_type: point_type.to_string(),
            direction: direction.to_string(),
            operate: operate.to_string(),
            price: Some(turn_price),
            dt: Some(last_bi.end_dt()),
            description: format!(
                "官方 {name} → {}{window}；判据 {criteria}。点位由 engine 补：{}；@{:.2}（末笔终点分型）",
                h.raw,
                action_text(point_type, direction),
                turn_price
            ),
            confirm_dt: cdt,
            anchor_bi: h.anchor_bi,
            signal_name: name.to_string(),
            source: "czsc_official".to_string(),
            in_finished_bis: in_fin,
            method_label: String::new(),
            qualification: Map::new(),
            stop_ref: Map::new(),
            level: String::new(),
            source_method: String::new(),
            ref_price: ref_p,
            ref_price_basis: ref_basis.to_string(),
        });
    }

    // 去重（每个类型+方向只保留最新一个）
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut unique: Vec<BuySellPoint> = Vec::new();
    for p in points {
        let key = (p.point_type.clone(), p.direction.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        unique.push(p);
    }
    unique
}

/// 结构观察要引用的官方背驰侧信号（ROLE_BC），按信息量排序。
pub const BC_EVIDENCE: [&str; 5] = [
    "tas_macd_bc_V240307",
    "zdy_macd_bc_V230422",
    "tas_macd_bc_V221201",
    "tas_macd_bc_ubi_V230804",
    "cxt_double_zs_V230311",
];

/// 结构观察（S5）—— 背驰与盘整背驰住在这里，**不住在买卖点里**。
pub fn structure_evidence(
    c: &CZSC,
    divergence: Option<&DivergenceResult>,
    hits: Option<&[SignalHit]>,
) -> Vec<String> {
    let dv_owned;
    let dv: &DivergenceResult = match divergence {
        Some(d) => d,
        None => {
            dv_owned = analyze_divergence(c, None, None);
            &dv_owned
        }
    };
    let hits_owned;
    let hits: &[SignalHit] = match hits {
        Some(h) => h,
        None => {
            hits_owned = official::hits(c, None);
            &hits_owned
        }
    };
    let by = |name: &str| hits.iter().find(|h| h.name == name);
    let mut out: Vec<String> = Vec::new();

    let osc = by("cxt_range_oscillation_V230620");
    let shape = by("cxt_bi_trend_V230824");
    let in_range = osc.map(|h| h.fired).unwrap_or(false)
        || shape.map(|h| h.category == "横盘").unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    if let Some(h) = osc {
        if h.fired {
            parts.push(format!("{}(震荡)", h.raw));
        }
    }
    if let Some(h) = shape {
        if h.fired {
            parts.push(format!("{}(笔趋势)", h.raw));
        }
    }
    let state_txt = if parts.is_empty() {
        "官方形态状态信号未给出震荡/横盘".to_string()
    } else {
        parts.join("、")
    };

    if dv.is_divergence && dv.divergence_type == "盘整背驰" {
        let head = if in_range {
            "盘整背驰(非买卖点)"
        } else {
            "盘整背驰(结构条件成立，但官方形态信号未确认震荡/横盘 ⇒ 不作提示依据)"
        };
        out.push(format!(
            "- {head}：{}；两轨={}；形态状态 {state_txt}。\
             盘整背驰不保证反转（SKILL.md 反例 ce08），不构成第一类买卖点，\
             操作上一律 HO(持币/持有)观察，不占 operate 字段",
            dv.detail, dv.tracks
        ));
    }

    for name in BC_EVIDENCE {
        let Some(h) = by(name) else { continue };
        if !h.fired {
            continue;
        }
        let dir = if h.direction.is_empty() { "—" } else { h.direction.as_str() };
        out.push(format!(
            "- 官方背驰侧证据 {name} → {}（读法：{}，方向语义 {dir}；**描述，不出点**）",
            h.raw, h.category
        ));
    }
    out
}

/// 历史趋势转折候选（T18 逻辑，T21 更名）。纯结构条件，未经背驰确认。
#[derive(Clone, Debug, Serialize)]
pub struct Retest {
    pub direction: String,
    pub price: f64,
    pub dt: DateTime<Utc>,
    pub cdt: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurningCandidate {
    pub trend: String,
    pub turn_direction: String,
    pub turn_price: f64,
    pub turn_dt: DateTime<Utc>,
    pub turn_cdt: Option<DateTime<Utc>>,
    pub retest: Option<Retest>,
}

pub fn scan_trend_turning_candidates(c: &CZSC, zss: Option<&[ZS]>) -> Vec<TurningCandidate> {
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };
    let bis = &c.bi_list;
    let mut out: Vec<TurningCandidate> = Vec::new();
    let mut used_turn_idx: Vec<usize> = Vec::new();

    for pair in zss.windows(2) {
        let (z1, z2) = (&pair[0], &pair[1]);
        let trend_up = if z2.zd > z1.zg {
            true
        } else if z2.zg < z1.zd {
            false
        } else {
            continue;
        };
        let want = if trend_up {
            czsc_core::objects::direction::Direction::Up
        } else {
            czsc_core::objects::direction::Direction::Down
        };
        let mut turn: Option<(usize, &czsc_core::objects::bi::BI)> = None;
        for (i, b) in bis.iter().enumerate() {
            if b.start_dt() >= z2.edt && b.direction == want {
                turn = Some((i, b));
                break;
            }
        }
        let Some((i_t, b_t)) = turn else { continue };
        if used_turn_idx.contains(&i_t) {
            continue;
        }
        used_turn_idx.push(i_t);

        let mut rec = TurningCandidate {
            trend: if trend_up { "上涨" } else { "下跌" }.to_string(),
            turn_direction: if trend_up { "卖" } else { "买" }.to_string(),
            turn_price: b_t.fx_b.fx,
            turn_dt: b_t.end_dt(),
            turn_cdt: bi_confirm_dt(c, b_t),
            retest: None,
        };
        if i_t + 2 < bis.len() {
            let pull = &bis[i_t + 2];
            if pull.direction == b_t.direction {
                if trend_up && pull.get_high() < b_t.get_high() {
                    rec.retest = Some(Retest {
                        direction: "卖".to_string(),
                        price: pull.get_high(),
                        dt: pull.end_dt(),
                        cdt: bi_confirm_dt(c, pull),
                    });
                } else if !trend_up && pull.get_low() > b_t.get_low() {
                    rec.retest = Some(Retest {
                        direction: "买".to_string(),
                        price: pull.get_low(),
                        dt: pull.end_dt(),
                        cdt: bi_confirm_dt(c, pull),
                    });
                }
            }
        }
        out.push(rec);
    }
    out
}

/// 买卖点摘要（供 pipeline 步骤7 / 桥 `/api/czsc` 使用）。
#[derive(Clone, Debug, Serialize)]
pub struct BuySellSummary {
    pub trend: Trend,
    pub divergence: DivergenceBrief,
    pub points: Vec<BuySellPoint>,
    pub evidence: Vec<String>,
    pub primary_signals: Vec<PrimarySignal>,
    pub official_signals: Vec<SignalHit>,
    pub turning_candidates: TurningCandidates,
}

#[derive(Clone, Debug, Serialize)]
pub struct DivergenceBrief {
    pub divergence_type: String,
    pub is_divergence: bool,
    pub macd_confirm: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PrimarySignal {
    pub name: String,
    pub raw: String,
    pub direction: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurningCandidates {
    pub note: &'static str,
    pub items: Vec<TurningCandidate>,
}

pub fn buy_sell_summary(c: &CZSC, zss: Option<&[ZS]>) -> BuySellSummary {
    let zss_owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            zss_owned = valid_zs_list(c, None);
            &zss_owned
        }
    };
    let divergence = analyze_divergence(c, Some(zss), None);
    let hits = official::hits(c, None);
    let mut points = identify_buy_sell_points(c, Some(&divergence), Some(&hits));

    // L2/L3 方法论裁定（只写字段，不改官方 points；异常不拖垮扫描）
    let l2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::method::qualify_points(c, &mut points, Some(zss), Some(&divergence), None)
    }));
    if let Err(payload) = l2 {
        let msg = crate::official::panic_text(payload.as_ref());
        for p in &mut points {
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Null);
            q.insert("method_ver".into(), Value::from(L2_METHOD_VER));
            q.insert("error".into(), Value::from(msg.clone()));
            p.qualification = q;
        }
    }

    let trend = classify_trend(c, Some(zss));
    let candidates = scan_trend_turning_candidates(c, Some(zss));
    let primary = official::primary_fired(c, Some(&hits))
        .into_iter()
        .map(|h| PrimarySignal {
            name: h.name,
            raw: h.raw,
            direction: h.direction,
        })
        .collect();

    BuySellSummary {
        trend,
        divergence: DivergenceBrief {
            divergence_type: divergence.divergence_type.clone(),
            is_divergence: divergence.is_divergence,
            macd_confirm: divergence.macd_confirm,
        },
        points,
        evidence: structure_evidence(c, Some(&divergence), Some(&hits)),
        primary_signals: primary,
        official_signals: hits,
        turning_candidates: TurningCandidates {
            note: "仅结构条件（相邻中枢无重叠+方向），未经背驰确认；\
                   不等于第一/第二类买卖点，可执行信号以步骤7为准",
            items: candidates,
        },
    }
}

/// 供 payload 层复用的中枢摘要（避免循环依赖时找不到类型）。
pub type ZhongshuItem = ZSState;

/// 供 payload 层复用的背驰摘要。
pub type DivergenceOut = DivergenceSummary;

/// 日期格式化的薄封装（payload 层按周期切换 intraday）。
pub fn point_dt_str(dt: Option<DateTime<Utc>>, intraday: bool) -> String {
    dt.map(|d| fmt_dt(&d, intraday)).unwrap_or_default()
}

/// 价格 3 位小数（`_r3`）。
pub fn r3(v: Option<f64>) -> Option<f64> {
    v.map(|x| py_round(x, 3))
}
