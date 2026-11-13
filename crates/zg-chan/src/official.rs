//! L1 官方判据入口层 —— `modules/czsc_bridge/signals_official.py` 逐行移植。
//!
//! 判据权威 = czsc 1.0.2 官方注册信号（222 kline + 24 trader）。本层只做三件事：
//!   1. **参数钉死**：显式传上游默认值，避免上游改默认值后结论翻转；
//!   2. **词表解析**：把 `Signal.value`（如 `二买_任意_任意_0`）拆成方向/锚点；
//!   3. **异常降级**：任何问题都变成 `fired=false` + `reason`，不外溢。
//!
//! 与 Python 的四处硬对齐（漏一条就不是"一模一样"）：
//!   - 只消费 `sigs[0]`，其余丢弃（`signals_official.py:282-289`）；
//!   - 参数键白名单由 `param_template` 反解，含 `freq` 之外的全部键；
//!   - `_CONSTRAINTS` 本地拦截 `di>=1`、`w>5`（上游是 `assert!`，直调即 panic）；
//!   - 未命中/降级文案逐字照抄（报告与前端直接展示）。

use std::collections::HashMap;

use czsc_core::analyze::CZSC;
use czsc_signals::registry::SIGNAL_REGISTRY;
use czsc_signals::types::TaCache;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{REGISTRY, REGISTRY_ERROR};

pub const BUY: &str = "买";
pub const SELL: &str = "卖";
pub const WATCH: &str = "观察";

/// 来源标注（`/api/czsc` 的 `sources.bis`/`sources.points` 用）。
/// Python 从运行时 `czsc.__version__` 派生；Rust 侧版本由 `=1.0.2` 钉死，故为常量。
pub const SIGNALS_SOURCE: &str = "czsc1.0.2";

/// 买卖点候选（官方直接给出「几买/几卖」）。
pub const ROLE_POINT: &str = "point";
/// 背驰/力度侧证据，不单独构成买卖点。
pub const ROLE_BC: &str = "divergence";
/// 形态与区间状态，用于门控其它信号。
pub const ROLE_STATE: &str = "state";

/// 钉死参数的值（与 Python `SPECS[...].params` 的 JSON 类型一致）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PVal {
    Int(i64),
    Str(&'static str),
}

impl PVal {
    pub fn to_json(self) -> Value {
        match self {
            PVal::Int(v) => Value::from(v),
            PVal::Str(v) => Value::from(v),
        }
    }
}

/// 一个官方信号的接入规格：判定字段、命中词表、钉死的参数。
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub role: &'static str,
    /// `value.split('_')[field]` 承载判定
    pub field: usize,
    /// (值 → 方向)，词表来自实测全串值域
    pub tokens: &'static [(&'static str, &'static str)],
    /// 显式钉死的上游默认值
    pub params: &'static [(&'static str, PVal)],
    /// 原版 Rust 出处
    pub cite: &'static str,
    /// 词形陷阱等必须随代码走的信息
    pub note: &'static str,
}

impl Spec {
    pub fn param_dict(&self) -> HashMap<String, Value> {
        let mut out = HashMap::new();
        for (k, v) in self.params {
            out.insert((*k).to_string(), v.to_json());
        }
        out
    }

    pub fn direction(&self, token: &str) -> Option<&'static str> {
        self.tokens
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, d)| *d)
    }
}

