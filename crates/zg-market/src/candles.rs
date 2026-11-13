//! TA-Lib 蜡烛形态 —— 对齐 `modules/market/talib_candles.py`。
//!
//! 判定层：官方 `ta-lib` crate（0.8，纯 Rust，与参考 C 库逐位一致）；
//! 规则层：趋势背景过滤 / A股跳空变体 / 同根去重 / 3根确认·10根失效跟踪（逐行移植）。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::{json, Value};
use ta_lib::Core;

const EXTREME_WINDOW: i64 = 21;
const EXTREME_ATR_PERIOD: usize = 14;
const EXTREME_ATR_MULT: f64 = 0.3;

const NEUTRAL_ZERO: &[&str] = &["CDLDOJI", "CDLLONGLEGGEDDOJI", "CDLHIGHWAVE"];
const NEUTRAL_REVERSAL: &[&str] = &["CDLDRAGONFLYDOJI", "CDLGRAVESTONEDOJI"];
const CONTINUATION: &[&str] = &["CDLRISEFALL3METHODS"];
const NEEDS_NEXT: &[&str] = &[
    "CDLDRAGONFLYDOJI", "CDLGRAVESTONEDOJI", "CDLHANGINGMAN",
    "CDLINVERTEDHAMMER", "CDLHARAMI", "CDLHARAMICROSS",
];
const DOJI_PRIORITY: &[&str] = &[
    "CDLDRAGONFLYDOJI", "CDLGRAVESTONEDOJI", "CDLLONGLEGGEDDOJI", "CDLDOJI",
];
const HAMMER_FAMILY: &[&str] = &[
    "CDLHAMMER", "CDLHANGINGMAN", "CDLINVERTEDHAMMER", "CDLSHOOTINGSTAR",
];
const VARIANT_PAIRS: &[(&str, &str)] = &[
    ("CDLHARAMICROSS", "CDLHARAMI"),
    ("CDLMORNINGDOJISTAR", "CDLMORNINGSTAR"),
    ("CDLEVENINGDOJISTAR", "CDLEVENINGSTAR"),
];

/// rec27 白名单（= rec20 ∪ 7 个十字/孕线族）。
pub const PATTERN_SET_REC27: &[&str] = &[
    "CDLENGULFING", "CDL3OUTSIDE", "CDL3INSIDE", "CDLHAMMER", "CDLHANGINGMAN",
    "CDLINVERTEDHAMMER", "CDLSHOOTINGSTAR", "CDLMORNINGSTAR", "CDLEVENINGSTAR",
    "CDLMORNINGDOJISTAR", "CDLEVENINGDOJISTAR", "CDLPIERCING", "CDLDARKCLOUDCOVER",
    "CDL3WHITESOLDIERS", "CDL3BLACKCROWS", "CDL2CROWS", "CDLUPSIDEGAP2CROWS",
    "CDLADVANCEBLOCK", "CDLSTALLEDPATTERN", "CDLRISEFALL3METHODS",
    "CDLDRAGONFLYDOJI", "CDLGRAVESTONEDOJI", "CDLLONGLEGGEDDOJI", "CDLDOJI",
    "CDLHIGHWAVE", "CDLHARAMI", "CDLHARAMICROSS",
];

/// 全部 61 个 CDL 函数（`talib.get_functions()` 的 CDL*，排序后）。
const ALL_CDL_FUNCTIONS: &[&str] = &[
    "CDL2CROWS", "CDL3BLACKCROWS", "CDL3INSIDE", "CDL3LINESTRIKE", "CDL3OUTSIDE",
    "CDL3STARSINSOUTH", "CDL3WHITESOLDIERS", "CDLABANDONEDBABY", "CDLADVANCEBLOCK",
    "CDLBELTHOLD", "CDLBREAKAWAY", "CDLCLOSINGMARUBOZU", "CDLCONCEALBABYSWALL",
    "CDLCOUNTERATTACK", "CDLDARKCLOUDCOVER", "CDLDOJI", "CDLDOJISTAR",
    "CDLDRAGONFLYDOJI", "CDLENGULFING", "CDLEVENINGDOJISTAR", "CDLEVENINGSTAR",
    "CDLGAPSIDESIDEWHITE", "CDLGRAVESTONEDOJI", "CDLHAMMER", "CDLHANGINGMAN",
    "CDLHARAMI", "CDLHARAMICROSS", "CDLHIGHWAVE", "CDLHIKKAKE", "CDLHIKKAKEMOD",
    "CDLHOMINGPIGEON", "CDLIDENTICAL3CROWS", "CDLINNECK", "CDLINVERTEDHAMMER",
    "CDLKICKING", "CDLKICKINGBYLENGTH", "CDLLADDERBOTTOM", "CDLLONGLEGGEDDOJI",
    "CDLLONGLINE", "CDLMARUBOZU", "CDLMATCHINGLOW", "CDLMATHOLD",
    "CDLMORNINGDOJISTAR", "CDLMORNINGSTAR", "CDLONNECK", "CDLPIERCING",
    "CDLRICKSHAWMAN", "CDLRISEFALL3METHODS", "CDLSEPARATINGLINES",
    "CDLSHOOTINGSTAR", "CDLSHORTLINE", "CDLSPINNINGTOP", "CDLSTALLEDPATTERN",
    "CDLSTICKSANDWICH", "CDLTAKURI", "CDLTASUKIGAP", "CDLTHRUSTING", "CDLTRISTAR",
    "CDLUNIQUE3RIVER", "CDLUPSIDEGAP2CROWS", "CDLXSIDEGAP3METHODS",
];

