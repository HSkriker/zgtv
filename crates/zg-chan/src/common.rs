//! 缠论层公共小工具：日期格式化、方向名、Python `round` 等价。
//!
//! 一切数值/文案都要与 Python oracle 逐字节相同，故这里只做**翻译**，
//! 不做任何"更合理"的改造。

use chrono::{DateTime, Datelike, Timelike, Utc};
use czsc_core::objects::direction::Direction;
use czsc_core::objects::freq::Freq;

pub use zg_core::model::py_round;

/// Python `str(dt)[:10]` —— `YYYY-MM-DD`。
pub fn date_str(dt: &DateTime<Utc>) -> String {
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

/// Python `str(dt)[:16]` —— `YYYY-MM-DD HH:MM`。
pub fn datetime_str(dt: &DateTime<Utc>) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

/// `_d(value, intraday)` 的 Rust 版。
pub fn fmt_dt(dt: &DateTime<Utc>, intraday: bool) -> String {
    if intraday {
        datetime_str(dt)
    } else {
        date_str(dt)
    }
}

/// `Direction.name`（Python 侧 `.name` = "Up"/"Down"）。
pub fn dir_name(d: Direction) -> &'static str {
    match d {
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

/// Python `Freq.name`（= Rust variant 名，如 `D`/`F30`/`Tick`）。
pub fn freq_name(f: Freq) -> &'static str {
    match f {
        Freq::Tick => "Tick",
        Freq::F1 => "F1",
        Freq::F2 => "F2",
        Freq::F3 => "F3",
        Freq::F4 => "F4",
        Freq::F5 => "F5",
        Freq::F6 => "F6",
        Freq::F10 => "F10",
        Freq::F12 => "F12",
        Freq::F15 => "F15",
        Freq::F20 => "F20",
        Freq::F30 => "F30",
        Freq::F60 => "F60",
        Freq::F120 => "F120",
        Freq::F240 => "F240",
        Freq::F360 => "F360",
        Freq::D => "D",
        Freq::W => "W",
        Freq::M => "M",
        Freq::S => "S",
        Freq::Y => "Y",
    }
}

/// Python `round(float(x), n)`；`None` 透传。
pub fn round_opt(x: Option<f64>, digits: usize) -> Option<f64> {
    x.map(|v| py_round(v, digits))
}

/// 朴素从左到右累加（numpy 小数组 `pairwise_sum` 的实现就是它）。
pub fn seq_sum(xs: &[f64]) -> f64 {
    let mut acc = 0.0;
    for x in xs {
        acc += *x;
    }
    acc
}

/// Python 3.12+ `sum(list_of_float)` —— **Neumaier 补偿求和**（gh-100425）。
///
/// 不能写成朴素累加：CPython 3.12 起内建 `sum()` 对 float 走带补偿的加法，
/// 实测 2 万组随机小数组里约 45% 与朴素累加**不逐位相等**，
/// 平均力度 `:.2f` 就可能在 `.xx5` 边界上翻 0.01（工单 P2-2 实锤：
/// `前段平均力度=7.54` vs `7.53`）。
pub fn py_sum(xs: &[f64]) -> f64 {
    let mut f = 0.0f64;
    let mut c = 0.0f64;
    for &x in xs {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    f + c
}

/// npy_pairwise_sum：numpy/pandas 的 `Series.sum()` 使用的成对求和。
///
/// 逐行对照 numpy `loops_utils.h.src::pairwise_sum_FLOAT`（PW_BLOCKSIZE=128）：
/// 小于 8 直接顺序加；小于 128 用 8 个累加器再按 `((r0+r1)+(r2+r3))+((r4+r5)+(r6+r7))`
/// 归并；更大则二分（中点向下对齐到 8 的倍数）。**不要**换成朴素循环 ——
/// MACD 面积是判断背驰的承重数值，1 ulp 就可能在临界点上翻结论。
pub fn pairwise_sum(a: &[f64]) -> f64 {
    const PW_BLOCKSIZE: usize = 128;
    let n = a.len();
    if n < 8 {
        return seq_sum(a);
    }
    if n < PW_BLOCKSIZE {
        let mut r = [0.0f64; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for k in 0..8 {
                r[k] += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    pairwise_sum(&a[..n2]) + pairwise_sum(&a[n2..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let dt = DateTime::parse_from_rfc3339("2023-10-20T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(date_str(&dt), "2023-10-20");
        assert_eq!(datetime_str(&dt), "2023-10-20 00:00");
    }

    #[test]
    fn py_sum_is_neumaier() {
        // CPython 3.12+ 的 sum()：朴素累加 0.30000000000000004，补偿后 0.3
        // 向量取自 Python 3.14 实测：sum([2.56,6.7,10.41,7.53])=27.2，
        // 朴素累加 = 27.200000000000003（差 3.55e-15，足以让 :.2f 翻 0.01）。
        let xs = [2.56, 6.7, 10.41, 7.53];
        assert_eq!(seq_sum(&xs), 27.200000000000003);
        assert_eq!(py_sum(&xs), 27.2);
        // 短数组（n<8）numpy 走朴素累加，两者故意不同 —— 不可串用。
        assert_eq!(py_sum(&[0.1, 0.2]), 0.30000000000000004);
    }

    #[test]
    fn pairwise_matches_small_sequential() {
        let a = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(pairwise_sum(&a), seq_sum(&a));
        // 大数组只验证与自身分块一致（Python 侧有专门对表）
        let b: Vec<f64> = (0..1000).map(|i| (i as f64) * 0.001).collect();
        assert!(pairwise_sum(&b).is_finite());
    }
}
