use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::{json, Value};

use zg_core::reader::resolve;

/// 统一后的 K 线行（日/周/月来自 zg-core，季/年在本模块重采样）。
#[derive(Clone, Debug)]
pub struct Agg {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub vol: f64,
    pub amount: f64,
}

/// date/open/high/low/close/vol/amount 七元组，季/年重采样的输入形态。
pub type Row7 = (String, f64, f64, f64, f64, f64, f64);

/// 全市场交易日基准 = sh000001 末根日期；本地重算的「过期」判定都拿它对表。
pub fn market_last_date() -> Option<String> {
    // 4 根缓冲：末行偶尔是 TDX 写盘中的脏行；parse_day_rows 会跳过非法日期
    zg_core::api::read_day_bars_tail("sh", "000001", 4)
        .ok()
        .and_then(|b| b.last().map(|x| x.date.clone()))
}

/// 交易日基准文件路径（热度缓存的 ts-vs-mtime 过期判定用）
pub fn market_axis_path() -> std::path::PathBuf {
    zg_core::reader::day_path("sh", "000001")
}

/// 本地行情版本：交易日 + 三市场基准文件指纹（缓存失效用）。
///
/// **不要用 lday 目录 mtime**：直读 DFZQ 后 TDX 往已有 .day 里追加/原地改写，
/// 不新建目录项 ⇒ 目录 mtime 不更新（2026-11-06 实测目录 16:07:48 / 文件 16:08:01）。
pub fn market_stamp() -> String {
    let stamp = |m: &str, c: &str| {
        crate::cache::file_stamp(&zg_core::reader::day_path(m, c))
    };
    format!(
        "{}|{}|{}|{}",
        market_last_date().unwrap_or_default(),
        stamp("sh", "000001"),
        stamp("sz", "399001"),
        stamp("bj", "899050"),
    )
}

fn bucket(date: &str, period: &str) -> String {
    let y = date.get(0..4).unwrap_or("0000");
    if period == "year" {
        return y.to_string();
    }
    let m: i32 = date.get(5..7).and_then(|v| v.parse().ok()).unwrap_or(0);
    format!("{y}-Q{}", (m - 1) / 3 + 1)
}

/// 日线 → 季/年。zg-core 的 `resample_dwm` 只到周/月（且不许改），这里按它的口径补齐：
/// 桶 = 自然季 / 自然年，标签 = 桶内最后一个交易日，OHLC 取首开 / 最高 / 最低 / 末收，量额求和。
/// 输入若已是 `chart_bars` 的 3 位小数价，max/min 与取首末在单调舍入下与「先聚合后舍入」同值。
pub fn resample_qy(rows: &[Row7], period: &str) -> Vec<Agg> {
    let mut out: Vec<Agg> = Vec::new();
    let mut cur: Option<(String, Agg)> = None;
    for (d, o, h, l, c, v, a) in rows {
        let key = bucket(d, period);
        match cur.as_mut() {
            Some((k, x)) if *k == key => {
                x.date = d.clone();
                if *h > x.high {
                    x.high = *h;
                }
                if *l < x.low {
                    x.low = *l;
                }
                x.close = *c;
                x.vol += v;
                x.amount += a;
            }
            _ => {
                if let Some((_, x)) = cur.take() {
                    out.push(x);
                }
                cur = Some((
                    key,
                    Agg {
                        date: d.clone(),
                        open: *o,
                        high: *h,
                        low: *l,
                        close: *c,
                        vol: *v,
                        amount: *a,
                    },
                ));
            }
        }
    }
    if let Some((_, x)) = cur {
        out.push(x);
    }
    out
}

/// 形态集合。请求参数 `pset` 优先（rec27 / all），缺省看环境变量
/// `TV_CANDLE_PATTERNS`，再缺省 = all（TA-Lib 全部 61 个 CDL）。
/// zg 的 `analyze_candles` 只在 Some("all") 时不加白名单。
pub fn pattern_set(pset: &str) -> &'static str {
    match pset.trim().to_lowercase().as_str() {
        "all" | "rec61" | "61" => "all",
        "rec27" | "rec20" | "27" | "20" => "rec27",
        _ => {
            static SET: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
            SET.get_or_init(|| match std::env::var("TV_CANDLE_PATTERNS").ok().as_deref() {
                Some("rec27") | Some("rec20") => "rec27",
                _ => "all",
            })
        }
    }
}

