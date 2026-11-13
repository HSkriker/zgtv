//! 8786 路由 —— 逐条对齐 `modules/zgapp/handler.py::do_GET/do_POST`。

use zg_core::sync::MutexExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::aux_svc;
use crate::http::{Req, Resp};
use crate::state::{tdx_flag, watch_add, WebState};

pub struct App {
    pub state: Arc<WebState>,
    /// POST /api/shared/shutdown 触发（server loop 轮询）
    pub shutdown: Arc<AtomicBool>,
}

pub fn dispatch(app: &App, req: &Req) -> Resp {
    // 只绑 127.0.0.1：Host 头是外部域名时一律拒绝，防 DNS rebinding 把本服务当同源
    let host = req.header("host");
    if !host.is_empty() && !crate::http::is_local_host(host) {
        return Resp::json(&json!({"error": "forbidden host"}), 403);
    }
    let p = req.path.as_str();
    match req.method.as_str() {
        "GET" => do_get(app, req, p),
        "POST" => do_post(app, req),
        // 与 8787 一致用 405（路径存在但方法不允许），此前 8786 返 404
        _ => Resp::json(&json!({"detail": "Method Not Allowed"}), 405),
    }
}

fn do_get(app: &App, req: &Req, p: &str) -> Resp {
    if is_side_effect_get(p, req) && crate::http::cross_site(req) {
        return Resp::json(&json!({"error": "跨站请求已拒绝"}), 403);
    }
    let state = &app.state;
    let result: Result<Resp, String> = (|| {
        if p == "/api/shared/status" {
            return Ok(Resp::json(&json!({
                "name": "整合工作台",
                "port": state.port,
                "list_time": state.list_time.lock_ok().clone(),
                "data_source": state.data_source.lock_ok().clone(),
            }), 200));
        }
        if p == "/api/shared/stocks" {
            return Ok(Resp::json(&json!({
                "items": state.stock_list.lock_ok().clone(),
                "updated": state.list_time.lock_ok().clone(),
            }), 200));
        }
        if p.starts_with("/shared/") {
            return Ok(crate::staticfiles::shared_path(p, Some(req.header("if-none-match")))
                .unwrap_or_else(Resp::not_found));
        }
        if p == "/api/meta" {
            return Ok(Resp::json(&json!({
                "port": state.port,
                "tdx": tdx_flag(),
                "live_cycle": zg_market::consts::LIVE_CYCLE,
                "rule": "各周期独立取数；盘中巡检仅做报价与预警判定",
            }), 200));
        }
        if p == "/api/tdx-backup" {
            return Ok(Resp::json(&aux_svc::tdx_backup_status(state), 200));
        }
        if p == "/api/status" {
            return Ok(Resp::json(&json!({
                "list_time": state.list_time.lock_ok().clone(),
                "data_source": state.data_source.lock_ok().clone(),
                "market_open": zg_core::calendar::market_open_now(None),
                "server_time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                "tq": zg_market::tq::status(),
            }), 200));
        }
        if p == "/api/list" {
            if state.stock_list.lock_ok().is_empty() {
                // 先从 stocks.json 兜底（离线行为），仍为空且联网 → 后台刷新（Python 同语义）
                let path = zg_core::paths::get().market_data.join("stocks.json");
                if let Ok(t) = std::fs::read_to_string(path) {
                    if let Ok(list) = serde_json::from_str::<Vec<Value>>(
                        t.trim_start_matches('\u{feff}'),
                    ) {
                        *state.stock_list.lock_ok() = list;
                    }
                }
                if !state.offline && state.stock_list.lock_ok().is_empty() {
                    let st = Arc::clone(state);
                    std::thread::spawn(move || {
                        let _ = aux_svc::refresh_stock_list(&st, false);
                    });
                }
            }
            return Ok(Resp::json(&json!({
                "list": state.stock_list.lock_ok().clone(),
                "time": state.list_time.lock_ok().clone(),
            }), 200));
        }
        if p == "/api/kline" {
            let code = req.q("code").unwrap_or("").trim().to_string();
            let period = req.q_or("period", "day");
            let lmt: usize = req.q("lmt").and_then(|v| v.parse().ok()).unwrap_or(250);
            let end = req.q("end").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            return match zg_market::market_data::api_kline(
                &state.market, &code, &period, lmt, end.as_deref(),
            ) {
                Ok(v) => Ok(Resp::json(&v, 200)),
                Err(e) if e.contains("北交所暂不支持分钟级数据") => {
                    Ok(Resp::json(&json!({"error": e}), 400))
                }
                Err(e) => Err(e),
            };
        }
        if p == "/api/quote" {
            let code = req.q("code").unwrap_or("").trim().to_string();
            return match zg_market::quotes::get_quote(&code) {
                Ok(v) => Ok(Resp::json(&v, 200)),
                Err(e) => Err(e),
            };
        }
        if p == "/api/live" {
            return Ok(Resp::json(&state.live.lock_ok().clone(), 200));
        }
        if p == "/api/theme-refresh" {
            return Ok(Resp::json(&state.theme_prog.lock_ok().clone(), 200));
        }
        if p == "/api/theme" {
            let c = req.q("code").unwrap_or("").trim().to_string();
            if c.is_empty() {
                return Ok(Resp::json(&json!({"error": "缺少 code 参数"}), 200));
            }
            return Ok(Resp::json(&aux_svc::get_stock_theme(state, &c), 200));
        }
        if p == "/api/industry-heat/all" {
            let order = req.q_or("order", "desc");
            return Ok(Resp::json(
                &aux_svc::industry_heat_all_request(state, &order, state.offline), 200));
        }
        if p == "/api/industry-heat" {
            let parent = req.q_or("parent", "");
            let order = req.q_or("order", "desc");
            return Ok(Resp::json(
                &aux_svc::industry_heat_request(state, &parent, &order, state.offline), 200));
        }
        if p == "/api/market-indices" {
            let force = req.q_or("force", "") == "1";
            return Ok(Resp::json(
                &zg_market::indices::snapshot(state.offline, force), 200));
        }
        if p == "/api/index-constituents" {
            let code = req.q_or("code", "");
            let force = req.q_or("force", "") == "1";
            return Ok(Resp::json(
                &zg_market::indices::constituents(&code, state.offline, force), 200));
        }
        if p == "/api/board-kline" {
            let code = req.q_or("code", "");
            let period = req.q_or("period", "day");
            let lmt: usize = req.q("lmt").and_then(|v| v.parse().ok()).unwrap_or(0);
            return Ok(Resp::json(
                &zg_market::indices::board_kline(&code, &period, lmt, state.offline), 200));
        }
        if p == "/api/watch" {
            let code = req.q("code").unwrap_or("").trim().to_string();
            let n = if code.is_empty() { state.watch_extra.lock_ok().len() } else { watch_add(&state.watch_extra, &code) };
            return Ok(Resp::json(&json!({"ok": true, "watching": n}), 200));
        }
        if p == "/api/czsc" {
            let code = req.q("code").unwrap_or("").trim().to_string();
            let period = req.q_or("period", "day");
            let freq_ok = crate::consts::czsc_freq_of_period(&period).is_some();
            if code.is_empty() || !freq_ok {
                return Ok(Resp::json(&json!({"ok": false, "error": format!(
                    "不支持的周期: {period}（支持 day/week/month/min1/min5/min15/min30/min60）")}), 200));
            }
            if req.q_or("board", "") == "1" {
                // 指数/板块缠论：bars 与 /api/board-kline 同一份（000001 在此语境=上证指数）
                let lmt = if period.starts_with("min") {
                    zg_market::consts::MINUTE_LMT.iter()
                        .find(|(pp, _)| *pp == period).map(|(_, v)| *v).unwrap_or(480)
                } else {
                    match period.as_str() { "day" => 600, "week" => 300, _ => 240 }
                };
                let bk = zg_market::indices::board_kline(&code, &period, lmt, state.offline);
                let bars = bk.get("bars").and_then(|b| b.as_array()).cloned().unwrap_or_default();
                if bars.is_empty() {
                    let err = bk.get("error").and_then(|e| e.as_str())
                        .unwrap_or("暂无可用K线数据").to_string();
                    return Ok(Resp::json(&json!({
                        "ok": false,
                        "code": bk.get("code").cloned().unwrap_or(json!(code)),
                        "error": err,
                    }), 200));
                }
                let cd = bk.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let mkt = bk.get("market").and_then(|c| c.as_str()).unwrap_or("sh").to_string();
                return Ok(Resp::json(
                    &aux_svc::czsc_analyze_ex(&cd, &period, Some(&bars), Some(&mkt)), 200));
            }
            if period.starts_with("min") {
                let lmt = zg_market::consts::MINUTE_LMT.iter()
                    .find(|(p, _)| *p == period).map(|(_, v)| *v).unwrap_or(480);
                match zg_market::market_data::api_kline(&state.market, &code, &period, lmt, None) {
                    Ok(data) => {
                        let bars = data.get("bars").and_then(|b| b.as_array()).cloned();
                        return Ok(Resp::json(
                            &aux_svc::czsc_analyze(&code, &period, bars.as_ref()), 200));
                    }
                    Err(e) => {
                        return Ok(Resp::json(&json!({"ok": false, "error": e}), 200));
                    }
                }
            }
            return Ok(Resp::json(&aux_svc::czsc_analyze(&code, &period, None), 200));
        }
        if p == "/api/czsc-scan" {
            return Ok(Resp::json(&aux_svc::czsc_scan_payload(state), 200));
        }
        if p == "/api/czsc-quotes" {
            let mut codes: Vec<String> = Vec::new();
            for x in req.q("codes").unwrap_or("").split(',') {
                let x = x.trim();
                if x.len() == 6 && x.chars().all(|c| c.is_ascii_digit()) {
                    codes.push(x.to_string());
                }
            }
            codes.truncate(2000);
            return Ok(Resp::json(&json!({
                "ok": true,
                "ts": chrono::Local::now().format("%H:%M:%S").to_string(),
                "quotes": aux_svc::czsc_quotes(state, &codes),
            }), 200));
        }
        if p == "/api/czsc-status" {
            return Ok(Resp::json(&aux_svc::czsc_status(), 200));
        }
        if p == "/api/daily-k" {
            let act = req.q_or("action", "status");
            if act == "build" {
                let force = req.q("force").map(|v| v == "1" || v == "true").unwrap_or(false);
                return Ok(Resp::json(&aux_svc::build_daily_cache(state, force), 200));
            }
            // 无缓存是正常状态（未构建），返回 200 + codes=0，不照抄旧实现的 500
            let store_guard = state.market.daily_cache.lock_ok();
            let status = match store_guard.as_ref() {
                Some(store) => daily_status(state, store),
                None => daily_status_empty(state),
            };
            return Ok(Resp::json(&status, 200));
        }
        if p == "/api/weekly-k" {
            let act = req.q_or("action", "status");
            if act == "build" {
                let force = req.q_or("force", "0") == "1";
                return Ok(Resp::json(&aux_svc::build_weekly_cache(state, force), 200));
            }
            return Ok(Resp::json(&weekly_status(state), 200));
        }
        if p == "/api/min-k" {
            let act = req.q_or("action", "status");
            if act == "build" {
                let force = req.q_or("force", "0") == "1";
                let raw = req.q_or("period", "");
                let periods = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw.split(',').map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()).collect::<Vec<_>>())
                };
                return Ok(Resp::json(&aux_svc::build_min_cache(state, force, periods), 200));
            }
            return Ok(Resp::json(&min_status(state), 200));
        }
        if p == "/api/alerts" {
            return Ok(Resp::json(&aux_svc::alerts_payload(state, &req.query), 200));
        }
        Ok(crate::staticfiles::static_path(p, Some(req.header("if-none-match")))
            .unwrap_or_else(Resp::not_found))
    })();
    match result {
        Ok(r) => r,
        // 错误原文可能带上游 URL/响应片段，回显前截断
        Err(e) => {
            let msg: String = e.chars().take(300).collect();
            Resp::json(&json!({"error": msg}), 500)
        }
    }
}

