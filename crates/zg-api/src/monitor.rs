//! 8787 行情监控 —— 对齐 `modules/monitor_app`（routers/api.py + routers/pages.py + auth）。
//!
//! 本轮（P4-3a）覆盖：页面 19 条 / 静态 4 条 / 本地数据包 API（sector/flow/sw3/pick/
//! trace/home/reports/codemap/state/rankings）/ 404 与 403 契约 / 旧站 redirect /
//! 动作队列（action）。实时采集类（flowlab×7、commodity、limitup、etf、ths_debug、
//! open_stock）依赖 flow/live/fund/ths 采集循环 —— P4-3b 移植。

use zg_core::sync::MutexExt;
use std::path::PathBuf;
use std::sync::Mutex;

use bytes::Bytes;
use serde_json::{json, Value};

use crate::http::{percent_decode, Req, Resp};

// ---------------------------------------------------------------- 配置

fn base_dir() -> PathBuf {
    zg_core::paths::get()
        .root
        .join("modules")
        .join("market")
        .join("行情监控")
}

fn web_dir() -> PathBuf {
    zg_core::paths::get().root.join("web")
}

pub(crate) fn data_dir() -> PathBuf {
    match std::env::var("DATA_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => base_dir().join("data"),
    }
}

fn auth_password() -> String {
    std::env::var("AUTH_PASSWORD").unwrap_or_default()
}

fn web_mode_next() -> (bool, Vec<String>) {
    let mode = std::env::var("ZG_WEB").unwrap_or_else(|_| "next".into()).trim().to_lowercase();
    let pages: Vec<String> = std::env::var("ZG_WEB_PAGES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (mode == "next", pages)
}

const OPEN_PATHS: &[&str] = &[
    "/api/login", "/api/actions/pending", "/api/actions/complete", "/login",
    "/base.css", "/base.js", "/manifest.json", "/favicon.ico",
];

const RESEARCH_KEYS: &[&str] = &["pool", "factors", "combo", "dip", "sw3", "etf", "etfgo"];

const ALLOWED_CMDS: &[&str] = &[
    "buy", "check", "monitor_add", "monitor_del", "hold_set", "sector_add",
    "sector_del", "sector_follow_add", "sector_follow_del",
    "narrative_deep", "attr_report", "trade_add", "trade_del", "report_del",
    "combo_bt", "sector_sweep", "etf_rotate",
];

const LEGACY_REDIRECTS: &[(&str, &str)] = &[
    ("/decide", "/hold"),
    ("/picklab", "/pick"),
    ("/sw3lab", "/research/sw3"),
    ("/opps", "/sectors"),
    ("/picks", "/reports"),
    ("/stats", "/review"),
    ("/narratives", "/reports?type=narrative"),
    ("/tools", "/research"),
    ("/scan", "/sectors"),
    ("/votes", "/review"),
    ("/replay", "/review"),
    ("/train", "/research"),
    ("/fund", "/research"),
    ("/etf", "/limitup"),
];

// ---------------------------------------------------------------- 缓存读写（多编码回退）

/// `cache.read_json`：utf-8 → gbk → locale 回退；空文件/不存在 → None。
pub fn read_json(p: &std::path::Path) -> Option<Value> {
    let raw = std::fs::read(p).ok()?;
    if raw.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
        return Some(v);
    }
    let (cow, _, had_errors) = encoding_rs::GBK.decode(&raw);
    if had_errors {
        return None;
    }
    serde_json::from_str::<Value>(&cow).ok()
}

fn read_two(p1: PathBuf, p2: PathBuf, fallback: Value) -> Value {
    read_json(&p1)
        .or_else(|| read_json(&p2))
        .unwrap_or(fallback)
}

/// FastAPI `JSONResponse` 头：`application/json`（无 charset）、无 Cache-Control。
fn j(v: &Value, status: u16) -> Resp {
    Resp {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()).into(),
    }
}

// ---------------------------------------------------------------- 鉴权

/// 8787 THS 凭据（与 `zgcore.ths.api_key()` 同源：环境变量 / credentials.env / ths_key.txt）。
pub(crate) fn ths_api_key() -> Option<String> {
    zg_market::ths::api_key()
}