/// 蜡烛形态信号（对齐 zg：talib 引擎、rec27 集、盘中末根 provisional）。
/// 只保留落在关键位的形态（近 21 根高/低点 ±0.3×ATR14，即 zg 的 extreme 标记），
/// 非关键位的形态不上图不上报。缺 extreme 字段按 zg 口径视为关键位。
/// 返回 (engine, pattern_set, signals)，调用方负责注入响应 JSON。
pub fn candles(ohlc: &[(String, f64, f64, f64, f64)], period: &str, want: &str) -> (Value, Value, Value) {
    let rows: Vec<Value> = ohlc
        .iter()
        .map(|(d, o, h, l, c)| json!({"date": d, "open": o, "high": h, "low": l, "close": c}))
        .collect();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let last10 = ohlc.last().map(|x| x.0.get(..10).unwrap_or("")).unwrap_or("");
    let provisional = zg_core::calendar::market_open_now(None) && last10 == today;
    let pset = pattern_set(want);
    let res = zg_market::candles::analyze_candles(&rows, period, provisional, Some(pset));
    let sigs: Vec<Value> = res
        .get("signals")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|s| s.get("extreme").and_then(|x| x.as_bool()).unwrap_or(true))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    (json!("talib"), json!(pset), Value::Array(sigs))
}

/// 日 / 周 / 月 / 季 / 年：日周月走 zg-core（已含前复权、重采样、指数不除 100 的处理），
/// 季 / 年由本模块在同一口径上对日线再采样。
pub fn dwm(code: &str, period: &str, lmt: usize, want: &str) -> Result<Value, String> {
    let (mkt, cd) = resolve(code, None).map_err(|e| e.to_string())?;
    let period = period.to_lowercase();
    if !matches!(period.as_str(), "day" | "week" | "month" | "quarter" | "year") {
        return Err(format!("不支持的周期: {period}"));
    }
    // 盘中/收盘两套优先级、本地末根门禁、多源降级全部在 live 层；这里只做响应组装。
    let s = crate::live::day_series(code, &period, lmt)?;
    let rows = s.rows;
    let bars: Vec<Value> = rows
        .iter()
        .map(|b| bar_row(Value::String(b.date.clone()), b))
        .collect();
    let (eng, pset, sigs) = candles(
        &rows.iter().map(|b| (b.date.clone(), b.open, b.high, b.low, b.close)).collect::<Vec<_>>(),
        &candle_label(&period),
        want,
    );
    Ok(json!({
        "ok": true,
        "code": cd,
        "market": mkt,
        "period": period,
        "n": bars.len(),
        "bars": bars,
        "meta": {
            "source": s.source,
            "detail": s.detail,
            "as_of": s.as_of,
            "stale": s.stale,
            "session": crate::live::session_today(),
            "cache_date": s.as_of,
            "min_period": false
        },
        "candle_engine": eng,
        "candle_pattern_set": pset,
        "candle_signals": sigs,
    }))
}

/// zg 的形态标签表（PERIOD_NAMES）只有日/周/月/30/60 分，季/年会原样出现在 tooltip 里；
/// 传中文名单即可正确显示「季线 · 启明星」，且该参数在引擎内只参与文案，不影响判定。
pub fn candle_label(period: &str) -> String {
    match period {
        "quarter" => "季线".to_string(),
        "year" => "年线".to_string(),
        other => other.to_string(),
    }
}

/// 本地时间字符串 → 北京时间（+08:00）纪元秒。支持 `YYYY-MM-DD HH:MM`、带秒、纯日期（零点）。
/// bars.rs 分钟行与 chan.rs 的图层时间共用这一份解析，避免两份实现漂移。
pub(crate) fn bj_epoch(date: &str) -> Option<i64> {
    let tz = FixedOffset::east_opt(8 * 3600).expect("+08:00");
    let naive = NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| {
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).expect("midnight"))
        })
        .ok()?;
    tz.from_local_datetime(&naive).single().map(|t| t.timestamp())
}

/// bars→JSON 公共行：time 由调用方给（日/周/月/季/年为日期串，分钟为 +08:00 纪元秒）。
/// bars.rs dwm/minute 与 board.rs payload 三处共用，防止字段漂移。
pub(crate) fn bar_row(time: Value, b: &Agg) -> Value {
    json!({
        "time": time,
        "open": b.open,
        "high": b.high,
        "low": b.low,
        "close": b.close,
        "vol": b.vol,
        "amount": b.amount,
    })
}

