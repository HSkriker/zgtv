//! kcache v1 二进制缓存 —— 与 `zgcore/bars/cache.py`（及 `modules/market/kcache.py`）
//! **字节级兼容**：
//!
//!   <prefix>.idx.json     version/kind/lmt/date/rows/bin/names/index
//!   <prefix>.<token>.bin  numpy 结构化数组（day: date+6 float64；min: +uint16 minute）
//!
//! numpy dtype 未对齐打包：
//!   day 52B = i32 date + 6×f64
//!   min 54B = i32 date + u16 minute + 6×f64
//!
//! 新代码必须能读旧缓存、旧代码必须能读新写的 idx；bin 文件名不承诺稳定。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::model::{day_int, day_str, minute_parts, minute_str};

pub const KIND_DAY: &str = "day";
pub const KIND_MIN: &str = "min";

pub const DAY_ROW: usize = 52;
pub const MIN_ROW: usize = 54;

const IDX_VERSION: i64 = 1;

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct KCacheStore {
    pub kind: String,
    pub lmt: usize,
    pub arr: Vec<u8>,
    /// 插入顺序即 Python dict 顺序（idx.json 的 index 键序要与旧文件一致）
    pub index: Vec<(String, (usize, usize))>,
    pub names: Vec<(String, String)>,
    pub date: String,
    bin: String,
    next: usize,
    last: HashMap<String, i32>,
    pos: HashMap<String, (usize, usize)>,
}

fn row_size(kind: &str) -> usize {
    if kind == KIND_MIN {
        MIN_ROW
    } else {
        DAY_ROW
    }
}

fn f64_at(b: &[u8], off: usize) -> f64 {
    f64::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3],
                        b[off + 4], b[off + 5], b[off + 6], b[off + 7]])
}

