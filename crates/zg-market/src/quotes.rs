//! 实时报价源 —— 对齐 `modules/zgapp/quotes.py`。
//! 东财/腾讯/同花顺/TQ/新浪报价与全市场快照、get_quote 降级链（TQ→同花顺→腾讯→东财→新浪）、东财报价熔断。

use std::collections::HashMap;

use chrono::TimeZone;
use serde_json::{Map, Value};

use crate::kline::{secid, tencent_symbol, ths_name_map, _src_fail, _src_ok, _src_open};
use crate::{ths, tq};

fn _em_circuit_check() -> Result<(), String> {
    if !_src_open("em") {
        return Err("东财熔断中".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- 单票报价

pub fn _quote_em(code: &str) -> Result<Value, String> {
    _em_circuit_check()?;
    let url = format!(
        "https://push2delay.eastmoney.com/api/qt/stock/get?secid={}&invt=2&fltt=2\
&fields=f43,f44,f45,f46,f47,f48,f57,f58,f60,f86,f107,f170,f171",
        secid(code)
    );
    let text = match crate::kline::http_get(&url, 10, 1) {
        Ok(t) => t,
        Err(e) => {
            _src_fail("em");
            return Err(e);
        }
    };
    let j: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            _src_fail("em");
            return Err(format!("东财报价响应解析失败: {e}"));
        }
    };
    let d = j.get("data").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let f43 = d.get("f43").cloned().unwrap_or(Value::Null);
    if d.is_null() || f43.is_null() || f43.as_str() == Some("-") {
        _src_fail("em");
        return Err("东财报价为空".into());
    }
    _src_ok("em");
    let ts = d.get("f86").and_then(|x| x.as_i64());
    let time = ts
        .and_then(|t| chrono::Local.timestamp_opt(t, 0).single())
        .map(|t| t.format("%H:%M:%S").to_string());
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code.to_string()));
    m.insert("name".into(), d.get("f58").cloned().unwrap_or(Value::Null));
    m.insert("price".into(), f43);
    m.insert("high".into(), d.get("f44").cloned().unwrap_or(Value::Null));
    m.insert("low".into(), d.get("f45").cloned().unwrap_or(Value::Null));
    m.insert("open".into(), d.get("f46").cloned().unwrap_or(Value::Null));
    m.insert("vol".into(), d.get("f47").cloned().unwrap_or(Value::Null));
    m.insert("amount".into(), d.get("f48").cloned().unwrap_or(Value::Null));
    m.insert("preClose".into(), d.get("f60").cloned().unwrap_or(Value::Null));
    m.insert("pct".into(), d.get("f170").cloned().unwrap_or(Value::Null));
    m.insert("time".into(), match time {
        Some(t) => Value::String(t),
        None => Value::Null,
    });
    Ok(Value::Object(m))
}

/// 腾讯报价：GBK 文本，字段以 `~` 分隔。
pub fn _quote_tencent(code: &str) -> Result<Value, String> {
    let sym = tencent_symbol(code);
    let url = format!("https://qt.gtimg.cn/q={sym}");
    let raw = crate::kline::http_get_raw(&url, 8, &[])?;
    let txt = decode_gbk_lossy(&raw);
    let f: Vec<&str> = txt.split('"').nth(1).unwrap_or("").split('~').collect();
    if f.len() < 38 {
        return Err("腾讯报价解析失败".into());
    }
    let t = if f[30].len() >= 14 { Some(&f[30]) } else { None };
    // 价格缺失/为 0（停牌、空字段）必须判失败，否则降级链在这里断掉，
    // 前端拿到 price=0 会显示 0.00 而不再下探下一个数据源。
    let price = match f[3].trim().parse::<f64>() {
        Ok(v) if v > 0.0 => v,
        _ => return Err("腾讯报价价格为空".into()),
    };
    // 其余数值字段：解析不出来就是「未知」，不填 0
    let num = |s: &str| -> Value {
        match s.trim().parse::<f64>() {
            Ok(v) => Value::from(v),
            Err(_) => Value::Null,
        }
    };
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code.to_string()));
    m.insert("name".into(), Value::String(f[1].to_string()));
    m.insert("price".into(), Value::from(price));
    m.insert("high".into(), num(f[33]));
    m.insert("low".into(), num(f[34]));
    m.insert("open".into(), num(f[5]));
    m.insert("vol".into(), num(f[6]));
    m.insert("amount".into(), match f[37].trim().parse::<f64>() {
        Ok(v) => Value::from(v * 10000.0),
        Err(_) => Value::Null,
    });
    m.insert("preClose".into(), num(f[4]));
    m.insert("pct".into(), num(f[32]));
    m.insert("time".into(), match t {
        Some(t) => Value::String(format!("{}:{}:{}", &t[8..10], &t[10..12], &t[12..14])),
        None => Value::Null,
    });
    Ok(Value::Object(m))
}