/// 词表 = `cmp/vocab_probe.json` 在 2736 个真实量前推点上测得的全串值域。
///
/// 每个信号的非命中词形不统一，故逐信号列表，禁止统一写 `startswith('其他')` 当判据：
///   `tas_macd_bc_V221201` 非命中是 `其他_绿柱_任意_0`（第 2 字段另有信息），
///   `cxt_bi_trend_V230824` 的 `横盘` 是有效状态不是非命中。
///
/// 顺序即 Python dict 的插入顺序 —— `hits()` 的输出顺序依赖它，不可重排。
pub static SPECS: &[Spec] = &[
    Spec {
        name: "cxt_first_buy_V221126",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("一买", BUY)],
        params: &[("di", PVal::Int(1))],
        cite: "cxt.rs:1552 → utils/cxt.rs:188-245",
        note: "判据 bc_price ∧ (bc_volume ∨ bc_length)，**量是承重输入**；\
               第 2 字段是样本窗口笔数 n∈{5..21 奇}，不是所锚中枢笔数",
    },
    Spec {
        name: "cxt_first_sell_V221126",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("一卖", SELL)],
        params: &[("di", PVal::Int(1))],
        cite: "cxt.rs:1590",
        note: "同一买的镜像",
    },
    Spec {
        name: "cxt_second_bs_V240524",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("二买", BUY), ("二卖", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("w", PVal::Int(9)),
            ("t", PVal::Int(2)),
        ],
        cite: "cxt.rs:2861",
        note: "纯笔结构，无中枢、无背驰；官方二类主判据（发生率 11.81%）",
    },
    Spec {
        name: "cxt_second_bs_V230320",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("二买", BUY), ("二卖", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("ma_type", PVal::Str("SMA")),
            ("timeperiod", PVal::Int(21)),
        ],
        cite: "cxt.rs:841",
        note: "均线辅助，发生率 34.86% —— 与 V240524 不同源，不可当同一路，只作侧证据",
    },
    Spec {
        name: "cxt_third_bs_V230319",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("三买", BUY), ("三卖", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("ma_type", PVal::Str("SMA")),
            ("timeperiod", PVal::Int(34)),
        ],
        cite: "cxt.rs:1787",
        note: "v1 纯结构（中枢由末 5 笔现场构造），第 2 字段是均线形态不是判据；\
               发生率 13.56%",
    },
    Spec {
        name: "cxt_third_bs_V230318",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("三买", BUY), ("三卖", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("ma_type", PVal::Str("SMA")),
            ("timeperiod", PVal::Int(34)),
        ],
        cite: "cxt.rs:929",
        note: "把均线条件与进 v1 → V230319 的严格子集（实测 7.20%，\
               其三买 94 个恰好等于 V230319 的 `三买_均线新高` 94 个）",
    },
    Spec {
        name: "cxt_third_buy_V230228",
        role: ROLE_POINT,
        field: 0,
        tokens: &[("三买", BUY)],
        params: &[("di", PVal::Int(1))],
        cite: "cxt.rs:1712",
        note: "另一套结构（n+1 笔 + 1.618 扩张）：**只出三买**，笔数取偶数 6~14",
    },
    Spec {
        name: "cxt_double_zs_V230311",
        role: ROLE_BC,
        field: 0,
        tokens: &[("看多", BUY), ("看空", SELL)],
        params: &[("di", PVal::Int(1))],
        cite: "cxt.rs:1034",
        note: "官方「趋势」本体：自 get_zs_seq(末20笔) 取 ≥2 个 ≥2 笔中枢，\
               比 gg/dd 包络 + 时间延伸，**从不看 [zd,zg] 是否重叠**",
    },
    Spec {
        name: "tas_macd_bc_V221201",
        role: ROLE_BC,
        field: 0,
        tokens: &[("底部", BUY), ("顶部", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("n", PVal::Int(3)),
            ("m", PVal::Int(50)),
            ("fastperiod", PVal::Int(12)),
            ("slowperiod", PVal::Int(26)),
            ("signalperiod", PVal::Int(9)),
        ],
        cite: "tas.rs:2672",
        note: "第 1 字段=柱色，与第 0 字段**各自独立填充**（实测存在 底部_红柱 25 例、\
               顶部_绿柱 12 例），故命中谓词只看第 0 字段",
    },
    Spec {
        name: "tas_macd_bc_V230803",
        role: ROLE_BC,
        field: 0,
        tokens: &[("多头", BUY), ("空头", SELL)],
        params: &[],
        cite: "tas.rs:3037",
        note: "无参数；bi_list<3 或 bars_ubi≥7 直接返 其他。\
               **多头/空头 是操作方向语义**（实测 tas.rs:3120 顶部背驰→空头、底背驰→多头），\
               与 zdy_macd_bc_V230422 的 上涨/下跌 不同 —— 那组是趋势方向",
    },
    Spec {
        name: "tas_macd_bc_V230804",
        role: ROLE_BC,
        field: 0,
        tokens: &[("多头", BUY), ("空头", SELL)],
        params: &[("di", PVal::Int(1))],
        cite: "tas.rs:4824",
        note: "bi_list<7 或 bars_ubi≥7 直接返 其他；空头=顶背驰（b5 向上创新高清瘦）",
    },
    Spec {
        name: "tas_macd_bc_V240307",
        role: ROLE_BC,
        field: 0,
        tokens: &[("底背驰", BUY), ("顶背驰", SELL)],
        params: &[("di", PVal::Int(1)), ("n", PVal::Int(20))],
        cite: "tas.rs:3151",
        note: "第 1 字段是第几次背驰（1~7+），可作强度描述",
    },
    Spec {
        name: "tas_macd_bc_ubi_V230804",
        role: ROLE_BC,
        field: 0,
        tokens: &[("多头", BUY), ("空头", SELL)],
        params: &[],
        cite: "tas.rs:4965",
        note: "在途笔（UBI）口径，无参数；盘整背驰的背驰侧证据",
    },
    Spec {
        name: "tas_boll_bc_V221118",
        role: ROLE_BC,
        field: 0,
        tokens: &[("一买", BUY), ("一卖", SELL)],
        params: &[
            ("di", PVal::Int(1)),
            ("n", PVal::Int(3)),
            ("m", PVal::Int(10)),
            ("line", PVal::Int(3)),
            ("timeperiod", PVal::Int(20)),
        ],
        cite: "tas.rs:4075",
        note: "发生率极低（实测 2736 点上 一买5/一卖18）",
    },
    Spec {
        name: "zdy_macd_bc_V230422",
        role: ROLE_BC,
        field: 0,
        tokens: &[("上涨", SELL), ("下跌", BUY)],
        params: &[("di", PVal::Int(1)), ("th", PVal::Int(50))],
        cite: "zdy.rs:352",
        note: "**词形陷阱**：这里 v1 说的是【正在背驰的趋势方向】不是操作方向 —— \
               zdy.rs:469 `上涨` ⇔ bi1 Up ∧ bi2 新高 ∧ MACD 面积萎缩 ∧ bi1_dif>bi2_dif，\
               即上涨趋势的顶背驰 ⇒ 映射到 **卖**；`下跌` 反之映射到买。\
               （对照 tas_macd_bc_* 的 多头/空头 就是操作方向语义，zdy.rs 无文档可查）",
    },
    Spec {
        name: "cxt_range_oscillation_V230620",
        role: ROLE_STATE,
        field: 0,
        tokens: &[
            ("2笔震荡", WATCH),
            ("3笔震荡", WATCH),
            ("4笔震荡", WATCH),
            ("5笔震荡", WATCH),
            ("6笔震荡", WATCH),
            ("7笔震荡", WATCH),
            ("8笔震荡", WATCH),
        ],
        params: &[("di", PVal::Int(1)), ("th", PVal::Int(2))],
        cite: "cxt.rs:2353",
        note: "实测值域 2~8 笔震荡 + 第 1 字段向上/向下；非命中 `其他_其他_任意_0`",
    },
    Spec {
        name: "cxt_bi_trend_V230824",
        role: ROLE_STATE,
        field: 0,
        tokens: &[("向上", WATCH), ("向下", WATCH), ("横盘", WATCH)],
        params: &[
            ("di", PVal::Int(1)),
            ("n", PVal::Int(4)),
            ("th", PVal::Int(2)),
        ],
        cite: "cxt.rs:735",
        note: "**横盘是有效状态不是非命中**，只有 `其他` 才是没结论（实测 3/2736）",
    },
];