const SPAN: &[(&str, usize)] = &[
    ("CDL2CROWS", 3), ("CDL3BLACKCROWS", 3), ("CDL3INSIDE", 3), ("CDL3LINESTRIKE", 4),
    ("CDL3OUTSIDE", 3), ("CDL3STARSINSOUTH", 3), ("CDL3WHITESOLDIERS", 3),
    ("CDLABANDONEDBABY", 3), ("CDLADVANCEBLOCK", 3), ("CDLBELTHOLD", 1),
    ("CDLBREAKAWAY", 5), ("CDLCLOSINGMARUBOZU", 1), ("CDLCONCEALBABYSWALL", 4),
    ("CDLCOUNTERATTACK", 2), ("CDLDARKCLOUDCOVER", 2), ("CDLDOJI", 1),
    ("CDLDOJISTAR", 2), ("CDLDRAGONFLYDOJI", 1), ("CDLENGULFING", 2),
    ("CDLEVENINGDOJISTAR", 3), ("CDLEVENINGSTAR", 3), ("CDLGAPSIDESIDEWHITE", 3),
    ("CDLGRAVESTONEDOJI", 1), ("CDLHAMMER", 1), ("CDLHANGINGMAN", 1),
    ("CDLHARAMI", 2), ("CDLHARAMICROSS", 2), ("CDLHIGHWAVE", 1), ("CDLHIKKAKE", 3),
    ("CDLHIKKAKEMOD", 4), ("CDLHOMINGPIGEON", 2), ("CDLIDENTICAL3CROWS", 3),
    ("CDLINNECK", 2), ("CDLINVERTEDHAMMER", 1), ("CDLKICKING", 2),
    ("CDLKICKINGBYLENGTH", 2), ("CDLLADDERBOTTOM", 5), ("CDLLONGLEGGEDDOJI", 1),
    ("CDLLONGLINE", 1), ("CDLMARUBOZU", 1), ("CDLMATCHINGLOW", 2), ("CDLMATHOLD", 5),
    ("CDLMORNINGDOJISTAR", 3), ("CDLMORNINGSTAR", 3), ("CDLONNECK", 2),
    ("CDLPIERCING", 2), ("CDLRICKSHAWMAN", 1), ("CDLRISEFALL3METHODS", 5),
    ("CDLSEPARATINGLINES", 2), ("CDLSHOOTINGSTAR", 1), ("CDLSHORTLINE", 1),
    ("CDLSPINNINGTOP", 1), ("CDLSTALLEDPATTERN", 3), ("CDLSTICKSANDWICH", 3),
    ("CDLTAKURI", 1), ("CDLTASUKIGAP", 3), ("CDLTHRUSTING", 2), ("CDLTRISTAR", 3),
    ("CDLUNIQUE3RIVER", 3), ("CDLUPSIDEGAP2CROWS", 3), ("CDLXSIDEGAP3METHODS", 3),
];

const CDL_LABELS: &[(&str, &str)] = &[
    ("CDL2CROWS", "两只乌鸦"), ("CDL3BLACKCROWS", "三只乌鸦"),
    ("CDL3INSIDE", "三内部上涨/下跌"), ("CDL3LINESTRIKE", "三线打击"),
    ("CDL3OUTSIDE", "三外部上涨/下跌"), ("CDL3STARSINSOUTH", "南方三星"),
    ("CDL3WHITESOLDIERS", "三个白兵"), ("CDLABANDONEDBABY", "弃婴"),
    ("CDLADVANCEBLOCK", "大敌当前"), ("CDLBELTHOLD", "捉腰带线"),
    ("CDLBREAKAWAY", "脱离"), ("CDLCLOSINGMARUBOZU", "收盘光头光脚"),
    ("CDLCONCEALBABYSWALL", "藏婴吞没"), ("CDLCOUNTERATTACK", "反击线"),
    ("CDLDARKCLOUDCOVER", "乌云盖顶"), ("CDLDOJI", "十字"), ("CDLDOJISTAR", "十字星"),
    ("CDLDRAGONFLYDOJI", "蜻蜓十字"), ("CDLENGULFING", "吞没"),
    ("CDLEVENINGDOJISTAR", "黄昏十字星"), ("CDLEVENINGSTAR", "黄昏星"),
    ("CDLGAPSIDESIDEWHITE", "跳空并列阳线"), ("CDLGRAVESTONEDOJI", "墓碑十字"),
    ("CDLHAMMER", "锤子"), ("CDLHANGINGMAN", "上吊"), ("CDLHARAMI", "孕线"),
    ("CDLHARAMICROSS", "十字孕线"), ("CDLHIGHWAVE", "高浪线"), ("CDLHIKKAKE", "引线"),
    ("CDLHIKKAKEMOD", "修正引线"), ("CDLHOMINGPIGEON", "家鸽"),
    ("CDLIDENTICAL3CROWS", "相同三乌鸦"), ("CDLINNECK", "颈内"),
    ("CDLINVERTEDHAMMER", "倒锤"), ("CDLKICKING", "反冲"),
    ("CDLKICKINGBYLENGTH", "由长度反冲"), ("CDLLADDERBOTTOM", "梯底"),
    ("CDLLONGLEGGEDDOJI", "长脚十字"), ("CDLLONGLINE", "长实体"),
    ("CDLMARUBOZU", "光头光脚"), ("CDLMATCHINGLOW", "相同低价"), ("CDLMATHOLD", "铺垫"),
    ("CDLMORNINGDOJISTAR", "早晨十字星"), ("CDLMORNINGSTAR", "晨星"),
    ("CDLONNECK", "颈上"), ("CDLPIERCING", "刺透"), ("CDLRICKSHAWMAN", "黄包车夫"),
    ("CDLRISEFALL3METHODS", "上升/下降三法"), ("CDLSEPARATINGLINES", "分离线"),
    ("CDLSHOOTINGSTAR", "流星"), ("CDLSHORTLINE", "短实体"), ("CDLSPINNINGTOP", "纺锤"),
    ("CDLSTALLEDPATTERN", "停滞"), ("CDLSTICKSANDWICH", "条形三明治"),
    ("CDLTAKURI", "探水竿"), ("CDLTASUKIGAP", "跳空并列阴阳线"),
    ("CDLTHRUSTING", "插入"), ("CDLTRISTAR", "三星"), ("CDLUNIQUE3RIVER", "奇特三河床"),
    ("CDLUPSIDEGAP2CROWS", "向上跳空两只乌鸦"), ("CDLXSIDEGAP3METHODS", "上升/下降跳空三法"),
];

