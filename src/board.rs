use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::daymerge::{merge_days, MergePolicy};
use crate::paths;

type SectorMap = HashMap<String, (String, String)>;

static SECTORS: OnceLock<Mutex<Option<(String, SectorMap)>>> = OnceLock::new();

fn catalog_path() -> std::path::PathBuf {
    paths::tv_root().join("modules").join("market").join("industry_catalog.json")
}

fn load_sectors() -> Option<SectorMap> {
    let text = std::fs::read_to_string(catalog_path()).ok()?;
    let j: Value = serde_json::from_str(&text).ok()?;
    let nodes = j.get("nodes")?.as_array()?;
    let mut m = HashMap::new();
    for n in nodes {
        let Some(code) = n.get("code").and_then(|x| x.as_str())
            .filter(|c| c.starts_with("88")) else { continue };
        let name = n.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        m.insert(code.to_string(), (code.to_string(), name.clone()));
        if let Some(r) = n.get("ref").and_then(|x| x.as_str()).filter(|r| !r.is_empty()) {
            m.insert(r.to_string(), (code.to_string(), name));
        }
    }
    Some(m)
}

/// 板块码/行业 ref → (实际日线码, 名称)。按 catalog 文件指纹缓存：
/// 同步重生成 catalog 后无需重启即可生效（原来 OnceLock 进程内永不刷新）。
pub(crate) fn sector(code: &str) -> Option<(String, String)> {
    let stamp = crate::cache::file_stamp(&catalog_path());
    let mut g = SECTORS.get_or_init(|| Mutex::new(None)).lock().ok()?;
    let fresh = matches!(g.as_ref(), Some((s, _)) if *s == stamp);
    if !fresh {
        *g = Some((stamp, load_sectors()?));
    }
    g.as_ref().and_then(|(_, m)| m.get(code)).cloned()
}

fn index_info(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "000001" => ("sh", "上证指数"),
        "399001" => ("sz", "深成指"),
        "399006" => ("sz", "创业板指数"),
        "000688" => ("sh", "科创板指数"),
        "000300" => ("sh", "沪深300"),
        "000905" => ("sh", "中证500"),
        "000852" => ("sh", "中证1000"),
        "932000" => ("", "中证2000"),
        "000016" => ("sh", "上证50"),
        "930050" => ("", "中证A50"),
        "000510" => ("sh", "中证A500"),
        _ => return None,
    })
}

/// 解析板块/指数代码 → (market, 名称, 实际日线代码)。market 为空串表示本地无该股线。
pub fn resolve_code(code: &str) -> Option<(&'static str, String, String)> {
    if let Some((market, name)) = index_info(code) {
        return Some((market, name.to_string(), code.to_string()));
    }
    let (sector_code, name) = sector(code)?;
    Some(("sh", name, sector_code))
}

/// 缓存指纹：本地有日线用文件指纹，否则用 live 标记；盘中再叠 TTL 时间桶。
pub fn cache_stamp(code: &str, period: &str) -> String {
    let base = match resolve_code(code) {
        Some((market, _, resolved)) if !market.is_empty() => {
            crate::cache::file_stamp(&zg_core::reader::day_path(market, &resolved))
        }
        _ => "live".to_string(),
    };
    match crate::live::ttl_bucket(period) {
        Some(b) => format!("{base}|b{b}"),
        None => base,
    }
}

fn local_rows(market: &str, resolved: &str) -> Result<Vec<crate::bars::Agg>, String> {
    let daily = zg_core::api::read_day_bars(market, resolved)
        .map_err(|e| format!("读本地板块/指数日线失败: {e}"))?;
    if daily.is_empty() {
        return Err("本地板块日线为空".to_string());
    }
    Ok(daily
        .iter()
        .map(|b| crate::bars::Agg {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.volume,
            amount: b.amount,
        })
        .collect())
}

fn resample_agg(rows: &[crate::bars::Agg], period: &str) -> Vec<crate::bars::Agg> {
    if period == "quarter" || period == "year" {
        let t: Vec<crate::bars::Row7> = rows
            .iter()
            .map(|b| (b.date.clone(), b.open, b.high, b.low, b.close, b.vol, b.amount))
            .collect();
        return crate::bars::resample_qy(&t, period);
    }
    let kbars: Vec<zg_market::kbar::KBar> = rows
        .iter()
        .map(|b| zg_market::kbar::KBar {
            date: b.date.clone(),
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol,
            amount: b.amount,
        })
        .collect();
    zg_market::market_data::synth_wm_from_daily(&kbars, period)
        .into_iter()
        .map(|b| crate::bars::Agg {
            date: b.date,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            vol: b.vol,
            amount: b.amount,
        })
        .collect()
}

