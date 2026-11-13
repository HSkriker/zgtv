use std::path::Path;

use zg_api::http::Resp;

const ALLOWED: &[&str] = &[
    "html", "js", "css", "json", "png", "jpg", "jpeg", "svg", "ico", "woff", "woff2",
];

pub fn serve(path: &str, if_none_match: &str) -> Resp {
    // 根目录按运行期解析（分发包可搬），不再是编译机绝对路径。
    serve_with_root(&crate::paths::tv_root(), path, if_none_match)
}

/// 同 `serve`，但显式指定根目录——测试用临时 fixture 验证判定顺序，不碰真实 web/。
fn serve_with_root(root: &Path, path: &str, if_none_match: &str) -> Resp {
    let rel = if path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    // 目录穿越直接拒绝，不做任何“聪明”的规范化
    if rel.split('/').any(|seg| seg == "..") {
        return Resp::not_found();
    }
    // Windows 盘符路径对 Path::join 是绝对路径会替换 base——Linux 部署不可达，仍显式拒绝
    if rel.contains(':') {
        return Resp::not_found();
    }

    // 只放行两个明确的根：web/（页面与脚本）与仓库根下的 vendor/（本地 vendored 图表库）。
    // 原第二 base 曾是整个仓库树（会暴露 data/market/*.json 等），收紧后仅 vendor/ 前缀可达。
    let base = if rel.starts_with("vendor/") { root.to_path_buf() } else { root.join("web") };
    let p = base.join(&rel);
    if !p.is_file() || !allowed(&p) {
        return Resp::not_found();
    }
    let etag = etag_of(&p);
    // 条件请求命中就直接 304：不要再把整个文件读进内存（/js/panels.js 已近 60KB，
    // 满深度分钟线 JSON 更大）。顺序必须与 zg-api 的 staticfiles.rs 一致。
    if !if_none_match.is_empty() && if_none_match == etag {
        return Resp::not_modified(&etag);
    }
    let Ok(body) = std::fs::read(&p) else { return Resp::not_found() };
    let mut resp =
        Resp::file_etag(body.into(), zg_api::staticfiles::guess_mime(&p), charset(&p), Some(&etag));
    // 页面无内联脚本，script 锁死 'self'；但面板渲染大量依赖内联 style 属性
    // （bcell 背景条宽度/颜色、图例色点、ov-chip 底色——动态数值无法用静态 class 表达，
    // 2026-11-02 实测 CSP 拦截后全部不可见），故 style-src 单独放行 'unsafe-inline'
    if ext(&p) == "html" {
        resp.headers.push((
            "Content-Security-Policy".to_string(),
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:".to_string(),
        ));
    }
    resp
}

fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

fn allowed(p: &Path) -> bool {
    ALLOWED.contains(&ext(p).as_str())
}

fn charset(p: &Path) -> bool {
    matches!(ext(p).as_str(), "html" | "js" | "css" | "json")
}

fn etag_of(p: &Path) -> String {
    let md = std::fs::metadata(p).ok();
    let len = md.as_ref().map(|m| m.len()).unwrap_or(0);
    let ms = md
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("\"{:x}-{:x}\"", ms, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// 临时 fixture 根：自建 web/js/probe.js，测试全程不碰真实仓库文件。
    /// （旧做法把真实 api.js chmod 000 靠 Drop 守卫恢复——测试进程被 SIGKILL
    /// 时文件会留在 000，页面全挂；fixture 留在 /tmp 里则无害且可被清理。）
    struct Fixture {
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!(
                "tv_static_files_{}_{}",
                std::process::id(),
                tag
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("web/js")).unwrap();
            std::fs::write(root.join("web/js/probe.js"), b"probe").unwrap();
            Fixture { root }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// ETag 命中必须在 `fs::read` 之前返回：把 fixture 权限改成 000（读不了），
    /// 仍要回 304 —— 说明 304 判定根本没走到读盘那一步。
    /// （chmod 只动 ctime 不动 mtime，etag_of 的 mtime+len 指纹保持可比。）
    #[cfg(unix)]
    #[test]
    fn etag_hit_does_not_need_read_permission() {
        let fx = Fixture::new("etag304");
        let p = fx.root.join("web/js/probe.js");
        let etag = etag_of(&p);
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&p, perms).unwrap();
        let r = serve_with_root(&fx.root, "/js/probe.js", &etag);
        assert_eq!(r.status, 304, "ETag 命中必须在 fs::read 之前返回");
    }
}
