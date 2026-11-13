//! 通达信研究行业目录生成器 —— `tools/build_industry_catalog.py` 的 Rust 移植。
//!
//! 只读本地通达信 `hq_cache`，生成/更新 `modules/market/industry_catalog.json`：
//! - `tdxzs3.cfg` 里 `type=12` 的行 → 30 一级 / 128 二级 / 309 三级（X10 / X1001 / X100101）；
//! - `tdxhy.cfg` 第 6 列 → 逐股 X 码（B 股与个别新股无归属，源文件里就是空）。
//!
//! 名字优先继承旧 catalog（`--out` 已存在时读取），缺失回落为代码——`tdxhy.cfg`
//! 本身不带股票名，没有旧文件时整表都会是代码。
//!
//! **硬校验**：层级必须是 30/128/309、每个二级必须有成分、二级名不得重复。
//! 任一条不满足就退出（不写文件），与 Python 版一致。
//!
//! 输出格式也按 Python `json.dumps(..., ensure_ascii=False)` 的口径手写：
//! `", "` / `": "` 分隔、字段顺序固定、无末尾换行。这样通达信每晚更新分类文件后
//! 重生成的 catalog 在 git 里只有真实变化，而不是整文件格式化 diff。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

struct Node {
    name: String,
    code: String,
    sub_type: i64,
    ref_: String,
}

struct Member {
    code: String,
    market: i64,
    industry: String,
    name: String,
}

/// 生成 catalog。返回给终端打印的摘要（与 Python 版同样的信息）。
pub fn build(hq_cache: &Path, out: &Path, date: &str) -> Result<String, String> {
    let zs_rows = read_rows(&hq_cache.join("tdxzs3.cfg"))?;
    let mut nodes = Vec::new();
    for p in &zs_rows {
        if p[2] != "12" {
            continue;
        }
        let sub = p[3].trim();
        let sub_type = if sub.is_empty() {
            0
        } else {
            sub.parse::<i64>()
                .map_err(|_| format!("tdxzs3.cfg 的 sub_type 非法: {:?}", p[3]))?
        };
        nodes.push(Node {
            name: p[0].trim().to_string(),
            code: p[1].trim().to_string(),
            sub_type,
            ref_: p[5].trim().to_string(),
        });
    }
    let refs: HashSet<&str> = nodes.iter().map(|n| n.ref_.as_str()).collect();

    // 旧 catalog 的名字继承表（code -> name）
    let mut names: HashMap<String, String> = HashMap::new();
    if out.exists() {
        let text = std::fs::read_to_string(out)
            .map_err(|e| format!("读不到旧 catalog {}: {e}", out.display()))?;
        let old: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("旧 catalog 解析失败（{}）: {e}", out.display()))?;
        if let Some(members) = old.get("members").and_then(|v| v.as_array()) {
            for m in members {
                let Some(code) = m.get("code").and_then(|v| v.as_str()) else {
                    continue;
                };
                if let Some(name) = m.get("name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        names.insert(code.to_string(), name.to_string());
                    }
                }
            }
        }
    }

    let hy_rows = read_rows(&hq_cache.join("tdxhy.cfg"))?;
    let mut members = Vec::new();
    let mut orphans: Vec<(String, String)> = Vec::new();
    for p in &hy_rows {
        let code = p[1].trim().to_string();
        let mut ref_ = p[5].trim().to_string();
        if ref_.is_empty() {
            continue;
        }
        if !refs.contains(ref_.as_str()) {
            // X 码已不在树中：按前 5 位归二级
            orphans.push((code.clone(), ref_.clone()));
            ref_ = ref_.chars().take(5).collect();
            if !refs.contains(ref_.as_str()) {
                return Err(format!("{code} 的 {} 前缀 {ref_} 也不在树中", p[5]));
            }
        }
        let market: i64 = p[0]
            .trim()
            .parse()
            .map_err(|_| format!("tdxhy.cfg 的 market 非法: {:?}", p[0]))?;
        let name = names.get(&code).cloned().unwrap_or_else(|| code.clone());
        members.push(Member {
            code,
            market,
            industry: ref_,
            name,
        });
    }

    // ---- 硬校验 ----
    let mut levels: BTreeMap<usize, usize> = BTreeMap::new();
    for n in &nodes {
        *levels.entry(n.ref_.chars().count()).or_insert(0) += 1;
    }
    let expected: BTreeMap<usize, usize> = [(3, 30), (5, 128), (7, 309)].into_iter().collect();
    if levels != expected {
        return Err(format!("层级异常: {levels:?}"));
    }

    let lv2: Vec<&Node> = nodes.iter().filter(|n| n.ref_.chars().count() == 5).collect();
    let mut size: HashMap<String, usize> = HashMap::new();
    for m in &members {
        let key: String = m.industry.chars().take(5).collect();
        *size.entry(key).or_insert(0) += 1;
    }
    let empty: Vec<&str> = lv2
        .iter()
        .filter(|n| size.get(&n.ref_).copied().unwrap_or(0) == 0)
        .map(|n| n.name.as_str())
        .collect();
    let mut name_count: HashMap<&str, usize> = HashMap::new();
    for n in &lv2 {
        *name_count.entry(n.name.as_str()).or_insert(0) += 1;
    }
    let dup: Vec<&str> = name_count
        .iter()
        .filter(|(_, v)| **v > 1)
        .map(|(k, _)| *k)
        .collect();
    if !empty.is_empty() || !dup.is_empty() {
        return Err(format!("二级空成分 {empty:?} / 重名 {dup:?}"));
    }

    // ---- 写文件（Python json.dumps 口径）----
    let text = render(date, &nodes, &members);
    zg_core::jsonio::write_text_atomic(out, &text, false)
        .then_some(())
        .ok_or_else(|| format!("写入失败: {}", out.display()))?;

    // ---- 终端摘要（与 Python 版同样的信息）----
    let mut sizes: Vec<usize> = lv2
        .iter()
        .map(|n| size.get(&n.ref_).copied().unwrap_or(0))
        .collect();
    sizes.sort_unstable();
    let mut top: Vec<&Node> = lv2.clone();
    top.sort_by_key(|n| std::cmp::Reverse(size.get(&n.ref_).copied().unwrap_or(0)));
    top.truncate(3);

    let mut msg = String::new();
    msg.push_str(&format!(
        "nodes={} (一级{} / 二级{} / 三级{})\n",
        nodes.len(),
        levels.get(&3).copied().unwrap_or(0),
        levels.get(&5).copied().unwrap_or(0),
        levels.get(&7).copied().unwrap_or(0),
    ));
    msg.push_str(&format!(
        "members={} 二级={} 空成分=0 重名=0\n",
        members.len(),
        lv2.len()
    ));
    msg.push_str(&format!(
        "成分: 中位 {} 均 {:.1} min {} max {}\n",
        sizes[sizes.len() / 2],
        sizes.iter().sum::<usize>() as f64 / sizes.len() as f64,
        sizes[0],
        sizes[sizes.len() - 1],
    ));
    let top_str: Vec<String> = top
        .iter()
        .map(|n| format!("{}{}", n.name, size.get(&n.ref_).copied().unwrap_or(0)))
        .collect();
    msg.push_str(&format!("最大: {}\n", top_str.join(", ")));
    if !orphans.is_empty() {
        let orph: Vec<String> = orphans
            .iter()
            .map(|(c, r)| format!("{c} {r}->{}", r.chars().take(5).collect::<String>()))
            .collect();
        msg.push_str(&format!("X 码不在树、已归二级: {}\n", orph.join(", ")));
    }
    msg.push_str(&format!(
        "名字缺失(回落代码)={}\n",
        members.iter().filter(|m| m.name == m.code).count()
    ));
    msg.push_str(&format!("写入 {}", out.display()));
    Ok(msg)
}

