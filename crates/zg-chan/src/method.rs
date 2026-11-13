//! `method_qualification.py` 的 Rust 移植：czsc-thinking L2/L3 裁定层（官方主判据不动）。
//!
//! 分层：
//!   L2 一买趋势门（末对中枢必须下跌）+ 二买前置一买门
//!   L3 全部 `reference`：锚点确认、MACD 第二意见、MACD 线轨、二买前低分级、
//!      三买严格模式、止损参考、级别
//!
//! **任何 L2 异常都不能让扫描失败**：`qualify_points` 逐点 `catch_unwind`
//! （等价 Python 的逐点 `try/except BaseException`），失败只写
//! `qualification.error`，官方 hit 原样保留。

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use czsc_core::analyze::CZSC;
use czsc_core::objects::bi::BI;
use czsc_core::objects::zs::ZS;
use serde_json::{json, Map, Value};

use crate::common::{date_str, dir_name, freq_name, py_round};
use crate::divergence::DivergenceResult;
use crate::first_buy::{replay, Event};
use crate::macd::{macd_at, segment_macd_area, MacdFrame};
use crate::points::BuySellPoint;
use crate::zhongshu::valid_zs_list;

/// 二买前置一买的最远笔距（上限；Phase 2 先用 60）。
pub static MAX_PRIOR_BIS: LazyLock<usize> = LazyLock::new(|| {
    std::env::var("THINKING_MAX_PRIOR_BIS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(60)
});

/// L2/L3 总开关；0 = 只走官方，字段留空。
pub static THINKING_LAYER: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("THINKING_LAYER")
        .map(|v| v != "0")
        .unwrap_or(true)
});

/// 三买严格模式；默认关闭。
pub static THINKING_THIRD_STRICT: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("THINKING_THIRD_STRICT")
        .map(|v| v == "1")
        .unwrap_or(false)
});

/// `macd_line`（L3 线轨）开关；0 = 不计算，字段为 null。
pub static THINKING_MACD_LINE: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("THINKING_MACD_LINE")
        .map(|v| v != "0")
        .unwrap_or(true)
});

/// 方法论版本（`macd_line`/`ref_price` 与二买前置归因修正；`method_ok` 判据不变）。
pub const METHOD_VER: i64 = crate::CZSC_METHOD_VER;

pub const METHOD_SOURCE: &str = "czsc-thinking+czsc-108";
pub const STRICT_SOURCE: &str = "czsc-108/strict";

fn level_of(freq_name: &str) -> String {
    match freq_name {
        "D" => "日线",
        "W" => "周线",
        "M" => "月线",
        "F1" => "1分钟",
        "F5" => "5分钟",
        "F15" => "15分钟",
        "F30" => "30分钟",
        "F60" => "60分钟",
        other => other,
    }
    .to_string()
}

pub fn level_from_czsc(c: &CZSC) -> String {
    level_of(freq_name(c.freq))
}

// ---------------------------------------------------------------- 一买趋势门

/// `trend_context()` 的返回结构（键名与 Python dict 一致）。
#[derive(Clone, Debug)]
pub struct TrendCtx {
    pub trend_context: String,
    pub trend_basis: Map<String, Value>,
    pub method_ok: bool,
    pub method_label: String,
    pub reason: String,
}

fn zs_refs(ids: &[usize]) -> Value {
    Value::Array(ids.iter().map(|i| Value::from(*i as i64)).collect())
}