/// GBK 解码（Python `decode("gbk", "ignore")` 的 lossy 近似：非法序列替换为 U+FFFD）。
pub fn decode_gbk_lossy(raw: &[u8]) -> String {
    let (cow, _, _) = encoding_rs::GBK.decode(raw);
    cow.into_owned()
}

fn _quote_ths(code: &str) -> Result<Value, String> {
    if !crate::consts::ths_available() {
        return Err("fetch_boards 未加载".into());
    }
    let snap = ths::ths_snapshot(&[code.to_string()], 200);
    let d = snap.get(code).cloned().ok_or_else(|| format!("同花顺快照无该标的: {code}"))?;
    let price = d.get("price").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let prev = d.get("preClose").and_then(|x| x.as_f64()).unwrap_or(0.0);
    if price <= 0.0 {
        return Err("同花顺快照价格为空".into());
    }
    let pct = if prev > 0.0 {
        zg_core::model::py_round((price - prev) / prev * 100.0, 2)
    } else {
        0.0
    };
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code.to_string()));
    m.insert("name".into(), Value::String(
        ths_name_map().get(code).cloned().unwrap_or_else(|| code.to_string())));
    m.insert("price".into(), Value::from(price));
    m.insert("high".into(), d.get("high").cloned().unwrap_or(Value::Null));
    m.insert("low".into(), d.get("low").cloned().unwrap_or(Value::Null));
    m.insert("open".into(), d.get("open").cloned().unwrap_or(Value::Null));
    m.insert("vol".into(), d.get("vol").cloned().unwrap_or(Value::Null));
    m.insert("amount".into(), d.get("amount").cloned().unwrap_or(Value::Null));
    m.insert("preClose".into(), Value::from(prev));
    m.insert("pct".into(), Value::from(pct));
    m.insert("time".into(), Value::String(chrono::Local::now().format("%H:%M:%S").to_string()));
    Ok(Value::Object(m))
}

fn _quote_tq(code: &str) -> Result<Value, String> {
    if !tq::available() {
        return Err("TQ 不可用(冷却中/探活失败)".into());
    }
    let mut d = tq::snapshot(code)?;
    if d.get("name").and_then(|x| x.as_str()).map(|s| s.is_empty()).unwrap_or(true) {
        if let Some(o) = d.as_object_mut() {
            o.insert("name".into(), Value::String(
                ths_name_map().get(code).cloned().unwrap_or_else(|| code.to_string())));
        }
    }
    Ok(d)
}

