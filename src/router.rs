//! tv 路由分派：从 main.rs 搬出（2026-11-07 工单 1），dispatch 签名与全部路由行为不变。

use serde_json::Value;
use zg_api::http::{Req, Resp};

use crate::{
    api, bars, board, cache, chan, concepts, drawings, heat, indices, ind, live, overview, scan,
    static_files, symbols, tags,
};

/// 防异常 lmt：缓存键含 lmt，999999999 这类值会撑出巨型 payload 并刷爆缓存。
/// 分钟档 UI 满深度 28800（.lc1 全深约 3.7 万根）给 4 万上限；日/周/月档本地数据有限，5 千足够。
fn clamp_lmt(v: usize, is_min: bool) -> usize {
    v.min(if is_min { 40_000 } else { 5_000 })
}

/// 合法周期：日/周/月/季/年 + 五档分钟（进 dispatch 前校验，输入错误归 400 而非 500）
fn valid_period(p: &str) -> bool {
    matches!(p, "day" | "week" | "month" | "quarter" | "year" | "1" | "5" | "15" | "30" | "60")
}

/// 三条缓存路由（bars / ind / chan）共用的「取数 + ETag」语义。
///
/// **错误映射不在这里做**：三条路由对同一个 Err 字符串的状态码不同
/// （bars 的「北交所」是 400，其余是 500）。把映射塞进 helper 会让状态码
/// 从调用点消失，等于把行为藏起来。这里只返回 Result，调用点原样 match。
fn serve_cached<F>(
    req: &Req,
    cache: &cache::Cache,
    key: &str,
    stamp: &str,
    build: F,
) -> Result<Resp, String>
where
    F: FnOnce() -> Result<Vec<u8>, String>,
{
    let (etag, body, _hit) = cache.get_or_build(key, stamp, build)?;
    if req.header("if-none-match") == etag {
        Ok(Resp::not_modified(&etag))
    } else {
        Ok(Resp::file_etag(body, "application/json", true, Some(&etag)))
    }
}