/// 一买趋势上下文：中枢内优先判盘整，否则看最后两个合法中枢的末对关系。
pub fn trend_context(point_dt: Option<DateTime<Utc>>, zss: &[ZS]) -> TrendCtx {
    let pdt = point_dt.map(|d| date_str(&d)).unwrap_or_default();

    // 1) 先判「一买在中枢内」——盘整中的背驰不是第一类买卖点
    for (i, z) in zss.iter().enumerate() {
        let sdt = date_str(&z.sdt);
        let edt = date_str(&z.edt);
        if !sdt.is_empty() && !edt.is_empty() && sdt <= pdt && pdt <= edt {
            let mut basis = Map::new();
            basis.insert("zs_count".into(), json!(zss.len()));
            basis.insert("last_pair".into(), Value::Null);
            basis.insert("zs_refs".into(), zs_refs(&[i]));
            basis.insert("inside_zs".into(), Value::Bool(true));
            return TrendCtx {
                trend_context: "盘整".into(),
                trend_basis: basis,
                method_ok: false,
                method_label: "盘整背驰低点".into(),
                reason: format!("一买位于中枢内（{sdt}~{edt}），按盘整背驰处理"),
            };
        }
    }

    // 2) 只看 point.dt 之前已完成的中枢
    let prior: Vec<(usize, &ZS)> = zss
        .iter()
        .enumerate()
        .filter(|(_, z)| !date_str(&z.edt).is_empty() && date_str(&z.edt) < pdt)
        .collect();

    if prior.len() >= 2 {
        let (i1, z1) = prior[prior.len() - 2];
        let (i2, z2) = prior[prior.len() - 1];
        let (z1d, z1g) = (z1.zd, z1.zg);
        let (z2d, z2g) = (z2.zd, z2.zg);
        let mut basis = Map::new();
        basis.insert("zs_count".into(), json!(zss.len()));
        basis.insert("zs_refs".into(), zs_refs(&[i1, i2]));
        if z2g < z1d {
            basis.insert("last_pair".into(), json!("Down"));
            return TrendCtx {
                trend_context: "下跌趋势".into(),
                trend_basis: basis,
                method_ok: true,
                method_label: "一买".into(),
                reason: format!(
                    "末对中枢下跌（ZS#{i2}.ZG {z2g:.2} < ZS#{i1}.ZD {z1d:.2}）"
                ),
            };
        }
        if z2d > z1g {
            basis.insert("last_pair".into(), json!("Up"));
            return TrendCtx {
                trend_context: "上涨趋势".into(),
                trend_basis: basis,
                method_ok: false,
                method_label: "上涨趋势中的笔级背驰低点".into(),
                reason: format!(
                    "末对中枢上涨（ZS#{i2}.ZD {z2d:.2} > ZS#{i1}.ZG {z1g:.2}）"
                ),
            };
        }
        basis.insert("last_pair".into(), json!("Mixed"));
        return TrendCtx {
            trend_context: "盘整".into(),
            trend_basis: basis,
            method_ok: false,
            method_label: "盘整背驰低点".into(),
            reason: "末对中枢区间重叠，不构成下跌趋势".into(),
        };
    }

    if prior.len() == 1 {
        let (i1, _z1) = prior[0];
        let mut basis = Map::new();
        basis.insert("zs_count".into(), json!(zss.len()));
        basis.insert("last_pair".into(), Value::Null);
        basis.insert("zs_refs".into(), zs_refs(&[i1]));
        basis.insert("inside_zs".into(), Value::Bool(false));
        return TrendCtx {
            trend_context: "盘整".into(),
            trend_basis: basis,
            method_ok: false,
            method_label: "盘整背驰低点".into(),
            reason: "point.dt 之前只有 1 个合法中枢，无法构成趋势".into(),
        };
    }

    let mut basis = Map::new();
    basis.insert("zs_count".into(), json!(zss.len()));
    basis.insert("last_pair".into(), Value::Null);
    basis.insert("zs_refs".into(), zs_refs(&[]));
    basis.insert("inside_zs".into(), Value::Bool(false));
    TrendCtx {
        trend_context: "未形成中枢".into(),
        trend_basis: basis,
        method_ok: false,
        method_label: "笔级背驰低点".into(),
        reason: "point.dt 之前没有合法中枢，只有笔级背驰".into(),
    }
}

// ---------------------------------------------------------------- 二买前置一买

/// 二买前置一买的归因结果。
pub struct PriorResult {
    pub prior: Map<String, Value>,
    pub relation: Option<String>,
    pub method_ok: bool,
    pub method_label: String,
    pub reason: String,
}