/// 官方一/二类主判据 —— 下游（区间套开关、买卖点）只认这三个，S5 用。
pub const PRIMARY_POINT_NAMES: [&str; 3] = [
    "cxt_first_buy_V221126",
    "cxt_first_sell_V221126",
    "cxt_second_bs_V240524",
];

/// 一次官方信号调用的定格结果。`raw` 原样保留，报告可直接引用。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SignalHit {
    pub name: String,
    /// `Signal.key`，如 `日线_D1W9T2_第二买卖点V240524`（含生效参数）
    pub key: String,
    /// `Signal.value`，如 `二买_任意_任意_0`
    pub raw: String,
    /// 判定词 = `value.split('_')[spec.field]`，如 `二买`/`三卖`/`横盘`
    pub category: String,
    /// 买 / 卖 / 观察 / ""（未命中）
    pub direction: String,
    /// 值域里的 "N笔" 字段；官方一类 = **样本窗口笔数**
    pub anchor_bi: Option<i64>,
    pub fired: bool,
    pub role: String,
    /// 未命中/降级原因，`fired=false` 时必填
    pub reason: String,
}

/// 名字不在 SPECS。
pub fn spec_of(name: &str) -> Result<&'static Spec, String> {
    SPECS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("{name} 未接入（SPECS 里没有）"))
}

/// Python `sorted(set)` 的列表 repr（`['a', 'b']`）—— 用于错误文案。
fn py_sorted_list(items: &[&str]) -> String {
    let mut v: Vec<&str> = items.to_vec();
    v.sort_unstable();
    let body: Vec<String> = v.iter().map(|s| zg_core::py_repr(s)).collect();
    format!("[{}]", body.join(", "))
}

