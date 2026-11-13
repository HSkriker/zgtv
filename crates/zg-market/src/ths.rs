//! 同花顺数据源 —— 对齐 `modules/market/fetch_boards.py` 的服务器路径子集：
//! key 读取（**绝不打日志**）、`ths_get`（4001/429 指数退避）、`to_thscode`、
//! `ths_snapshot`（坏码剔除重试）、历史 K 线（kline_ths 的取数段）。

use zg_core::sync::MutexExt;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::TimeZone;
use serde_json::Value;
use zg_core::net::read_url;

use crate::kbar::{num, py_float, KBar};

const THS_BASE: &str = "https://fuyao.aicubes.cn";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 同花顺凭据：环境变量 > credentials.env > THS_API_KEY > 本地 ths_key.txt。
/// 返回的 key 只用于请求头，任何日志/错误消息都不允许包含它。
pub fn api_key() -> Option<String> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| {
        for var in ["HITHINK_FINANCE_API_KEY", "HITHINK_API_KEY", "THS_API_KEY"] {
            if let Ok(v) = std::env::var(var) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = std::path::Path::new(&appdata)
                .join("hithink-finance")
                .join("credentials.env");
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("HITHINK_FINANCE_API_KEY=") {
                        let v = rest.trim().trim_matches('"').trim_matches('\'');
                        if !v.is_empty() {
                            return Some(v.to_string());
                        }
                    }
                }
            }
        }
        // 独立启动回退读集中目录（同 Python：同目录 + data/settings）
        let root = zg_core::paths::get();
        for p in [
            root.root.join("modules").join("market").join("ths_key.txt"),
            root.settings.join("ths_key.txt"),
        ] {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let v = text.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        None
    })
    .clone()
}

/// 是否配置了 key；未配置时同花顺作为最低权重源整体跳过（不发起网络、不产生错误日志）
pub fn enabled() -> bool {
    api_key().is_some()
}

/// 6 位纯代码 -> 带后缀 thscode（SH/SZ/BJ）。
/// 口径与 `zg_core::reader::market_of_prefix` 一致：920xxx→.BJ、88xxx→.SH、
/// 其余 8xxxx→.BJ、6/9/5xxxxx→.SH、其余→.SZ。
pub fn to_thscode(code6: &str) -> String {
    let c = code6.trim();
    match zg_core::reader::market_of_prefix(c) {
        Some(zg_core::reader::MKT_SH) => format!("{c}.SH"),
        Some(zg_core::reader::MKT_BJ) => format!("{c}.BJ"),
        _ => format!("{c}.SZ"),
    }
}

fn urlencode(params: &[(String, String)]) -> String {
    // Python urllib.parse.urlencode：quote_plus；参数均为无空格简单值
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// 同花顺 REST 调用，返回 data 字典。统一处理 4001 频率超限退避、认证失败、业务错误。
pub fn ths_get(path: &str, params: &[(String, String)], timeout: u64, retries: u32)
    -> Result<Value, String> {
    let key = api_key().ok_or_else(|| {
        "同花顺未配置 API key（该源已跳过）".to_string()
    })?;
    if let Some(t) = crate::net_stub::get(&format!("ths:{path}")) {
        let j: Value = serde_json::from_str(&t).map_err(|e| format!("同花顺响应解析失败: {e}"))?;
        let code = j.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            return Err(format!("业务错误 code={code} msg={}",
                               j.get("message").and_then(|m| m.as_str()).unwrap_or("")));
        }
        return Ok(j.get("data").cloned().unwrap_or_else(|| Value::Object(Default::default())));
    }
    let mut url = format!("{THS_BASE}{path}");
    if !params.is_empty() {
        url.push('?');
        url.push_str(&urlencode(params));
    }
    let mut last = String::new();
    for attempt in 0..retries {
        let out = read_url(
            &url,
            Some(&[("X-api-key", &key), ("User-Agent", UA), ("Accept", "application/json")]),
            timeout,
            0,
            None,
            None,
        );
        match out {
            Ok(raw) => {
                let j: Value = serde_json::from_slice(&raw)
                    .map_err(|e| format!("同花顺响应解析失败: {e}"))?;
                let code = j.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                match code {
                    0 => return Ok(j.get("data").cloned().unwrap_or(Value::Object(Default::default()))),
                    4001 => {
                        // 频率超限：指数退避后重试
                        sleep_ms(500 * (attempt as u64 + 1));
                        last = "频率超限(4001)".into();
                    }
                    2001 | 2003 => {
                        return Err(format!("API key 认证/权限失败 code={code}"));
                    }
                    _ => {
                        return Err(format!("业务错误 code={code} msg={}",
                                           j.get("message").and_then(|m| m.as_str()).unwrap_or("")));
                    }
                }
            }
            Err(e) => {
                if e.contains("429") {
                    sleep_ms(500 * (attempt as u64 + 1));
                } else {
                    sleep_ms(600 * (attempt as u64 + 1));
                }
                last = e;
            }
        }
    }
    Err(format!("同花顺请求失败(已重试{retries}次): {last}"))
}