/// 找二买之前最近的一买事件（ZG-CZSC-CORPUS-20261025 §5.5 归因修正）。
pub fn prior_first_buy(
    point_dt: Option<DateTime<Utc>>,
    point_price: Option<f64>,
    current_bi_index: usize,
    events: &[Event],
    max_bis: Option<usize>,
    point_confirm_dt: Option<DateTime<Utc>>,
) -> PriorResult {
    let max_bis = max_bis.unwrap_or(*MAX_PRIOR_BIS);
    let pdt = point_dt.map(|d| date_str(&d)).unwrap_or_default();
    let cut = point_confirm_dt
        .map(|d| date_str(&d))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| pdt.clone());

    let mut real: Vec<(&Event, bool)> = Vec::new();
    let mut same: Vec<(&Event, bool)> = Vec::new();
    let has_events = !events.is_empty();
    for e in events {
        if e.confirm_dt.is_empty() || e.confirm_dt > cut {
            continue;
        }
        if e.bi_index < current_bi_index {
            real.push((e, false));
        } else if e.bi_index == current_bi_index && e.pivot_dt == pdt {
            same.push((e, true));
        }
    }

    // 真前置优先；仅当不存在真前置才落到同点共燃。
    let cands = if !real.is_empty() { real } else { same };

    let empty_prior = |coverage: &str, label: &str, reason: String| PriorResult {
        prior: {
            let mut m = Map::new();
            m.insert("exists".into(), Value::Bool(false));
            m.insert("same_point".into(), Value::Bool(false));
            m.insert("coverage".into(), json!(coverage));
            m.insert("pivot_dt".into(), Value::Null);
            m.insert("confirm_dt".into(), Value::Null);
            m.insert("low".into(), Value::Null);
            m.insert("distance_bis".into(), Value::Null);
            m.insert("source".into(), json!("current_structure_replay"));
            m
        },
        relation: Some("无前置".into()),
        method_ok: false,
        method_label: label.into(),
        reason,
    };

    if cands.is_empty() {
        let coverage = if !has_events {
            "empty_replay"
        } else {
            "no_prior_in_window"
        };
        let label = if coverage == "empty_replay" {
            "类二买（事件表无覆盖）"
        } else {
            "类二买"
        };
        let mut reason =
            "当前结构回放未找到 confirm_dt <= 二买确认日的前置一买".to_string();
        if !has_events {
            reason.push_str("（回放窗口内无一买事件）");
        }
        return empty_prior(coverage, label, reason);
    }

    // max(cands, key=(bi_index, confirm_dt))：Python max 取首个最大值。
    let mut best: (&Event, bool) = cands[0];
    for c in cands.iter().skip(1) {
        if (c.0.bi_index, &c.0.confirm_dt) > (best.0.bi_index, &best.0.confirm_dt) {
            best = *c;
        }
    }
    let (e, same_point) = best;
    let dist = current_bi_index as i64 - e.bi_index as i64;
    let relation = match (point_price, Some(e.low)) {
        (Some(p), Some(low)) => Some(if p >= low { "不破" } else { "跌破" }.to_string()),
        _ => None,
    };

    let mut prior = Map::new();
    prior.insert("exists".into(), Value::Bool(true));
    prior.insert("same_point".into(), Value::Bool(same_point));
    prior.insert("pivot_dt".into(), json!(e.pivot_dt));
    prior.insert("confirm_dt".into(), json!(e.confirm_dt));
    prior.insert("low".into(), json!(e.low));
    prior.insert("distance_bis".into(), json!(dist));
    prior.insert("source".into(), json!("current_structure_replay"));

    if same_point {
        return PriorResult {
            prior,
            relation,
            method_ok: false,
            method_label: "类二买（同点）".into(),
            reason: format!(
                "官方一买/二买落在同一末笔（枢轴 {}），无中间回升笔，不构成二买",
                e.pivot_dt
            ),
        };
    }
    if dist <= max_bis as i64 {
        return PriorResult {
            prior,
            relation,
            method_ok: true,
            method_label: "二买".into(),
            reason: format!(
                "前置一买 {}（确认 {}，距今 {} 笔）",
                e.pivot_dt, e.confirm_dt, dist
            ),
        };
    }
    PriorResult {
        prior,
        relation,
        method_ok: false,
        method_label: "类二买（前置过远）".into(),
        reason: format!("前置一买距今 {dist} 笔 > {max_bis}，不视为同级别前置"),
    }
}

// ---------------------------------------------------------------- 止损参考

fn r3_json(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(py_round(x, 3)),
        None => Value::Null,
    }
}

/// L3 止损参考（reference，非官方止损）。
pub fn stop_ref(
    p: &BuySellPoint,
    prior: Option<&Map<String, Value>>,
    strict_zs_zg: Option<f64>,
) -> Map<String, Value> {
    if p.direction != "买" {
        return Map::new();
    }
    let ptype = p.point_type.as_str();
    let price = p.price;

    let (price_v, basis) = if ptype == "第一类" {
        (r3_json(price), "一买低点".to_string())
    } else if ptype == "第二类" {
        let prior = prior.cloned().unwrap_or_default();
        let exists = prior.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
        let low = prior.get("low").and_then(|v| v.as_f64());
        if exists {
            if let Some(pl) = low {
                if price.is_some() && price.unwrap() >= pl {
                    (r3_json(Some(pl)), "前置一买低点".to_string())
                } else {
                    (
                        r3_json(price),
                        "二买回试低点（已跌破前置一买低点，原止损失效）".to_string(),
                    )
                }
            } else {
                (r3_json(price), "二买回试低点（无前置，降级）".to_string())
            }
        } else {
            (r3_json(price), "二买回试低点（无前置，降级）".to_string())
        }
    } else if ptype == "第三类" {
        if let Some(zg) = strict_zs_zg {
            (r3_json(Some(zg)), "三买 ZG（严格模式）".to_string())
        } else {
            (r3_json(price), "回试低点（官方局部中枢）".to_string())
        }
    } else {
        return Map::new();
    };

    let mut out = Map::new();
    out.insert("price".into(), price_v);
    out.insert("basis".into(), json!(basis));
    out.insert("source".into(), json!("reference"));
    out.insert("note".into(), json!("非官方止损，仅供风控参考"));
    out
}

// ---------------------------------------------------------------- 三买严格模式

