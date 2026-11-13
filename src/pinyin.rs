//! 拼音转换 —— pypinyin 0.55.0 的 Rust 移植子集（`lazy_pinyin` + `NORMAL` / `FIRST_LETTER`）。
//!
//! 只用 pypinyin 的两份**数据文件**（`phrases_dict.json` / `pinyin_dict.json`），
//! 算法在这里重写，运行期不再需要 Python：
//!
//! 1. `simpleseg`：按 RE_HANS 把字符串切成「汉字段 / 非汉字段」；
//! 2. `mmseg`：汉字段做**正向最大匹配**（词表 + 前缀表，严格模式：匹配不到词就单字）；
//! 3. 每个词取词库首读音（`heteronym=False` 等价）；单字取 `pinyin_dict` 首读音；
//! 4. `NORMAL` 去声调；`FIRST_LETTER` 去声调后取首字母；非汉字段原样保留；
//! 5. 整串最后 `.lower()`（与 `''.join(lazy_pinyin(name)).lower()` 一致）。
//!
//! 为什么不能只按单字转：pypinyin 用 47,111 条词语做多音字消歧。实测 5,579 个
//! 股票名里裸单字转换会错 108 个（1.94%），例如 平安银行 → pinganyinxing、
//! 中国长城 → zhongguozhangcheng、厦门信达 → shamenxinda。`data/symbols.json`
//! 的逐行对表（tests 里的 parity 用例）是这条移植的验收口径。

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// pypinyin 的两份词库 + 由词表生成的前缀表。
pub struct Dict {
    /// 单字 → 首读音（已去声调）
    chars: HashMap<char, String>,
    /// 词 → 逐字首读音（已去声调）
    phrases: HashMap<String, Vec<String>>,
    /// 词表里出现过的所有前缀（mmseg 的 `PrefixSet`）
    prefixes: HashSet<String>,
}

impl Dict {
    /// 从 `modules/market/vendor/pypinyin/` 装载（目录里只剩两份 JSON）。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let char_path = dir.join("pinyin_dict.json");
        let char_text = std::fs::read_to_string(&char_path)
            .map_err(|e| format!("读不到 {}: {e}", char_path.display()))?;
        let raw_chars: HashMap<String, String> = serde_json::from_str(&char_text)
            .map_err(|e| format!("{} 解析失败: {e}", char_path.display()))?;

        let mut chars = HashMap::with_capacity(raw_chars.len());
        for (key, value) in raw_chars {
            let cp: u32 = key
                .parse()
                .map_err(|_| format!("pinyin_dict 键不是十进制码位: {key}"))?;
            let Some(ch) = char::from_u32(cp) else { continue };
            // PINYIN_DICT[num].split(',')[0]，heteronym=False 只取第一个读音
            let first = value.split(',').next().unwrap_or("");
            chars.insert(ch, strip_tone(first));
        }

        let phrase_path = dir.join("phrases_dict.json");
        let phrase_text = std::fs::read_to_string(&phrase_path)
            .map_err(|e| format!("读不到 {}: {e}", phrase_path.display()))?;
        let raw_phrases: HashMap<String, Vec<Vec<String>>> = serde_json::from_str(&phrase_text)
            .map_err(|e| format!("{} 解析失败: {e}", phrase_path.display()))?;

        let mut phrases = HashMap::with_capacity(raw_phrases.len());
        let mut prefixes: HashSet<String> = HashSet::new();
        for (word, readings) in raw_phrases {
            let syllables: Vec<String> = readings
                .iter()
                .map(|item| strip_tone(item.first().map(String::as_str).unwrap_or("")))
                .collect();
            // PrefixSet.train：把这个词的每个前缀（按码位切）都放进去
            let mut prefix = String::new();
            for ch in word.chars() {
                prefix.push(ch);
                prefixes.insert(prefix.clone());
            }
            phrases.insert(word, syllables);
        }

        Ok(Self {
            chars,
            phrases,
            prefixes,
        })
    }
}