/// 分钟：F1 / F5 / F15 / F30 / F60。count=0 表示全深度（与日/周/月 lmt=0 语义一致）。
pub fn minute(code: &str, period: &str, count: usize, want: &str) -> Result<Value, String> {
    let (mkt, cd) = resolve(code, None).map_err(|e| e.to_string())?;
    // 本地 15 分钟新鲜度门禁 → TQ → 腾讯 → tdx，远端与本地历史按时间合并（见 live.rs）。
    let s = crate::live::minute_series(code, period, count)?;
    let bars = s.rows;

    let rows: Vec<Value> = bars
        .iter()
        .map(|b| {
            let ts = bj_epoch(&b.date)
                .ok_or_else(|| format!("分钟时间解析失败或不唯一: {}", b.date))?;
            Ok(bar_row(Value::from(ts), b))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let (eng, pset, sigs) = candles(
        &bars.iter().map(|b| (b.date.clone(), b.open, b.high, b.low, b.close)).collect::<Vec<_>>(),
        period,
        want,
    );
    Ok(json!({
        "ok": true,
        "code": cd,
        "market": mkt,
        "period": period,
        "n": rows.len(),
        "bars": rows,
        "meta": {
            "source": s.source,
            "detail": s.detail,
            "last": s.as_of,
            "as_of": s.as_of,
            "n": bars.len(),
            "stale": s.stale,
            "session": crate::live::session_today(),
            "cache_date": s.as_of,
            "min_period": true
        },
        "candle_engine": eng,
        "candle_pattern_set": pset,
        "candle_signals": sigs,
    }))
}

/// 缓存键与失效指纹。minutes 同时受 .lc1 与 .day（复权因子）影响；
/// gbbq 事件文件（前复权因子的输入）也在指纹里——事件表晚于 .day 更新
/// （除权日当天补同步）时只盯 .day 会整天供旧复权价。rsync -a 保 mtime，
/// gbbq 内容没变不会误失效。
pub fn cache_key(prefix: &str, code: &str, period: &str, lmt: usize, tag: &str)
    -> Result<(String, String), String>
{
    let (mkt, cd) = resolve(code, None).map_err(|e| e.to_string())?;
    let day = crate::cache::file_stamp(&zg_core::reader::day_path(&mkt, &cd));
    let min = crate::cache::file_stamp(&zg_core::reader::min_path(&mkt, &cd));
    let adj = zg_core::adjust::gbbq_mtime_millis();
    let base = match period {
        "1" | "5" | "15" | "30" | "60" => format!("{day}|{min}|{adj}"),
        _ => format!("{day}|{adj}"),
    };
    // 盘中远端数据没有文件 mtime：用 TTL 时间桶让缓存按周期过期；收盘后仍走指纹。
    let stamp = match crate::live::ttl_bucket(period) {
        Some(b) => format!("{base}|b{b}"),
        None => base,
    };
    let key = if tag.is_empty() {
        format!("{prefix}:{cd}:{mkt}:{period}:{lmt}")
    } else {
        format!("{prefix}:{cd}:{mkt}:{period}:{lmt}:{tag}")
    };
    Ok((key, stamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(date: &str, p: f64) -> Row7 {
        (date.to_string(), p, p + 1.0, p - 1.0, p, 10.0, 100.0)
    }

    #[test]
    fn quarter_labels_last_day_of_bucket() {
        let rows = vec![
            d("2026-01-05", 10.0),
            d("2026-03-31", 12.0),
            d("2026-04-01", 20.0),
            d("2026-12-30", 30.0),
        ];
        let q = resample_qy(&rows, "quarter");
        assert_eq!(q.len(), 3); // 2026Q1(2根) / Q2(1) / Q4(1)
        assert_eq!(q[0].date, "2026-03-31");
        assert_eq!((q[0].open, q[0].close), (10.0, 12.0));
        assert_eq!((q[0].high, q[0].low), (13.0, 9.0));
        assert_eq!((q[0].vol, q[0].amount), (20.0, 200.0));
    }

    #[test]
    fn march_is_q1_and_april_is_q2() {
        assert_eq!(resample_qy(&[d("2026-03-02", 1.0), d("2026-04-02", 2.0)], "quarter").len(), 2);
    }

    #[test]
    fn year_sums_whole_history() {
        let rows = vec![d("2025-05-06", 1.0), d("2026-01-02", 2.0), d("2026-12-31", 3.0)];
        let y = resample_qy(&rows, "year");
        assert_eq!(y.len(), 2);
        assert_eq!((y[1].open, y[1].close, y[1].vol), (2.0, 3.0, 20.0));
        assert_eq!(y[1].date, "2026-12-31");
    }
}