pub struct StrictResult {
    pub strict: Map<String, Value>,
    pub method_ok: bool,
    pub method_label: String,
    pub reason: String,
}

fn strict_base(enabled: bool) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("enabled".into(), Value::Bool(enabled));
    m.insert("valid_zs".into(), Value::Null);
    m.insert("first_retest".into(), Value::Null);
    m.insert("zs_zg".into(), Value::Null);
    m.insert("zs_ref".into(), Value::Null);
    m
}

/// 三买严格模式：完整合法中枢 + 首次回试。默认关闭。
pub fn third_buy_strict(c: &CZSC, zss: &[ZS], enabled: Option<bool>) -> StrictResult {
    let enabled = enabled.unwrap_or(*THINKING_THIRD_STRICT);
    if !enabled {
        return StrictResult {
            strict: strict_base(false),
            method_ok: true,
            method_label: "三买".into(),
            reason: "官方局部 5 笔中枢；严格模式关闭（czsc-108/strict 未启用）".into(),
        };
    }

    let bis: &[BI] = &c.bi_list;
    let Some(b5) = bis.last() else {
        return StrictResult {
            strict: strict_base(true),
            method_ok: false,
            method_label: "中枢上沿回试".into(),
            reason: "无笔数据".into(),
        };
    };
    if b5.direction != czsc_core::objects::direction::Direction::Down {
        return StrictResult {
            strict: strict_base(true),
            method_ok: false,
            method_label: "中枢上沿回试".into(),
            reason: "末笔不是向下笔，不满足三买前提".into(),
        };
    }

    // 中枢必须在 b5 起点之前结束（b5 是离开中枢后的回试笔）
    let b5_start = date_str(&b5.start_dt());
    let cands: Vec<(usize, &ZS)> = zss
        .iter()
        .enumerate()
        .filter(|(_, z)| !date_str(&z.edt).is_empty() && date_str(&z.edt) <= b5_start)
        .collect();
    let Some((i, z)) = cands.last().copied() else {
        return StrictResult {
            strict: strict_base(true),
            method_ok: false,
            method_label: "中枢上沿回试".into(),
            reason: "b5 之前没有已完成合法中枢".into(),
        };
    };
    let zg = z.zg;
    let b5_low = b5.get_low();
    let valid = b5_low > zg;

    // 首次回试：b5 必须是中枢结束后的第一笔向下笔
    let z_edt = date_str(&z.edt);
    let mut first = true;
    let last_idx = bis.len() - 1;
    for (idx, b) in bis.iter().enumerate() {
        if date_str(&b.start_dt()) < z_edt {
            continue;
        }
        if idx == last_idx {
            break;
        }
        if b.direction == czsc_core::objects::direction::Direction::Down {
            first = false;
            break;
        }
    }

    let mut strict = Map::new();
    strict.insert("enabled".into(), Value::Bool(true));
    strict.insert("valid_zs".into(), Value::Bool(valid));
    strict.insert("first_retest".into(), Value::Bool(first));
    strict.insert("zs_zg".into(), r3_json(Some(zg)));
    strict.insert("zs_ref".into(), json!(i as i64));
    if valid && first {
        return StrictResult {
            strict,
            method_ok: true,
            method_label: "三买（完整中枢）".into(),
            reason: format!("完整中枢 ZG={zg:.2} 之上回试，且为首次回试"),
        };
    }
    let mut reasons: Vec<String> = Vec::new();
    if !valid {
        reasons.push(format!("回试低点 {b5_low:.2} <= 中枢 ZG {zg:.2}"));
    }
    if !first {
        reasons.push("不是中枢结束后的首次向下笔".to_string());
    }
    let reason = if reasons.is_empty() {
        "严格模式未通过".to_string()
    } else {
        reasons.join("；")
    };
    StrictResult {
        strict,
        method_ok: false,
        method_label: "中枢上沿回试".into(),
        reason,
    }
}

// ---------------------------------------------------------------- L3 其余

/// 一买低点之后首个向下笔是否跌破一买低点。
pub fn anchor_confirmation(c: &CZSC, p: &BuySellPoint, current_bi_index: usize) -> String {
    let Some(price) = p.price else {
        return "未确认".into();
    };
    let bis = &c.bi_list;
    if current_bi_index + 2 >= bis.len() {
        return "未确认".into();
    }
    let nxt = &bis[current_bi_index + 2];
    if nxt.direction != czsc_core::objects::direction::Direction::Down {
        return "未确认".into();
    }
    if nxt.get_low() >= price {
        "已确认".into()
    } else {
        "未确认（跌破）".into()
    }
}