pub fn dispatch(req: &Req) -> Resp {
    // 只绑 127.0.0.1：Host 头是外部域名即 DNS rebinding 尝试，一律拒绝（对齐 zg routes 的用法）
    let host = req.header("host");
    if !host.is_empty() && !zg_api::http::is_local_host(host) {
        return Resp::json(&serde_json::json!({"error": "forbidden host"}), 403);
    }
    // 副作用端点加跨站防护（zg-api 现成的 Sec-Fetch-Site/Origin 判定）
    if matches!(req.path.as_str(), "/api/heat-rebuild" | "/api/scan-rebuild" | "/api/concepts-rebuild" | "/api/drawings")
        && zg_api::http::cross_site(req)
    {
        return Resp::json(&serde_json::json!({"error": "跨站请求已拒绝"}), 403);
    }
    if req.path == "/api/health" {
        return Resp::json(
            &serde_json::json!({
                "ok": true,
                "ver": env!("CARGO_PKG_VERSION"),
                "port": crate::PORT,
                "live": live::enabled(),
                "session": live::session_today(),
                "data_as_of": bars::market_last_date(),
            }),
            200,
        );
    }
    if req.path == "/api/market-status" {
        return api::ok(live::status());
    }
    if req.path == "/api/quote" {
        if !live::enabled() {
            return api::err("实时行情已关闭（TV_LIVE=0 / TV_OFFLINE=1）", 503);
        }
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        return match live::quote(code) {
            Ok(v) => api::ok(v),
            Err(e) => api::err(&e, 502),
        };
    }
    if req.path == "/api/live" {
        if !live::enabled() {
            return api::err("实时行情已关闭（TV_LIVE=0 / TV_OFFLINE=1）", 503);
        }
        let raw = req.q("codes").unwrap_or("");
        let codes: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if codes.is_empty() {
            return api::err("缺少 codes 参数（逗号分隔）", 400);
        }
        return api::ok(live::quotes(&codes));
    }
    if req.path == "/api/echo" {
        let mut m = serde_json::Map::new();
        for (k, v) in &req.query {
            m.insert(k.clone(), serde_json::json!(v));
        }
        return api::ok(serde_json::json!({"ok": true, "query": m}));
    }
    if req.path == "/api/bars" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        let period = req.q_or("period", "day");
        if !valid_period(&period) {
            return api::err(&format!("不支持的 period: {period}"), 400);
        }
        let is_min = matches!(period.as_str(), "1" | "5" | "15" | "30" | "60");
        let lmt = clamp_lmt(req.q("lmt").and_then(|v| v.parse().ok()).unwrap_or(1200), is_min);
        // 形态集合（rec27 / all）：进缓存键，切集合不会串数据
        let pset = bars::pattern_set(&req.q_or("pset", ""));

        let (key, stamp) = match bars::cache_key("bars", code, &period, lmt, pset) {
            Ok(v) => v,
            Err(e) => return api::err(&e, 500),
        };

        return match serve_cached(req, cache::bars(), &key, &stamp, || {
            let v = if is_min {
                bars::minute(code, &period, lmt, pset)?
            } else {
                bars::dwm(code, &period, lmt, pset)?
            };
            serde_json::to_vec(&v).map_err(|e| e.to_string())
        }) {
            Ok(r) => r,
            // 北交所个股 zg-core chart 链路未覆盖：客户端输入类限制归 400，不是服务端故障
            Err(e) if e.contains("北交所") => api::err(&e, 400),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/ind" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        let period = req.q_or("period", "day");
        if !valid_period(&period) {
            return api::err(&format!("不支持的 period: {period}"), 400);
        }
        let lmt = clamp_lmt(req.q("lmt").and_then(|v| v.parse().ok()).unwrap_or(1200),
                            matches!(period.as_str(), "1" | "5" | "15" | "30" | "60"));
        let board = req.q_or("board", "") == "1";
        let name = req.q_or("name", "ma").to_lowercase();
        let params = req.q_or("p", "");

        let (key, stamp) = if board {
            let Some((_market, _, resolved)) = board::resolve_code(code) else {
                return api::err("未找到该板块/指数代码", 404);
            };
            let stamp = board::cache_stamp(code, &period);
            (format!("ind:b:{resolved}:{period}:{lmt}:{name}:{params}"), stamp)
        } else {
            match bars::cache_key("ind", code, &period, lmt, "") {
                Ok((k, s)) => (format!("{k}:{name}:{params}"), s),
                Err(e) => return api::err(&e, 500),
            }
        };

        return match serve_cached(req, cache::ind(), &key, &stamp, || {
            let v = ind::payload(code, &period, lmt, board, &name, &params)?;
            serde_json::to_vec(&v).map_err(|e| e.to_string())
        }) {
            Ok(r) => r,
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/chan" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        let period = req.q_or("period", "day");
        if !valid_period(&period) {
            return api::err(&format!("不支持的 period: {period}"), 400);
        }
        let is_board = req.q_or("board", "") == "1";

        let (key, stamp) = if is_board {
            let Some((_market, _, resolved)) = board::resolve_code(code) else {
                return api::err("未找到该板块/指数代码", 404);
            };
            let stamp = board::cache_stamp(code, &period);
            // biwin2：板块缠论窗口放开到全深度（原 1200 根），旧键载荷不串用
            (format!("chan-board:{resolved}:{period}:biwin2"), stamp)
        } else {
            match bars::cache_key("chan", code, &period, 0, "biwin") {
                Ok(v) => v,
                Err(e) => return api::err(&e, 500),
            }
        };

        return match serve_cached(req, cache::chan(), &key, &stamp, || {
            let v = if is_board {
                chan::payload_board(code, &period)?
            } else {
                chan::payload(code, &period)?
            };
            serde_json::to_vec(&v).map_err(|e| e.to_string())
        }) {
            Ok(r) => r,
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/scan" {
        let code = req.q("code").unwrap_or("").trim();
        let freq = req.q_or("freq", "D").to_uppercase();
        let limit: usize = req.q("limit").and_then(|v| v.parse().ok()).unwrap_or(50);
        return match scan::payload(code, &freq, limit) {
            Ok(v) => api::ok(v),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/drawings" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        let period = req.q_or("period", "day");

        if req.method == "POST" {
            let body: Value = match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(e) => return api::err(&format!("请求体不是 JSON: {e}"), 400),
            };
            let rows = body.get("rows").cloned().unwrap_or_else(|| serde_json::json!([]));
            return match drawings::put(code, &period, rows) {
                Ok(v) => api::ok(v),
                Err(e) => api::err(&e, 500),
            };
        }

        return match drawings::get(code, &period) {
            Ok(v) => api::ok(v),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/board-kline" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        let period = req.q_or("period", "day");
        if !matches!(period.as_str(), "day" | "week" | "month" | "quarter" | "year") {
            return api::err(&format!("不支持的 period: {period}"), 400);
        }
        let lmt = clamp_lmt(req.q("lmt").and_then(|v| v.parse().ok()).unwrap_or(1200), false);
        let pset = bars::pattern_set(&req.q_or("pset", ""));
        // 输入类错误（未知代码/离线无日线）归 400；读文件等内部错误归 500
        return match board::payload(code, &period, lmt, pset) {
            Ok(v) => api::ok(v),
            Err(e) if e.starts_with("未找到") || e.contains("离线模式") => api::err(&e, 400),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/market-indices" {
        let force = req.q_or("force", "") == "1";
        return api::ok(indices::market_indices(force));
    }
    if req.path == "/api/index-constituents" {
        let code = req.q("code").unwrap_or("").trim();
        return match indices::constituents(code) {
            Ok(v) => api::ok(v),
            Err(e) => api::err(&e, 404),
        };
    }
    if req.path == "/api/heat" {
        let order = req.q_or("order", "desc");
        return api::ok(heat::payload(&order));
    }
    if req.path == "/api/heat-rebuild" {
        return match heat::start_rebuild() {
            Ok(()) => api::ok(serde_json::json!({"ok": true, "started": true,
                "note": "本地日线重算约 1 分钟，稍后 GET /api/heat 看 updated/source"})),
            Err(e) => api::err(&e, 409),
        };
    }
    if req.path == "/api/scan-rebuild" {
        return match scan::start_rebuild() {
            Ok(()) => api::ok(serde_json::json!({"ok": true, "started": true,
                "note": "全市场重扫约 1-6 分钟，稍后 GET /api/scan 看 data_last"})),
            Err(e) => api::err(&e, 409),
        };
    }
    if req.path == "/api/concepts-rebuild" {
        return match concepts::start_rebuild() {
            Ok(()) => api::ok(serde_json::json!({"ok": true, "started": true,
                "note": "全市场概念题材刷新约 40-60 秒，稍后 GET /api/concepts-stats 看 fetched_at/stocks"})),
            Err(e) => api::err(&e, 409),
        };
    }
    if req.path == "/api/concepts-stats" {
        return api::ok(concepts::stats());
    }
    if req.path == "/api/sector-overview" {
        let force = req.q_or("force", "") == "1";
        return match overview::payload(force) {
            Ok(v) => api::ok(v),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/symbols" {
        return match symbols::all() {
            Ok(rows) => Resp::json(&serde_json::json!({"ok": true, "rows": rows}), 200),
            Err(e) => api::err(&e, 500),
        };
    }
    if req.path == "/api/stock-tags" {
        let code = req.q("code").unwrap_or("").trim();
        if code.is_empty() {
            return api::err("缺少 code 参数", 400);
        }
        return api::ok(tags::payload(code));
    }
    if req.path.starts_with("/api/") {
        return api::err("未知接口", 404);
    }
    static_files::serve(&req.path, req.header("if-none-match"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn req() -> Req {
        Req {
            method: "GET".into(),
            path: "/api/bars".into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    fn with_inm(etag: &str) -> Req {
        let mut r = req();
        r.headers.insert("if-none-match".into(), etag.to_string());
        r
    }

    fn builder(calls: Arc<AtomicUsize>) -> impl FnOnce() -> Result<Vec<u8>, String> {
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(b"{\"ok\":true}".to_vec())
        }
    }

    #[test]
    fn cache_hit_returns_304_and_builds_once() {
        let c = cache::Cache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let first = serve_cached(&req(), &c, "k", "s1", builder(calls.clone())).unwrap();
        assert_eq!(first.status, 200);
        let etag = first
            .headers
            .iter()
            .find(|(k, _)| k == "ETag")
            .map(|(_, v)| v.clone())
            .expect("200 必须带 ETag");

        let second = serve_cached(&with_inm(&etag), &c, "k", "s1", builder(calls.clone())).unwrap();
        assert_eq!(second.status, 304);
        assert!(second.body.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "命中缓存不得重建");

        // stamp 变了必须重建（日线同步后缓存失效的唯一判据）
        let third = serve_cached(&req(), &c, "k", "s2", builder(calls.clone())).unwrap();
        assert_eq!(third.status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn build_error_is_propagated_verbatim() {
        let c = cache::Cache::new();
        let e = serve_cached(&req(), &c, "k", "s", || Err("北交所个股不支持".to_string()))
            .unwrap_err();
        assert_eq!(e, "北交所个股不支持", "helper 不得改写错误文本（调用点靠它判 400）");
    }
}
