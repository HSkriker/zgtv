//! 全市场股票列表：五源轮换刷新（同花顺 → 东财 → Ashare → 腾讯 → 新浪）。
//!
//! 对齐 `modules/zgapp/jobs/stock_list.py`：每源 2 次尝试、成功即返回
//! 东财 clist 字段形状（f2/f3/f5/f6/f12/f14/f15/f16/f17/f18），
//! 由调用方落 `stocks.json`（`{code, name}`）。
//!
//! 与 Python 的差异：AKShare 源无 Rust 等价实现，直接跳过（Python 未安装时同样跳过）。

use std::collections::HashMap;

use serde_json::{json, Value};
use zg_core::net::{http_get, http_get_json};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

const CLIST_HOSTS: &[&str] = &[
    "push2delay.eastmoney.com",
    "push2.eastmoney.com",
    "48.push2.eastmoney.com",
    "83.push2.eastmoney.com",
    "90.push2.eastmoney.com",
];

const MIN_ROWS: usize = 4000;

fn sleep_secs(s: f64) {
    std::thread::sleep(std::time::Duration::from_millis((s * 1000.0) as u64));
}

/// Python `random.random()` 的替身：只要 0..1 的抖动，不需要密码学随机。
fn rand01() -> f64 {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (n % 1000) as f64 / 1000.0
}

fn safe_num(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Number(n)) => Value::Number(n.clone()),
        Some(Value::String(s)) => match s.parse::<f64>() {
            Ok(f) => json!(f),
            Err(_) => json!("-"),
        },
        _ => json!("-"),
    }
}

fn cached_codes() -> Vec<String> {
    let path = zg_core::paths::get().market_data.join("stocks.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_str::<Vec<Value>>(text.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|it| it.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .collect()
}

// ---------------------------------------------------------------- 同花顺

/// 分页硬上限：2000 条/页 × 10 页 = 2 万只，覆盖全市场绰绰有余；
/// 超过即视为服务端忽略 offset（否则会死循环 + Vec 无限增长）。
const THS_MAX_PAGES: usize = 10;

/// 同花顺：名称来自 `/api/meta/tickers/list`，价量来自 `/api/a-share/prices/snapshot` 分页。
pub fn fetch_stock_list_ths() -> Result<Vec<Value>, String> {
    if !crate::consts::ths_available() {
        return Err("同花顺源不可用".into());
    }
    let mut names: HashMap<String, String> = HashMap::new();
    let mut off = 0usize;
    let mut pages = 0usize;
    loop {
        pages += 1;
        if pages > THS_MAX_PAGES {
            return Err(format!(
                "同花顺 tickers 分页超过 {THS_MAX_PAGES} 页（offset 可能被忽略），中止"
            ));
        }
        let d = crate::ths::ths_get(
            "/api/meta/tickers/list",
            &[
                ("exchange".into(), "SH,SZ,BJ".into()),
                ("asset_type".into(), "a-share".into()),
                ("limit".into(), "2000".into()),
                ("offset".into(), off.to_string()),
            ],
            20,
            2,
        )?;
        let its = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if its.is_empty() {
            break;
        }
        for it in &its {
            let tk = it
                .get("ticker")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    it.get("thscode")
                        .and_then(|v| v.as_str())
                        .map(|s| s.chars().take_while(|c| *c != '.').collect())
                })
                .unwrap_or_default();
            if !tk.is_empty() {
                names.insert(
                    tk,
                    it.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                );
            }
        }
        if its.len() < 2000 {
            break;
        }
        off += 2000;
    }

    let mut rows: Vec<Value> = Vec::new();
    let limit = 2000usize;
    let mut offset = 0usize;
    loop {
        // 同上：服务端忽略 offset 时 len 恒等于 limit，会无限翻页
        if offset / limit >= THS_MAX_PAGES {
            return Err(format!(
                "同花顺行情分页超过 {THS_MAX_PAGES} 页（offset 可能被忽略），中止"
            ));
        }
        let d = crate::ths::ths_get(
            "/api/a-share/prices/snapshot",
            &[
                ("limit".into(), limit.to_string()),
                ("offset".into(), offset.to_string()),
            ],
            25,
            2,
        )?;
        let its = d.get("item").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if its.is_empty() {
            break;
        }
        for it in &its {
            let Some(tk) = it.get("ticker").and_then(|v| v.as_str()) else {
                continue;
            };
            // 停牌/无成交标的只有代码没有价量：保留 name/code（列表搜索用），数值置 null
            let num = |k: &str| -> Value {
                match it.get(k).and_then(|v| v.as_f64()) {
                    Some(v) => Value::from(v),
                    None => Value::Null,
                }
            };
            rows.push(json!({
                "f2": num("last_price"), "f3": num("price_change_ratio_pct"),
                "f5": num("volume"), "f6": num("turnover"), "f12": tk,
                "f14": names.get(tk).cloned().unwrap_or_default(),
                "f15": num("high_price"), "f16": num("low_price"),
                "f17": num("open_price"), "f18": num("prev_price"),
            }));
        }
        if its.len() < limit {
            break;
        }
        offset += limit;
    }
    if rows.len() < MIN_ROWS {
        return Err(format!("同花顺股票列表获取不完整({}条)", rows.len()));
    }
    Ok(rows)
}