/// 有副作用的 GET：会写 watch 列表、改预警、启动扫描/构建缓存。
fn is_side_effect_get(p: &str, req: &Req) -> bool {
    match p {
        "/api/watch" | "/api/alerts" | "/api/czsc-scan" => true,
        // 列表为空时每请求新建线程刷新股票列表（H8）
        "/api/list" => true,
        // 缓存过期即起线程写盘/打上游（H8）
        "/api/industry-heat" | "/api/industry-heat/all" => true,
        // force=1 绕过缓存直打上游（H8）
        "/api/market-indices" | "/api/index-constituents" => req.q_or("force", "") == "1",
        "/api/daily-k" | "/api/weekly-k" | "/api/min-k" => req.q_or("action", "status") == "build",
        _ => false,
    }
}

fn do_post(app: &App, req: &Req) -> Resp {
    let origin = req.header("origin");
    let host = req.header("host");
    if !origin.is_empty() && origin != format!("http://{host}") {
        return Resp::json(&json!({"error": "请求来源不匹配"}), 403);
    }
    let ctype = req.header("content-type").split(';').next().unwrap_or("").trim().to_string();
    if ctype != "application/json" {
        return Resp::json(&json!({"error": "需要 JSON 请求"}), 415);
    }
    if req.path == "/api/shared/shutdown" {
        app.shutdown.store(true, Ordering::SeqCst);
        return Resp::json(&json!({"ok": true}), 200);
    }
    if req.path == "/api/theme-refresh" {
        let force = req.q_or("force", "") == "1";
        let days: i64 = req.q_or("days", "3").parse().unwrap_or(3);
        let limit = req.q("limit").and_then(|v| v.parse::<usize>().ok());
        return Resp::json(&aux_svc::theme_refresh_start(&app.state, force, days, limit), 200);
    }
    Resp::not_found()
}

