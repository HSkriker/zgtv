//! 个股标签：L2 行业（industry_catalog.json）+ 概念题材（MAC 在线源，回退本地 block_gn.dat）。
//!
//! 行业部分全来自本地静态文件，进程内 OnceLock 建反查表；无磁盘缓存、无网络请求。
//!
//! L2 解析规则：members[].industry 是 "X"+层级码（X2102=L2 酿酒、X210205=L3 白酒）；
//! 取前 4 位数字即 L2 ref，直接命中的 813 只、从 L3 前缀解析的 4774 只，0 缺口（对拍过）。
//!
//! 概念题材有两个来源，按优先级：
//! 1. **MAC 在线源**（`concepts.rs`）：通达信扩展行情的板块数据，是**当前的**题材口径
//!    （本地 block_gn.dat 那份停在 2022-08-24，没有 AI智能体 / DeepSeek概念 这类近年题材）；
//! 2. **本地 block_gn.dat**：通达信客户端的板块文件（Registry 1.0：384 字节头 + u16 板块数 +
//!    每板块[9 字节 GBK 名 + u16 股数 + u16 类型 + 2800 字节股票区(7 字节/股)]），
//!    只在 MAC 不可用时兜底，且**不冒充新数据**（响应里会标来源）。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::concepts;
use crate::paths;

/// 非概念题材的板块（通达信88 是自编指数成分，不是概念），展示时剔除
const EXCLUDED_BLOCKS: &[&str] = &["通达信88"];

const BLOCK_HEADER: usize = 384;
const BLOCK_STOCK_AREA: usize = 2800;
const CODE_REC: usize = 7;

/// 股票代码 → L2 行业 ref（"X2102" 式），节点名查 board::sectors()
fn industry_map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        let p = paths::tv_root()
            .join("modules")
            .join("market")
            .join("industry_catalog.json");
        let Ok(text) = std::fs::read_to_string(p) else { return m };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { return m };
        let Some(members) = j.get("members").and_then(|x| x.as_array()) else { return m };
        for mm in members {
            let Some(c) = mm.get("code").and_then(|x| x.as_str()) else { continue };
            let Some(ind) = mm.get("industry").and_then(|x| x.as_str()) else { continue };
            let d = ind.trim_start_matches('X');
            if !d.is_ascii() || d.len() < 4 {
                continue;
            }
            m.insert(c.to_string(), format!("X{}", &d[..4]));
        }
        m
    })
}

/// 股票代码 → 概念板块名列表（本地 block_gn.dat 兜底用；文件顺序，同名块去重）
fn legacy_concepts_map() -> &'static HashMap<String, Vec<String>> {
    static MAP: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        let dir = zg_core::paths::get().gbbq_file.parent().map(|d| d.to_path_buf());
        let Some(dir) = dir else { return m };
        let p = dir.join("block_gn.dat");
        let Ok(data) = std::fs::read(&p) else {
            eprintln!("[tv] 概念文件缺失: {}（概念列将为空）", p.display());
            return m;
        };
        if data.len() < BLOCK_HEADER + 2 {
            return m;
        }
        let n = u16::from_le_bytes([data[BLOCK_HEADER], data[BLOCK_HEADER + 1]]) as usize;
        let mut pos = BLOCK_HEADER + 2;
        for _ in 0..n {
            if pos + 9 + 4 > data.len() {
                break;
            }
            let (name, _, _) = encoding_rs::GBK.decode(&data[pos..pos + 9]);
            let name = name.trim_end_matches('\0').to_string();
            let cnt = u16::from_le_bytes([data[pos + 9], data[pos + 10]]) as usize;
            let bt = u16::from_le_bytes([data[pos + 11], data[pos + 12]]);
            let begin = pos + 13;
            pos = begin + BLOCK_STOCK_AREA;
            if pos > data.len() {
                break;
            }
            if bt != 2 || EXCLUDED_BLOCKS.contains(&name.as_str()) {
                continue;
            }
            let end = (begin + cnt * CODE_REC).min(begin + BLOCK_STOCK_AREA).min(data.len());
            let mut p2 = begin;
            while p2 + CODE_REC <= end {
                let c = String::from_utf8_lossy(&data[p2..p2 + 6])
                    .trim_end_matches('\0')
                    .to_string();
                if !c.is_empty() {
                    let v = m.entry(c).or_default();
                    if !v.contains(&name) {
                        v.push(name.clone());
                    }
                }
                p2 += CODE_REC;
            }
        }
        m
    })
}

/// GET /api/stock-tags?code=600519 → L2 行业 + 概念题材（个股之外代码两字段皆空）
pub fn payload(code: &str) -> Value {
    let code = code.trim();
    let industry = industry_map()
        .get(code)
        .and_then(|r| crate::board::sector(r))
        .map(|(c, n)| json!({"code": c, "name": n}));

    // 概念：MAC 在线源优先（当前口径），失败才回退本地 block_gn.dat（2022 旧数据）
    let (concepts_list, regions, styles, source, fetched_at) = match concepts::for_stock(code) {
        Ok(t) => (t.concepts, t.regions, t.styles, t.source, t.fetched_at),
        Err(e) => {
            let legacy = legacy_concepts_map().get(code).cloned().unwrap_or_default();
            (
                legacy,
                Vec::new(),
                Vec::new(),
                format!("tdx_block_gn_legacy（MAC 不可用：{e}）"),
                String::new(),
            )
        }
    };

    json!({
        "ok": true,
        "code": code,
        "industry": industry,
        "concepts": concepts_list,
        "regions": regions,
        "styles": styles,
        "concept_source": source,
        "concept_fetched_at": fetched_at,
        "note": "L2=industry_catalog.json 研究行业二级；概念=MAC 在线板块数据（回退=本地 block_gn.dat）",
    })
}