/// 已知同花顺不识别的 thscode（如部分北交所/退市），避免整批失败。
static BAD_TS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn bad_ts() -> &'static Mutex<HashSet<String>> {
    BAD_TS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 批量实时快照：6 位纯代码列表 -> {code6: {price,open,high,low,preClose,vol,amount}}。
/// vol 由「股」换算成「手」；amount 用成交额(元)。坏码剔除重试，其余错误整批跳过。
pub fn ths_snapshot(codes6: &[String], batch: usize) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
    if codes6.is_empty() || !enabled() {
        // 未配置 key：最低权重源整体跳过，不发起任何请求
        return out;
    }
    let items: Vec<String> = codes6.to_vec();
    let mut i = 0;
    let unknown_re = regex::Regex::new(r"Unknown thscode:\s*([0-9]{6}\.[A-Z]+)").unwrap();
    // 上一轮本批的代码数：只有本批真的变短才允许重试，否则重发的是同一个请求 → 死循环
    let mut prev_len = usize::MAX;
    while i < items.len() {
        let seg: Vec<String> = items[i..items.len().min(i + batch)]
            .iter()
            .map(|c| to_thscode(c))
            .filter(|t| !bad_ts().lock_ok().contains(t))
            .collect();
        if seg.is_empty() {
            i += batch;
            prev_len = usize::MAX;
            continue;
        }
        let cur_len = seg.len();
        let params = vec![("thscodes".to_string(), seg.join(","))];
        match ths_get("/api/a-share/prices/snapshot", &params, 8, 2) {
            Ok(d) => {
                for it in (d.get("item").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
                    let ts = it.get("thscode").and_then(|x| x.as_str()).unwrap_or("");
                    let code: String = ts.chars().take_while(|c| *c != '.').collect();
                    let mut m = serde_json::Map::new();
                    m.insert("price".into(), Value::from(num(it.get("last_price"))));
                    m.insert("open".into(), Value::from(num(it.get("open_price"))));
                    m.insert("high".into(), Value::from(num(it.get("high_price"))));
                    m.insert("low".into(), Value::from(num(it.get("low_price"))));
                    m.insert("preClose".into(), Value::from(num(it.get("prev_price"))));
                    m.insert("pct".into(), Value::from(num(it.get("price_change_ratio_pct"))));
                    m.insert("vol".into(), Value::from(num(it.get("volume")) / 100.0));
                    m.insert("amount".into(), Value::from(num(it.get("turnover"))));
                    out.insert(code, Value::Object(m));
                }
            }
            Err(e) => {
                let mut retry = false;
                if let Some(cap) = unknown_re.captures(&e) {
                    if let Some(g) = cap.get(1) {
                        bad_ts().lock_ok().insert(g.as_str().to_string());
                        // 剔除的坏码必须真的在本批里（后缀一致才剔得掉）；
                        // 否则重试发的还是同一个请求 → 无限重发
                        retry = cur_len < prev_len;
                    }
                }
                if retry {
                    prev_len = cur_len;
                    continue;
                }
                i += batch; // 其他错误整批跳过
                prev_len = usize::MAX;
                continue;
            }
        }
        i += batch;
        prev_len = usize::MAX;
    }
    out
}