/// 上游硬约束：违反即 Rust panic（进程级），必须在本地拦掉。
/// 依据 `czsc-signals/src/utils/sig.rs:19`（di>=1）、`cxt.rs:2865`（w>5）。
fn constraint_ok(key: &str, value: &Value) -> bool {
    match key {
        "di" => value.as_i64().map(|v| v >= 1).unwrap_or(false),
        "w" => value.as_i64().map(|v| v > 5).unwrap_or(false),
        _ => true,
    }
}

/// 规格自检：名字在不在注册表、是不是 kline、参数键对不对得上上游模板。
///
/// 返回 `None` 表示没问题；否则返回一句可直接写进报告的原因。
/// 配置校验放启动段 —— 等它变成 PanicException 就晚了。
pub fn check_spec(name: &str) -> Option<String> {
    if let Some(err) = REGISTRY_ERROR {
        return Some(format!("czsc 注册表不可用（{err}），拒绝调用官方信号"));
    }
    let Some(s) = SPECS.iter().find(|s| s.name == name) else {
        return Some(format!("{name} 不在 SPECS"));
    };
    let Some(row) = REGISTRY.get(name) else {
        return Some(format!("{name} 不在 czsc 注册表（版本不符？）"));
    };
    if row.category != "kline" {
        return Some(format!(
            "{name} 是 {} 类信号，call_signal 直调会抛 TypeError",
            row.category
        ));
    }
    let ours: Vec<&str> = s.params.iter().map(|(k, _)| *k).collect();
    let theirs: Vec<&str> = row.param_keys.iter().map(|s| s.as_str()).collect();

    let bad: Vec<&str> = ours
        .iter()
        .copied()
        .filter(|k| !theirs.contains(k))
        .collect();
    if !bad.is_empty() {
        return Some(format!(
            "{name} 参数键 {} 不在上游模板 {} 里 —— 未知键会被静默忽略，等于没钉参数",
            py_sorted_list(&bad),
            row.param_template
        ));
    }
    let missing: Vec<&str> = theirs
        .iter()
        .copied()
        .filter(|k| !ours.contains(k))
        .collect();
    if !missing.is_empty() {
        return Some(format!(
            "{name} 参数键不全，漏了 {}（会静默吃上游默认值）",
            py_sorted_list(&missing)
        ));
    }
    for (k, v) in s.params {
        let value = v.to_json();
        if !constraint_ok(k, &value) {
            return Some(format!(
                "{name} 参数 {k}={} 违反上游硬约束，直调会 Rust panic",
                py_repr_value(&value)
            ));
        }
    }
    None
}

/// Python `repr()` 的标量近似（约束检查只涉及 int）。
fn py_repr_value(v: &Value) -> String {
    match v {
        Value::String(s) => zg_core::py_repr(s),
        other => other.to_string(),
    }
}

/// 所有已接入规格的启动段体检。返回问题列表，空列表 = 干净。
pub fn selfcheck() -> Vec<String> {
    SPECS.iter().filter_map(|s| check_spec(s.name)).collect()
}

/// 表驱动解析 value 串。**纯函数，不碰 czsc，可脱线单测。**
pub fn parse(name: &str, value: &str) -> Result<SignalHit, String> {
    let s = spec_of(name)?;
    let fields: Vec<&str> = value.split('_').collect();
    let token = fields.get(s.field).copied().unwrap_or("");
    let direction = s.direction(token);
    let anchor = anchor_of(&fields);
    let fired = direction.is_some();
    Ok(SignalHit {
        name: name.to_string(),
        key: String::new(),
        raw: value.to_string(),
        category: token.to_string(),
        direction: direction.unwrap_or("").to_string(),
        anchor_bi: anchor,
        fired,
        role: s.role.to_string(),
        reason: if fired {
            String::new()
        } else {
            format!("未命中（{token} 不在 {name} 的命中词表）")
        },
    })
}

/// `_ANCHOR_RE = re.compile(r"^(\d+)笔")` —— 前缀匹配，兼收 `7笔` 与 `2笔震荡`。
fn anchor_of(fields: &[&str]) -> Option<i64> {
    for f in fields {
        let digits: String = f.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && f[digits.len()..].starts_with('笔') {
            return digits.parse::<i64>().ok();
        }
    }
    None
}

