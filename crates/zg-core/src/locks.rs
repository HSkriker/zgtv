//! 跨进程文件锁 + 进程内锁 —— 与 `zgcore/locks.py` 同语义。
//!
//! 平台：Windows LockFileEx / POSIX flock，均非阻塞轮询 + 超时（由 fs2 提供）。

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;

#[derive(Debug)]
pub struct FileLock {
    path: String,
    timeout: Duration,
    poll: Duration,
    file: Option<File>,
}

impl FileLock {
    pub fn new(path: impl AsRef<Path>, timeout: f64, poll: f64) -> Self {
        Self {
            path: path.as_ref().to_string_lossy().to_string(),
            timeout: Duration::from_secs_f64(timeout.max(0.0)),
            poll: Duration::from_secs_f64(poll.max(0.001)),
            file: None,
        }
    }

    pub fn acquire(&mut self) -> bool {
        if let Some(parent) = Path::new(&self.path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let file = match OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&self.path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let deadline = Instant::now() + self.timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(_) => {
                    self.file = Some(file);
                    return true;
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(self.poll);
                }
            }
        }
    }

    pub fn release(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// 取得即持锁的 RAII 包装；超时返回 Err。
pub fn file_lock(path: impl AsRef<Path>, timeout: f64, poll: f64) -> Result<FileLock, String> {
    let mut lock = FileLock::new(path, timeout, poll);
    if lock.acquire() {
        Ok(lock)
    } else {
        Err(format!("文件锁超时: {}", lock.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_times_out() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("x.lock");
        let _a = file_lock(&p, 0.2, 0.01).unwrap();
        // 同一进程内 fs2 的独占锁在 Windows 会阻塞第二次获取；POSIX 上 flock 也如此。
        let mut b = FileLock::new(&p, 0.05, 0.01);
        assert!(!b.acquire());
    }
}
