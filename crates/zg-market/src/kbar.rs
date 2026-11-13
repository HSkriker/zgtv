//! 单根 K 线（Python kline 源的 dict 形态），键序对齐 Python 构造顺序。

use serde::Serialize;
use serde_json::{Map, Value};

/// date/open/close/high/low/vol/amount（kline 源构造顺序）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KBar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub vol: f64,
    pub amount: f64,
}

impl KBar {
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("date".into(), Value::String(self.date.clone()));
        m.insert("open".into(), json_num(self.open));
        m.insert("close".into(), json_num(self.close));
        m.insert("high".into(), json_num(self.high));
        m.insert("low".into(), json_num(self.low));
        m.insert("vol".into(), json_num(self.vol));
        m.insert("amount".into(), json_num(self.amount));
        Value::Object(m)
    }

    /// chart_bars 形态的键序（date/open/high/low/close/vol/amount，Python zgcore api 输出）。
    pub fn to_value_ohlc(&self) -> Value {
        let mut m = Map::new();
        m.insert("date".into(), Value::String(self.date.clone()));
        m.insert("open".into(), json_num(self.open));
        m.insert("high".into(), json_num(self.high));
        m.insert("low".into(), json_num(self.low));
        m.insert("close".into(), json_num(self.close));
        m.insert("vol".into(), json_num(self.vol));
        m.insert("amount".into(), json_num(self.amount));
        Value::Object(m)
    }

    /// 从 Python dict 解析（值可能为缺省/None/字符串）。
    pub fn from_value(v: &Value) -> Option<KBar> {
        let o = v.as_object()?;
        Some(KBar {
            date: o.get("date").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            open: num(o.get("open")),
            high: num(o.get("high")),
            low: num(o.get("low")),
            close: num(o.get("close")),
            vol: num(o.get("vol")),
            amount: num(o.get("amount")),
        })
    }
}

pub fn json_num(x: f64) -> Value {
    // Python 语义：整数值不带小数点（repr 最短往返）；serde_json 数字默认即如此。
    Value::from(x)
}

/// Python `float(x)` 的宽松解析（None/缺省 → 0.0；字符串/数字皆可）。
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        None | Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        Some(_) => 0.0,
    }
}

/// Python `float(x)` 的异常语义：非法值返回 Err（调用方按 Python 的
/// `except (ValueError, TypeError): continue` 跳过整条记录，不落入 0.0）。
/// 与 `num` 的区别：None/Null/类型不匹配 → Err；字符串带前后空白 → 正常解析。
#[allow(clippy::result_unit_err)]
pub fn py_float(v: Option<&Value>) -> Result<f64, ()> {
    match v {
        None | Some(Value::Null) => Err(()),
        Some(Value::Number(n)) => n.as_f64().ok_or(()),
        Some(Value::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        Some(Value::String(s)) => s.trim().parse::<f64>().map_err(|_| ()),
        Some(_) => Err(()),
    }
}