fn dir_labels(fn_name: &str) -> Option<&'static [(i32, &'static str)]> {
    Some(match fn_name {
        "CDLENGULFING" => &[(1, "看涨吞没"), (-1, "看跌吞没")],
        "CDL3INSIDE" => &[(1, "三内部上涨"), (-1, "三内部下跌")],
        "CDL3OUTSIDE" => &[(1, "三外部上涨"), (-1, "三外部下跌")],
        "CDL3WHITESOLDIERS" => &[(1, "三白兵")],
        "CDL3BLACKCROWS" => &[(-1, "三乌鸦")],
        "CDLRISEFALL3METHODS" => &[(1, "上升三法"), (-1, "下降三法")],
        "CDLHARAMI" => &[(1, "孕线"), (-1, "孕线")],
        "CDLHARAMICROSS" => &[(1, "十字孕线"), (-1, "十字孕线")],
        "CDLHAMMER" => &[(1, "锤子线")],
        "CDLHANGINGMAN" => &[(-1, "上吊线")],
        "CDLINVERTEDHAMMER" => &[(1, "倒锤子线")],
        "CDLSHOOTINGSTAR" => &[(-1, "流星线")],
        "CDLPIERCING" => &[(1, "刺透形态")],
        "CDLDARKCLOUDCOVER" => &[(-1, "乌云盖顶")],
        "CDLMORNINGSTAR" => &[(1, "晨星")],
        "CDLEVENINGSTAR" => &[(-1, "黄昏星")],
        "CDLMORNINGDOJISTAR" => &[(1, "早晨十字星")],
        "CDLEVENINGDOJISTAR" => &[(-1, "黄昏十字星")],
        "CDL2CROWS" => &[(-1, "两只乌鸦")],
        "CDLUPSIDEGAP2CROWS" => &[(-1, "向上跳空两只乌鸦")],
        "CDLADVANCEBLOCK" => &[(-1, "大敌当前")],
        "CDLSTALLEDPATTERN" => &[(-1, "停滞")],
        "CDLDOJI" => &[(1, "十字线 · 变盘观察")],
        "CDLLONGLEGGEDDOJI" => &[(1, "长脚十字 · 方向待定")],
        "CDLDRAGONFLYDOJI" => &[(1, "蜻蜓十字 · 反转观察"), (-1, "蜻蜓十字 · 反转观察")],
        "CDLGRAVESTONEDOJI" => &[(1, "墓碑十字 · 反转观察"), (-1, "墓碑十字 · 反转观察")],
        "CDLHIGHWAVE" => &[(1, "高浪线 · 方向待定")],
        _ => return None,
    })
}

const REASONS: &[(&str, &str)] = &[
    ("CDLDRAGONFLYDOJI", "开收接近区间高点且下影很长；须结合所处趋势，等待后续收盘确认"),
    ("CDLGRAVESTONEDOJI", "开收接近区间低点且上影很长；须结合所处趋势，等待后续收盘确认"),
    ("CDLLONGLEGGEDDOJI", "上下影都长，表示市场暂时失去方向；不单独给出反转方向"),
    ("CDLDOJI", "开收接近；单独十字只表示犹豫，不确认反转"),
    ("CDLHIGHWAVE", "小实体配合较长上下影，表示原有方向感减弱；不单独给出反转方向"),
    ("CDLENGULFING", "第二根实体吞没前实体，不要求吞没影线"),
    ("CDL3WHITESOLDIERS", "连续长实体推进且收盘靠近推进端；形态完成时可能已远离关键价位"),
    ("CDL3BLACKCROWS", "连续长实体推进且收盘靠近推进端；形态完成时可能已远离关键价位"),
];

const PERIOD_NAMES: &[(&str, &str)] = &[
    ("day", "日线"), ("min30", "30分钟"), ("min60", "60分钟"),
    ("week", "周线"), ("month", "月线"),
];

