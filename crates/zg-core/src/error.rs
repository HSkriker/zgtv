//! 错误类型 —— 文案与 Python `zgcore.BridgeError` 逐字一致（对表按字符串比）。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZgError {
    /// 数据层可读错误（Python 的 `BridgeError`）
    Bridge(String),
    /// 前复权锚点断言（Python 的 `AssertionError`）
    Assertion(String),
    /// IO / 解析等底层错误（保留原始文案）
    Io(String),
}

impl ZgError {
    pub fn bridge(msg: impl Into<String>) -> Self {
        ZgError::Bridge(msg.into())
    }

    pub fn assertion(msg: impl Into<String>) -> Self {
        ZgError::Assertion(msg.into())
    }

    /// 与 Python `str(e)` 对齐。
    pub fn message(&self) -> &str {
        match self {
            ZgError::Bridge(m) | ZgError::Assertion(m) | ZgError::Io(m) => m,
        }
    }

    /// 与 Python `type(e).__name__` 对齐。
    pub fn kind(&self) -> &'static str {
        match self {
            ZgError::Bridge(_) => "BridgeError",
            ZgError::Assertion(_) => "AssertionError",
            ZgError::Io(_) => "OSError",
        }
    }
}

impl fmt::Display for ZgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ZgError {}

impl From<std::io::Error> for ZgError {
    fn from(e: std::io::Error) -> Self {
        ZgError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for ZgError {
    fn from(e: serde_json::Error) -> Self {
        ZgError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ZgError>;

/// Python `%r`（str）的最小等价实现：ASCII 用单引号；含单引号则用双引号。
/// 本项目的参数都是市场/周期/代码这类短 ASCII 串。
pub fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{escaped}'")
    }
}