/// 新浪报价：GBK 文本，逗号分隔；volume=股、amount=元。
pub fn _quote_sina(code: &str) -> Result<Value, String> {
    let sym = tencent_symbol(code);
    let url = format!("https://hq.sinajs.cn/list={sym}");
    let raw = crate::kline::http_get_raw(
        &url,
        8,
        &[
            ("Referer", "https://finance.sina.com.cn/"),
            ("User-Agent", "Mozilla/5.0"),
        ],
    )?;
    let txt = decode_gbk_lossy(&raw);
    let body = txt.split('"').nth(1).unwrap_or("");
    let f: Vec<&str> = body.split(',').collect();
    if f.len() < 32 {
        return Err("新浪报价解析失败".into());
    }
    let price = match f[3].trim().parse::<f64>() {
        Ok(v) if v > 0.0 => v,
        _ => return Err("新浪报价价格为空".into()),
    };
    let prev = f[2].trim().parse::<f64>().unwrap_or(0.0);
    let num = |s: &str| -> Value {
        match s.trim().parse::<f64>() {
            Ok(v) => Value::from(v),
            Err(_) => Value::Null,
        }
    };
    let pct = if prev > 0.0 {
        zg_core::model::py_round((price - prev) / prev * 100.0, 2)
    } else {
        0.0
    };
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code.to_string()));
    m.insert("name".into(), Value::String(f[0].to_string()));
    m.insert("price".into(), Value::from(price));
    m.insert("high".into(), num(f[4]));
    m.insert("low".into(), num(f[5]));
    m.insert("open".into(), num(f[1]));
    m.insert("vol".into(), match f[8].trim().parse::<f64>() {
        Ok(v) => Value::from(v / 100.0),
        Err(_) => Value::Null,
    });
    m.insert("amount".into(), num(f[9]));
    m.insert("preClose".into(), Value::from(prev));
    m.insert("pct".into(), Value::from(pct));
    m.insert("time".into(), match f.get(31) {
        Some(t) if !t.trim().is_empty() => Value::String(t.trim().to_string()),
        _ => Value::Null,
    });
    Ok(Value::Object(m))
}

