//! HTTP 客户端 —— 与 `zgcore/net.py` 同口径（UA / 主机轮换 / gzip / 回环绕过代理）。
//!
//! 重试/轮换/退避口径与旧实现一致（顺序轮换、0.4s 线性退避、返回解码文本）。

use std::time::Duration;

use serde_json::Value;

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

const LOCAL_HOSTS: &[&str] = &["127.0.0.1", "localhost", "::1", "0.0.0.0"];

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub fn is_local(url: &str) -> bool {
    host_of(url)
        .map(|h| LOCAL_HOSTS.contains(&h.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// URL 里的端口（不含冒号）；IPv6 字面量（`[::1]:port`）按方括号边界取。
fn port_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = hostport.rsplit('@').next().unwrap_or(hostport);
    let after = match hostport.find(']') {
        Some(i) => &hostport[i + 1..],
        None => hostport,
    };
    let (_, p) = after.rsplit_once(':')?;
    if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(p.to_string())
}

fn rotate_host(url: &str, host: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let tail = rest.find('/').map(|i| &rest[i..]).unwrap_or("");
            // 原 URL 带端口就保留：否则 http://127.0.0.1:17709/ 会被换成 http://127.0.0.1/
            let port = port_of(url).map(|p| format!(":{p}")).unwrap_or_default();
            format!("{scheme}://{host}{port}{tail}")
        }
        None => url.to_string(),
    }
}

fn agent(local: bool, timeout: u64) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout))
        .redirects(5);
    if !local {
        if let Ok(p) = std::env::var("HTTP_PROXY").or_else(|_| std::env::var("http_proxy")) {
            if let Ok(proxy) = ureq::Proxy::new(&p) {
                b = b.proxy(proxy);
            }
        }
    }
    b.build()
}

/// GET 原始字节；hosts=按序轮换的主机名列表（替换 URL 的 host 部分）。
pub fn read_url(url: &str, headers: Option<&[(&str, &str)]>, timeout: u64, retries: u32,
                hosts: Option<&[&str]>, bypass_proxy: Option<bool>) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for i in 0..=retries {
        let target = match hosts {
            Some(hs) if !hs.is_empty() => rotate_host(url, hs[i as usize % hs.len()]),
            _ => url.to_string(),
        };
        let direct = bypass_proxy.unwrap_or_else(|| is_local(&target));
        let ag: ureq::Agent = agent(direct, timeout);
        let mut req = ag.get(&target).set("User-Agent", UA).set("Connection", "close");
        if let Some(hs) = headers {
            for (k, v) in hs {
                req = req.set(k, v);
            }
        }
        match req.call() {
            Ok(resp) => {
                let mut buf = Vec::new();
                use std::io::Read;
                if resp.into_reader().read_to_end(&mut buf).is_ok() {
                    return Ok(buf);
                }
                last = "read body failed".into();
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(400 * (i as u64 + 1)));
    }
    Err(last)
}

/// GET → utf-8 文本（忽略解码错误）。
pub fn http_get(url: &str, timeout: u64, retries: u32, hosts: Option<&[&str]>,
                headers: Option<&[(&str, &str)]>, bypass_proxy: Option<bool>) -> Result<String, String> {
    read_url(url, headers, timeout, retries, hosts, bypass_proxy)
        .map(|b| String::from_utf8_lossy(&b).to_string())
}

pub fn http_get_json(url: &str, timeout: u64, retries: u32, hosts: Option<&[&str]>,
                     headers: Option<&[(&str, &str)]>, bypass_proxy: Option<bool>)
    -> Result<Value, String> {
    let text = http_get(url, timeout, retries, hosts, headers, bypass_proxy)?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing_and_rotation() {
        assert_eq!(host_of("http://127.0.0.1:8786/x"), Some("127.0.0.1"));
        assert!(is_local("http://localhost:8786/api"));
        assert!(!is_local("https://push2.eastmoney.com/api"));
        // 轮换主机名时端口必须保留：http://127.0.0.1:17709/ 不能被换成 http://localhost/
        assert_eq!(
            rotate_host("https://a.example.com:8080/x?y=1", "b.example.com"),
            "https://b.example.com:8080/x?y=1"
        );
        assert_eq!(
            rotate_host("http://127.0.0.1:17709/", "localhost"),
            "http://localhost:17709/"
        );
        // 本来就没有端口的 URL 不受影响
        assert_eq!(
            rotate_host("https://a.example.com/x?y=1", "b.example.com"),
            "https://b.example.com/x?y=1"
        );
    }
}
