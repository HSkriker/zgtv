//! 技术指标：后端算，前端只画。
//!
//! 引擎是 `ta-lib` 0.8 官方 Rust 端口（纯 Rust、无 C 链接），与 `zg-market` 跑蜡烛形态的是同一份。
//! 所有指标都支持 `params`（逗号分隔的数字），前端参数面板直接透传。

use std::sync::OnceLock;

use serde_json::{json, Value};
use ta_lib::{Core, MAType};

pub const CALIBER: &str =
    "ta-lib 0.8.3 官方 Rust 端口（纯 Rust，无 C 链接）；MACD 柱按通达信口径取 2×(DIF-DEA)";

fn core() -> &'static Core {
    static CORE: OnceLock<Core> = OnceLock::new();
    CORE.get_or_init(Core::new)
}

struct Bars {
    time: Vec<Value>,
    close: Vec<f64>,
}

fn take(src: &Value) -> Result<Bars, String> {
    let rows = src
        .get("bars")
        .and_then(Value::as_array)
        .ok_or_else(|| "行情为空".to_string())?;
    if rows.len() < 2 {
        return Err("K 线不足 2 根，指标无法计算".to_string());
    }
    let mut b = Bars {
        time: Vec::with_capacity(rows.len()),
        close: Vec::with_capacity(rows.len()),
    };
    for r in rows {
        let num = |k: &str| r.get(k).and_then(Value::as_f64);
        let Some(c) = num("close") else {
            return Err("行情字段缺失".to_string());
        };
        b.time.push(r.get("time").cloned().unwrap_or(Value::Null));
        b.close.push(c);
    }
    Ok(b)
}

/// 只输出有限值（预热期为空），保留 4 位小数以压小 1 分钟档的响应。
fn line(bars: &Bars, r: &ta_lib::OutRange, out: &[f64]) -> Vec<Value> {
    let mut v = Vec::with_capacity(r.count);
    for (j, &x) in out.iter().take(r.count).enumerate() {
        if x.is_finite() {
            v.push(json!([bars.time[r.beg_idx + j], (x * 1e4).round() / 1e4]));
        }
    }
    v
}

fn series(key: &str, values: Vec<Value>) -> Value {
    json!({ "key": key, "values": values })
}

fn ints(s: &str, def: &[i32]) -> Vec<i32> {
    let v: Vec<i32> = s
        .split(',')
        .filter_map(|x| x.trim().parse::<i32>().ok())
        .filter(|x| (1..=1000).contains(x))
        .collect();
    if v.is_empty() { def.to_vec() } else { v }
}

/// `name`：ma / ema / boll / macd / rsi。`params` 是按指标定义逗号分隔的空串或数字。
pub fn payload(
    code: &str,
    period: &str,
    lmt: usize,
    board: bool,
    name: &str,
    params: &str,
) -> Result<Value, String> {
    let src = if board {
        crate::board::payload(code, period, lmt, "")?
    } else if matches!(period, "1" | "5" | "15" | "30" | "60") {
        crate::bars::minute(code, period, lmt, "")?
    } else {
        crate::bars::dwm(code, period, lmt, "")?
    };
    let bars = take(&src)?;
    let n = bars.close.len();
    let end = n - 1;
    let c = core();

    let mut out: Vec<Value> = Vec::new();
    match name {
        "ma" | "ema" => {
            // 默认周期对齐 zg：SMA 族 7 条、EMA 族 4 条
            let ps = if name == "ema" {
                ints(params, &[13, 21, 70, 84])
            } else {
                ints(params, &[5, 10, 20, 40, 60, 120, 250])
            };
            for p in ps {
                let mut buf = vec![0.0f64; n];
                let r = if name == "ma" {
                    c.sma(0, end, &bars.close, p, &mut buf)
                } else {
                    c.ema(0, end, &bars.close, p, &mut buf)
                }
                .map_err(|e| format!("{name} 计算失败: {e:?}"))?;
                out.push(series(&format!("{name}{p}"), line(&bars, &r, &buf)));
            }
        }
        "boll" => {
            // 周期必填（显式留空要报错，不能静默回落到 20）；倍数缺省 2
            let mut toks = params.split(',');
            let p_tok = toks.next().unwrap_or("").trim();
            let m_tok = toks.next().unwrap_or("").trim();
            let p: i32 = if p_tok.is_empty() {
                if params.trim().is_empty() {
                    20
                } else {
                    return Err("BOLL 周期不能为空".to_string());
                }
            } else {
                p_tok
                    .parse::<i32>()
                    .ok()
                    .filter(|v| (2..=1000).contains(v))
                    .ok_or_else(|| format!("BOLL 周期需为 2~1000 的整数: {p_tok}"))?
            };
            let mult = if m_tok.is_empty() {
                2.0
            } else {
                m_tok
                    .parse::<f64>()
                    .ok()
                    .filter(|x| *x > 0.0 && *x < 20.0)
                    .ok_or_else(|| format!("BOLL 倍数需在 0~20 之间: {m_tok}"))?
            };
            let (mut up, mut mid, mut low) =
                (vec![0.0f64; n], vec![0.0f64; n], vec![0.0f64; n]);
            let r = c
                .bbands(0, end, &bars.close, p, mult, mult, MAType::SMA, &mut up, &mut mid, &mut low)
                .map_err(|e| format!("boll 计算失败: {e:?}"))?;
            out.push(series("boll-up", line(&bars, &r, &up)));
            out.push(series("boll-mid", line(&bars, &r, &mid)));
            out.push(series("boll-low", line(&bars, &r, &low)));
        }
        "macd" => {
            let a = ints(params, &[12, 26, 9]);
            let (Some(&f), Some(&s), Some(&sig)) = (a.first(), a.get(1), a.get(2)) else {
                return Err("MACD 需要 3 个参数（快线,慢线,信号），留空会少一条线".to_string());
            };
            if f >= s {
                return Err(format!("MACD 快线必须小于慢线（收到 {f},{s}）"));
            }
            // TA-Lib 的 hist 是 DIF-DEA；通达信/同花顺的 MACD 柱是 2×(DIF-DEA)
            let (mut dif, mut dea, mut hist) =
                (vec![0.0f64; n], vec![0.0f64; n], vec![0.0f64; n]);
            let r = c
                .macd(0, end, &bars.close, f, s, sig, &mut dif, &mut dea, &mut hist)
                .map_err(|e| format!("macd 计算失败: {e:?}"))?;
            let mut hist2 = hist;
            for v in hist2.iter_mut() {
                *v *= 2.0;
            }
            out.push(series("dif", line(&bars, &r, &dif)));
            out.push(series("dea", line(&bars, &r, &dea)));
            out.push(series("hist", line(&bars, &r, &hist2)));
        }
        "rsi" => {
            let ps = ints(params, &[6, 12, 24]);
            for p in ps {
                let mut buf = vec![0.0f64; n];
                let r = c
                    .rsi(0, end, &bars.close, p, &mut buf)
                    .map_err(|e| format!("rsi 计算失败: {e:?}"))?;
                out.push(series(&format!("rsi{p}"), line(&bars, &r, &buf)));
            }
        }
        other => return Err(format!("不支持的指标: {other}（ma/ema/boll/macd/rsi）")),
    }

    Ok(json!({
        "ok": true,
        "name": name,
        "code": code,
        "period": period,
        "board": board,
        "n": n,
        "caliber": CALIBER,
        "series": out,
    }))
}
