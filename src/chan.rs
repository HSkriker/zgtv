use serde_json::{json, Value};
use std::sync::Arc;

use czsc_core::analyze::CZSC;
use czsc_core::objects::bar::RawBar;
use czsc_core::objects::freq::Freq;
use zg_chan::payload::build_payload_from_bars;
use zg_chan::service::{InlineBar, MIN_BI_LEN, MIN_BARS};
use zg_core::reader::{max_bi_num_for, resolve};

fn freq_of(period: &str) -> Option<&'static str> {
    match period {
        "day" => Some("D"),
        "week" => Some("W"),
        "month" => Some("M"),
        "quarter" => Some("S"),
        "1" => Some("F1"),
        "5" => Some("F5"),
        "15" => Some("F15"),
        "30" => Some("F30"),
        "60" => Some("F60"),
        _ => None,
    }
}

/// 季线缠论专用：zg-chan 的 freq 表只认 D/W/M/F*（红线：不改 zg），而笔 / 中枢算法与
/// `RawBar.freq` 标签无关，但 `qualify_points` 会按 `c.freq` 给每个买卖点写级别名
/// （"月线三买" 这类文案）——所以季线不能借标成 M 喂进去，必须以 `Freq::S` 自建 RawBar，
/// 再调 zg_chan 公开的 `assemble`；MIN_BARS / MIN_BI_LEN / id=0 / 稳定排序与
/// `build_payload_from_bars` 逐项同构，因此除 freq 标签外与上游链路结果全等。
fn payload_labeled(rows: &[crate::bars::Agg], cd: &str, mkt: &str, label: &str) -> Result<Value, String> {
    let cn = if label == "S" { "季线" } else { "年线" };
    if rows.len() < MIN_BARS {
        return Err(format!("{cn}仅{}根，不足{}根，缠论无法计算（上市时间太短）", rows.len(), MIN_BARS));
    }
    let freq = if label == "S" { Freq::S } else { Freq::Y };
    let mut raw: Vec<RawBar> = Vec::with_capacity(rows.len());
    for b in rows {
        let dt = zg_chan::service::parse_dt(&b.date).ok_or_else(|| format!("无法解析日期: {}", b.date))?;
        raw.push(RawBar {
            symbol: Arc::from(cd),
            dt,
            freq,
            id: 0, // 与 zg-chan 同：生产链路 RawBar.id 恒 0
            open: b.open,
            close: b.close,
            high: b.high,
            low: b.low,
            vol: b.vol,
            amount: b.amount,
        });
    }
    let t0 = std::time::Instant::now();
    let c = CZSC::new(raw.clone(), max_bi_num_for(label), MIN_BI_LEN);
    let ms = t0.elapsed().as_millis() as i64;
    let mut v = zg_chan::payload::assemble(&c, &raw, label, cd, mkt, ms);
    if let Some(o) = v.as_object_mut() {
        o.remove("bars");
    }
    rewrite_times(&mut v, false);
    Ok(v)
}

/// 统一入口：与图表同源的 Agg 行 → 缠论 payload（按笔数上限截窗）。
fn payload_from_rows(
    rows: &[crate::bars::Agg],
    cd: &str,
    mkt: &str,
    freq: &str,
) -> Result<Value, String> {
    if rows.len() < MIN_BARS {
        return Err(format!("仅{}根，不足{}根，缠论无法计算（上市时间太短）", rows.len(), MIN_BARS));
    }
    let cap = max_bi_num_for(freq) * MIN_BI_LEN;
    let rows = if rows.len() > cap { &rows[rows.len() - cap..] } else { rows };
    let inline: Vec<InlineBar> = rows
        .iter()
        .map(|b| InlineBar {
            date: b.date.clone(),
            open: b.open,
            close: b.close,
            high: b.high,
            low: b.low,
            vol: b.vol,
            amount: b.amount,
        })
        .collect();
    let mut v = build_payload_from_bars(cd, freq, &inline, Some(mkt))?;
    if let Some(o) = v.as_object_mut() {
        o.remove("bars");
    }
    Ok(v)
}

