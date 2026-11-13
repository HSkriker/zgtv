//! `first_buy_events.py` 的 Rust 移植：历史一买事件回放（current_structure）。

use chrono::{DateTime, Utc};
use czsc_core::analyze::CZSC;
use czsc_core::objects::bi::BI;
use czsc_core::objects::direction::Direction;
use serde::{Deserialize, Serialize};

use crate::common::{date_str, py_round};

/// 与官方 `cxt_first_buy_v221126` 的 `for n in [21,19,...,5]` 完全一致。
pub const N_SEQ: [usize; 9] = [21, 19, 17, 15, 13, 11, 9, 7, 5];

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        crate::common::py_sum(xs) / (xs.len() as f64)
    }
}

/// 笔的「可知时刻」= 终点分型的确认根（与 `points::bi_confirm_dt` 同义）。
pub fn bi_confirm_dt(c: &CZSC, bi: &BI) -> DateTime<Utc> {
    match bi.fx_b.elements.last() {
        Some(nb) if bi.fx_b.elements.len() >= 3 => nb.dt,
        _ => c.bars_raw.last().map(|b| b.dt).unwrap_or_else(|| bi.fx_b.dt),
    }
}

/// 官方 `check_first_buy` 的等价移植（cxt.rs:188-245）。
pub fn check_first_buy(bis: &[BI]) -> bool {
    if bis.is_empty() || bis.len() % 2 != 1 {
        return false;
    }
    let last = &bis[bis.len() - 1];
    if last.direction != Direction::Down {
        return false;
    }
    if bis[0].direction != last.direction {
        return false;
    }

    let max_high = bis.iter().map(|b| b.get_high()).fold(f64::NEG_INFINITY, f64::max);
    let min_low = bis.iter().map(|b| b.get_low()).fold(f64::INFINITY, f64::min);
    if max_high != bis[0].get_high() || min_low != last.get_low() {
        return false;
    }

    let mut key_bis: Vec<&BI> = Vec::new();
    let mut i = 0usize;
    while i + 2 < bis.len() {
        // i==0 短路，不会越界取 bis[i-2]
        if i == 0 || bis[i].get_low() < bis[i - 2].get_low() {
            key_bis.push(&bis[i]);
        }
        i += 2;
    }
    if key_bis.is_empty() {
        return false;
    }

    let prev = &bis[bis.len() - 3];
    let key_price = mean(&key_bis.iter().map(|b| b.get_power_price()).collect::<Vec<_>>());
    let key_volume = mean(&key_bis.iter().map(|b| b.get_power_volume()).collect::<Vec<_>>());
    let key_length = mean(
        &key_bis
            .iter()
            .map(|b| b.get_length() as f64)
            .collect::<Vec<_>>(),
    );

    let bc_price = last.get_power_price() < prev.get_power_price().max(key_price);
    let bc_volume = last.get_power_volume() < prev.get_power_volume().max(key_volume);
    let bc_length = (last.get_length() as f64) < (prev.get_length() as f64).max(key_length);
    bc_price && (bc_volume || bc_length)
}

/// 在 bis 的尾部按官方 21..5 顺序找一买；返回命中的 n。
pub fn first_buy_n(bis: &[BI]) -> Option<usize> {
    for n in N_SEQ {
        if bis.len() < n {
            continue;
        }
        if check_first_buy(&bis[bis.len() - n..]) {
            return Some(n);
        }
    }
    None
}

/// 一条历史一买事件。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub pivot_dt: String,
    pub confirm_dt: String,
    pub low: f64,
    pub n: usize,
    pub bi_index: usize,
    pub raw: String,
}

/// 当前结构前缀回放：每个向下笔终点尝试一次一买判据。
pub fn replay(c: &CZSC) -> Vec<Event> {
    let bis = &c.bi_list;
    let mut out = Vec::new();
    for (i, bi) in bis.iter().enumerate() {
        if bi.direction != Direction::Down {
            continue;
        }
        let Some(n) = first_buy_n(&bis[..i + 1]) else {
            continue;
        };
        out.push(Event {
            pivot_dt: date_str(&bi.fx_b.dt),
            confirm_dt: date_str(&bi_confirm_dt(c, bi)),
            low: py_round(bi.fx_b.fx, 3),
            n,
            bi_index: i,
            raw: format!("一买_{n}笔_任意_0"),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_seq_order() {
        assert_eq!(N_SEQ, [21, 19, 17, 15, 13, 11, 9, 7, 5]);
    }

    #[test]
    fn empty_series() {
        assert!(!check_first_buy(&[]));
    }
}
