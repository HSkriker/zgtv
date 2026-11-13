use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

type SymbolsMemo = Option<(String, Arc<Vec<Value>>)>;

static CACHE: OnceLock<Mutex<SymbolsMemo>> = OnceLock::new();

fn slot() -> &'static Mutex<SymbolsMemo> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 全市场 symbols.json：按文件指纹缓存（新上市股票重跑 `tv --gen-symbols` 后无需重启）。
pub fn all() -> Result<Arc<Vec<Value>>, String> {
    let path = crate::paths::tv_root()
        .join("data")
        .join("symbols.json");
    let stamp = crate::cache::file_stamp(&path);
    let mut g = slot().lock().map_err(|_| "symbols 缓存锁损坏".to_string())?;
    if let Some((s, v)) = g.as_ref() {
        if *s == stamp {
            return Ok(v.clone());
        }
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读不到 {}: {e}（先跑 tv --gen-symbols）", path.display()))?;
    let rows: Vec<Value> = serde_json::from_str(&text).map_err(|e| format!("symbols.json 解析失败: {e}"))?;
    let arc = Arc::new(rows);
    *g = Some((stamp, arc.clone()));
    Ok(arc)
}

// ---------------------------------------------------------------- 生成 / 拉取

/// `tv --gen-symbols`：`data/market/stocks.json` → `data/symbols.json`。
///
/// 逐行补 `market` / `py`（首字母）/ `pyfull`（全拼），按 code 排序，紧凑 JSON
/// （与旧的 `tools/gen_symbols.py` 输出逐字节同格式）。
pub fn generate() -> Result<String, String> {
    let root = crate::paths::tv_root();
    let src = root.join("data").join("market").join("stocks.json");
    let text = std::fs::read_to_string(&src)
        .map_err(|e| format!("读不到 {}: {e}（先跑 tv --fetch-stocks）", src.display()))?;
    // stocks.json 可能带 UTF-8 BOM（Windows 工具写出）；json.loads 不认，先剥掉
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let rows: Vec<Value> =
        serde_json::from_str(text).map_err(|e| format!("{} 解析失败: {e}", src.display()))?;

    let vendor = root
        .join("modules")
        .join("market")
        .join("vendor")
        .join("pypinyin");
    let dict = crate::pinyin::Dict::load(&vendor)?;

    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let code = r
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("stocks.json 第 {i} 行缺 code"))?;
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let head = code
            .chars()
            .next()
            .ok_or_else(|| format!("stocks.json 第 {i} 行 code 为空"))?;
        let market = if matches!(head, '5' | '6') {
            "sh"
        } else if matches!(head, '0' | '3') {
            "sz"
        } else {
            "bj"
        };
        out.push(json!({
            "code": code,
            "name": name,
            "market": market,
            "py": crate::pinyin::convert(&dict, name, true),
            "pyfull": crate::pinyin::convert(&dict, name, false),
        }));
    }
    out.sort_by(|a, b| {
        a.get("code")
            .and_then(|v| v.as_str())
            .cmp(&b.get("code").and_then(|v| v.as_str()))
    });

    let dst = root.join("data").join("symbols.json");
    let body = serde_json::to_string(&Value::Array(out.clone()))
        .map_err(|e| format!("symbols.json 序列化失败: {e}"))?;
    if !zg_core::jsonio::write_text_atomic(&dst, &body, false) {
        return Err(format!("写入失败: {}", dst.display()));
    }
    Ok(format!("wrote {} rows={}", dst.display(), out.len()))
}

/// `tv --fetch-stocks`：联网拉全市场股票列表 → `data/market/stocks.json`。
///
/// 复用 `zg_market::stock_list::fetch_all()` 的五源轮换（同花顺 → 东财 → Ashare
/// → 腾讯 → 新浪），只保留 `code` / `name` 两个字段——与 zg 侧 `refresh_stock_list`
/// 落盘的形状一致。新机器/新 clone 没有 `stocks.json` 时用它补洞。
pub fn fetch_stocks() -> Result<String, String> {
    let (rows, src) = zg_market::stock_list::fetch_all()?;
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for r in &rows {
        let Some(code) = r.get("f12").and_then(|v| v.as_str()) else {
            continue;
        };
        out.push(json!({
            "code": code,
            "name": r.get("f14").and_then(|v| v.as_str()).unwrap_or(""),
        }));
    }
    if out.is_empty() {
        return Err("股票列表为空".to_string());
    }
    let dst = crate::paths::tv_root()
        .join("data")
        .join("market")
        .join("stocks.json");
    if !zg_core::jsonio::write_json_atomic(&dst, &Value::Array(out.clone()), false) {
        return Err(format!("写入失败: {}", dst.display()));
    }
    Ok(format!(
        "拉取股票列表 {} 条（源: {src}）→ {}",
        out.len(),
        dst.display()
    ))
}