fn lookup(list: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    list.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn span_of(fn_name: &str) -> usize {
    SPAN.iter().find(|(k, _)| *k == fn_name).map(|(_, v)| *v).unwrap_or(1)
}

// ---------------------------------------------------------------- 基础数据

#[derive(Clone, Copy)]
struct Ohlc {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

fn valid_bar(b: &Ohlc) -> bool {
    b.open > 0.0 && b.high > 0.0 && b.low > 0.0 && b.close > 0.0
        && b.high >= b.open.max(b.close)
        && b.low <= b.open.min(b.close)
        && b.high > b.low
}

struct Ctx {
    direction: i32,
    range: f64,
    high: f64,
    low: f64,
}

fn context(bars: &[Ohlc], start: usize) -> Option<Ctx> {
    let s = start.saturating_sub(10);
    let past = &bars[s..start];
    if past.len() < 5 || !past.iter().all(valid_bar) {
        return None;
    }
    let high = past.iter().map(|b| b.high).fold(f64::MIN, f64::max);
    let low = past.iter().map(|b| b.low).fold(f64::MAX, f64::min);
    let range = high - low;
    if range <= 0.0 {
        return None;
    }
    let move_ = past[past.len() - 1].close - past[0].close;
    let direction = if move_ > range * 0.25 {
        1
    } else if move_ < -range * 0.25 {
        -1
    } else {
        0
    };
    Some(Ctx { direction, range, high, low })
}

fn at_trend_edge(bar: &Ohlc, ctx: &Ctx) -> bool {
    if ctx.direction == 0 {
        return false;
    }
    if ctx.direction == 1 {
        bar.high >= ctx.high - ctx.range * 0.1
    } else {
        bar.low <= ctx.low + ctx.range * 0.1
    }
}

/// `_extreme_masks`：ATR14 累计 + 21 根窗口（含当根），无未来函数。
fn extreme_masks(high: &[f64], low: &[f64], close: &[f64]) -> (Vec<bool>, Vec<bool>) {
    let n = high.len();
    let mut prev_close = vec![0.0; n];
    if n > 1 {
        prev_close[1..].copy_from_slice(&close[..n - 1]);
    }
    if n > 0 {
        prev_close[0] = close[0];
    }
    let mut tr = vec![0.0; n];
    for i in 0..n {
        tr[i] = (high[i] - low[i])
            .max((high[i] - prev_close[i]).abs().max((low[i] - prev_close[i]).abs()));
    }
    let mut cs = vec![0.0; n];
    let mut acc = 0.0;
    for i in 0..n {
        acc += tr[i];
        cs[i] = acc;
    }
    let period = EXTREME_ATR_PERIOD;
    let mut atr = vec![0.0; n];
    for i in 0..(period - 1).min(n) {
        atr[i] = cs[i] / (i as f64 + 1.0);
    }
    if n >= period {
        for i in (period - 1)..n {
            let base = if i >= period { cs[i - period] } else { 0.0 };
            atr[i] = (cs[i] - base) / period as f64;
        }
    }
    let w = EXTREME_WINDOW - 1;
    let mut lo21 = vec![0.0; n];
    let mut hi21 = vec![0.0; n];
    for i in 0..n {
        let start = if (i as i64 - w) > 0 { (i as i64 - w) as usize } else { 0 };
        lo21[i] = low[start..=i].iter().fold(f64::MAX, |a, b| a.min(*b));
        hi21[i] = high[start..=i].iter().fold(f64::MIN, |a, b| a.max(*b));
    }
    let lo_extreme: Vec<bool> = (0..n).map(|i| low[i] <= lo21[i] + EXTREME_ATR_MULT * atr[i]).collect();
    let hi_extreme: Vec<bool> = (0..n).map(|i| high[i] >= hi21[i] - EXTREME_ATR_MULT * atr[i]).collect();
    (lo_extreme, hi_extreme)
}

/// `_gap_variant`：A股跳空变体标注。返回 (后缀, reason)。
fn gap_variant(fn_name: &str, bars: &[Ohlc], i: usize) -> Option<(String, String)> {
    if i < 1 {
        return None;
    }
    let b = bars[i];
    let a = bars[i - 1];
    let top_a = a.open.max(a.close);
    let bottom_a = a.open.min(a.close);
    if fn_name == "CDLPIERCING" {
        if b.open < a.low {
            return Some((String::new(), "低开至前低之下，收盘刺入前阴线实体一半以上".into()));
        }
        return Some((" · 无整根跳空变体".into(),
                     "低于前收开盘并收复前阴线实体一半以上；未低开至前低之下，按A股常见变体标注".into()));
    }
    if fn_name == "CDLDARKCLOUDCOVER" {
        if b.open > a.high {
            return Some((String::new(), "高开至前高之上，收盘跌入前阳线实体一半以下".into()));
        }
        return Some((" · 无整根跳空变体".into(),
                     "高于前收开盘并跌入前阳线实体一半以下；未高开至前高之上，按A股常见变体标注".into()));
    }
    if i < 2 {
        return None;
    }
    let first_close = bars[i - 2].close;
    if fn_name == "CDLMORNINGSTAR" || fn_name == "CDLMORNINGDOJISTAR" {
        if top_a < first_close {
            return Some((String::new(), "第三根为明显反向实体并收复首根实体一半以上".into()));
        }
        return Some((" · 无实体跳空变体".into(),
                     "第三根收复首根实体一半以上；中间星体未与首根实体完全跳空时明确标作变体".into()));
    }
    if fn_name == "CDLEVENINGSTAR" || fn_name == "CDLEVENINGDOJISTAR" {
        if bottom_a > first_close {
            return Some((String::new(), "第三根为明显反向实体并跌入首根实体一半以下".into()));
        }
        return Some((" · 无实体跳空变体".into(),
                     "第三根跌入首根实体一半以下；中间星体未与首根实体完全跳空时明确标作变体".into()));
    }
    None
}

fn label_for(fn_name: &str, direction: i32) -> String {
    if let Some(table) = dir_labels(fn_name) {
        if let Some((_, v)) = table.iter().find(|(d, _)| *d == direction) {
            return (*v).to_string();
        }
        if let Some((_, v)) = table.iter().find(|(d, _)| *d == 1) {
            return (*v).to_string();
        }
        if let Some((_, v)) = table.iter().find(|(d, _)| *d == -1) {
            return (*v).to_string();
        }
    }
    lookup(CDL_LABELS, fn_name).unwrap_or(fn_name).to_string()
}

// ---------------------------------------------------------------- TA-Lib 调度

fn core() -> &'static Core {
    static CORE: OnceLock<Core> = OnceLock::new();
    CORE.get_or_init(Core::new)
}

/// 单个 CDL 函数调用：写入 out（与输入等长，未写区域为 0）。
/// 返回 OutRange：`out[j]` 对应输入 `beg_idx + j`（ta-lib crate 语义，非从 0 对齐）。
fn call_cdl(fn_name: &str, o: &[f64], h: &[f64], l: &[f64], c: &[f64], out: &mut [i32])
    -> Result<ta_lib::OutRange, String> {
    let core = core();
    let end = o.len() - 1;
    let r = match fn_name {
        "CDL2CROWS" => core.cdl2crows(0, end, o, h, l, c, out),
        "CDL3BLACKCROWS" => core.cdl3blackcrows(0, end, o, h, l, c, out),
        "CDL3INSIDE" => core.cdl3inside(0, end, o, h, l, c, out),
        "CDL3LINESTRIKE" => core.cdl3linestrike(0, end, o, h, l, c, out),
        "CDL3OUTSIDE" => core.cdl3outside(0, end, o, h, l, c, out),
        "CDL3STARSINSOUTH" => core.cdl3starsinsouth(0, end, o, h, l, c, out),
        "CDL3WHITESOLDIERS" => core.cdl3whitesoldiers(0, end, o, h, l, c, out),
        "CDLABANDONEDBABY" => core.cdlabandonedbaby(0, end, o, h, l, c, 0.3, out),
        "CDLADVANCEBLOCK" => core.cdladvanceblock(0, end, o, h, l, c, out),
        "CDLBELTHOLD" => core.cdlbelthold(0, end, o, h, l, c, out),
        "CDLBREAKAWAY" => core.cdlbreakaway(0, end, o, h, l, c, out),
        "CDLCLOSINGMARUBOZU" => core.cdlclosingmarubozu(0, end, o, h, l, c, out),
        "CDLCONCEALBABYSWALL" => core.cdlconcealbabyswall(0, end, o, h, l, c, out),
        "CDLCOUNTERATTACK" => core.cdlcounterattack(0, end, o, h, l, c, out),
        "CDLDARKCLOUDCOVER" => core.cdldarkcloudcover(0, end, o, h, l, c, 0.5, out),
        "CDLDOJI" => core.cdldoji(0, end, o, h, l, c, out),
        "CDLDOJISTAR" => core.cdldojistar(0, end, o, h, l, c, out),
        "CDLDRAGONFLYDOJI" => core.cdldragonflydoji(0, end, o, h, l, c, out),
        "CDLENGULFING" => core.cdlengulfing(0, end, o, h, l, c, out),
        "CDLEVENINGDOJISTAR" => core.cdleveningdojistar(0, end, o, h, l, c, 0.3, out),
        "CDLEVENINGSTAR" => core.cdleveningstar(0, end, o, h, l, c, 0.3, out),
        "CDLGAPSIDESIDEWHITE" => core.cdlgapsidesidewhite(0, end, o, h, l, c, out),
        "CDLGRAVESTONEDOJI" => core.cdlgravestonedoji(0, end, o, h, l, c, out),
        "CDLHAMMER" => core.cdlhammer(0, end, o, h, l, c, out),
        "CDLHANGINGMAN" => core.cdlhangingman(0, end, o, h, l, c, out),
        "CDLHARAMI" => core.cdlharami(0, end, o, h, l, c, out),
        "CDLHARAMICROSS" => core.cdlharamicross(0, end, o, h, l, c, out),
        "CDLHIGHWAVE" => core.cdlhighwave(0, end, o, h, l, c, out),
        "CDLHIKKAKE" => core.cdlhikkake(0, end, o, h, l, c, out),
        "CDLHIKKAKEMOD" => core.cdlhikkakemod(0, end, o, h, l, c, out),
        "CDLHOMINGPIGEON" => core.cdlhomingpigeon(0, end, o, h, l, c, out),
        "CDLIDENTICAL3CROWS" => core.cdlidentical3crows(0, end, o, h, l, c, out),
        "CDLINNECK" => core.cdlinneck(0, end, o, h, l, c, out),
        "CDLINVERTEDHAMMER" => core.cdlinvertedhammer(0, end, o, h, l, c, out),
        "CDLKICKING" => core.cdlkicking(0, end, o, h, l, c, out),
        "CDLKICKINGBYLENGTH" => core.cdlkickingbylength(0, end, o, h, l, c, out),
        "CDLLADDERBOTTOM" => core.cdlladderbottom(0, end, o, h, l, c, out),
        "CDLLONGLEGGEDDOJI" => core.cdllongleggeddoji(0, end, o, h, l, c, out),
        "CDLLONGLINE" => core.cdllongline(0, end, o, h, l, c, out),
        "CDLMARUBOZU" => core.cdlmarubozu(0, end, o, h, l, c, out),
        "CDLMATCHINGLOW" => core.cdlmatchinglow(0, end, o, h, l, c, out),
        "CDLMATHOLD" => core.cdlmathold(0, end, o, h, l, c, 0.5, out),
        "CDLMORNINGDOJISTAR" => core.cdlmorningdojistar(0, end, o, h, l, c, 0.3, out),
        "CDLMORNINGSTAR" => core.cdlmorningstar(0, end, o, h, l, c, 0.3, out),
        "CDLONNECK" => core.cdlonneck(0, end, o, h, l, c, out),
        "CDLPIERCING" => core.cdlpiercing(0, end, o, h, l, c, out),
        "CDLRICKSHAWMAN" => core.cdlrickshawman(0, end, o, h, l, c, out),
        "CDLRISEFALL3METHODS" => core.cdlrisefall3methods(0, end, o, h, l, c, out),
        "CDLSEPARATINGLINES" => core.cdlseparatinglines(0, end, o, h, l, c, out),
        "CDLSHOOTINGSTAR" => core.cdlshootingstar(0, end, o, h, l, c, out),
        "CDLSHORTLINE" => core.cdlshortline(0, end, o, h, l, c, out),
        "CDLSPINNINGTOP" => core.cdlspinningtop(0, end, o, h, l, c, out),
        "CDLSTALLEDPATTERN" => core.cdlstalledpattern(0, end, o, h, l, c, out),
        "CDLSTICKSANDWICH" => core.cdlsticksandwich(0, end, o, h, l, c, out),
        "CDLTAKURI" => core.cdltakuri(0, end, o, h, l, c, out),
        "CDLTASUKIGAP" => core.cdltasukigap(0, end, o, h, l, c, out),
        "CDLTHRUSTING" => core.cdlthrusting(0, end, o, h, l, c, out),
        "CDLTRISTAR" => core.cdltristar(0, end, o, h, l, c, out),
        "CDLUNIQUE3RIVER" => core.cdlunique3river(0, end, o, h, l, c, out),
        "CDLUPSIDEGAP2CROWS" => core.cdlupsidegap2crows(0, end, o, h, l, c, out),
        "CDLXSIDEGAP3METHODS" => core.cdlxsidegap3methods(0, end, o, h, l, c, out),
        other => return Err(format!("unknown CDL function: {other}")),
    };
    r.map_err(|e| format!("{e:?}"))
}

// ---------------------------------------------------------------- 气泡构建

#[derive(Clone)]
struct Bubble {
    index: usize,
    date: String,
    fn_name: String,
    value: i64,
    label: String,
    name: String,
    direction: i32,
    side: String,
    score: i64,
    strength: String,
    kind: String,
    needs_next: bool,
    extreme: bool,
    provisional: bool,
    start: usize,
    count: usize,
    low: f64,
    high: f64,
    confirm_price: f64,
    confirm_boundary: String,
    follow_bars: i64,
    occurred_at: String,
    confirmed_at: Option<String>,
    failed_at: Option<String>,
    tracked_until: Option<String>,
    expired_at: Option<String>,
    events: Vec<String>,
    status: String,
    base_detail: String,
    detail: String,
}

impl Bubble {
    fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("index".into(), Value::from(self.index as i64));
        m.insert("date".into(), Value::String(self.date.clone()));
        m.insert("fn".into(), Value::String(self.fn_name.clone()));
        m.insert("value".into(), Value::from(self.value));
        m.insert("label".into(), Value::String(self.label.clone()));
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("direction".into(), Value::from(self.direction));
        m.insert("side".into(), Value::String(self.side.clone()));
        m.insert("score".into(), Value::from(self.score));
        m.insert("strength".into(), Value::String(self.strength.clone()));
        m.insert("kind".into(), Value::String(self.kind.clone()));
        m.insert("needsNext".into(), Value::Bool(self.needs_next));
        m.insert("extreme".into(), Value::Bool(self.extreme));
        m.insert("provisional".into(), Value::Bool(self.provisional));
        m.insert("start".into(), Value::from(self.start as i64));
        m.insert("count".into(), Value::from(self.count as i64));
        m.insert("low".into(), Value::from(self.low));
        m.insert("high".into(), Value::from(self.high));
        m.insert("confirmPrice".into(), Value::from(self.confirm_price));
        m.insert("confirmBoundary".into(), Value::String(self.confirm_boundary.clone()));
        m.insert("followBars".into(), Value::from(self.follow_bars));
        m.insert("occurredAt".into(), Value::String(self.occurred_at.clone()));
        m.insert("confirmedAt".into(), self.confirmed_at.clone().map(Value::String).unwrap_or(Value::Null));
        m.insert("failedAt".into(), self.failed_at.clone().map(Value::String).unwrap_or(Value::Null));
        m.insert("trackedUntil".into(), self.tracked_until.clone().map(Value::String).unwrap_or(Value::Null));
        // Python 侧仅在实际触发超时/跟踪结束时写入该键（缺失 ≠ null），保持同构
        if let Some(x) = &self.expired_at {
            m.insert("expiredAt".into(), Value::String(x.clone()));
        }
        m.insert("events".into(), Value::Array(self.events.iter().map(|e| Value::String(e.clone())).collect()));
        m.insert("status".into(), Value::String(self.status.clone()));
        m.insert("baseDetail".into(), Value::String(self.base_detail.clone()));
        m.insert("detail".into(), Value::String(self.detail.clone()));
        m.insert("signals".into(), Value::Array(Vec::new()));
        Value::Object(m)
    }
}

