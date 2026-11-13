use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::paths;

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> std::path::PathBuf {
    paths::data_dir().join("drawings.json")
}

/// 读全量画线：文件缺失 = 首次使用（空表）；解析失败 = 损坏，必须让调用方知情——
/// put 侧据此拒绝写入（否则一次保存会把其它 code|period 的画线全部抹掉）。
fn read_all() -> Result<Value, String> {
    let text = match std::fs::read_to_string(file()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(format!("读画线文件失败: {e}")),
    };
    serde_json::from_str(&text)
        .map_err(|e| format!("画线文件损坏，拒绝覆盖（请先手工修复 data/drawings.json）: {e}"))
}

pub fn get(code: &str, period: &str) -> Result<Value, String> {
    let _g = LOCK.lock().map_err(|_| "drawings 锁损坏".to_string())?;
    // get 侧容错降级：损坏也当空表返回（只读无破坏），错误进日志
    let all = match read_all() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[tv] drawings: {e}");
            json!({})
        }
    };
    let key = format!("{code}|{period}");
    Ok(json!({
        "ok": true,
        "rows": all.get(&key).cloned().unwrap_or_else(|| json!([])),
    }))
}

pub fn put(code: &str, period: &str, rows: Value) -> Result<Value, String> {
    if !rows.is_array() {
        return Err("rows 必须是数组".to_string());
    }
    let _g = LOCK.lock().map_err(|_| "drawings 锁损坏".to_string())?;

    // 损坏文件在此整体拒绝写入：绝不静默当空表覆盖
    let mut all = match read_all()? {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    all.insert(format!("{code}|{period}"), rows.clone());

    let text = serde_json::to_string_pretty(&Value::Object(all))
        .map_err(|e| e.to_string())?;
    let path = file();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换 {}: {e}", path.display()))?;

    Ok(json!({"ok": true, "n": rows.as_array().map(|a| a.len()).unwrap_or(0)}))
}
