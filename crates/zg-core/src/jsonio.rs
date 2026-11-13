//! JSON 原子读写 —— 与 `zgcore/jsonio.py` 同语义。
//!
//! 同目录 PID+线程+纳秒唯一临时文件 → dump → 可选 fsync → rename。
//! Windows 上目标被占用时 rename 失败：保留旧文件、清临时文件、返回 false。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 唯一临时文件名（同目录，PID+线程+纳秒+序号）。
/// 并发写同一目标时各写各的 tmp，再 rename 覆盖，不会互相截断出半截 JSON。
pub fn tmp_name(path: impl AsRef<Path>) -> String {
    let path = path.as_ref().to_string_lossy().to_string();
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{path}.tmp.{}.{:?}.{}.{seq}",
        std::process::id(),
        std::thread::current().id(),
        nanos
    )
}

/// 读 JSON；缺失/损坏返回 default（损坏时留痕，不抛）。
pub fn read_json(path: impl AsRef<Path>, default: Option<Value>) -> Option<Value> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("[jsonio] 读取失败 {}: {e}", path.display());
                default
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default,
        Err(e) => {
            eprintln!("[jsonio] 读取失败 {}: {e}", path.display());
            default
        }
    }
}

/// 原子写 JSON（紧凑格式），返回是否成功。
pub fn write_json_atomic(path: impl AsRef<Path>, obj: &Value, fsync: bool) -> bool {
    let text = serde_json::to_string(obj).unwrap_or_default();
    write_text_atomic(path, &text, fsync)
}

/// 原子写文本，返回是否成功。
pub fn write_text_atomic(path: impl AsRef<Path>, text: &str, fsync: bool) -> bool {
    let path = path.as_ref().to_string_lossy().to_string();
    let tmp = tmp_name(&path);
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        if fsync {
            f.flush()?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    })();
    match result {
        Ok(_) => true,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            eprintln!("[jsonio] 落盘失败 {path}: {e}");
            false
        }
    }
}

pub fn path_of(p: impl AsRef<Path>) -> PathBuf {
    p.as_ref().to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_roundtrip() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("a/b.json");
        let v = serde_json::json!({"x": 1, "中文": "值"});
        assert!(write_json_atomic(&p, &v, true));
        assert_eq!(read_json(&p, None).unwrap(), v);
        assert_eq!(read_json(td.path().join("missing.json"), None), None);
    }
}
