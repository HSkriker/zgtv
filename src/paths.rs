use std::path::PathBuf;

/// tv 根目录镜像了 zg 的布局（modules/market、data/market），
/// 2026-11-01 起所有 catalog/缓存/行情数据都来自 tv 自身，不再依赖平级 zg 仓库。
///
/// 解析优先级（2026-11-03 起可搬）：
/// 1. 环境变量 `ZG_ROOT` —— 调用方显式指定，最优先；**必须校验它真有该布局**，
///    指错时静默用下去的后果是「服务起来了但页面全 404」，比启动即失败难查得多；
/// 2. **可执行文件所在布局**：从 exe 目录向上找同时含 `modules/` 与 `data/` 的目录。
///    分发包把二进制放在 `<root>/bin/`，源码构建落在 `<root>/target/<profile>/`，
///    两处都能向上找到仓库根；
/// 3. 编译期目录（`env!("CARGO_MANIFEST_DIR")`）——仅作兜底，并打印一声。
///
/// **为什么不能只用编译期目录**：那会把编译机的绝对路径烧进二进制，
/// 拷到别的机器上 `modules/market/industry_catalog.json`、`web/` 静态资源、
/// `data/symbols.json` 全部找不到（界面都打不开）。分发包必须能解压即跑。
pub fn tv_root() -> PathBuf {
    if let Ok(v) = std::env::var("ZG_ROOT") {
        if !v.is_empty() {
            let p = PathBuf::from(&v);
            if has_layout(&p) {
                return p;
            }
            eprintln!("[tv] ZG_ROOT={} 下没有 modules/ 与 data/，忽略该值改用可执行文件布局", v);
        }
    }
    detect_root().unwrap_or_else(|| {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        eprintln!("[tv] 按可执行文件布局找不到数据根，回退编译期目录 {}", p.display());
        p
    })
}

/// 一个目录要能当 tv 根：同时有 `modules/`（行业目录）与 `data/`（行情与缓存）。
fn has_layout(dir: &std::path::Path) -> bool {
    dir.join("modules").is_dir() && dir.join("data").is_dir()
}

/// 从可执行文件位置向上找 tv 根目录（最多 5 层）。
fn detect_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..5 {
        if has_layout(&dir) {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

pub fn data_dir() -> PathBuf {
    let d = tv_root().join("data");
    let _ = std::fs::create_dir_all(&d);
    d
}
