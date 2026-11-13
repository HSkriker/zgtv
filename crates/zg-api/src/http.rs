//! 请求/响应原语 —— 对齐 Python `BaseHTTPRequestHandler` 的用法子集。

use std::collections::HashMap;

use bytes::Bytes;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    /// 未解码的原始路径（`urlparse(self.path).path`）
    pub path: String,
    /// 解析后的 query（`parse_qs` 语义：首次出现 + 值为空列表时不出现）
    pub query: HashMap<String, Vec<String>>,
    /// 小写头名 → 值
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn q(&self, key: &str) -> Option<&str> {
        self.query.get(key).and_then(|v| v.first()).map(|s| s.as_str())
    }

    pub fn q_or(&self, key: &str, default: &str) -> String {
        self.q(key).unwrap_or(default).to_string()
    }

    pub fn header(&self, key: &str) -> &str {
        self.headers.get(&key.to_lowercase()).map(|s| s.as_str()).unwrap_or("")
    }
}

#[derive(Clone, Debug)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl Resp {
    /// `_json`：Content-Type application/json; charset=utf-8 + Cache-Control no-store。
    pub fn json(v: &Value, status: u16) -> Resp {
        let body = Bytes::from(serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()));
        Resp {
            status,
            headers: vec![
                ("Content-Type".into(), "application/json; charset=utf-8".into()),
                ("Cache-Control".into(), "no-store".into()),
            ],
            body,
        }
    }

    /// `send_error(404)` 的响应体（Python 默认错误页 HTML）。
    pub fn not_found() -> Resp {
        let body = Bytes::from_static(
            b"<html><head><title>Error response</title></head><body>\
<h1>Error response</h1><p>Error code: 404</p><p>Message: File not found.</p></body></html>\n",
        );
        Resp {
            status: 404,
            headers: vec![("Content-Type".into(), "text/html;charset=utf-8".into())],
            body,
        }
    }

    /// `send_response(200)` + 指定 mime 的文件响应（staticfiles.send_file）。
    pub fn file(body: Bytes, mime: &str, charset: bool) -> Resp {
        Resp::file_etag(body, mime, charset, None)
    }

    /// 同 `file`，但带 ETag（静态资源条件请求：命中即回 304，不再重传正文）。
    pub fn file_etag(body: Bytes, mime: &str, charset: bool, etag: Option<&str>) -> Resp {
        let ct = if charset { format!("{mime}; charset=utf-8") } else { mime.to_string() };
        let mut headers = vec![
            ("Content-Type".into(), ct),
            ("Cache-Control".into(), "no-cache".into()),
        ];
        if let Some(e) = etag {
            headers.push(("ETag".into(), e.to_string()));
        }
        Resp { status: 200, headers, body }
    }

    /// 304 Not Modified：条件请求命中 ETag，浏览器复用本地副本（静态资源刷新不再全量下载）。
    pub fn not_modified(etag: &str) -> Resp {
        Resp {
            status: 304,
            headers: vec![
                ("ETag".into(), etag.into()),
                ("Cache-Control".into(), "no-cache".into()),
            ],
            body: Bytes::new(),
        }
    }
}

/// Python `parse_qs` 语义：`a=1&a=2&b=` → {"a":["1","2"], "b":[""]}；
/// 无 `=` 的键按空值处理；解码 `%XX` 与 `+`。
pub fn parse_qs(query: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if query.is_empty() {
        return out;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        };
        out.entry(k).or_default().push(v);
    }
    out
}

/// Host 头里的主机名（去端口；IPv6 去方括号）。
pub fn host_name(host: &str) -> &str {
    let h = host.trim();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    if h.matches(':').count() != 1 {
        return h;
    }
    let (name, port) = h.split_once(':').unwrap_or((h, ""));
    if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
        name
    } else {
        h
    }
}

/// 服务只绑定 127.0.0.1，Host 头出现其它名字即 DNS rebinding 尝试。
pub fn is_local_host(host: &str) -> bool {
    matches!(host_name(host), "localhost" | "127.0.0.1" | "::1")
}

/// 浏览器跨站请求判定：`Sec-Fetch-Site: cross-site` 直接成立；
/// 无该头时退回 Origin 与 Host 同源比较（无 Origin 的普通 GET 视为同源）。
pub fn cross_site(req: &Req) -> bool {
    let site = req.header("sec-fetch-site");
    if !site.is_empty() {
        return site == "cross-site";
    }
    let origin = req.header("origin");
    if origin.is_empty() {
        return false;
    }
    let host = req.header("host");
    if host.is_empty() {
        return true;
    }
    let authority = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    authority.split('/').next().unwrap_or(authority) != host
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_qs_semantics() {
        let q = parse_qs("code=600519&period=day&codes=a,b&empty=");
        assert_eq!(q.get("code").unwrap()[0], "600519");
        assert_eq!(q.get("period").unwrap()[0], "day");
        assert_eq!(q.get("empty").unwrap()[0], "");
        let q2 = parse_qs("name=%E7%94%B5%E5%AD%90&x=1+2");
        assert_eq!(q2.get("name").unwrap()[0], "电子");
        assert_eq!(q2.get("x").unwrap()[0], "1 2");
        assert!(parse_qs("").is_empty());
    }

    #[test]
    fn host_and_cross_site() {
        assert_eq!(host_name("127.0.0.1:8786"), "127.0.0.1");
        assert_eq!(host_name("localhost"), "localhost");
        assert_eq!(host_name("[::1]:8787"), "::1");
        assert_eq!(host_name("::1"), "::1");
        assert!(is_local_host("localhost:8787"));
        assert!(is_local_host("127.0.0.1"));
        assert!(!is_local_host("evil.example:8787"));

        let mut headers = HashMap::new();
        headers.insert("host".to_string(), "127.0.0.1:8786".to_string());
        let mut req = Req {
            method: "GET".into(),
            path: "/api/watch".into(),
            query: HashMap::new(),
            headers,
            body: Vec::new(),
        };
        assert!(!cross_site(&req));
        req.headers.insert("sec-fetch-site".into(), "cross-site".into());
        assert!(cross_site(&req));
        req.headers.remove("sec-fetch-site");
        req.headers.insert("origin".into(), "http://evil.example".into());
        assert!(cross_site(&req));
        req.headers.insert("origin".into(), "http://127.0.0.1:8786".into());
        assert!(!cross_site(&req));
    }
}

#[cfg(test)]
mod body_tests {
    use super::*;

    #[test]
    fn not_modified_has_empty_bytes_body() {
        let r = Resp::not_modified("\"x\"");
        assert_eq!(r.status, 304);
        assert!(r.body.is_empty());
    }

    #[test]
    fn file_etag_keeps_body_and_header() {
        let r = Resp::file_etag(Bytes::from_static(b"abc"), "text/plain", false, Some("\"e\""));
        assert_eq!(r.status, 200);
        assert_eq!(&r.body[..], b"abc");
        assert!(r.headers.iter().any(|(k, v)| k == "ETag" && v == "\"e\""));
    }

    #[test]
    fn bytes_clone_does_not_copy_content() {
        let a = Bytes::from_static(b"abcdef");
        let b = a.clone();
        assert_eq!(a.as_ptr(), b.as_ptr(), "Bytes clone 必须共享同一块内存");
    }
}