// ---------------------------------------------------------------- 东财

/// 东财 clist 分页（100 条/页，镜像轮换 + 限速）。
pub fn fetch_stock_list() -> Result<Vec<Value>, String> {
    let mut rows: Vec<Value> = Vec::new();
    let mut pn = 1usize;
    let mut total = usize::MAX;
    while pn <= 70 && rows.len() < total {
        let url = format!(
            "https://push2delay.eastmoney.com/api/qt/clist/get?pn={pn}&pz=100&po=1&np=1\
&fltt=2&invt=2&fid=f12&fs=m:0+t:6,m:0+t:80,m:1+t:2,m:1+t:23\
&fields=f2,f3,f5,f6,f12,f14,f15,f16,f17,f18"
        );
        let mut j: Option<Value> = None;
        for rnd in 0..3 {
            match http_get_json(&url, 12, 0, Some(CLIST_HOSTS), None, None) {
                Ok(v) => {
                    j = Some(v);
                    break;
                }
                Err(_) => sleep_secs(3.0 + rnd as f64 * 3.0),
            }
        }
        let j = j.ok_or_else(|| format!("第{pn}页获取失败"))?;
        let d = j.get("data").cloned().unwrap_or(json!({}));
        let diff: Vec<Value> = match d.get("diff") {
            Some(Value::Array(a)) => a.clone(),
            // 少数镜像在 np!=1 时返回 {"0":{...},"1":{...}}
            Some(Value::Object(o)) => o.values().cloned().collect(),
            _ => Vec::new(),
        };
        if diff.is_empty() {
            break;
        }
        rows.extend(diff);
        total = d.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        pn += 1;
        sleep_secs(0.4 + rand01() * 0.3);
    }
    if rows.len() < MIN_ROWS {
        return Err(format!("股票列表获取不完整({}条)，将重试", rows.len()));
    }
    Ok(rows)
}

// ---------------------------------------------------------------- 新浪