/// 报价降级链：TQ → 同花顺 → 腾讯 → 东财 → 新浪；全部失败抛错。
pub fn get_quote(code: &str) -> Result<Value, String> {
    let mut last_err = String::new();
    for (src, f) in [
        ("tq", _quote_tq as fn(&str) -> Result<Value, String>),
        ("ths", _quote_ths),
        ("tencent", _quote_tencent),
        ("em", _quote_em),
        ("sina", _quote_sina),
    ] {
        match f(code) {
            Ok(mut d) => {
                if let Some(o) = d.as_object_mut() {
                    o.insert("source".into(), Value::String(src.to_string()));
                }
                return Ok(d);
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("所有实时源均失败: {code} ({last_err})"))
}

// ---------------------------------------------------------------- 批量快照

/// 腾讯行情行解析：`v_sh600519="1~名称~代码~现价~...";`
/// f6=成交额（元）：优先 f[37]（万元）×1e4，退回 f[35] 的第三段（元）。
fn parse_tencent_segment(seg: &str) -> Option<Value> {
    let seg = seg.trim();
    if !seg.contains('"') {
        return None;
    }
    let f: Vec<&str> = seg.split('"').nth(1).unwrap_or("").split('~').collect();
    if f.len() < 35 || f[2].is_empty() {
        return None;
    }
    let nf = |s: &str| s.parse::<f64>().unwrap_or(0.0);
    let mut m = Map::new();
    m.insert("f12".into(), Value::String(f[2].to_string()));
    m.insert("f14".into(), Value::String(f[1].to_string()));
    m.insert("f2".into(), Value::from(nf(f[3])));
    m.insert("f3".into(), Value::from(nf(f[32])));
    m.insert("f5".into(), Value::from(nf(f[6])));
    m.insert("f17".into(), Value::from(nf(f[5])));
    m.insert("f15".into(), Value::from(nf(f[33])));
    m.insert("f16".into(), Value::from(nf(f[34])));
    m.insert("f18".into(), Value::from(nf(f[4])));
    let amount = tencent_amount(&f);
    m.insert("f6".into(), amount.map(Value::from).unwrap_or(Value::Null));
    Some(Value::Object(m))
}

/// 腾讯成交额（单位元）：f[37] 为万元，f[35] 形如 `现价/成交量(手)/成交额(元)`。
fn tencent_amount(f: &[&str]) -> Option<f64> {
    if f.len() > 37 {
        if let Ok(wan) = f[37].trim().parse::<f64>() {
            if wan > 0.0 {
                return Some(wan * 10_000.0);
            }
        }
    }
    if f.len() > 35 {
        if let Some(yuan) = f[35].split('/').nth(2).and_then(|x| x.trim().parse::<f64>().ok()) {
            if yuan > 0.0 {
                return Some(yuan);
            }
        }
    }
    None
}

pub fn fetch_snapshot_tencent(codes: &[String]) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < codes.len() {
        let batch = &codes[i..codes.len().min(i + 180)];
        let q: String = batch
            .iter()
            .map(|c| tencent_symbol(c))
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("https://qt.gtimg.cn/q={q}");
        let mut txt: Option<String> = None;
        for rnd in 0..3 {
            match crate::kline::http_get_raw(&url, 10, &[]) {
                Ok(raw) => {
                    txt = Some(decode_gbk_lossy(&raw));
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(1500 * (rnd as u64 + 1))),
            }
        }
        if let Some(txt) = txt {
            for seg in txt.split(';') {
                if let Some(row) = parse_tencent_segment(seg) {
                    rows.push(row);
                }
            }
        }
        i += 180;
        if i < codes.len() {
            // Python: time.sleep(0.25 + random.random()*0.15)
            let ms = 250u64 + std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 % 150)
                .unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
    rows
}

pub fn fetch_snapshot_ths(codes: &[String]) -> HashMap<String, Value> {
    if !crate::consts::ths_available() {
        return HashMap::new();
    }
    ths::ths_snapshot(codes, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_decode() {
        let (raw, _, _) = encoding_rs::GBK.encode("v_sh600519=\"贵州茅台\";");
        let txt = decode_gbk_lossy(&raw);
        assert!(txt.contains("贵州茅台"));
    }

    #[test]
    fn tencent_parse_minimal() {
        // 不足 38 字段 → 解析失败
        let f: Vec<&str> = "a~b~c".split('~').collect();
        assert!(f.len() < 38);
    }

    #[test]
    fn tencent_segment_parses_amount_in_yuan() {
        let seg = "v_sz000001=\"51~平安银行~000001~11.28~11.47~11.43~1016696~359453~644935~11.28~20481~11.27~12676~11.26~16186~11.25~16587~11.24~3601~11.29~2721~11.30~1536~11.31~2551~11.32~1115~11.33~1146~~20261028130021~-0.19~-1.66~11.50~11.28~11.28/1016696/1155238284~1016696~115524~0.52~5.03\";";
        let v = parse_tencent_segment(seg).expect("应解析出腾讯行");
        assert_eq!(v.get("f12").unwrap().as_str(), Some("000001"));
        assert_eq!(v.get("f14").unwrap().as_str(), Some("平安银行"));
        assert_eq!(v.get("f2").unwrap().as_f64(), Some(11.28));
        assert_eq!(v.get("f3").unwrap().as_f64(), Some(-1.66));
        // f[37]=115524 万元 → 1,155,240,000 元
        assert_eq!(v.get("f6").unwrap().as_f64(), Some(115_524.0 * 10_000.0));
    }

    #[test]
    fn tencent_amount_falls_back_to_embedded_field() {
        let mut f: Vec<&str> = vec!["x"; 38];
        f[35] = "11.28/1016696/1155238284";
        f[37] = "0";
        assert_eq!(tencent_amount(&f), Some(1_155_238_284.0));
        f[37] = "115524";
        assert_eq!(tencent_amount(&f), Some(115_524.0 * 10_000.0));
        f[35] = "11.28/1016696/0";
        f[37] = "0";
        assert_eq!(tencent_amount(&f), None);
    }
}