// ---------------------------------------------------------------- 状态端点（cache_store 等价）

fn weekly_status(state: &WebState) -> Value {
    let lt = zg_core::calendar::last_trading_date(None);
    let ready = *state.market.weekly_ready.lock_ok();
    let date = state.market.weekly_date.lock_ok().clone();
    let codes = state.market.weekly_cache.lock_ok().as_ref().map(|s| s.len()).unwrap_or(0);
    let build = state.weekly_build.lock_ok().clone();
    json!({
        "date": date,
        "codes": codes,
        "ready": ready,
        "running": build.get("running").cloned().unwrap_or(json!(false)),
        "total": build.get("total").cloned().unwrap_or(json!(0)),
        "done": build.get("done").cloned().unwrap_or(json!(0)),
        "secs": build.get("secs").cloned().unwrap_or(json!(0.0)),
        "src": build.get("src").cloned().unwrap_or(json!("")),
        "lmt": zg_market::consts::WEEKLY_K_LMT,
        "last_trading_date": lt,
        "stale": codes > 0 && date != lt,
    })
}

fn min_status(state: &WebState) -> Value {
    let lt = zg_core::calendar::last_trading_date(None);
    let ready = *state.market.min_ready.lock_ok();
    let date = state.market.min_date.lock_ok().clone();
    let mut codes = serde_json::Map::new();
    let mut stale_codes = serde_json::Map::new();
    let lt_int = zg_core::model::day_int(&lt);
    {
        let mut cache = state.market.min_cache.lock_ok();
        for p in zg_market::consts::MIN_K_PERIODS {
            let mut st = cache.get_mut(*p);
            codes.insert(p.to_string(), Value::from(st.as_ref().map(|s| s.len()).unwrap_or(0)));
            stale_codes.insert(p.to_string(), Value::from(st.as_mut().map(|s| s.stale_count(lt_int)).unwrap_or(0)));
        }
    }
    let build = state.min_build.lock_ok().clone();
    let marks: serde_json::Map<String, Value> = state.market.min_marks.lock_ok().iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    let mut lmt = serde_json::Map::new();
    for (k, v) in zg_market::consts::MIN_K_LMT {
        lmt.insert((*k).to_string(), Value::from(*v));
    }
    json!({
        "date": date,
        "codes": Value::Object(codes),
        "ready": ready,
        "running": build.get("running").cloned().unwrap_or(json!(false)),
        "period": build.get("period").cloned().unwrap_or(json!("")),
        "total": build.get("total").cloned().unwrap_or(json!(0)),
        "done": build.get("done").cloned().unwrap_or(json!(0)),
        "secs": build.get("secs").cloned().unwrap_or(json!(0.0)),
        "src": build.get("src").cloned().unwrap_or(json!("")),
        "stale_codes": Value::Object(stale_codes),
        "lmt": Value::Object(lmt),
        "marks": Value::Object(marks),
        "now": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        "due": min_due_periods(&state.market.min_marks.lock_ok().clone()),
        "market_open": zg_core::calendar::market_open_now(None),
        "last_trading_date": lt,
        "stale": !state.market.min_cache.lock_ok().is_empty() && date != lt,
    })
}