fn i32_at(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn put_i32(buf: &mut [u8], off: usize, v: i32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f64(buf: &mut [u8], off: usize, v: f64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

impl KCacheStore {
    fn base(kind: &str, lmt: usize) -> Self {
        Self {
            kind: kind.to_string(),
            lmt,
            arr: Vec::new(),
            index: Vec::new(),
            names: Vec::new(),
            date: String::new(),
            bin: String::new(),
            next: 0,
            last: HashMap::new(),
            pos: HashMap::new(),
        }
    }

    /// 磁盘缺失/格式不符/截断 → None（调用方决定迁移或重建）。
    pub fn load(prefix: &str, kind: &str, lmt: usize) -> Option<Self> {
        let idx_path = format!("{prefix}.idx.json");
        let text = match std::fs::read_to_string(&idx_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                eprintln!("[kcache] 索引读取失败 {idx_path}: {e}");
                return None;
            }
        };
        let idx: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[kcache] 索引读取失败 {idx_path}: {e}");
                return None;
            }
        };
        if idx.get("version").and_then(|v| v.as_i64()) != Some(IDX_VERSION) {
            return None;
        }
        if idx.get("kind").and_then(|v| v.as_str()) != Some(kind) {
            return None;
        }
        if idx.get("lmt").and_then(|v| v.as_i64()) != Some(lmt as i64) {
            return None;
        }
        let rows = idx.get("rows").and_then(|v| v.as_i64()).unwrap_or(0);
        let bin_name = idx.get("bin").and_then(|v| v.as_str()).unwrap_or("");
        if rows <= 0 || bin_name.is_empty() {
            return None;
        }
        let dir = Path::new(prefix).parent().unwrap_or_else(|| Path::new("."));
        let bin_path = dir.join(bin_name);
        let arr = match std::fs::read(&bin_path) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[kcache] 缓存加载失败 {prefix}: {e}");
                return None;
            }
        };
        let rs = row_size(kind);
        if arr.len() < rows as usize * rs {
            eprintln!("[kcache] 缓存加载失败 {prefix}: bin 截断");
            return None;
        }
        let limit = arr.len() / rs;
        let mut st = Self::base(kind, lmt);
        st.bin = bin_name.to_string();
        st.arr = arr;
        if let Some(obj) = idx.get("index").and_then(|v| v.as_object()) {
            for (c, v) in obj {
                if let Some(a) = v.as_array() {
                    if a.len() == 2 {
                        if let (Some(o), Some(n)) = (a[0].as_i64(), a[1].as_i64()) {
                            let (o, n) = (o.max(0) as usize, n.max(0) as usize);
                            // idx.json 与 bin 不同步时（手工改/中断写/跨机拷贝）必须整体作废重建，
                            // 否则 view() 切片越界 panic，相关接口永久 500。
                            if o.checked_add(n).is_none_or(|end| end > limit) {
                                eprintln!(
                                    "[kcache] 索引越界 {prefix}: {c} off={o} n={n} rows={limit}，作废重建"
                                );
                                return None;
                            }
                            st.index.push((c.clone(), (o, n)));
                            st.pos.insert(c.clone(), (o, n));
                        }
                    }
                }
            }
        }
        if st.index.is_empty() {
            return None;
        }
        if let Some(obj) = idx.get("names").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    st.names.push((k.clone(), s.to_string()));
                }
            }
        }
        st.date = idx.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Self::cleanup_bins(prefix, bin_name);
        Some(st)
    }

    pub fn create(prefix: &str, kind: &str, lmt: usize, slots: usize) -> Self {
        let seq = TOKEN_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let token = format!("{millis}-{}-{seq}", std::process::id());
        let mut st = Self::base(kind, lmt);
        st.bin = format!("{prefix}.{token}.bin");
        let total = slots * lmt;
        st.arr = vec![0u8; total * row_size(kind)];
        st
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 实际写入的行数（不含预分配零尾巴）。
    pub fn written_rows(&self) -> usize {
        self.next.max(self.index.iter().map(|(_, (_, n))| *n).sum::<usize>())
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains(&self, code: &str) -> bool {
        self.pos.contains_key(code)
    }

    pub fn view(&self, code: &str) -> Option<&[u8]> {
        let (off, n) = *self.pos.get(code)?;
        let rs = row_size(&self.kind);
        let start = off.checked_mul(rs)?;
        let end = off.checked_add(n)?.checked_mul(rs)?;
        // get 而非切片：越界返回 None，绝不 panic（load 已校验，这里是纵深防御）
        self.arr.get(start..end)
    }

    /// 旧 JSON 结构：[[date, o, h, l, c, v, a], ...]。
    pub fn legacy(&self, code: &str) -> Option<Vec<Value>> {
        let v = self.view(code)?;
        let rs = row_size(&self.kind);
        let n = v.len() / rs;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let r = &v[i * rs..(i + 1) * rs];
            let d = i32_at(r, 0);
            let (date, base) = if self.kind == KIND_MIN {
                (minute_str(d, u16_at(r, 4) as i32), 6)
            } else {
                (day_str(d), 4)
            };
            out.push(serde_json::json!([
                date,
                f64_at(r, base),
                f64_at(r, base + 8),
                f64_at(r, base + 16),
                f64_at(r, base + 24),
                f64_at(r, base + 32),
                f64_at(r, base + 40),
            ]));
        }
        Some(out)
    }

    /// Python `dicts`：vol/amount 取整（int(round(x))）。
    pub fn dicts(&self, code: &str) -> Option<Vec<Value>> {
        let v = self.view(code)?;
        let rs = row_size(&self.kind);
        let n = v.len() / rs;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let r = &v[i * rs..(i + 1) * rs];
            let d = i32_at(r, 0);
            let (date, base) = if self.kind == KIND_MIN {
                (minute_str(d, u16_at(r, 4) as i32), 6)
            } else {
                (day_str(d), 4)
            };
            out.push(serde_json::json!({
                "date": date,
                "open": f64_at(r, base),
                "high": f64_at(r, base + 8),
                "low": f64_at(r, base + 16),
                "close": f64_at(r, base + 24),
                "vol": py_int_round(f64_at(r, base + 32)),
                "amount": py_int_round(f64_at(r, base + 40)),
            }));
        }
        Some(out)
    }

    pub fn last_day(&mut self, code: &str) -> i32 {
        if let Some(v) = self.last.get(code) {
            return *v;
        }
        let rs = row_size(&self.kind);
        let d = match self.pos.get(code) {
            // Python 取 v["date"][-1]：末根，不是首根
            Some((off, n)) if *n > 0 => i32_at(&self.arr[(off + n - 1) * rs..], 0),
            _ => 0,
        };
        self.last.insert(code.to_string(), d);
        d
    }

    pub fn max_date(&mut self) -> String {
        let codes: Vec<String> = self.index.iter().map(|(c, _)| c.clone()).collect();
        let mut best = 0;
        for code in codes {
            let d = self.last_day(&code);
            if d > best {
                best = d;
            }
        }
        if best > 0 {
            day_str(best)
        } else {
            String::new()
        }
    }

    pub fn stale_count(&mut self, lt_int: i32) -> usize {
        let codes: Vec<String> = self.index.iter().map(|(c, _)| c.clone()).collect();
        codes.iter().filter(|c| self.last_day(c) < lt_int).count()
    }

    /// Python `write(code, rows)`：rows = [[date, o, h, l, c, v, a], ...]。
    pub fn write(&mut self, code: &str, rows: &[(String, f64, f64, f64, f64, f64, f64)]) {
        if rows.is_empty() {
            return;
        }
        let rs = row_size(&self.kind);
        let mut buf = vec![0u8; rows.len() * rs];
        for (i, r) in rows.iter().enumerate() {
            let o = i * rs;
            if self.kind == KIND_MIN {
                let (d, m) = minute_parts(&r.0);
                put_i32(&mut buf, o, d);
                buf[o + 4..o + 6].copy_from_slice(&(m as u16).to_le_bytes());
                for (k, v) in [r.1, r.2, r.3, r.4, r.5, r.6].iter().enumerate() {
                    put_f64(&mut buf, o + 6 + k * 8, *v);
                }
            } else {
                put_i32(&mut buf, o, day_int(&r.0));
                for (k, v) in [r.1, r.2, r.3, r.4, r.5, r.6].iter().enumerate() {
                    put_f64(&mut buf, o + 4 + k * 8, *v);
                }
            }
        }
        self.put_bytes(code, &buf);
    }

    fn put_bytes(&mut self, code: &str, buf: &[u8]) {
        let rs = row_size(&self.kind);
        let mut n = buf.len() / rs;
        let mut src = buf;
        if n > self.lmt && self.lmt > 0 {
            src = &buf[(n - self.lmt) * rs..];
            n = self.lmt;
        }
        if n == 0 {
            return;
        }
        // 同 code 重写且新数据不超出旧区间 → 原地覆盖：否则旧区间变垃圾、bin 单调膨胀
        if let Some(&(off, old_n)) = self.pos.get(code) {
            if n <= old_n && old_n > 0 {
                self.arr[off * rs..off * rs + n * rs].copy_from_slice(&src[..n * rs]);
                self.set_index(code, (off, n));
                return;
            }
        }
        let off = self.next;
        self.next += n;
        let need = self.next * rs;
        if self.arr.len() < need {
            self.arr.resize(need, 0);
        }
        self.arr[off * rs..off * rs + n * rs].copy_from_slice(src);
        self.set_index(code, (off, n));
    }

    fn set_index(&mut self, code: &str, ent: (usize, usize)) {
        match self.pos.get(code) {
            Some(_) => {
                for (c, v) in self.index.iter_mut() {
                    if c == code {
                        *v = ent;
                        break;
                    }
                }
            }
            None => self.index.push((code.to_string(), ent)),
        }
        self.pos.insert(code.to_string(), ent);
        // 写入改变了该 code 的数据区间，last_day 的缓存必须作废，
        // 否则 max_date / stale_count 一直用写入前的旧末根日期
        self.last.remove(code);
    }

    /// flush bin → 原子替换索引；失败保留旧索引（旧数据仍可用）。
    pub fn save(&mut self, prefix: &str) -> bool {
        let mut tmp = String::new();
        let result = (|| -> std::io::Result<()> {
            if !self.bin.is_empty() && !self.arr.is_empty() {
                if let Some(parent) = Path::new(&self.bin).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let f = std::fs::File::create(&self.bin)?;
                let mut w = std::io::BufWriter::new(f);
                w.write_all(&self.arr)?;
                w.flush()?;
            }
            let idx = self.idx_json();
            let path = format!("{prefix}.idx.json");
            tmp = format!("{path}.tmp.{}", std::process::id());
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(idx.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
            let mut last_err = None;
            for _ in 0..5 {
                match std::fs::rename(&tmp, &path) {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        last_err = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
            Err(last_err.unwrap())
        })();
        match result {
            Ok(_) => {
                if let Some(name) = Path::new(&self.bin).file_name().and_then(|s| s.to_str()) {
                    Self::cleanup_bins(prefix, name);
                }
                true
            }
            Err(e) => {
                eprintln!("[kcache] 索引落盘失败 {prefix}: {e}");
                if !tmp.is_empty() {
                    let _ = std::fs::remove_file(&tmp);
                }
                false
            }
        }
    }

    fn idx_json(&self) -> String {
        let esc = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        let names: Vec<String> = self
            .names
            .iter()
            .map(|(k, v)| format!("{}: {}", esc(k), esc(v)))
            .collect();
        let index: Vec<String> = self
            .index
            .iter()
            .map(|(c, (o, n))| format!("{}: [{}, {}]", esc(c), o, n))
            .collect();
        format!(
            "{{\"version\": {}, \"kind\": {}, \"lmt\": {}, \"date\": {}, \"rows\": {}, \"bin\": {}, \"names\": {{{}}}, \"index\": {{{}}}}}",
            IDX_VERSION,
            esc(&self.kind),
            self.lmt,
            esc(&self.date),
            self.next.max(
                self.index.iter().map(|(_, (_, n))| *n).sum::<usize>(),
            ),
            esc(Path::new(&self.bin).file_name().and_then(|s| s.to_str()).unwrap_or("")),
            names.join(", "),
            index.join(", ")
        )
    }

    pub fn cleanup_old_bins(&self, prefix: &str) {
        if let Some(name) = Path::new(&self.bin).file_name().and_then(|s| s.to_str()) {
            Self::cleanup_bins(prefix, name);
        }
    }

    fn cleanup_bins(prefix: &str, keep_name: &str) {
        let p = Path::new(prefix);
        let dir = p.parent().unwrap_or_else(|| Path::new("."));
        let base = format!(
            "{}.",
            p.file_name().and_then(|s| s.to_str()).unwrap_or_default()
        );
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for ent in entries.flatten() {
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&base) && name.ends_with(".bin") && name != keep_name {
                let _ = std::fs::remove_file(ent.path());
            }
        }
    }
}

fn py_int_round(x: f64) -> i64 {
    if !x.is_finite() {
        return 0;
    }
    x.round_ties_even() as i64
}

/// 旧 JSON → 二进制（与 Python `load_legacy` 的读取口径一致）。
pub fn load_legacy_dict(prefix: &str, kind: &str, lmt: usize) -> Option<Value> {
    if let Some(st) = KCacheStore::load(prefix, kind, lmt) {
        let codes: Vec<String> = st.index.iter().map(|(c, _)| c.clone()).collect();
        let mut data = serde_json::Map::new();
        for code in codes {
            if let Some(v) = st.legacy(&code) {
                data.insert(code, Value::Array(v));
            }
        }
        return Some(serde_json::json!({
            "lmt": lmt,
            "date": st.date,
            "names": Value::Object(
                st.names.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect(),
            ),
            "data": Value::Object(data),
        }));
    }
    let path = format!("{prefix}.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 便利：把 Vec<Bar> 转成 `write` 需要的行。
pub fn bar_rows(bars: &[crate::model::Bar]) -> Vec<(String, f64, f64, f64, f64, f64, f64)> {
    bars.iter()
        .map(|b| (b.date.clone(), b.open, b.high, b.low, b.close, b.volume, b.amount))
        .collect()
}

/// bin 文件路径（测试/诊断用）。
pub fn bin_path_of(prefix: &str, store: &KCacheStore) -> PathBuf {
    Path::new(prefix)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&store.bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(String, f64, f64, f64, f64, f64, f64)> {
        vec![
            ("2026-10-20".into(), 10.0, 10.5, 9.9, 10.2, 12345.0, 1.2345e8),
            ("2026-10-21".into(), 10.2, 10.6, 10.1, 10.4, 23456.0, 2.3456e8),
            ("2026-10-22".into(), 10.4, 10.7, 10.2, 10.3, 34567.0, 3.4567e8),
        ]
    }

    #[test]
    fn day_roundtrip() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path().join("day_cache");
        let prefix = prefix.to_str().unwrap();
        let mut st = KCacheStore::create(prefix, KIND_DAY, 480, 2);
        st.write("600519", &rows());
        st.names.push(("600519".into(), "贵州茅台".into()));
        st.date = "2026-10-22".into();
        assert!(st.save(prefix));

        let mut back = KCacheStore::load(prefix, KIND_DAY, 480).expect("load");
        let d = back.dicts("600519").unwrap();
        assert_eq!(d[0]["date"], "2026-10-20");
        assert_eq!(d[0]["vol"], 12345);
        assert_eq!(back.last_day("600519"), 20261022);
        assert_eq!(back.max_date(), "2026-10-22");
    }

    #[test]
    fn min_roundtrip() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path().join("min_cache");
        let prefix = prefix.to_str().unwrap();
        let mut st = KCacheStore::create(prefix, KIND_MIN, 480, 2);
        st.write("600519", &[
            ("2026-10-22 14:59".into(), 10.1, 10.2, 10.0, 10.15, 1000.0, 1.0e6),
            ("2026-10-22 15:00".into(), 10.15, 10.3, 10.1, 10.28, 2000.0, 2.0e6),
        ]);
        assert!(st.save(prefix));
        let mut back = KCacheStore::load(prefix, KIND_MIN, 480).expect("load");
        let d = back.legacy("600519").unwrap();
        assert_eq!(d[0][0], "2026-10-22 14:59");
        assert_eq!(back.last_day("600519"), 20261022);
    }
}