fn auth_token() -> String {
    // Python: hashlib.sha256(("v2:" + AUTH_PASSWORD).encode()).hexdigest()[:32]
    sha256_impl::sha256_hex(format!("v2:{}", auth_password()).as_bytes())
        .chars().take(32).collect()
}

mod sha256_impl {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub fn sha256_hex(data: &[u8]) -> String {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut msg = data.to_vec();
        let bit_len = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }
}

fn authorized(req: &Req) -> bool {
    let p = req.path.as_str();
    if OPEN_PATHS.contains(&p) {
        return true;
    }
    if auth_password().is_empty() {
        return true;
    }
    // cookie 解析：v2_auth=<token>
    let cookie = req.header("cookie");
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("v2_auth=") {
            if v == auth_token() {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------- 页面/静态

fn page_file(name: &str) -> PathBuf {
    let (next_on, pages) = web_mode_next();
    if next_on && (pages.is_empty() || pages.iter().any(|p| p == name)) {
        let f = base_dir().join("next").join(name);
        if f.is_file() {
            return f;
        }
    }
    base_dir().join(name)
}

/// `pages.page`：base.js/base.css 拼 mtime 版本号；HTML no-store。
fn page(name: &str) -> Resp {
    let f = page_file(name);
    let html = match std::fs::read_to_string(&f) {
        Ok(h) => h,
        Err(_) => {
            return crate::staticfiles::public_file(&base_dir(), name, None)
                .unwrap_or_else(Resp::not_found)
        }
    };
    let mut html = html;
    for asset in ["base.js", "base.css"] {
        if let Ok(meta) = std::fs::metadata(base_dir().join(asset)) {
            if let Ok(mtime) = meta.modified() {
                let secs = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let needle = format!("/{asset}\"");
                let repl = format!("/{asset}?v={secs}\"");
                html = html.replace(&needle, &repl);
            }
        }
    }
    Resp {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "text/html; charset=utf-8".into()),
            ("Cache-Control".into(), "no-store, must-revalidate".into()),
            ("Pragma".into(), "no-cache".into()),
            ("Expires".into(), "0".into()),
        ],
        body: html.into_bytes().into(),
    }
}

fn file_resp(path: &PathBuf, media_type: Option<&str>) -> Resp {
    match std::fs::read(path) {
        Ok(body) => {
            let mt = media_type
                .map(|s| s.to_string())
                .unwrap_or_else(|| crate::staticfiles::guess_mime(path).to_string());
            // Starlette FileResponse：media_type 以 text/ 开头且无 charset 时自动补 charset=utf-8
            let ct = if mt.starts_with("text/") && !mt.contains("charset") {
                format!("{mt}; charset=utf-8")
            } else {
                mt
            };
            Resp {
                status: 200,
                headers: vec![
                    ("Content-Type".into(), ct),
                    ("Cache-Control".into(), "no-cache".into()),
                ],
                body: body.into(),
            }
        }
        Err(_) => Resp::not_found(),
    }
}

fn redirect(location: &str, status: u16) -> Resp {
    Resp {
        status,
        headers: vec![("Location".into(), location.to_string())],
        body: Bytes::new(),
    }
}

// ---------------------------------------------------------------- 动作队列

fn actions_file() -> PathBuf {
    base_dir().join("actions_queue.json")
}

static ACT_LOCK: Mutex<()> = Mutex::new(());

fn actions_load() -> Value {
    read_json(&actions_file())
        .unwrap_or_else(|| json!({"seq": 0, "items": []}))
}

fn actions_save(d: &Value) {
    let p = actions_file();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = zg_core::jsonio::tmp_name(&p);
    if let Ok(body) = serde_json::to_vec(d) {
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

// ---------------------------------------------------------------- 路由

pub struct MonitorApp {
    pub shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub fn dispatch(app: &MonitorApp, req: &Req) -> Resp {
    let _ = app;
    // 只绑 127.0.0.1：Host 头是外部域名时一律拒绝，防 DNS rebinding 把本服务当同源
    let host = req.header("host");
    if !host.is_empty() && !crate::http::is_local_host(host) {
        return j(&json!({"detail": "forbidden host"}), 403);
    }
    if !authorized(req) {
        if req.path.starts_with("/api/") {
            return j(&json!({"detail": "未授权"}), 401);
        }
        // 页面：返回 login.html（FileResponse 语义）
        return file_resp(&base_dir().join("login.html"), None);
    }
    match req.method.as_str() {
        "GET" => do_get(req),
        "POST" => do_post(req),
        _ => j(&json!({"detail": "Method Not Allowed"}), 405),
    }
}

fn do_get(req: &Req) -> Resp {
    let p = req.path.as_str();
    // 带副作用的 GET（会发网络请求/唤起客户端）不接受跨站调用。
    // flowlab 系列在缓存过期时会实时探测同花顺，外部页面反复请求会打爆配额（H9）。
    if is_side_effect_get(p) && crate::http::cross_site(req) {
        return j(&json!({"detail": "cross-site request rejected"}), 403);
    }
    let data = data_dir();
    match p {
        "/base.css" => file_resp(&base_dir().join("base.css"), Some("text/css")),
        "/base.js" => file_resp(&base_dir().join("base.js"), Some("application/javascript")),
        "/manifest.json" => file_resp(&base_dir().join("manifest.json"), Some("application/manifest+json")),
        "/favicon.ico" => Resp { status: 204, headers: vec![], body: Bytes::new() },
        "/api/sector" => {
            let v = read_two(data.join("sector_web.json"),
                             data.join("sector_data").join("sector_web.json"),
                             json!({"error": "暂无板块数据包"}));
            j(&v, 200)
        }
        "/api/flow" => j(&read_two(data.join("flow.json"),
                                            data.join("sector_data").join("flow.json"),
                                            json!({"error": "暂无资金流数据包"})), 200),
        "/api/sw3" => j(&read_two(data.join("sw3.json"),
                                           data.join("sector_data").join("sw3.json"),
                                           json!({"error": "暂无三级行业数据包"})), 200),
        "/api/pick" => j(&read_two(data.join("pick.json"),
                                            data.join("sector_data").join("pick.json"),
                                            json!({"error": "暂无标的数据包"})), 200),
        "/api/trace" => j(&read_two(data.join("trace.json"),
                                             data.join("sector_data").join("trace.json"),
                                             json!({"error": "暂无今日溯源数据包"})), 200),
        "/api/home" => j(&read_two(data.join("home.json"),
                                            data.join("sector_data").join("home.json"),
                                            json!({"error": "暂无总览数据包"})), 200),
        "/api/reports" => j(&read_two(
            data.join("web_reports").join("index.json"),
            data.join("sector_data").join("web_reports").join("index.json"),
            json!({"reports": []})), 200),
        "/api/sector/codemap" => j(&read_two(
            data.join("code_l1.json"),
            data.join("sector_data").join("code_l1.json"),
            json!({})), 200),
        "/api/state" => {
            let web = read_json(&data.join("webdata.json")).unwrap_or(json!({}));
            let facts = read_json(&data.join("market_facts.json")).unwrap_or(json!({}));
            j(&json!({
                "webdata": web,
                "facts": facts,
                "live": crate::monitor_live::live_snapshot(),
                "now": crate::state::now(),
            }), 200)
        }
        "/api/etf" => j(&crate::monitor_services::etf_payload(), 200),
        "/api/ths_debug" => j(&crate::monitor_services::ths_debug_payload(), 200),
        "/api/limitup" => {
            let force = req.q("force").map(|v| v != "0" && !v.is_empty()).unwrap_or(false);
            match crate::monitor_services::limitup_payload(force) {
                Ok(v) => j(&v, 200),
                Err(e) => j(&json!({"error": e.chars().take(200).collect::<String>()}), 200),
            }
        }
        "/api/flowlab/rankings" => {
            let d = read_json(&data.join("flow_rankings.json"))
                .unwrap_or_else(|| json!({"week": "", "days": {}, "locked": ""}));
            let lt = chrono::Local::now();
            use chrono::Datelike;
            j(&json!({
                "week": d.get("week").cloned().unwrap_or(json!("")),
                "days": d.get("days").cloned().unwrap_or(json!({})),
                "locked": d.get("locked").cloned().unwrap_or(json!("")),
                "today": lt.format("%Y%m%d").to_string(),
                "weekday": lt.weekday().num_days_from_monday(),
                "error": Value::Null,
            }), 200)
        }
        "/api/open_stock" => {
            let (status, body, ct) = crate::monitor_yuanhang::open_in_yuanhang(
                req.q("code").unwrap_or(""), req.q("mode").unwrap_or("max"));
            Resp {
                status,
                headers: vec![("Content-Type".into(), ct.to_string())],
                body: body.into(),
            }
        }
        "/api/flowlab" => j(&crate::monitor_sentiment::flowlab_payload(), 200),
        "/api/flowlab/live" => {
            let mut payload = crate::monitor_flow::flow_live();
            let ts = payload.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if let Some(o) = payload.as_object_mut() {
                o.insert("age".into(), if ts > 0.0 {
                    Value::from(((crate::state::now() - ts) * 10.0).round() / 10.0)
                } else {
                    Value::Null
                });
                o.insert("trading".into(), Value::Bool(crate::monitor_live::in_trading()));
            }
            j(&payload, 200)
        }
        "/api/flowlab/micro" => j(&crate::monitor_sentiment::micro_payload(), 200),
        "/api/flowlab/sentiment" => j(&crate::monitor_sentiment::sentiment_payload(), 200),
        "/api/flowlab/sector_sentiment" => {
            j(&crate::monitor_sentiment::sector_sentiment_payload(req.q("name").unwrap_or("")), 200)
        }
        "/api/flowlab/indexes" => j(&crate::monitor_sentiment::indexes_payload(), 200),
        "/api/flowlab/constituents" => {
            j(&crate::monitor_sentiment::constituents_payload(req.q("code").unwrap_or("")), 200)
        }
        "/api/actions/pending" => {
            if !worker_ok(req) {
                return j(&json!({"detail": "forbidden"}), 403);
            }
            let d = actions_load();
            let items: Vec<Value> = d.get("items").and_then(|x| x.as_array())
                .map(|a| a.iter().filter(|x| x.get("status").and_then(|s| s.as_str()) == Some("pending")).cloned().collect())
                .unwrap_or_default();
            j(&json!({"items": items}), 200)
        }
        _ => {
            if let Some(l1) = p.strip_prefix("/api/sector/detail/") {
                let l1 = percent_decode(l1);
                // 只接受单段名字：解码出的 / \ .. 会跳出 sector_details 目录
                if l1.is_empty()
                    || l1 == "."
                    || l1 == ".."
                    || l1.contains('/')
                    || l1.contains('\\')
                    || l1.contains('\0')
                {
                    return j(&json!({"error": "参数非法"}), 400);
                }
                let d = read_json(&data.join("sector_details").join(format!("{l1}.json")))
                    .or_else(|| read_json(&data.join("sector_data").join("web_details").join(format!("{l1}.json"))));
                return match d {
                    Some(v) => j(&v, 200),
                    None => j(&json!({"error": "详情未生成"}), 404),
                };
            }
            if let Some(fname) = p.strip_prefix("/report/") {
                let fname = fname.rsplit('/').next().unwrap_or(fname).to_string();
                if let Some(stem) = fname.strip_suffix(".html") {
                    if RESEARCH_KEYS.contains(&stem) {
                        return redirect(&format!("/research/{stem}"), 301);
                    }
                }
                for base in [data.join("web_reports"), data.join("sector_data").join("web_reports")] {
                    let f = base.join(&fname);
                    if f.is_file() && matches!(f.extension().and_then(|e| e.to_str()), Some("html") | Some("htm")) {
                        return file_resp(&f, Some("text/html"));
                    }
                }
                return j(&json!({"error": "not found"}), 404);
            }
            if p == "/research/sw3" {
                return page("sw3.html");
            }
            if p == "/research/sw3.raw" {
                for base in [data.join("web_reports"), data.join("sector_data").join("web_reports")] {
                    let f = base.join("sw3.html");
                    if f.exists() {
                        return file_resp(&f, Some("text/html"));
                    }
                }
                return j(&json!({"error": "大表未生成"}), 404);
            }
            if let Some(key) = p.strip_prefix("/research/") {
                if RESEARCH_KEYS.contains(&key) {
                    for base in [data.join("web_reports"), data.join("sector_data").join("web_reports")] {
                        let f = base.join(format!("{key}.html"));
                        if f.exists() {
                            return file_resp(&f, Some("text/html"));
                        }
                    }
                    return j(&json!({"error": "大表未生成"}), 404);
                }
                return j(&json!({"error": "not found"}), 404);
            }
            if let Some(name) = p.strip_prefix("/shared/") {
                let root = web_dir();
                let rel = percent_decode(name);
                return match crate::staticfiles::public_file(&root, &rel, Some(req.header("if-none-match"))) {
                    Some(mut r) => {
                        // Python 对 .html/.js/.css/.json 显式追加 charset
                        if let Some(ct) = r.headers.iter_mut().find(|(k, _)| k == "Content-Type") {
                            let needs = [".html", ".js", ".css", ".json"].iter()
                                .any(|suf| rel.to_lowercase().ends_with(suf));
                            if needs && !ct.1.contains("charset") {
                                ct.1.push_str("; charset=utf-8");
                            }
                        }
                        r
                    }
                    None => j(&json!({"detail": "Not Found"}), 404),
                };
            }
            // 页面
            let page_name = match p {
                "/" => Some("home.html"),
                "/pick" => Some("pick.html"),
                "/trace" => Some("trace.html"),
                "/flowlab" => Some("flowlab.html"),
                "/flowtree" => Some("flowtree.html"),
                "/limitup" => Some("limitup.html"),
                "/pulse" => Some("pulse.html"),
                "/login" => Some("login.html"),
                "/hold" => Some("hold.html"),
                "/review" => Some("review.html"),
                "/research" => Some("research.html"),
                "/sectors" => Some("sectors.html"),
                "/help" => Some("help.html"),
                "/reports" => Some("reports.html"),
                _ => None,
            };
            if let Some(name) = page_name {
                return page(name);
            }
            if p.starts_with("/sector/") {
                return page("sector.html");
            }
            if p.starts_with("/stock/") {
                return page("stock.html");
            }
            // 旧站 redirect（catch-all 最后）
            let norm = format!("/{}", p.trim_matches('/'));
            if let Some((_, target)) = LEGACY_REDIRECTS.iter().find(|(k, _)| *k == norm) {
                return redirect(target, 302);
            }
            j(&json!({"error": "not found"}), 404)
        }
    }
}

fn worker_ok(req: &Req) -> bool {
    let pw = auth_password();
    !pw.is_empty() && req.header("x-worker-key") == pw
}

/// 8787 上会触发上游网络请求/唤起客户端的 GET（跨站一律拒绝）。
fn is_side_effect_get(p: &str) -> bool {
    matches!(
        p,
        "/api/open_stock"
            | "/api/limitup"
            | "/api/ths_debug"
            | "/api/flowlab"
            | "/api/flowlab/live"
            | "/api/flowlab/micro"
            | "/api/flowlab/sentiment"
            | "/api/flowlab/sector_sentiment"
            | "/api/flowlab/indexes"
            | "/api/flowlab/constituents"
    )
}

fn do_post(req: &Req) -> Resp {
    let p = req.path.as_str();
    // 跨站 POST 一律拒绝（H2）：否则任意网页可用 text/plain 简单请求直接
    // 注入 /api/action（浏览器不发预检也不读响应）。
    if crate::http::cross_site(req) {
        return j(&json!({"detail": "cross-site request rejected"}), 403);
    }
    // 浏览器侧接口强制 JSON（触发预检，与 8786 口径一致）；
    // /api/actions/complete 供本地 worker 调用，保持原有宽松口径。
    if matches!(p, "/api/login" | "/api/action") {
        let ctype = req.header("content-type").split(';').next().unwrap_or("").trim();
        if !ctype.eq_ignore_ascii_case("application/json") {
            return j(&json!({"detail": "需要 JSON 请求"}), 415);
        }
    }
    match p {
        "/api/login" => {
            let pw = auth_password();
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
            let ok = !pw.is_empty() && body.get("password").and_then(|x| x.as_str()) == Some(pw.as_str());
            if ok {
                let mut r = j(&json!({"ok": true}), 200);
                r.headers.push((
                    "Set-Cookie".into(),
                    format!("v2_auth={}; Max-Age={}; HttpOnly; Path=/", auth_token(), 90 * 86400),
                ));
                r
            } else {
                j(&json!({"detail": "密码错误"}), 401)
            }
        }
        "/api/action" => {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
            let cmd = body.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
            if !ALLOWED_CMDS.contains(&cmd.as_str()) {
                return j(&json!({"detail": "未知动作"}), 400);
            }
            let args = body.get("args").cloned().unwrap_or(json!({}));
            let _g = ACT_LOCK.lock_ok();
            let mut d = actions_load();
            let seq = d.get("seq").and_then(|s| s.as_i64()).unwrap_or(0) + 1;
            if let Some(o) = d.as_object_mut() {
                o.insert("seq".into(), Value::from(seq));
            }
            let now_t = crate::state::now();
            let item = json!({"id": seq, "cmd": cmd, "args": args, "status": "pending",
                              "ts": now_t, "result": Value::Null});
            let items: Vec<Value> = d.get("items").and_then(|x| x.as_array())
                .map(|a| a.iter()
                    .filter(|x| now_t - x.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0) < 86400.0)
                    .cloned().collect())
                .unwrap_or_default();
            let mut items = items;
            if items.len() > 200 {
                items = items[items.len() - 200..].to_vec();
            }
            items.push(item);
            if let Some(o) = d.as_object_mut() {
                o.insert("items".into(), Value::Array(items));
            }
            actions_save(&d);
            j(&json!({"id": seq}), 200)
        }
        "/api/actions/complete" => {
            if !worker_ok(req) {
                return j(&json!({"detail": "forbidden"}), 403);
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
            let id = body.get("id");
            let status = body.get("status").and_then(|s| s.as_str()).unwrap_or("done").to_string();
            let result: String = body.get("result").map(|r| r.to_string()).unwrap_or_default()
                .chars().take(8000).collect();
            let _g = ACT_LOCK.lock_ok();
            let mut d = actions_load();
            if let Some(items) = d.get_mut("items").and_then(|x| x.as_array_mut()) {
                for x in items.iter_mut() {
                    if x.get("id") == id {
                        if let Some(o) = x.as_object_mut() {
                            o.insert("status".into(), Value::String(status.clone()));
                            o.insert("result".into(), Value::String(result.clone()));
                        }
                    }
                }
            }
            actions_save(&d);
            j(&json!({"ok": true}), 200)
        }
        _ => {
            if let Some(aid) = p.strip_prefix("/api/action/") {
                let aid: i64 = aid.parse().unwrap_or(-1);
                let _g = ACT_LOCK.lock_ok();
                let d = actions_load();
                if let Some(items) = d.get("items").and_then(|x| x.as_array()) {
                    for x in items {
                        if x.get("id").and_then(|i| i.as_i64()) == Some(aid) {
                            return j(x, 200);
                        }
                    }
                }
                return j(&json!({"detail": "not found"}), 404);
            }
            j(&json!({"detail": "Not Found"}), 404)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_reference() {
        // 已知向量：sha256("abc")
        assert_eq!(
            sha256_impl::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 空串
        assert_eq!(
            sha256_impl::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn read_json_gbk_fallback() {
        let dir = std::env::temp_dir().join("zg_monitor_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("gbk.json");
        let (bytes, _, _) = encoding_rs::GBK.encode("{\"name\": \"电子\"}");
        std::fs::write(&p, bytes.as_ref()).ok();
        let v = read_json(&p);
        assert!(v.is_some());
        assert_eq!(v.unwrap()["name"], "电子");
    }
}
