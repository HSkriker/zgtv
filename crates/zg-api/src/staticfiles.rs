//! 静态文件服务 —— 对齐 `modules/zgapp/staticfiles.py`。

use std::path::{Path, PathBuf};

use crate::http::Resp;

pub const ALLOWED_SUFFIX: &[&str] = &[
    "html", "js", "css", "json", "png", "jpg", "jpeg", "svg", "ico", "woff", "woff2",
];
pub const CHARSET_SUFFIX: &[&str] = &["html", "js", "css", "json"];

/// Python `mimetypes.guess_type` 的等价映射（仅覆盖 ALLOWED_SUFFIX）。
pub fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
        Some("html") => "text/html",
        Some("js") => "application/javascript", // Python 3.13+（RFC 9239）
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon", // Python 3.13+ 行为
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn has_allowed_suffix(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ALLOWED_SUFFIX.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn under_root(path: &Path, root: &Path) -> bool {
    // Python `path.is_relative_to(root.resolve())`：逻辑上等价的前缀判定
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(p), Ok(r)) => p.starts_with(&r),
        _ => false,
    }
}

/// `public_file`：root 下的文件，校验越界/后缀。
pub fn public_file(root: &Path, relative: &str, if_none_match: Option<&str>) -> Option<Resp> {
    let path = root.join(crate::http::percent_decode(relative));
    if !under_root(&path, root) || !path.is_file() || !has_allowed_suffix(&path) {
        return None;
    }
    send_file(&path, if_none_match)
}

/// 弱 ETag：mtime(纳秒)+size —— 文件一改就变，浏览器据此复用本地副本。
fn file_etag(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?
        .duration_since(std::time::UNIX_EPOCH).ok()?
        .as_nanos();
    Some(format!("W/\"{:x}-{:x}\"", mtime, md.len()))
}

/// `If-None-Match` 命中判定：逗号分隔列表，`*` 与 `W/` 强弱前缀差异都要认。
pub fn if_none_match_hit(header: &str, etag: &str) -> bool {
    let want = etag.trim();
    fn strip_weak(s: &str) -> &str { s.trim().trim_start_matches("W/").trim() }
    header.split(',').map(|s| s.trim()).any(|c| {
        c == "*" || c == want || strip_weak(c) == strip_weak(want)
    })
}

fn send_file(path: &Path, if_none_match: Option<&str>) -> Option<Resp> {
    let etag = file_etag(path);
    if let (Some(h), Some(e)) = (if_none_match, etag.as_deref()) {
        if if_none_match_hit(h, e) {
            return Some(Resp::not_modified(e));
        }
    }
    let body = std::fs::read(path).ok()?;
    let mime = guess_mime(path);
    let charset = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| CHARSET_SUFFIX.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);
    Some(Resp::file_etag(body.into(), mime, charset, etag.as_deref()))
}

fn next_enabled() -> bool {
    // config.WEB_MODE（默认 next）；WEB_PAGES 未列 index 时整站走 legacy
    let mode = std::env::var("ZG_WEB").unwrap_or_else(|_| "next".into()).trim().to_lowercase();
    if mode != "next" {
        return false;
    }
    let pages: Vec<String> = std::env::var("ZG_WEB_PAGES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    pages.is_empty() || pages.iter().any(|p| p == "index")
}

/// `static_file`：static_next 优先，缺文件回落 static。
pub fn static_file(relative: &str) -> Option<PathBuf> {
    let root = zg_core::paths::get().root;
    let market_dir = root.join("modules").join("market");
    let mut roots = Vec::new();
    if next_enabled() {
        roots.push(market_dir.join("static_next"));
    }
    roots.push(market_dir.join("static"));
    for r in roots {
        let path = r.join(relative);
        if under_root(&path, &r) && path.is_file() && has_allowed_suffix(&path) {
            return Some(path);
        }
    }
    None
}

/// `static_path`：`/` → index.html；`/static/xxx` → xxx。
pub fn static_path(path: &str, if_none_match: Option<&str>) -> Option<Resp> {
    let relative = if path == "/" {
        "index.html".to_string()
    } else {
        let p = path.strip_prefix("/static/").unwrap_or(path);
        p.trim_start_matches('/').to_string()
    };
    let f = static_file(&relative)?;
    send_file(&f, if_none_match)
}

/// `shared_path`：`/shared/xxx` → WEB_DIR/xxx。
pub fn shared_path(path: &str, if_none_match: Option<&str>) -> Option<Resp> {
    let root = zg_core::paths::get().root.join("web");
    let rel = path.strip_prefix("/shared/").unwrap_or(path);
    public_file(&root, rel, if_none_match)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_and_suffix() {
        assert_eq!(guess_mime(Path::new("a/b.js")), "application/javascript");
        assert_eq!(guess_mime(Path::new("a/b.ico")), "image/x-icon");
        assert_eq!(guess_mime(Path::new("a/b.bin")), "application/octet-stream");
        assert!(has_allowed_suffix(Path::new("x.JSON")));
        assert!(!has_allowed_suffix(Path::new("x.exe")));
    }

    #[test]
    fn traverse_blocked() {
        let root = std::env::temp_dir().join("zg_static_test");
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(root.join("ok.js"), "1").ok();
        assert!(public_file(&root, "ok.js", None).is_some());
        assert!(public_file(&root, "../etc/passwd", None).is_none());
        assert!(public_file(&root, "missing.js", None).is_none());
    }
    #[test]
    fn if_none_match_recognizes_lists_and_weak_prefix() {
        assert!(if_none_match_hit("W/\"abc-1\"", "W/\"abc-1\""));
        assert!(if_none_match_hit("\"abc-1\", W/\"def-2\"", "W/\"def-2\""));
        assert!(if_none_match_hit("*", "W/\"abc-1\""));
        assert!(if_none_match_hit("W/\"abc-1\"", "\"abc-1\""));
        assert!(!if_none_match_hit("W/\"other-9\"", "W/\"abc-1\""));
        assert!(!if_none_match_hit("", "W/\"abc-1\""));
    }

    #[test]
    fn etag_is_stable_for_same_file() {
        let dir = std::env::temp_dir().join("zg_etag_test");
        std::fs::create_dir_all(&dir).ok();
        let f = dir.join("a.js");
        std::fs::write(&f, b"console.log(1)").ok();
        let e1 = file_etag(&f).unwrap();
        let e2 = file_etag(&f).unwrap();
        assert_eq!(e1, e2);
        assert!(e1.starts_with("W/\""));
        std::fs::write(&f, b"console.log(2) // longer than before").ok();
        let e3 = file_etag(&f).unwrap();
        assert_ne!(e1, e3, "内容变更后 ETag 必须变化");
        std::fs::remove_dir_all(&dir).ok();
    }

}