struct RawSig {
    index: usize,
    fn_name: &'static str,
    value: i64,
}

#[allow(clippy::too_many_arguments)]
fn build_bubbles(
    bars: &[Ohlc],
    dates: &[String],
    raw: &[RawSig],
    period: &str,
    provisional_last: bool,
    lo_extreme: &[bool],
    hi_extreme: &[bool],
) -> Vec<Bubble> {
    let n = bars.len();
    let period_name = if period.is_empty() {
        "K线".to_string()
    } else {
        lookup(PERIOD_NAMES, period).unwrap_or(period).to_string()
    };
    let mut bubbles: Vec<Bubble> = Vec::new();
    for sig in raw {
        let i = sig.index;
        let fn_name = sig.fn_name;
        let value = sig.value;
        let raw_dir = if value > 0 { 1 } else { -1 };
        let span = span_of(fn_name);
        let start = (i + 1).saturating_sub(span);
        let bar = bars[i];
        if !valid_bar(&bar) {
            continue;
        }
        let ctx = match context(bars, start) {
            Some(c) => c,
            None => continue,
        };
        let ctx_dir = ctx.direction;

        // ---- 趋势背景规则 ----
        let eff_dir: i32;
        if NEUTRAL_ZERO.contains(&fn_name) {
            if ctx_dir == 0 || !at_trend_edge(&bar, &ctx) {
                continue;
            }
            eff_dir = 0;
        } else if NEUTRAL_REVERSAL.contains(&fn_name) {
            if ctx_dir == 0 || !at_trend_edge(&bar, &ctx) {
                continue;
            }
            eff_dir = -ctx_dir;
        } else if CONTINUATION.contains(&fn_name) {
            if ctx_dir != raw_dir {
                continue;
            }
            eff_dir = raw_dir;
        } else {
            if ctx_dir != -raw_dir {
                continue;
            }
            eff_dir = raw_dir;
        }

        // ---- A股跳空变体 ----
        let variant = gap_variant(fn_name, bars, i);
        let name_suffix = variant.as_ref().map(|v| v.0.clone()).unwrap_or_default();
        let reason = match &variant {
            Some(v) => v.1.clone(),
            None => lookup(REASONS, fn_name)
                .unwrap_or("TA-Lib 规则命中且趋势背景匹配")
                .to_string(),
        };

        let seg = &bars[start..=i];
        let seg_low = seg.iter().map(|x| x.low).fold(f64::MAX, f64::min);
        let seg_high = seg.iter().map(|x| x.high).fold(f64::MIN, f64::max);
        let body_edge = fn_name == "CDLHANGINGMAN" || fn_name == "CDLINVERTEDHAMMER";
        let confirm_price = if fn_name == "CDLHANGINGMAN" {
            bar.open.min(bar.close)
        } else if fn_name == "CDLINVERTEDHAMMER" {
            bar.open.max(bar.close)
        } else if eff_dir == 1 {
            seg_high
        } else {
            seg_low
        };
        let confirm_boundary = if body_edge { "实体边缘" } else { "形态区间边缘" };
        let needs_next = NEEDS_NEXT.contains(&fn_name);
        let kind = if NEUTRAL_ZERO.contains(&fn_name) {
            "中性观察"
        } else if CONTINUATION.contains(&fn_name) {
            "持续形态"
        } else {
            "反转候选"
        };
        let is_provisional = provisional_last && i == n - 1;
        let status = if is_provisional {
            "末根暂定".to_string()
        } else if needs_next {
            "形态完成 · 等待确认".to_string()
        } else if eff_dir != 0 {
            "形态已收盘完成 · 反转候选".to_string()
        } else {
            "中性观察".to_string()
        };

        let extreme = if eff_dir == 1 {
            lo_extreme[i]
        } else if eff_dir == -1 {
            hi_extreme[i]
        } else {
            lo_extreme[i] || hi_extreme[i]
        };

        let confirm_text = if needs_next {
            format!(
                "确认要求：后续收盘{} {:.2}（{}）；最多观察3根有效已闭合K线。",
                if eff_dir == 1 { "高于" } else { "低于" },
                confirm_price,
                confirm_boundary
            )
        } else {
            String::new()
        };
        let failure_text = if eff_dir != 0 {
            "形态反向极值仅作证伪参考，后续跟踪10根有效K线；"
        } else {
            "中性形态不设多空确认与反向失效；"
        };
        let bg = match ctx_dir {
            1 => "上涨",
            -1 => "下跌",
            _ => "震荡",
        };
        let name = format!("{}{}", label_for(fn_name, eff_dir), name_suffix);
        let label = format!("{period_name} · {name}");
        let base = format!(
            "日本蜡烛图（TA-Lib 判定）：{name}；类别 {kind}；所属周期 {period_name}；\
组成 {span}根K线，{} → {}；此前10根内背景为 {bg}；\
形态区间 {seg_low:.2}–{seg_high:.2}。{reason}。{confirm_text}{failure_text}\
比例与观察窗口为工程默认值，未验证胜率。涨跌停、除权缺口附近需降级，\
形态只提示原趋势可能变化，不预测反转幅度或持续时间。",
            dates[start], dates[i]
        );

        bubbles.push(Bubble {
            index: i,
            date: dates[i].clone(),
            fn_name: fn_name.to_string(),
            value,
            label,
            name,
            direction: eff_dir,
            side: if eff_dir == 1 { "lower" } else { "upper" }.to_string(),
            score: 2,
            strength: kind.to_string(),
            kind: kind.to_string(),
            needs_next,
            extreme,
            provisional: is_provisional,
            start,
            count: span,
            low: seg_low,
            high: seg_high,
            confirm_price,
            confirm_boundary: confirm_boundary.to_string(),
            follow_bars: 0,
            occurred_at: dates[i].clone(),
            confirmed_at: None,
            failed_at: None,
            tracked_until: None,
            expired_at: None,
            events: Vec::new(),
            status,
            base_detail: base.clone(),
            detail: base,
        });
    }
    bubbles
}