fn daily_status(state: &WebState, store: &zg_core::kcache::KCacheStore) -> Value {
    let lt = zg_core::calendar::last_trading_date(None);
    let build = state.daily_build.lock_ok().clone();
    let date = state.daily_date.lock_ok().clone();
    json!({
        "date": date,
        "codes": store.len(),
        "running": build.get("running").cloned().unwrap_or(json!(false)),
        "total": build.get("total").cloned().unwrap_or(json!(0)),
        "done": build.get("done").cloned().unwrap_or(json!(0)),
        "lmt": zg_market::consts::DAILY_K_LMT,
        "last_trading_date": lt,
        "stale": !store.is_empty() && date != lt,
        "error": build.get("error").cloned().unwrap_or(Value::Null),
    })
}

/// 未构建缓存时的状态：字段与构建态同构，codes=0、stale=false（无数据可陈旧）。
fn daily_status_empty(state: &WebState) -> Value {
    let lt = zg_core::calendar::last_trading_date(None);
    let build = state.daily_build.lock_ok().clone();
    json!({
        "date": state.daily_date.lock_ok().clone(),
        "codes": 0,
        "running": build.get("running").cloned().unwrap_or(json!(false)),
        "total": build.get("total").cloned().unwrap_or(json!(0)),
        "done": build.get("done").cloned().unwrap_or(json!(0)),
        "lmt": zg_market::consts::DAILY_K_LMT,
        "last_trading_date": lt,
        "stale": false,
        "error": build.get("error").cloned().unwrap_or(Value::Null),
    })
}

/// `cache_store.min_k_due_periods`
fn min_due_periods(marks: &std::collections::HashMap<String, String>) -> Vec<String> {
    let now = chrono::Local::now();
    if now.weekday().num_days_from_monday() >= 5 {
        return Vec::new();
    }
    use chrono::Datelike;
    let today = now.format("%Y-%m-%d").to_string();
    let hm = now.format("%H:%M").to_string();
    let mut due = Vec::new();
    for (period, bounds) in zg_market::consts::MIN_K_BOUNDARIES {
        let last_mark = marks.get(*period).cloned().unwrap_or_default();
        let passed: Vec<&str> = bounds.iter().copied().filter(|b| *b <= hm.as_str()).collect();
        if passed.is_empty() {
            continue;
        }
        let stamp = format!("{today} {}", passed[passed.len() - 1]);
        if last_mark != stamp {
            due.push((*period).to_string());
        }
    }
    due
}