/// 新浪 Market_Center 分页（100 条/页，最多 80 页）。
pub fn fetch_stock_list_sina() -> Result<Vec<Value>, String> {
    let mut rows: Vec<Value> = Vec::new();
    let mut page = 1usize;
    while page <= 80 {
        let url = format!(
            "https://vip.stock.finance.sina.com.cn/quotes_service/api/json_v2.php/\
Market_Center.getHQNodeData?page={page}&num=100&sort=symbol&asc=1&node=hs_a&symbol=&_s_r_a=sort"
        );
        let mut txt: Option<String> = None;
        for rnd in 0..3 {
            match http_get(
                &url,
                12,
                0,
                None,
                Some(&[("Referer", "https://finance.sina.com.cn/"), ("User-Agent", UA)]),
                None,
            ) {
                Ok(t) => {
                    txt = Some(t);
                    break;
                }
                Err(_) => sleep_secs(3.0 + rnd as f64 * 3.0),
            }
        }
        let Some(txt) = txt else { break };
        let t = txt.trim();
        if t.is_empty() || t == "[]" || t == "null" {
            break;
        }
        let Ok(data) = serde_json::from_str::<Value>(t) else {
            break;
        };
        let Some(arr) = data.as_array() else { break };
        if arr.is_empty() {
            break;
        }
        for item in arr {
            let code = item.get("code").and_then(|v| v.as_str()).unwrap_or("");
            if code.len() < 6 {
                continue;
            }
            rows.push(json!({
                "f2": safe_num(item.get("trade")),
                "f3": safe_num(item.get("changepercent")),
                "f5": safe_num(item.get("volume")),
                "f6": safe_num(item.get("amount")),
                "f12": code,
                "f14": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "f15": safe_num(item.get("high")),
                "f16": safe_num(item.get("low")),
                "f17": safe_num(item.get("open")),
                "f18": safe_num(item.get("settlement")),
            }));
        }
        page += 1;
        sleep_secs(0.3);
    }
    if rows.len() < MIN_ROWS {
        return Err(format!("新浪股票列表获取不完整({}条)", rows.len()));
    }
    Ok(rows)
}

// ---------------------------------------------------------------- 腾讯

/// 腾讯：需要 stocks.json 里的代码列表（≥4000 只）。
pub fn fetch_stock_list_tencent() -> Result<Vec<Value>, String> {
    let codes = cached_codes();
    if codes.len() < MIN_ROWS {
        return Err(format!(
            "腾讯源需要缓存代码列表，当前为空或不完整({}条)",
            codes.len()
        ));
    }
    let rows = crate::quotes::fetch_snapshot_tencent(&codes);
    if rows.len() < MIN_ROWS {
        return Err(format!("腾讯股票列表获取不完整({}条)", rows.len()));
    }
    Ok(rows)
}

// ---------------------------------------------------------------- Ashare（新浪 hq 批量）

/// 解析一行 `var hq_str_sh600519="名称,开,昨收,现价,高,低,...,量,额,...";`
pub fn parse_sina_line(line: &str) -> Option<Value> {
    let (head, rest) = line.split_once('=')?;
    let sym = head.trim().strip_prefix("var hq_str_")?;
    let code: String = sym.chars().skip(2).collect();
    if code.len() < 6 {
        return None;
    }
    let raw = rest.trim().trim_end_matches(';').trim().trim_matches('"');
    let f: Vec<&str> = raw.split(',').collect();
    if f.len() < 10 {
        return None;
    }
    let num = |i: usize| -> Value {
        match f[i].trim().parse::<f64>() {
            Ok(v) => json!(v),
            Err(_) => json!("-"),
        }
    };
    let price: f64 = match f[3].trim().parse() {
        Ok(v) => v,
        Err(_) => return None,
    };
    let pre = f[2].trim().parse::<f64>().ok();
    let pct = match pre {
        Some(pc) if pc > 0.0 => {
            json!(zg_core::model::py_round((price / pc - 1.0) * 100.0, 2))
        }
        _ => json!("-"),
    };
    Some(json!({
        "f2": price, "f3": pct, "f5": num(8), "f6": num(9),
        "f12": code, "f14": f[0],
        "f15": num(4), "f16": num(5), "f17": num(1), "f18": num(2),
    }))
}