/// 唯一的 signal 调用出口。任何问题都降级为 `fired=false` + `reason`，不外溢。
///
/// Python 侧 catch 的是 `BaseException`（Rust panic 经 PyO3 抛 PanicException，
/// 不是 Exception 子类）。Rust 侧用 `catch_unwind` 提供同等的进程级隔离；
/// 正常路径（参数已钉死 + 约束已本地拦截）永不触发。
pub fn call(name: &str, c: &CZSC) -> SignalHit {
    let Ok(s) = spec_of(name) else {
        return SignalHit {
            name: name.to_string(),
            reason: format!("{name} 未接入（SPECS 里没有）"),
            ..Default::default()
        };
    };
    if let Some(problem) = check_spec(name) {
        return SignalHit {
            name: name.to_string(),
            role: s.role.to_string(),
            reason: problem,
            ..Default::default()
        };
    }
    let Some(meta) = SIGNAL_REGISTRY.get(name) else {
        return SignalHit {
            name: name.to_string(),
            role: s.role.to_string(),
            reason: format!("KeyError: {name}"),
            ..Default::default()
        };
    };
    // Python 侧每次 call_signal 都新建 TaCache；逐信号独立，不得复用。
    let params = s.param_dict();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cache = TaCache::new();
        (meta.func)(c, &params, &mut cache)
    }));
    match outcome {
        Err(payload) => SignalHit {
            name: name.to_string(),
            role: s.role.to_string(),
            reason: format!("PanicException: {}", panic_message(&payload)),
            ..Default::default()
        },
        Ok(sigs) => {
            let Some(first) = sigs.first() else {
                return SignalHit {
                    name: name.to_string(),
                    role: s.role.to_string(),
                    reason: "上游返回空列表".to_string(),
                    ..Default::default()
                };
            };
            let Ok(hit) = parse(name, &first.value()) else {
                return SignalHit {
                    name: name.to_string(),
                    role: s.role.to_string(),
                    reason: format!("PanicException: parse failed for {name}"),
                    ..Default::default()
                };
            };
            SignalHit {
                key: first.key(),
                ..hit
            }
        }
    }
}

/// panic payload → 文案（供 method 层复用，模拟 Python 异常消息）。
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    panic_message(payload)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// 批量调用。`names=None` = 全部已接入信号（顺序同 `SPECS`）。
pub fn hits(c: &CZSC, names: Option<&[&str]>) -> Vec<SignalHit> {
    match names {
        Some(list) => list.iter().map(|n| call(n, c)).collect(),
        None => SPECS.iter().map(|s| call(s.name, c)).collect(),
    }
}

/// 只回命中的那些。
pub fn fired(c: &CZSC, names: Option<&[&str]>) -> Vec<SignalHit> {
    hits(c, names).into_iter().filter(|h| h.fired).collect()
}

/// 官方一/二类主判据命中 —— **区间套下钻开关的唯一依据**（S5）。
pub fn primary_fired(c: &CZSC, pre: Option<&[SignalHit]>) -> Vec<SignalHit> {
    let owned;
    let list: &[SignalHit] = match pre {
        Some(v) => v,
        None => {
            owned = hits(c, None);
            &owned
        }
    };
    list.iter()
        .filter(|h| {
            h.fired
                && PRIMARY_POINT_NAMES.contains(&h.name.as_str())
                && (h.direction == BUY || h.direction == SELL)
        })
        .cloned()
        .collect()
}