/// 复刻 pypinyin 的 `RE_HANS`（`SUPPORT_UCS4` 分支）：判断单个字符是否参与拼音转换。
fn is_han(c: char) -> bool {
    matches!(
        c,
        '\u{3007}'
            | '\u{fa18}'
            | '\u{2d016}'
            | '\u{30edd}'
            | '\u{30ede}'
            | '\u{e815}'..='\u{e864}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{20000}'..='\u{2a6df}'
            | '\u{2a703}'..='\u{2b73f}'
            | '\u{2b740}'..='\u{2b81d}'
            | '\u{2b825}'..='\u{2bf6e}'
            | '\u{2c029}'..='\u{2ce93}'
            | '\u{2d11b}'..='\u{2ebd9}'
            | '\u{2f80a}'..='\u{2fa1f}'
            | '\u{30000}'..='\u{3134a}'
            // pypinyin 原文这里还有一段 \U000300F7-\U00031288，完全被上一段包含
            | '\u{31350}'..='\u{32389}'
    )
}

/// 带声调拼音 → 无声调：`replace_symbol_to_no_symbol` 的等价实现。
///
/// pypinyin 的做法是先把声调符号换成「字母+数字」再删数字；这里直接映射回字母。
/// 注意 `phonetic_symbol.py` 里有四个**双码位**符号（m̄ / m̀ / ê̄ / ê̌），
/// 必须先按整串替换，否则逐字符处理会漏掉组合附加符。
fn strip_tone(s: &str) -> String {
    let s = s
        .replace("m\u{0304}", "m")
        .replace("m\u{0300}", "m")
        .replace("ê\u{0304}", "ê")
        .replace("ê\u{030C}", "ê");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        // 残余的组合附加符（U+0300..U+036F）与数字一律丢掉（RE_NUMBER = \d）
        if ('\u{0300}'..='\u{036f}').contains(&c) || c.is_ascii_digit() {
            continue;
        }
        out.push(match c {
            'ā' | 'á' | 'ǎ' | 'à' => 'a',
            'ē' | 'é' | 'ě' | 'è' => 'e',
            'ō' | 'ó' | 'ǒ' | 'ò' => 'o',
            'ī' | 'í' | 'ǐ' | 'ì' => 'i',
            'ū' | 'ú' | 'ǔ' | 'ù' => 'u',
            'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' | 'ü' => 'v',
            'ń' | 'ň' | 'ǹ' => 'n',
            'ḿ' => 'm',
            'ế' | 'ề' => 'ê',
            _ => c,
        });
    }
    out
}

/// `mmseg.Seg.cut`（`no_non_phrases=True`）：正向最大匹配，匹配不到词就吐单字。
fn cut(dict: &Dict, text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut last_valid = 0usize; // 最近一个「在词表里」的词长（码位数）
        let mut broke = false;
        for end in (start + 1)..=chars.len() {
            let word: String = chars[start..end].iter().collect();
            if dict.prefixes.contains(&word) {
                if dict.phrases.contains_key(&word) {
                    last_valid = end - start;
                }
            } else {
                // 前缀断了：有词就取最近的词，没有就单字（严格模式）
                let take = if last_valid > 0 { last_valid } else { 1 };
                out.push(chars[start..start + take].iter().collect());
                start += take;
                broke = true;
                break;
            }
        }
        if broke {
            continue;
        }
        // 整个剩余串都是前缀：有词取词，否则严格模式拆单字
        if last_valid > 0 {
            out.push(chars[start..start + last_valid].iter().collect());
            start += last_valid;
        } else {
            for c in &chars[start..] {
                out.push(c.to_string());
            }
            start = chars.len();
        }
    }
    out
}

/// 汉字段 → 拼音串（未小写）。
fn han_run_to_pinyin(dict: &Dict, run: &str, first_letter: bool, out: &mut String) {
    for word in cut(dict, run) {
        if let Some(syllables) = dict.phrases.get(&word) {
            for s in syllables {
                push_syllable(s, first_letter, out);
            }
            continue;
        }
        // 词表里没有（mmseg 已保证这种 word 只可能是一个字，见 cut 的严格模式）
        for ch in word.chars() {
            match dict.chars.get(&ch) {
                Some(py) => push_syllable(py, first_letter, out),
                // PINYIN_DICT 里没有这个汉字：pypinyin errors='default' 原样保留
                None => out.push(ch),
            }
        }
    }
}

fn push_syllable(syllable: &str, first_letter: bool, out: &mut String) {
    if first_letter {
        if let Some(c) = syllable.chars().next() {
            out.push(c);
        }
    } else {
        out.push_str(syllable);
    }
}