/// Ashare 源：新浪 `hq.sinajs.cn` 批量（300 只/批），不足 4000 时回落腾讯。
pub fn fetch_stock_list_ashare() -> Result<Vec<Value>, String> {
    let codes = cached_codes();
    if codes.len() < MIN_ROWS {
        if !codes.is_empty() {
            let rows = crate::quotes::fetch_snapshot_tencent(&codes);
            if rows.len() >= MIN_ROWS {
                return Ok(rows);
            }
        }
        return Err("Ashare源需要缓存代码列表，当前为空或不完整".into());
    }
    let mut rows: Vec<Value> = Vec::new();
    for chunk in codes.chunks(300) {
        let q: String = chunk
            .iter()
            .map(|c| {
                let mkt = if c.starts_with(['6', '9', '5']) { "sh" } else { "sz" };
                format!("{mkt}{c}")
            })
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("https://hq.sinajs.cn/list={q}");
        let mut txt: Option<String> = None;
        for rnd in 0..3 {
            match crate::kline::http_get_raw(
                &url,
                12,
                &[("Referer", "https://finance.sina.com.cn/"), ("User-Agent", UA)],
            ) {
                Ok(raw) => {
                    txt = Some(crate::quotes::decode_gbk_lossy(&raw));
                    break;
                }
                Err(_) => sleep_secs(2.0 + rnd as f64 * 2.0),
            }
        }
        if let Some(txt) = txt {
            for line in txt.lines() {
                if let Some(row) = parse_sina_line(line) {
                    rows.push(row);
                }
            }
        }
        sleep_secs(0.3 + rand01() * 0.2);
    }
    if rows.len() < MIN_ROWS {
        let tx = crate::quotes::fetch_snapshot_tencent(&codes);
        if tx.len() >= MIN_ROWS {
            return Ok(tx);
        }
        return Err(format!("Ashare源股票列表获取不完整({}条)", rows.len()));
    }
    Ok(rows)
}

// ---------------------------------------------------------------- 编排

/// 股票列表取数源：(源名, 取数函数)
type StockSource = (&'static str, fn() -> Result<Vec<Value>, String>);

/// 五源轮换（同花顺→东财→Ashare→腾讯→新浪），每源 2 次尝试。
pub fn fetch_all() -> Result<(Vec<Value>, &'static str), String> {
    // 用户 2026-10-30 裁定：同花顺 0.8s 全量，提到首位；东财失败时不再白等
    let mut sources: Vec<StockSource> = Vec::new();
    if crate::ths::enabled() {
        sources.push(("同花顺", fetch_stock_list_ths));
    }
    sources.extend([
        ("东财", fetch_stock_list as fn() -> Result<Vec<Value>, String>),
        ("Ashare", fetch_stock_list_ashare),
        ("腾讯", fetch_stock_list_tencent),
        ("新浪", fetch_stock_list_sina),
    ]);
    let mut last = String::new();
    for (name, f) in sources {
        for _ in 0..2 {
            match f() {
                Ok(rows) => return Ok((rows, name)),
                Err(e) => {
                    last = format!("{name}: {e}");
                    sleep_secs(3.0);
                }
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sina_line_parses() {
        let line = "var hq_str_sh600519=\"贵州茅台,1325.400,1322.600,1330.750,1357.000,1322.600,1330.740,1330.750,3880493,5187324416,0\";";
        let v = parse_sina_line(line).expect("应解析");
        assert_eq!(v.get("f12").unwrap().as_str(), Some("600519"));
        assert_eq!(v.get("f14").unwrap().as_str(), Some("贵州茅台"));
        assert_eq!(v.get("f2").unwrap().as_f64(), Some(1330.75));
        assert_eq!(v.get("f18").unwrap().as_f64(), Some(1322.6));
        assert_eq!(v.get("f3").unwrap().as_f64(), Some(0.62));
    }

    #[test]
    fn sina_line_rejects_short() {
        assert!(parse_sina_line("var hq_str_sh600519=\"x\";").is_none());
        assert!(parse_sina_line("garbage").is_none());
    }
}
