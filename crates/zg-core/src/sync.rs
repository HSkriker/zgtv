//! 进程内锁辅助。
//!
//! 本项目的 `Mutex` 绝大多数承载的是可重建的缓存：某个线程在持锁期间 panic 会让锁中毒，
//! 但缓存本身并不因此失效。`lock_ok` 取回内层数据继续服务，避免一次偶发 panic 让
//! 整个接口永久 500（例如行业快照、涨停池、代码表）。

use std::sync::{Mutex, MutexGuard};

pub trait MutexExt<T> {
    /// `lock().unwrap()` 的容错版：中毒后仍取回内层数据。
    fn lock_ok(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 作用域退出（含 panic 展开）时执行收尾动作：后台任务的「进行中」闸门、
/// 在途去重集合都靠它在异常路径上复位，否则一次 panic 会让功能永久卡住。
pub struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    pub fn new(f: F) -> Self {
        Self(Some(f))
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_from_poison() {
        let m = Mutex::new(7);
        let _ = std::panic::catch_unwind(|| {
            let _g = m.lock().unwrap();
            panic!("boom");
        });
        assert_eq!(*m.lock_ok(), 7);
    }

    #[test]
    fn on_drop_runs_on_panic() {
        let hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = hit.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = OnDrop::new(move || h.store(true, std::sync::atomic::Ordering::SeqCst));
            panic!("boom");
        }));
        assert!(r.is_err());
        assert!(hit.load(std::sync::atomic::Ordering::SeqCst));
    }
}