/// 历史日 K（/api/a-share/prices/historical，adjust=forward 前复权）。
/// `beg` 为 "YYYYMMDD"；返回的 KBar.vol 单位手（volume 股 ÷100）、amount 元。
pub fn kline_historical(code6: &str, lmt: usize, beg: Option<&str>) -> Result<Vec<KBar>, String> {
    let now_ms = now_ms();
    let start_ms = match beg {
        Some(b) => ths_ms_of_date(b),
        None => now_ms - ((lmt as f64 * 1.5 + 30.0) * 86400.0 * 1000.0) as i64,
    };
    let params = vec![
        ("thscode".to_string(), to_thscode(code6)),
        ("interval".to_string(), "1d".to_string()),
        ("start".to_string(), start_ms.to_string()),
        ("end".to_string(), now_ms.to_string()),
        ("adjust".to_string(), "forward".to_string()),
    ];
    let d = ths_get("/api/a-share/prices/historical", &params, 20, 5)?;
    let mut bars = Vec::new();
    for it in (d.get("item").and_then(|x| x.as_array()).cloned()).unwrap_or_default() {
        // Python 逐条 try/except (KeyError, ValueError, TypeError)：任一字段非法即跳过
        let date_ms = match it.get("date_ms").and_then(|x| x.as_i64()) {
            Some(v) => v,
            None => continue,
        };
        let date = ths_date_str(date_ms);
        let open = match py_float(it.get("open_price")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let close = match py_float(it.get("close_price")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let high = match py_float(it.get("high_price")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let low = match py_float(it.get("low_price")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let vol = match py_float(it.get("volume")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Python `float(it.get("turnover") or 0)`：None/空/0 等 falsy → 0
        let amount = match it.get("turnover") {
            Some(v) if !v.is_null() => py_float(Some(v)).unwrap_or(0.0),
            _ => 0.0,
        };
        bars.push(KBar { date, open, high, low, close, vol: vol / 100.0, amount });
    }
    if bars.is_empty() {
        return Err("同花顺K线为空".into());
    }
    Ok(bars)
}

/// 'YYYYMMDD' -> 当日 0 点 UTC 毫秒（Python `datetime.strptime(beg, "%Y%m%d").timestamp()`）。
fn ths_ms_of_date(beg: &str) -> i64 {
    let y: i32 = beg.get(0..4).and_then(|v| v.parse().ok()).unwrap_or(0);
    let m: u32 = beg.get(4..6).and_then(|v| v.parse().ok()).unwrap_or(1);
    let d: u32 = beg.get(6..8).and_then(|v| v.parse().ok()).unwrap_or(1);
    let dt = chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap_or(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    let ts = chrono::NaiveDateTime::new(dt, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap())
        .and_utc()
        .timestamp();
    ts * 1000
}

/// ms -> "%Y-%m-%d"（Python `datetime.fromtimestamp(ms/1000).strftime`，本地时区）。
fn ths_date_str(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thscode_mapping() {
        assert_eq!(to_thscode("600519"), "600519.SH");
        assert_eq!(to_thscode("000001"), "000001.SZ");
        // 与 reader::market_of_prefix / tq_symbol 同一口径：920xxx 北交所 .BJ、88xxx 板块 .SH
        assert_eq!(to_thscode("920001"), "920001.BJ");
        assert_eq!(to_thscode("881333"), "881333.SH");
        assert_eq!(to_thscode("430047"), "430047.BJ");
        assert_eq!(to_thscode("830799"), "830799.BJ");
        assert_eq!(to_thscode("900001"), "900001.SH");
    }

    #[test]
    fn ms_of_date_utc() {
        // Python: datetime(2026,1,2).timestamp()*1000 = 1767312000000
        assert_eq!(ths_ms_of_date("20260102"), 1767312000000);
    }
}