pub fn payload(code: &str, period: &str) -> Result<Value, String> {
    let (mkt, cd) = resolve(code, None).map_err(|e| e.to_string())?;
    let period = period.trim().to_lowercase();
    if period == "year" {
        return Err("年线最多 29 根，缠论需 ≥60 根才可算（无更长历史）".to_string());
    }
    if period == "quarter" {
        let s = crate::live::day_series(code, "quarter", 0)?;
        return payload_labeled(&s.rows, &cd, &mkt, "S");
    }
    let freq = freq_of(&period).ok_or_else(|| format!("不支持的周期: {period}"))?;
    let minutes: Option<u32> = period.parse().ok();
    let rows = if minutes.is_some() {
        crate::live::minute_series(code, &period, chan_depth(&period))?.rows
    } else {
        crate::live::day_series(code, &period, 0)?.rows
    };
    let mut v = payload_from_rows(&rows, &cd, &mkt, freq)?;
    rewrite_times(&mut v, minutes.is_some());
    Ok(v)
}

/// 各周期的缠论取数上限（0 = 全量）。日线及以上不受限（一次计算覆盖全历史，笔数由
/// czsc 的 max_bi_num 截断）；分钟只取本地图能给的深度。
pub fn chan_depth(period: &str) -> usize {
    match period {
        "1" => 120 * 240,
        "5" => 120 * 48,
        "15" => 120 * 16,
        "30" => 120 * 8,
        "60" => 120 * 4,
        _ => 0,
    }
}

/// 板块/指数缠论：与板块图同源（本地新鲜→本地，否则 TQ/腾讯），不再单独读 vipdoc。
pub fn payload_board(code: &str, period: &str) -> Result<Value, String> {
    let (market, _name, resolved) =
        crate::board::resolve_code(code.trim()).ok_or_else(|| "未找到该板块/指数代码".to_string())?;
    let p = period.trim().to_lowercase();
    if p == "year" {
        return Err("年线最多 29 根，缠论需 ≥60 根才可算（无更长历史）".to_string());
    }
    if !matches!(p.as_str(), "day" | "week" | "month" | "quarter") {
        return Err("板块/指数缠论支持日 / 周 / 月 / 季".to_string());
    }
    let mkt = if market.is_empty() { "sh" } else { market };
    let s = crate::board::daily_rows(code, &p, 0)?;
    if p == "quarter" {
        return payload_labeled(&s.rows, &resolved, mkt, "S");
    }
    let freq = freq_of(&p).ok_or_else(|| format!("不支持的周期: {p}"))?;
    // 与个股同深度：全历史进缠论；上限按笔数表不变式收紧。
    let mut v = payload_from_rows(&s.rows, &resolved, mkt, freq)?;
    rewrite_times(&mut v, false);
    Ok(v)
}

fn rewrite_times(v: &mut Value, intraday: bool) {
    for key in ["bis", "zs", "bs"] {
        let Some(arr) = v.get_mut(key).and_then(|x| x.as_array_mut()) else { continue };
        for item in arr {
            for field in ["sdt", "edt", "dt", "confirm_dt"] {
                let Some(s) = item.get(field).and_then(|x| x.as_str()).map(str::to_string) else {
                    continue;
                };
                item[field] = to_time(&s, intraday);
            }
        }
    }
}

fn to_time(s: &str, intraday: bool) -> Value {
    if !intraday {
        return Value::String(s.to_string());
    }
    // 与 bars.rs 分钟行共用同一份 +08:00 解析（支持带秒/纯日期兜底）
    match crate::bars::bj_epoch(s) {
        Some(ts) => Value::from(ts),
        None => Value::Null,
    }
}

/// 健康检查用：只返回计数，不返回明细
#[allow(dead_code)] // TV-30 终检会用到
pub fn summary(v: &Value) -> Value {
    let n = |k: &str| v.get(k).and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
    json!({"bis": n("bis"), "zs": n("zs"), "bs": n("bs")})
}