/// 一买 MACD 第二意见：一致/冲突/无；二买/三买不适用。
pub fn macd_agreement(p: &BuySellPoint, divergence: Option<&DivergenceResult>) -> Option<String> {
    if p.point_type != "第一类" || p.direction != "买" {
        return None;
    }
    let Some(dv) = divergence else {
        return Some("无".into());
    };
    match dv.macd_confirm {
        None => Some("无".into()),
        Some(true) => Some("一致".into()),
        Some(false) => Some("冲突".into()),
    }
}

pub fn macd_line_note() -> &'static str {
    "L3 描述，不参与 method_ok；area_* 用同向笔窗口，与 macd_agreement（中枢/离开段窗口）不同，禁止联立"
}

fn r4_json(v: Option<f64>) -> Value {
    match v {
        Some(x) => json!(py_round(x, 4)),
        None => Value::Null,
    }
}

fn vs_zero(v: Option<f64>) -> Value {
    match v {
        None => Value::Null,
        Some(x) if x < -1e-9 => json!("below"),
        Some(x) if x > 1e-9 => json!("above"),
        Some(_) => json!("at"),
    }
}

/// `_macd_df(c)`：全历史收盘 → MACD 表（<35 根视为数据不足）。
pub fn macd_df(c: &CZSC) -> Option<MacdFrame> {
    if c.bars_raw.len() < 35 {
        return None;
    }
    let bars: Vec<(DateTime<Utc>, f64)> = c.bars_raw.iter().map(|b| (b.dt, b.close)).collect();
    Some(crate::macd::frame_from_bars(&bars))
}

/// 买点的 MACD 线轨字段（工单 §5.3 数据契约）。
pub fn macd_line_for(
    c: &CZSC,
    p: &BuySellPoint,
    prior: Option<&Map<String, Value>>,
    df: Option<&MacdFrame>,
) -> Option<Map<String, Value>> {
    if !*THINKING_MACD_LINE {
        return None;
    }
    if p.direction != "买" {
        return None;
    }
    let ptype = p.point_type.as_str();
    let bis = &c.bi_list;
    if bis.is_empty() || !matches!(ptype, "第一类" | "第二类" | "第三类") {
        return None;
    }
    let owned;
    let df: &MacdFrame = match df {
        Some(d) => d,
        None => {
            owned = macd_df(c)?;
            &owned
        }
    };

    let anchor = bis.last()?; // 官方 di=1 ⇒ 所有买点锚在末笔上
    let a_dt = anchor.fx_b.dt;
    let dif = macd_at(df, "dif", Some(a_dt));
    let dea = macd_at(df, "dea", Some(a_dt));

    let mut out = Map::new();
    out.insert("ver".into(), json!(1));
    out.insert("dif".into(), r4_json(dif));
    out.insert("dea".into(), r4_json(dea));
    out.insert("dif_prev".into(), Value::Null);
    out.insert("dea_prev".into(), Value::Null);
    out.insert("prev_dt".into(), Value::Null);
    out.insert("prev_basis".into(), Value::Null);
    out.insert("dif_higher_low".into(), Value::Null);
    out.insert("dea_higher_low".into(), Value::Null);
    out.insert("line_higher_low".into(), Value::Null);
    out.insert("dif_vs_zero".into(), vs_zero(dif));
    out.insert("zero_cross_since_prior".into(), Value::Null);
    out.insert("area_now".into(), Value::Null);
    out.insert("area_prev".into(), Value::Null);
    out.insert("area_ok".into(), Value::Null);
    out.insert("note".into(), json!(macd_line_note()));

    // 线轨：与前一根同向笔比（三买不比——回试低点语义不同）
    if matches!(ptype, "第一类" | "第二类") && bis.len() >= 3 {
        let prev = &bis[bis.len() - 3];
        if dir_name(prev.direction) == dir_name(anchor.direction) {
            let p_dt = prev.fx_b.dt;
            let dp = macd_at(df, "dif", Some(p_dt));
            let ep = macd_at(df, "dea", Some(p_dt));
            out.insert("dif_prev".into(), r4_json(dp));
            out.insert("dea_prev".into(), r4_json(ep));
            out.insert(
                "prev_dt".into(),
                if p_dt.to_string().is_empty() {
                    Value::Null
                } else {
                    json!(date_str(&p_dt))
                },
            );
            out.insert("prev_basis".into(), json!("adjacent_same_direction_bi"));
            let dif_hl = match (dif, dp) {
                (Some(a), Some(b)) => Some(a >= b),
                _ => None,
            };
            let dea_hl = match (dea, ep) {
                (Some(a), Some(b)) => Some(a >= b),
                _ => None,
            };
            out.insert("dif_higher_low".into(), json!(dif_hl));
            out.insert("dea_higher_low".into(), json!(dea_hl));
            if let (Some(a), Some(b)) = (dif_hl, dea_hl) {
                out.insert("line_higher_low".into(), Value::Bool(a && b));
            }
        }
    }

    // 面积轨（一买）：同向笔窗口的绿柱面积，复用 divergence 实现
    let down = czsc_core::objects::direction::Direction::Down;
    if ptype == "第一类"
        && bis.len() >= 3
        && anchor.direction == down
        && bis[bis.len() - 3].direction == down
    {
        let a_now = segment_macd_area(df, Some(anchor.start_dt()), Some(anchor.end_dt()), Some("Down"));
        let prev = &bis[bis.len() - 3];
        let a_prev =
            segment_macd_area(df, Some(prev.start_dt()), Some(prev.end_dt()), Some("Down"));
        out.insert("area_now".into(), r3_json(a_now));
        out.insert("area_prev".into(), r3_json(a_prev));
        if let (Some(a), Some(b)) = (a_now, a_prev) {
            out.insert("area_ok".into(), Value::Bool(a < b));
        }
    }

    // 二买 0 轴相位：前置一买确认日 → 二买枢轴日窗口内 DIF 由负转正
    if ptype == "第二类" {
        let pr = prior.cloned().unwrap_or_default();
        let exists = pr.get("exists").and_then(|v| v.as_bool()).unwrap_or(false);
        if exists {
            let mut s = pr
                .get("confirm_dt")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string())
                .filter(|x| !x.is_empty())
                .or_else(|| {
                    pr.get("pivot_dt")
                        .and_then(|v| v.as_str())
                        .map(|x| x.to_string())
                        .filter(|x| !x.is_empty())
                });
            let pdt = p.dt.map(|d| date_str(&d)).unwrap_or_default();
            // 同点共燃时前置一买确认日会晚于二买枢轴日 → 退回前置枢轴日
            if let Some(cur) = s.clone() {
                if !pdt.is_empty() && cur > pdt {
                    s = pr
                        .get("pivot_dt")
                        .and_then(|v| v.as_str())
                        .map(|x| x.to_string())
                        .or(Some(cur));
                }
            }
            let sdt = s.as_deref().and_then(parse_date_str);
            out.insert(
                "zero_cross_since_prior".into(),
                match crate::macd::zero_cross(df, sdt, p.dt) {
                    Some(v) => Value::Bool(v),
                    None => Value::Null,
                },
            );
        }
    }

    Some(out)
}