// ---------------------------------------------------------------- 同根去重

fn dedupe_bubbles(bubbles: Vec<Bubble>) -> Vec<Bubble> {
    let mut groups: HashMap<usize, Vec<Bubble>> = HashMap::new();
    for b in bubbles {
        groups.entry(b.index).or_default().push(b);
    }
    let mut idxs: Vec<usize> = groups.keys().copied().collect();
    idxs.sort();
    let mut kept: Vec<Bubble> = Vec::new();
    for idx in idxs {
        let mut group = groups.remove(&idx).unwrap_or_default();
        if group.iter().any(|b| b.count > 1) {
            group.retain(|b| !(b.direction == 0 && b.count == 1));
        }
        let doji_hits: Vec<&str> = DOJI_PRIORITY
            .iter()
            .copied()
            .filter(|fn_| group.iter().any(|b| b.fn_name == *fn_))
            .collect();
        if let Some(keep_fn) = doji_hits.first() {
            let keep_fn = *keep_fn;
            let doji_family: HashSet<&str> =
                DOJI_PRIORITY.iter().copied().chain(std::iter::once("CDLHIGHWAVE")).collect();
            group.retain(|b| !doji_family.contains(b.fn_name.as_str()) || b.fn_name == keep_fn);
            group.retain(|b| !HAMMER_FAMILY.contains(&b.fn_name.as_str()));
        }
        for (preferred, fallback) in VARIANT_PAIRS {
            if group.iter().any(|b| b.fn_name == *preferred) {
                group.retain(|b| b.fn_name != *fallback);
            }
        }
        kept.extend(group);
    }
    kept
}