/// 读 GBK 文件 → 每行按 `|` 切成字段，只保留字段数 ≥ 6 的行。
///
/// Python 用 `decode("gbk", "ignore")` + `splitlines()`；这里用 encoding_rs 解 GBK
/// （非法序列替换为 U+FFFD，与 ignore 只在本机分类文件损坏时有差别），
/// 行分隔按 `\r` / `\n` 都切（config 是 CRLF）。
fn read_rows(path: &Path) -> Result<Vec<Vec<String>>, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("读不到 {}: {e}", path.display()))?;
    let (text, _, _) = encoding_rs::GBK.decode(&bytes);
    Ok(text
        .split(|c| c == '\n' || c == '\r')
        .filter(|line| !line.is_empty())
        .map(|line| line.split('|').map(str::to_string).collect::<Vec<_>>())
        .filter(|p| p.len() >= 6)
        .collect())
}

/// Python `json.dumps(..., ensure_ascii=False)` 的字符串转义口径。
fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{08}' => o.push_str("\\b"),
            '\u{0c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn render(date: &str, nodes: &[Node], members: &[Member]) -> String {
    let mut s = String::with_capacity(members.len() * 96 + nodes.len() * 96);
    s.push_str("{\"updated\": ");
    s.push_str(&json_str(date));
    s.push_str(", \"source\": ");
    s.push_str(&json_str("tdxzs3.cfg(type=12) + tdxhy.cfg 第6列"));
    s.push_str(", \"nodes\": [");
    for (i, n) in nodes.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str("{\"name\": ");
        s.push_str(&json_str(&n.name));
        s.push_str(", \"code\": ");
        s.push_str(&json_str(&n.code));
        s.push_str(&format!(", \"type\": 12, \"sub_type\": {}, \"ref\": ", n.sub_type));
        s.push_str(&json_str(&n.ref_));
        s.push('}');
    }
    s.push_str("], \"members\": [");
    for (i, m) in members.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str("{\"code\": ");
        s.push_str(&json_str(&m.code));
        s.push_str(&format!(", \"market\": {}, \"industry\": ", m.market));
        s.push_str(&json_str(&m.industry));
        s.push_str(", \"name\": ");
        s.push_str(&json_str(&m.name));
        s.push('}');
    }
    s.push_str("]}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_matches_python_defaults() {
        assert_eq!(json_str("煤炭"), "\"煤炭\"");
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
        assert_eq!(json_str("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn build_catalog_against_local_hq_cache() {
        let hq = Path::new("/mnt/g/Program Files/DFZQ/T0002/hq_cache");
        if !hq.join("tdxzs3.cfg").exists() {
            eprintln!("跳过：本机没有通达信 hq_cache");
            return;
        }
        let out = std::env::temp_dir().join(format!(
            "tv-catalog-test-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&out);
        let msg = build(hq, &out, "2026-01-01").expect("生成 catalog");
        assert!(msg.contains("nodes=467"));
        let text = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["nodes"].as_array().unwrap().len(), 467);
        // 5589：2026-11-13 catalog 重生成时北交所新增 1 只（920067），其余未变。
        // 这个数跟着本机 hq_cache 的上市家数走，换机/变盘后重跑 `tv --build-catalog` 再改这里。
        assert_eq!(v["members"].as_array().unwrap().len(), 5589);
        let _ = std::fs::remove_file(&out);
    }
}
