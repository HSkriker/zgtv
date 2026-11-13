use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 满深度行情（120 交易日的 1 分钟 ≈ 10MB/条）不能无限堆在内存里：
/// 条数与总字节双上限，按最近使用淘汰。
const MAX_ENTRIES: usize = 48;
const MAX_BYTES: usize = 128 * 1024 * 1024;

struct Entry {
    stamp: String,
    etag: String,
    body: Bytes,
    seen: u64,
}

struct Inner {
    map: HashMap<String, Entry>,
    building: HashSet<String>,
    clock: u64,
}

pub struct Cache {
    inner: Mutex<Inner>,
}

impl Cache {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner { map: HashMap::new(), building: HashSet::new(), clock: 0 }) }
    }

    /// stamp 未变 → 直接返回缓存；变了 → 重建。返回 (etag, body, 是否命中)。
    /// 同键并发做单飞：只有认领到 building 的线程构建，其余短轮询等结果落库，
    /// 避免分钟档（单次秒级）的惊群重复计算。构建 panic 由守卫清标志，等待方 15s 后接管。
    pub fn get_or_build<F>(&self, key: &str, stamp: &str, build: F)
        -> Result<(String, Bytes, bool), String>
    where
        F: FnOnce() -> Result<Vec<u8>, String>,
    {
        let mut waited = 0usize;
        loop {
            {
                let mut g = self.inner.lock().map_err(|_| "cache 互斥锁损坏".to_string())?;
                g.clock += 1;
                let now = g.clock;
                if let Some(e) = g.map.get_mut(key) {
                    if e.stamp == stamp {
                        e.seen = now;
                        return Ok((e.etag.clone(), e.body.clone(), true));
                    }
                }
                if g.building.insert(key.to_string()) {
                    break; // 本线程认领构建
                }
            }
            waited += 1;
            if waited > 750 {
                // 15s 仍无结果（构建方异常残留）：移除旧认领并接管
                let mut g = self.inner.lock().map_err(|_| "cache 互斥锁损坏".to_string())?;
                g.building.remove(key);
                g.building.insert(key.to_string());
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        struct Building<'a> {
            inner: &'a Mutex<Inner>,
            key: String,
        }
        impl Drop for Building<'_> {
            fn drop(&mut self) {
                // 构建期间不持锁，正常路径与 panic 栈展开都能清掉认领
                if let Ok(mut g) = self.inner.lock() {
                    g.building.remove(&self.key);
                }
            }
        }
        let _g = Building { inner: &self.inner, key: key.to_string() };

        // 建缓存要读盘/算指标，不能持锁；同键并发时只有认领方走到这里
        let built = build();
        let mut g = self.inner.lock().map_err(|_| "cache 互斥锁损坏".to_string())?;
        g.clock += 1;
        let seen = g.clock;
        match built {
            Ok(body) => {
                // key 里已含 code/period/lmt，必须进 ETag
                let etag = format!("\"{}-{}\"", key, stamp);
                // 落库时转 Bytes：命中路径的 clone 只是引用计数 +1，不再复制几 MB 正文
                let body = Bytes::from(body);
                let ret = body.clone();
                g.map.insert(
                    key.to_string(),
                    Entry { stamp: stamp.to_string(), etag: etag.clone(), body, seen },
                );
                evict(&mut g.map);
                Ok((etag, ret, false))
            }
            Err(e) => Err(e),
        }
    }
}

/// 超限时反复淘汰最久未用的一条，直到条数与字节都回到上限内。
fn evict(map: &mut HashMap<String, Entry>) {
    let total = |m: &HashMap<String, Entry>| m.values().map(|e| e.body.len()).sum::<usize>();
    while map.len() > MAX_ENTRIES || total(map) > MAX_BYTES {
        let Some(oldest) =
            map.iter().min_by_key(|(_, e)| e.seen).map(|(k, _)| k.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
}

static BARS: OnceLock<Cache> = OnceLock::new();
static CHAN: OnceLock<Cache> = OnceLock::new();
static IND: OnceLock<Cache> = OnceLock::new();

pub fn bars() -> &'static Cache { BARS.get_or_init(Cache::new) }
pub fn chan() -> &'static Cache { CHAN.get_or_init(Cache::new) }
pub fn ind() -> &'static Cache { IND.get_or_init(Cache::new) }

/// 文件指纹：mtime(ms)-字节数；文件不存在返回 "missing"
pub fn file_stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(md) => {
            let ms = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}-{}", ms, md.len())
        }
        Err(_) => "missing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn hit_returns_same_bytes_without_rebuild() {
        let c = Cache::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let build = |calls: Arc<AtomicUsize>| {
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"1234567890".to_vec())
            }
        };

        let (_, b1, hit1) = c.get_or_build("k", "s", build(calls.clone())).unwrap();
        assert!(!hit1);
        let (_, b2, hit2) = c.get_or_build("k", "s", build(calls.clone())).unwrap();
        assert!(hit2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(b1.as_ptr(), b2.as_ptr(), "命中必须是零拷贝（同指针）");
    }
}
