//! 测试用网络注入点（门 1 录播对表）：各取数入口先查 stub，命中则返回录制文本，
//! 否则走真实网络。生产代码路径不受影响（stub 默认未设置）。

use std::sync::{Mutex, OnceLock};

use zg_core::sync::MutexExt;

type Stub = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

static STUB: OnceLock<Mutex<Option<Stub>>> = OnceLock::new();

fn stub() -> &'static Mutex<Option<Stub>> {
    STUB.get_or_init(|| Mutex::new(None))
}

/// 测试用：设置 URL/键 → 录制文本的桩函数。串行用例间需 clear。
pub fn set(f: Stub) {
    *stub().lock_ok() = Some(f);
}

pub fn clear() {
    *stub().lock_ok() = None;
}

/// 各取数入口调用：命中返回 Some(文本)。
pub fn get(key: &str) -> Option<String> {
    let g = stub().lock_ok();
    g.as_ref().and_then(|f| f(key))
}