fn parse_date_str(s: &str) -> Option<DateTime<Utc>> {
    let head: String = s.chars().take(10).collect();
    chrono::NaiveDate::parse_from_str(&head, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| DateTime::<Utc>::from_naive_utc_and_offset(d, Utc))
}

// ---------------------------------------------------------------- 主编排

fn disabled_strict() -> Map<String, Value> {
    strict_base(false)
}

struct Ctx<'a> {
    c: &'a CZSC,
    zss: &'a [ZS],
    divergence: Option<&'a DivergenceResult>,
    events: &'a [Event],
    event_error: &'a str,
    current_idx: usize,
    level: &'a str,
    ml_df: Option<&'a MacdFrame>,
}

/// 给单个 BuySellPoint 填 L2/L3 字段。
fn qualify_one(ctx: &Ctx, p: &mut BuySellPoint) {
    if p.level.is_empty() {
        p.level = ctx.level.to_string();
    }
    if p.source_method.is_empty() {
        p.source_method = METHOD_SOURCE.to_string();
    }

    if p.direction != "买" {
        p.method_label = p.point_type.clone();
        let mut q = Map::new();
        q.insert("method_ok".into(), Value::Bool(true));
        q.insert("method_ver".into(), json!(METHOD_VER));
        q.insert("rules".into(), json!([]));
        q.insert(
            "reasons".into(),
            json!(["卖出点未接入方法论裁定（L2 只覆盖买点）"]),
        );
        q.insert("macd_line".into(), Value::Null);
        p.qualification = q;
        p.stop_ref = Map::new();
        p.source_method = "official".to_string();
        return;
    }

    match p.point_type.as_str() {
        "第一类" => {
            let ctxr = trend_context(p.dt, ctx.zss);
            p.method_label = ctxr.method_label.clone();
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Bool(ctxr.method_ok));
            q.insert("method_ver".into(), json!(METHOD_VER));
            q.insert(
                "rules".into(),
                json!([
                    "czsc-thinking:一买需下跌趋势",
                    "czsc-108:144,150（盘整中不存在第一类买卖点）"
                ]),
            );
            q.insert("trend_context".into(), json!(ctxr.trend_context));
            q.insert("trend_basis".into(), Value::Object(ctxr.trend_basis));
            q.insert("prior_first_buy".into(), Value::Null);
            q.insert("prior_low_relation".into(), Value::Null);
            q.insert(
                "anchor_confirmation".into(),
                json!(anchor_confirmation(ctx.c, p, ctx.current_idx)),
            );
            q.insert(
                "macd_agreement".into(),
                match macd_agreement(p, ctx.divergence) {
                    Some(s) => json!(s),
                    None => Value::Null,
                },
            );
            q.insert(
                "macd_line".into(),
                match macd_line_for(ctx.c, p, None, ctx.ml_df) {
                    Some(m) => Value::Object(m),
                    None => Value::Null,
                },
            );
            q.insert("third_buy_strict".into(), Value::Object(disabled_strict()));
            q.insert("reasons".into(), json!([ctxr.reason]));
            p.qualification = q;
            p.stop_ref = stop_ref(p, None, None);
            p.source_method = METHOD_SOURCE.to_string();
        }
        "第二类" => {
            let pr = prior_first_buy(
                p.dt,
                p.price,
                ctx.current_idx,
                ctx.events,
                None,
                p.confirm_dt,
            );
            p.method_label = pr.method_label.clone();
            let mut reasons: Vec<String> = vec![pr.reason.clone()];
            if pr.relation.as_deref() == Some("跌破") {
                reasons.push("最弱二买（跌破前置一买低点）".to_string());
            }
            if !ctx.event_error.is_empty() {
                reasons.push(format!("一买事件回放失败：{}", ctx.event_error));
            }
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Bool(pr.method_ok));
            q.insert("method_ver".into(), json!(METHOD_VER));
            q.insert(
                "rules".into(),
                json!([
                    "czsc-thinking:二买需同级别前置一买",
                    "czsc-108:32-33（不破前低=一般性）"
                ]),
            );
            q.insert("trend_context".into(), Value::Null);
            q.insert("trend_basis".into(), Value::Null);
            q.insert("prior_first_buy".into(), Value::Object(pr.prior.clone()));
            q.insert(
                "prior_low_relation".into(),
                match &pr.relation {
                    Some(s) => json!(s),
                    None => Value::Null,
                },
            );
            q.insert("anchor_confirmation".into(), Value::Null);
            q.insert("macd_agreement".into(), Value::Null);
            q.insert(
                "macd_line".into(),
                match macd_line_for(ctx.c, p, Some(&pr.prior), ctx.ml_df) {
                    Some(m) => Value::Object(m),
                    None => Value::Null,
                },
            );
            q.insert("third_buy_strict".into(), Value::Object(disabled_strict()));
            q.insert("reasons".into(), json!(reasons));
            p.qualification = q;
            p.stop_ref = stop_ref(p, Some(&pr.prior), None);
            p.source_method = METHOD_SOURCE.to_string();
        }
        "第三类" => {
            let sr = third_buy_strict(ctx.c, ctx.zss, None);
            p.method_label = sr.method_label.clone();
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Bool(sr.method_ok));
            q.insert("method_ver".into(), json!(METHOD_VER));
            q.insert(
                "rules".into(),
                json!([
                    "czsc-thinking:三买=向上离开中枢后的回调点",
                    "czsc-108:38（完整中枢+首次回试，仅严格模式）"
                ]),
            );
            q.insert("trend_context".into(), Value::Null);
            q.insert("trend_basis".into(), Value::Null);
            q.insert("prior_first_buy".into(), Value::Null);
            q.insert("prior_low_relation".into(), Value::Null);
            q.insert("anchor_confirmation".into(), Value::Null);
            q.insert("macd_agreement".into(), Value::Null);
            q.insert(
                "macd_line".into(),
                match macd_line_for(ctx.c, p, None, ctx.ml_df) {
                    Some(m) => Value::Object(m),
                    None => Value::Null,
                },
            );
            let strict_enabled = sr
                .strict
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let strict_valid = sr
                .strict
                .get("valid_zs")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let strict_zg = sr.strict.get("zs_zg").and_then(|v| v.as_f64());
            q.insert("third_buy_strict".into(), Value::Object(sr.strict.clone()));
            q.insert("reasons".into(), json!([sr.reason]));
            p.qualification = q;

            // 严格模式未通过（回试低点 <= ZG）时，ZG 在买点上方，不能当止损。
            if strict_enabled && !strict_valid {
                p.stop_ref = stop_ref(p, None, None);
                p.stop_ref
                    .insert("basis".into(), json!("回试低点（严格模式未通过）"));
            } else {
                p.stop_ref = stop_ref(p, None, strict_zg);
            }
            p.source_method = if strict_enabled {
                STRICT_SOURCE.to_string()
            } else {
                METHOD_SOURCE.to_string()
            };
        }
        _ => {
            p.method_label = p.point_type.clone();
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Bool(true));
            q.insert("method_ver".into(), json!(METHOD_VER));
            q.insert("rules".into(), json!([]));
            q.insert("reasons".into(), json!(["未知买点类型，未接入 L2"]));
            q.insert("macd_line".into(), Value::Null);
            p.qualification = q;
            p.stop_ref = Map::new();
        }
    }
}