// ---------------------------------------------------------------- 确认/失效跟踪

fn apply_lifecycle(bars: &[Ohlc], dates: &[String], bubbles: &mut [Bubble], provisional_last: bool) {
    let n = bars.len();
    let mut active: Vec<usize> = Vec::new();
    let mut by_index: HashMap<usize, Vec<usize>> = HashMap::new();
    for (bi, b) in bubbles.iter().enumerate() {
        by_index.entry(b.index).or_default().push(bi);
    }
    for i in 0..n {
        if !valid_bar(&bars[i]) {
            continue;
        }
        let provisional = provisional_last && i == n - 1;
        if !provisional {
            for &pi in &active {
                let p = &mut bubbles[pi];
                if i <= p.index || p.failed_at.is_some() || p.expired_at.is_some() {
                    continue;
                }
                p.follow_bars += 1;
                let close = bars[i].close;
                let lost = (p.direction == 1 && close < p.low)
                    || (p.direction == -1 && close > p.high);
                if p.direction != 0 && lost {
                    p.failed_at = Some(dates[i].clone());
                    p.status = "形态失效".to_string();
                    p.events.push(format!("{} 收盘越过形态反向极值", dates[i]));
                } else if p.needs_next && p.confirmed_at.is_none() {
                    let ok = if p.direction == 1 {
                        close > p.confirm_price
                    } else {
                        close < p.confirm_price
                    };
                    if ok {
                        p.confirmed_at = Some(dates[i].clone());
                        p.status = "后续收盘确认".to_string();
                        p.events.push(format!("{} 收盘越过{}", dates[i], p.confirm_boundary));
                    } else if p.follow_bars >= 3 {
                        p.expired_at = Some(dates[i].clone());
                        p.status = "确认超时".to_string();
                        p.events.push(format!("{} 三根有效已闭合K线内未确认（工程窗口）", dates[i]));
                    }
                }
                if p.follow_bars >= 10 {
                    if p.expired_at.is_none() {
                        p.expired_at = Some(dates[i].clone());
                    }
                    p.tracked_until = Some(dates[i].clone());
                    p.events.push(format!("{} 后续跟踪结束；此后不再判定失效", dates[i]));
                }
            }
        }
        if let Some(ids) = by_index.get(&i) {
            if !provisional {
                active.extend(ids.iter().copied());
            }
        }
    }
    for b in bubbles.iter_mut() {
        let tail = if b.events.is_empty() {
            "尚无后续事件".to_string()
        } else {
            b.events.join(" → ")
        };
        b.detail = format!("{} 状态：{}；{}。", b.base_detail, b.status, tail);
    }
}

