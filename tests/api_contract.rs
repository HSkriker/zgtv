//! 路由契约测试：不起服务、不碰网络，只验证 dispatch 的输入校验、Host 门与 304 语义。
//! ZG_ROOT 指向仓库根，静态文件与 data/ 都能找到。

use std::collections::HashMap;

use tv::router::dispatch;
use zg_api::http::{Req, Resp};

fn req(method: &str, path: &str) -> Req {
    Req {
        method: method.into(),
        path: path.into(),
        query: HashMap::new(),
        headers: HashMap::new(),
        body: Vec::new(),
    }
}

fn with_query(mut r: Req, pairs: &[(&str, &str)]) -> Req {
    for (k, v) in pairs {
        r.query.entry((*k).to_string()).or_default().push((*v).to_string());
    }
    r
}

fn with_header(mut r: Req, k: &str, v: &str) -> Req {
    r.headers.insert(k.to_lowercase(), v.to_string());
    r
}

/// tv_root() 每次调用都读环境变量；同一个测试进程里设一次即可。
/// 目录布局校验（modules/ + data/）不通过时它会自己回退，不会把测试带偏。
fn init_root() {
    std::env::set_var("ZG_ROOT", env!("CARGO_MANIFEST_DIR"));
}

fn header<'a>(r: &'a Resp, name: &str) -> &'a str {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

#[test]
fn rejects_non_local_host() {
    init_root();
    let r = with_header(req("GET", "/api/echo"), "Host", "evil.example:8790");
    assert_eq!(dispatch(&r).status, 403);
}

#[test]
fn unknown_api_is_404() {
    init_root();
    assert_eq!(dispatch(&req("GET", "/api/nope")).status, 404);
}

#[test]
fn bars_requires_code_and_valid_period() {
    init_root();
    assert_eq!(dispatch(&req("GET", "/api/bars")).status, 400);

    let r = with_query(req("GET", "/api/bars"), &[("code", "600519"), ("period", "7")]);
    assert_eq!(dispatch(&r).status, 400);
}

#[test]
fn static_file_etag_round_trip() {
    init_root();
    let first = dispatch(&req("GET", "/js/api.js"));
    assert_eq!(first.status, 200);
    assert!(!first.body.is_empty());

    let etag = header(&first, "ETag").to_string();
    assert!(etag.starts_with('"'));

    let second = with_header(req("GET", "/js/api.js"), "If-None-Match", &etag);
    let r = dispatch(&second);
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());
}

#[test]
fn path_traversal_rejected() {
    init_root();
    assert_eq!(dispatch(&req("GET", "/../Cargo.toml")).status, 404);
}
