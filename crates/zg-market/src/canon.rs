//! 规范序列化 —— 等价 Python `json.dumps(obj, ensure_ascii=False, sort_keys=True,
//! separators=(",", ":"))`（eq/canon.py 的 `canon.dumps`）。
//!
//! 前提：serde_json 已开 `preserve_order`（键序保持构建顺序）；浮点走 ryu
//! 最短往返表示，与 Python float repr 同规则。NaN/Infinity 在夹具中不出现
//! （serde_json 会拒绝序列化，与 Python 默认 allow_nan 行为不同，仅用于对表）。

use serde_json::{Map, Value};

pub fn dumps(v: &Value) -> String {
    let mut buf = String::new();
    write_value(&mut buf, v);
    buf
}

fn write_value(buf: &mut String, v: &Value) {
    match v {
        Value::Null => buf.push_str("null"),
        Value::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => buf.push_str(&n.to_string()),
        Value::String(s) => write_string(buf, s),
        Value::Array(items) => {
            buf.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                write_value(buf, item);
            }
            buf.push(']');
        }
        Value::Object(map) => {
            let mut sorted: Vec<(&String, &Value)> = map.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            buf.push('{');
            for (i, (k, val)) in sorted.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                write_string(buf, k);
                buf.push(':');
                write_value(buf, val);
            }
            buf.push('}');
        }
    }
}

fn write_string(buf: &mut String, s: &str) {
    buf.push('"');
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            '\u{08}' => buf.push_str("\\b"),
            '\u{0c}' => buf.push_str("\\f"),
            c if (c as u32) < 0x20 => buf.push_str(&format!("\\u{:04x}", c as u32)),
            c => buf.push(c),
        }
    }
    buf.push('"');
}

/// 递归排序对象键（便于调试/展示时与 canon.dumps 同构）。
pub fn sort_keys(v: &mut Value) {
    match v {
        Value::Object(map) => {
            sort_map_keys(map);
            for val in map.values_mut() {
                sort_keys(val);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

fn sort_map_keys(map: &mut Map<String, Value>) {
    // serde_json preserve_order 下 Map 为 IndexMap；取键排序后重建成新 Map 保序。
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    let mut new_map = Map::new();
    for k in keys {
        if let Some(v) = map.remove(&k) {
            new_map.insert(k, v);
        }
    }
    *map = new_map;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_compact_roundtrip() {
        let v = serde_json::json!({
            "b": [3.0, 2.5, {"y": null, "x": true}],
            "a": {"k2": "v2", "k1": "v1"},
            "s": "中文\"引号"
        });
        assert_eq!(
            dumps(&v),
            "{\"a\":{\"k1\":\"v1\",\"k2\":\"v2\"},\"b\":[3.0,2.5,{\"x\":true,\"y\":null}],\
             \"s\":\"中文\\\"引号\"}"
        );
    }

    #[test]
    fn float_repr_shortest() {
        assert_eq!(dumps(&serde_json::json!(0.30000000000000004)), "0.30000000000000004");
        assert_eq!(dumps(&serde_json::json!(27.2)), "27.2");
        assert_eq!(dumps(&serde_json::json!(1356.9200439453125)), "1356.9200439453125");
        assert_eq!(dumps(&serde_json::json!(1.0)), "1.0");
        assert_eq!(dumps(&serde_json::json!(0.0005)), "0.0005");
    }
}