/// 证明「钉死的参数 == 上游生效的默认值」。
///
/// 靠 `Signal.key`：k2 由上游用**实际生效的**参数格式化（如 `D1#SMA#34`），
/// 所以「显式传参」与「传 None」的 key 逐字相等，即钉参与默认值同值。
pub fn verify_defaults(c: &CZSC) -> Vec<String> {
    let mut bad = Vec::new();
    for s in SPECS {
        if check_spec(s.name).is_some() {
            continue;
        }
        let Some(meta) = SIGNAL_REGISTRY.get(s.name) else {
            continue;
        };
        let pinned = {
            let params = s.param_dict();
            let mut cache = TaCache::new();
            (meta.func)(c, &params, &mut cache)
        };
        let default = {
            let params: HashMap<String, Value> = HashMap::new();
            let mut cache = TaCache::new();
            (meta.func)(c, &params, &mut cache)
        };
        let kp = pinned.first().map(|s| s.key()).unwrap_or_else(|| "<EMPTY>".to_string());
        let kd = default.first().map(|s| s.key()).unwrap_or_else(|| "<EMPTY>".to_string());
        if kp != kd {
            bad.push(format!("{}: 钉参 key={} ≠ 默认 key={}", s.name, kp, kd));
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_count_and_selfcheck_clean() {
        assert_eq!(SPECS.len(), 17);
        assert!(selfcheck().is_empty(), "规格自检失败: {:?}", selfcheck());
    }

    #[test]
    fn parse_table_tokens() {
        let h = parse("cxt_second_bs_V240524", "二买_任意_任意_0").unwrap();
        assert!(h.fired);
        assert_eq!(h.direction, BUY);
        assert_eq!(h.category, "二买");
        assert_eq!(h.reason, "");

        let h = parse("tas_macd_bc_V221201", "其他_绿柱_任意_0").unwrap();
        assert!(!h.fired);
        assert_eq!(h.direction, "");
        assert_eq!(h.reason, "未命中（其他 不在 tas_macd_bc_V221201 的命中词表）");

        // 上涨 → 卖（趋势方向语义，词形陷阱）
        let h = parse("zdy_macd_bc_V230422", "上涨_任意_任意_0").unwrap();
        assert_eq!(h.direction, SELL);
    }

    #[test]
    fn parse_anchor_prefix() {
        let h = parse("cxt_first_buy_V221126", "一买_7笔_任意_0").unwrap();
        assert_eq!(h.anchor_bi, Some(7));
        let h = parse("cxt_range_oscillation_V230620", "3笔震荡_向下_任意_0").unwrap();
        assert_eq!(h.anchor_bi, Some(3));
        let h = parse("cxt_second_bs_V240524", "二买_任意_任意_0").unwrap();
        assert_eq!(h.anchor_bi, None);
    }

    #[test]
    fn unknown_signal_reason() {
        let h = call("nope_V000000", &dummy_czsc());
        assert!(!h.fired);
        assert_eq!(h.reason, "nope_V000000 未接入（SPECS 里没有）");
    }

    #[test]
    fn check_spec_order_and_clean() {
        // Python 的顺序：先查 SPECS 再查注册表 —— trader 类名字不在 SPECS，
        // 因此报的是「不在 SPECS」，不是 TypeError 分支（该分支要求先入 SPECS）。
        let trader = crate::registry::REGISTRY
            .values()
            .find(|r| r.category == "trader")
            .unwrap();
        assert_eq!(
            check_spec(&trader.name),
            Some(format!("{} 不在 SPECS", trader.name))
        );
        assert_eq!(
            check_spec("not_a_signal"),
            Some("not_a_signal 不在 SPECS".to_string())
        );
        assert_eq!(check_spec("cxt_first_buy_V221126"), None);
    }

    fn dummy_czsc() -> CZSC {
        use chrono::{TimeZone, Utc};
        use czsc_core::objects::bar::RawBar;
        use czsc_core::objects::freq::Freq;

        let bars: Vec<RawBar> = (0..80)
            .map(|i| {
                let base = 10.0 + ((i as f64) * 0.7).sin() * 2.0;
                RawBar {
                    symbol: "test".into(),
                    dt: Utc.timestamp_opt(1_600_000_000 + i * 86_400, 0).unwrap(),
                    freq: Freq::D,
                    id: i as i32,
                    open: base,
                    close: base + 0.1,
                    high: base + 0.3,
                    low: base - 0.3,
                    vol: 1000.0,
                    amount: 10_000.0,
                }
            })
            .collect();
        CZSC::new(bars, 60, 6)
    }

    #[test]
    fn all_specs_callable_without_panic() {
        let c = dummy_czsc();
        let all = hits(&c, None);
        assert_eq!(all.len(), SPECS.len());
        for h in &all {
            // fired=false ⇒ 必须有原因；fired=true ⇒ 原因为空（Python 同款不变式）
            if h.fired {
                assert_eq!(h.reason, "", "{} 命中却带原因", h.name);
                assert!(!h.key.is_empty(), "{} 命中却无 key", h.name);
                assert!(!h.raw.is_empty(), "{} 命中却无 raw", h.name);
            } else {
                assert!(!h.reason.is_empty(), "{} 未命中却没有原因", h.name);
                // 注意：跑过上游但未命中时 raw/key 仍保留（Python `call` 同款），
                // 只有规格自检失败/注册表缺失时才是空串。
            }
        }
        // verify_defaults 必须干净：钉参 == 上游默认值
        assert!(verify_defaults(&c).is_empty(), "{:?}", verify_defaults(&c));
    }
}