/// `''.join(lazy_pinyin(name, style=...)).lower()` 的等价实现。
///
/// `first_letter=true` 对应 `Style.FIRST_LETTER`，否则对应 `Style.NORMAL`。
pub fn convert(dict: &Dict, name: &str, first_letter: bool) -> String {
    let mut out = String::with_capacity(name.len() * 2);
    let mut run = String::new();
    let mut run_is_han = false;
    let mut started = false;

    for ch in name.chars() {
        let han = is_han(ch);
        if !started {
            run_is_han = han;
            run.push(ch);
            started = true;
        } else if han == run_is_han {
            run.push(ch);
        } else {
            flush(dict, &run, run_is_han, first_letter, &mut out);
            run.clear();
            run.push(ch);
            run_is_han = han;
        }
    }
    if started {
        flush(dict, &run, run_is_han, first_letter, &mut out);
    }
    out.to_lowercase()
}

fn flush(dict: &Dict, run: &str, is_han: bool, first_letter: bool, out: &mut String) {
    if is_han {
        han_run_to_pinyin(dict, run, first_letter, out);
    } else {
        // 非汉字段原样保留（simpleseg 不送进 converter）
        out.push_str(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dict {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("modules")
            .join("market")
            .join("vendor")
            .join("pypinyin");
        Dict::load(&dir).expect("装载 pypinyin 数据")
    }

    #[test]
    fn tone_strip_matches_pypinyin() {
        assert_eq!(strip_tone("zhōng"), "zhong");
        assert_eq!(strip_tone("lǜ"), "lv");
        assert_eq!(strip_tone("nǚ"), "nv");
        assert_eq!(strip_tone("ḿ"), "m");
        assert_eq!(strip_tone("m̄"), "m");
        assert_eq!(strip_tone("ê̄"), "ê");
    }

    #[test]
    fn phrase_disambiguation() {
        let d = dict();
        assert_eq!(convert(&d, "平安银行", false), "pinganyinhang");
        assert_eq!(convert(&d, "中国长城", false), "zhongguochangcheng");
        assert_eq!(convert(&d, "厦门信达", false), "xiamenxinda");
        assert_eq!(convert(&d, "重庆", false), "chongqing");
        assert_eq!(convert(&d, "长安汽车", false), "changanqiche");
    }

    #[test]
    fn non_han_and_first_letter() {
        let d = dict();
        assert_eq!(convert(&d, "*ST美丽", false), "*stmeili");
        assert_eq!(convert(&d, "*ST美丽", true), "*stml");
        assert_eq!(convert(&d, "恒生电子-U", false), "hengshengdianzi-u");
        assert_eq!(convert(&d, "恒生电子-U", true), "hsdz-u");
        assert_eq!(convert(&d, "万科Ａ", false), "wankeａ");
        assert_eq!(convert(&d, "万科Ａ", true), "wkａ");
    }

    /// 逐行对表：本地有 `data/symbols.json`（pypinyin 生成的旧产物）时，
    /// 两份拼音字段必须**逐字节一致**。缺失（未初始化 / 已重新生成）则跳过。
    #[test]
    fn parity_with_existing_symbols_json() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sym_path = root.join("data").join("symbols.json");
        let stocks_path = root.join("data").join("market").join("stocks.json");
        if !sym_path.exists() || !stocks_path.exists() {
            eprintln!("跳过：本地缺 data/symbols.json 或 data/market/stocks.json");
            return;
        }
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&stocks_path).unwrap()).unwrap();
        let expect: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&sym_path).unwrap()).unwrap();
        // stocks.json 与 symbols.json 的行序不一定相同，按 code 对齐
        let by_code: HashMap<String, &serde_json::Value> = expect
            .iter()
            .filter_map(|v| {
                v.get("code")
                    .and_then(|c| c.as_str())
                    .map(|c| (c.to_string(), v))
            })
            .collect();
        assert_eq!(by_code.len(), rows.len());
        let d = dict();
        let mut bad = Vec::new();
        for row in rows.iter() {
            let code = row.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let Some(e) = by_code.get(code) else {
                bad.push(format!("{code} 不在 symbols.json"));
                continue;
            };
            let py = convert(&d, name, true);
            let pyfull = convert(&d, name, false);
            let epy = e.get("py").and_then(|v| v.as_str()).unwrap_or("");
            let efull = e.get("pyfull").and_then(|v| v.as_str()).unwrap_or("");
            if py != epy || pyfull != efull {
                bad.push(format!("{name}: {py}/{pyfull} != {epy}/{efull}"));
            }
        }
        assert!(bad.is_empty(), "拼音不一致 {} 条：{:?}", bad.len(), &bad[..bad.len().min(10)]);
    }
}