fn parse_board_value(v: &Value) -> Result<(Vec<crate::bars::Agg>, String, String, bool), String> {
    if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
        return Err(e.to_string());
    }
    let rows: Vec<crate::bars::Agg> = v
        .get("bars")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    Some(crate::bars::Agg {
                        date: b.get("date")?.as_str()?.to_string(),
                        open: b.get("open")?.as_f64().unwrap_or(0.0),
                        high: b.get("high")?.as_f64().unwrap_or(0.0),
                        low: b.get("low")?.as_f64().unwrap_or(0.0),
                        close: b.get("close")?.as_f64().unwrap_or(0.0),
                        vol: b.get("vol").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        amount: b.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return Err("远端板块 K 线为空".to_string());
    }
    let source = v.get("source").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let as_of = v
        .get("cache_date")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| rows.last().map(|b| b.date.clone()))
        .unwrap_or_default();
    let stale = v.get("cache_stale").and_then(|x| x.as_bool()).unwrap_or(false);
    Ok((rows, source, as_of, stale))
}

fn remote_board(code: &str, period: &str, lmt: usize)
    -> Result<(Vec<crate::bars::Agg>, String, String, bool), String>
{
    let v = zg_market::indices::board_kline(code, period, lmt, false);
    parse_board_value(&v)
}

/// 板块/指数日 / 周 / 月 / 季 / 年：
/// 本地新鲜 → 本地；否则 TQ（>15 天陈旧才 refresh_kline）→ 腾讯 → 旧本地标 stale。
/// 季 / 年用本地全历史 + 远端最近几根合并，避免远端只给 250 根导致历史被截断。
pub fn daily_rows(code: &str, period: &str, lmt: usize) -> Result<crate::live::Series, String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("缺少 code 参数".to_string());
    }
    let period = period.trim().to_lowercase();
    if !matches!(period.as_str(), "day" | "week" | "month" | "quarter" | "year") {
        return Err("板块/指数仅支持日 / 周 / 月 / 季 / 年 K 线".to_string());
    }
    let (market, _name, resolved) =
        resolve_code(code).ok_or_else(|| "未找到该板块/指数代码".to_string())?;
    let reference = crate::live::reference_day();
    // 本地全历史（行业 881xxx / 指数）与远端最近日线合并后再重采样：
    // 远端周/月只有 1500 根日线的深度，直接当全量会把 10 年+ 历史截掉。
    let local_daily = if market.is_empty() {
        Vec::new()
    } else {
        local_rows(market, &resolved).unwrap_or_default()
    };
    let mut remote_daily = Vec::new();
    let mut remote_src = String::new();
    let mut remote_stale = false;
    if crate::live::enabled() {
        if let Ok((rb, src, _, st)) = remote_board(code, "day", 2430) {
            remote_daily = rb;
            remote_src = src;
            remote_stale = st;
            if !remote_daily.is_empty()
                && !crate::live::remote_mergeable(&local_daily, &remote_daily, &remote_src)
            {
                remote_daily.clear();
                remote_stale = true;
            }
        }
    }
    let remote_last = remote_daily
        .last()
        .map(|b| b.date.clone())
        .unwrap_or_default();
    let daily = merge_days(local_daily, remote_daily, MergePolicy::RemoteWins);
    if daily.is_empty() {
        return Err("板块/指数日线全部失败（本地与远端均无数据）".to_string());
    }
    let as_of = daily.last().map(|b| b.date.clone()).unwrap_or_default();
    let mut source = if !remote_src.is_empty() && as_of == remote_last {
        remote_src
    } else {
        "local".to_string()
    };
    if source.is_empty() {
        source = "local".to_string();
    }
    let stale = remote_stale || as_of.as_str() < reference.as_str();
    let mut rows = if period == "day" {
        daily
    } else {
        resample_agg(&daily, &period)
    };
    rows = if lmt > 0 && rows.len() > lmt {
        rows[rows.len() - lmt..].to_vec()
    } else {
        rows
    };
    Ok(crate::live::Series { rows, source, detail: String::new(), as_of, stale })
}

/// 板块/指数 K 线响应（TV 格式）。
pub fn payload(code: &str, period: &str, lmt: usize, want: &str) -> Result<Value, String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("缺少 code 参数".to_string());
    }
    let period = period.trim().to_lowercase();
    if !matches!(period.as_str(), "day" | "week" | "month" | "quarter" | "year") {
        return Err("板块/指数仅支持日 / 周 / 月 / 季 / 年 K 线".to_string());
    }
    let (_market, name, resolved_code) =
        resolve_code(code).ok_or_else(|| "未找到该板块/指数代码".to_string())?;
    let s = daily_rows(code, &period, lmt)?;
    let rows = s.rows;
    let bars: Vec<Value> = rows
        .iter()
        .map(|b| crate::bars::bar_row(Value::String(b.date.clone()), b))
        .collect();
    let (eng, pset, sigs) = crate::bars::candles(
        &rows.iter().map(|b| (b.date.clone(), b.open, b.high, b.low, b.close)).collect::<Vec<_>>(),
        &crate::bars::candle_label(&period),
        want,
    );
    Ok(json!({
        "ok": true,
        "code": resolved_code,
        "name": name,
        "period": period,
        "n": bars.len(),
        "bars": bars,
        "board": true,
        "candle_engine": eng,
        "candle_pattern_set": pset,
        "candle_signals": sigs,
        "meta": {
            "source": s.source,
            "detail": s.detail,
            "as_of": s.as_of,
            "stale": s.stale,
            "session": crate::live::session_today(),
            "cache_date": s.as_of,
            "min_period": false
        }
    }))
}