/// 给 BuySellPoint 填 L2/L3 字段；原地修改。
pub fn qualify_points(
    c: &CZSC,
    points: &mut [BuySellPoint],
    zss: Option<&[ZS]>,
    divergence: Option<&DivergenceResult>,
    events: Option<&[Event]>,
) {
    if !*THINKING_LAYER {
        return;
    }
    let owned;
    let zss: &[ZS] = match zss {
        Some(v) => v,
        None => {
            owned = valid_zs_list(c, None);
            &owned
        }
    };

    // 只有出现二买买点时才回放一买事件
    let mut event_error = String::new();
    let owned_events;
    let events: &[Event] = match events {
        Some(e) => e,
        None => {
            let need = points.iter().any(|p| {
                p.point_type == "第二类" && p.direction == "买"
            });
            if need {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay(c)));
                match outcome {
                    Ok(v) => {
                        owned_events = v;
                        &owned_events
                    }
                    Err(payload) => {
                        event_error = format!(
                            "PanicException: {}",
                            crate::official::panic_text(payload.as_ref())
                        );
                        owned_events = Vec::new();
                        &owned_events
                    }
                }
            } else {
                owned_events = Vec::new();
                &owned_events
            }
        }
    };

    let current_idx = c.bi_list.len().saturating_sub(1);
    let level = level_from_czsc(c);
    // macd_line 的指标序列整轮算一次（有买点才建）
    let ml_df = if *THINKING_MACD_LINE && !points.is_empty() {
        macd_df(c)
    } else {
        None
    };

    let ctx = Ctx {
        c,
        zss,
        divergence,
        events,
        event_error: &event_error,
        current_idx,
        level: &level,
        ml_df: ml_df.as_ref(),
    };

    for p in points.iter_mut() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            qualify_one(&ctx, p)
        }));
        if let Err(payload) = outcome {
            let msg = format!(
                "PanicException: {}",
                crate::official::panic_text(payload.as_ref())
            );
            let mut q = Map::new();
            q.insert("method_ok".into(), Value::Null);
            q.insert("method_ver".into(), json!(METHOD_VER));
            q.insert("error".into(), json!(msg));
            q.insert("reasons".into(), json!(["L2 计算异常，官方判据不受影响"]));
            q.insert("macd_line".into(), Value::Null);
            p.qualification = q;
            if p.method_label.is_empty() {
                p.method_label = p.point_type.clone();
            }
            if p.level.is_empty() {
                p.level = level.clone();
            }
            if p.source_method.is_empty() {
                p.source_method = METHOD_SOURCE.to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use czsc_core::objects::bar::RawBar;
    use czsc_core::objects::freq::Freq;

    fn czsc_with(n: usize) -> CZSC {
        let bars: Vec<RawBar> = (0..n)
            .map(|i| {
                let base = 10.0 + ((i as f64) * 0.7).sin() * 2.0;
                RawBar {
                    symbol: "test".into(),
                    dt: Utc.timestamp_opt(1_600_000_000 + i as i64 * 86_400, 0).unwrap(),
                    freq: Freq::D,
                    id: i as i32,
                    open: base,
                    close: base + 0.1,
                    high: base + 0.3,
                    low: base - 0.3,
                    vol: 1000.0,
                    amount: 10_000.0,
                }
            })
            .collect();
        CZSC::new(bars, 50, 6)
    }

    #[test]
    fn trend_context_no_zs() {
        let ctx = trend_context(None, &[]);
        assert_eq!(ctx.trend_context, "未形成中枢");
        assert_eq!(ctx.method_label, "笔级背驰低点");
        assert!(!ctx.method_ok);
        assert_eq!(ctx.reason, "point.dt 之前没有合法中枢，只有笔级背驰");
    }

    #[test]
    fn strict_disabled_default() {
        let c = czsc_with(80);
        let zss = valid_zs_list(&c, None);
        let r = third_buy_strict(&c, &zss, Some(false));
        assert!(r.method_ok);
        assert_eq!(r.method_label, "三买");
        assert_eq!(r.strict.get("enabled").unwrap(), &Value::Bool(false));
    }

    #[test]
    fn level_mapping() {
        assert_eq!(level_of("D"), "日线");
        assert_eq!(level_of("F30"), "30分钟");
        assert_eq!(level_of("X"), "X");
    }

    #[test]
    fn macd_line_note_text() {
        assert!(macd_line_note().starts_with("L3 描述，不参与 method_ok"));
    }

    #[test]
    fn qualify_no_points_is_noop() {
        let c = czsc_with(80);
        let mut pts: Vec<BuySellPoint> = Vec::new();
        qualify_points(&c, &mut pts, None, None, None);
        assert!(pts.is_empty());
    }
}