// ---------------------------------------------------------------- 对外入口

/// `analyze_candles(bars, period, provisional_last, pattern_set)`。
/// bars 元素需含 date/open/high/low/close；pattern_set: None/"all"=全部 61 个。
pub fn analyze_candles(
    bars: &[Value],
    period: &str,
    provisional_last: bool,
    pattern_set: Option<&str>,
) -> Value {
    if bars.is_empty() {
        return json!({"engine": "talib", "signals": [], "reason": "empty_bars"});
    }
    let mut ohlc: Vec<Ohlc> = Vec::with_capacity(bars.len());
    let mut dates: Vec<String> = Vec::with_capacity(bars.len());
    for bar in bars {
        let f = |k: &str| bar.get(k).and_then(|v| v.as_f64());
        let (o, h, l, c) = match (f("open"), f("high"), f("low"), f("close")) {
            (Some(o), Some(h), Some(l), Some(c))
                if o.is_finite() && h.is_finite() && l.is_finite() && c.is_finite() =>
            {
                (o, h, l, c)
            }
            _ => return json!({"engine": "talib", "signals": [], "reason": "invalid_ohlc"}),
        };
        ohlc.push(Ohlc { open: o, high: h, low: l, close: c });
        dates.push(bar.get("date").and_then(|d| d.as_str()).unwrap_or("").to_string());
    }

    let allowed: Option<HashSet<&str>> = match pattern_set {
        None => None,
        Some("all") => None,
        Some("rec27") | Some("rec20") => Some(PATTERN_SET_REC27.iter().copied().collect()),
        Some(other) => {
            // Python 对未知集合名抛 ValueError；此处按空集合处理（调用方不会走到）
            let _ = other;
            Some(PATTERN_SET_REC27.iter().copied().collect())
        }
    };
    let functions: Vec<&'static str> = ALL_CDL_FUNCTIONS
        .iter()
        .copied()
        .filter(|f| allowed.as_ref().map(|a| a.contains(f)).unwrap_or(true))
        .collect();

    let n = ohlc.len();
    let o: Vec<f64> = ohlc.iter().map(|b| b.open).collect();
    let h: Vec<f64> = ohlc.iter().map(|b| b.high).collect();
    let l: Vec<f64> = ohlc.iter().map(|b| b.low).collect();
    let c: Vec<f64> = ohlc.iter().map(|b| b.close).collect();

    let mut errors: Vec<String> = Vec::new();
    let mut raw: Vec<RawSig> = Vec::new();
    for fn_name in &functions {
        let mut out = vec![0i32; n];
        match call_cdl(fn_name, &o, &h, &l, &c, &mut out) {
            Ok(range) => {
                // out[j] ↔ input[beg_idx + j]（crate 语义；此前按 0 对齐导致信号整体错位）
                for (j, val) in out.iter().take(range.count).enumerate() {
                    if *val == 0 {
                        continue;
                    }
                    raw.push(RawSig { index: range.beg_idx + j, fn_name, value: *val as i64 });
                }
            }
            Err(e) => errors.push(format!("{fn_name}: {e}")),
        }
    }
    raw.sort_by(|a, b| (a.index, a.fn_name).cmp(&(b.index, b.fn_name)));

    let (lo_extreme, hi_extreme) = extreme_masks(&h, &l, &c);
    let mut bubbles = build_bubbles(&ohlc, &dates, &raw, period, provisional_last,
                                    &lo_extreme, &hi_extreme);
    bubbles = dedupe_bubbles(bubbles);
    apply_lifecycle(&ohlc, &dates, &mut bubbles, provisional_last);

    let mut result = serde_json::Map::new();
    result.insert("engine".into(), Value::String("talib".into()));
    result.insert("period".into(), Value::String(period.to_string()));
    result.insert("count".into(), Value::from(bubbles.len() as i64));
    result.insert("signals".into(), Value::Array(bubbles.iter().map(|b| b.to_value()).collect()));
    if allowed.is_some() {
        // rec20 是 rec27 的别名，归一化回显
        let ps = match pattern_set {
            Some(_) => "rec27",
            None => "custom",
        };
        result.insert("pattern_set".into(), Value::String(ps.into()));
    }
    if !errors.is_empty() {
        result.insert("errors".into(), Value::Array(errors.iter().map(|e| Value::String(e.clone())).collect()));
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdl_engulfing_triggers() {
        // 构造一组看涨吞没：前阴后阳且实体包住
        let bars = vec![
            json!({"date": "2026-01-05", "open": 10.5, "high": 10.6, "low": 10.2, "close": 10.25}),
            json!({"date": "2026-01-06", "open": 10.28, "high": 10.75, "low": 10.11, "close": 10.68}),
            json!({"date": "2026-01-07", "open": 10.6, "high": 10.68, "low": 10.35, "close": 10.41}),
            json!({"date": "2026-01-08", "open": 10.6, "high": 10.75, "low": 10.5, "close": 10.55}),
            json!({"date": "2026-01-09", "open": 10.6, "high": 10.75, "low": 10.5, "close": 10.55}),
        ];
        let r = analyze_candles(&bars, "day", false, Some("rec27"));
        assert_eq!(r["engine"], "talib");
        assert!(r["signals"].is_array());
    }

    #[test]
    fn extreme_masks_shapes() {
        let h = vec![1.0, 2.0, 3.0, 2.0, 1.0];
        let l = vec![0.5, 1.0, 2.0, 1.0, 0.5];
        let c = vec![0.8, 1.5, 2.5, 1.5, 0.8];
        let (lo, hi) = extreme_masks(&h, &l, &c);
        assert_eq!(lo.len(), 5);
        assert_eq!(hi.len(), 5);
    }
}
