//! gbbq（股本变迁）解析 —— 与 `easy_tdx/offline/gbbq.py` 的 `read_gbbq` 位级等价。
//!
//! 文件格式：`u32 count` + count × 29 字节记录。每条记录前 24 字节为 3 轮
//! 8 字节 XOR 置换加密，后 5 字节明文。
//!
//! 查表常量 `gbbq_keys.bin` 提取自 vendored `easy_tdx/offline/gbbq.py::_HEX_KEY`
//! （4176 字节，纯数据表；easy-tdx-plus 为 MIT，其 NOTICE 自陈该 key 移植自 pytdx）。
//! 改动该表会改变解码结果。署名见 `THIRD-PARTY-NOTICES.md` §2.5。

use crate::error::{Result, ZgError};

const KEYS: &[u8] = include_bytes!("gbbq_keys.bin");

#[derive(Clone, Debug, PartialEq)]
pub struct GbbqRecord {
    pub market: u8,
    pub code: String,
    pub datetime: u32,
    pub category: u8,
    pub hongli_panqianliutong: f32,
    pub peigujia_qianzongguben: f32,
    pub songgu_qianzongguben: f32,
    pub peigu_houzongguben: f32,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn key32(off: usize) -> u32 {
    u32_at(KEYS, off)
}

/// 解码单条 24 字节加密块 → 8 字节明文（调用 3 次，对应 Python 的 3 轮）。
fn decode_block(content: &[u8], off: usize, clear: &mut Vec<u8>) {
    let mut eax = key32(0x44);
    let mut ebx = u32_at(content, off);
    let mut num = eax ^ ebx;
    let mut numold = u32_at(content, off + 4);

    let mut j: i32 = 0x40;
    while j >= 0x04 {
        ebx = (num & 0x00FF_0000) >> 16;
        eax = key32(ebx as usize * 4 + 0x448);
        ebx = num >> 24;
        eax = eax.wrapping_add(key32(ebx as usize * 4 + 0x48));
        ebx = (num & 0x0000_FF00) >> 8;
        eax ^= key32(ebx as usize * 4 + 0x848);
        ebx = num & 0xFF;
        eax = eax.wrapping_add(key32(ebx as usize * 4 + 0xC48));
        eax ^= key32(j as usize);
        ebx = num;
        num = numold ^ eax;
        numold = ebx;
        j -= 4;
    }
    numold ^= key32(0);
    clear.extend_from_slice(&numold.to_le_bytes());
    clear.extend_from_slice(&num.to_le_bytes());
}

/// 从本地 gbbq 文件读取股本变迁数据（XOR 加密）。
pub fn read_gbbq(path: &std::path::Path) -> Result<Vec<GbbqRecord>> {
    let content = std::fs::read(path)?;
    if content.len() < 4 {
        return Ok(Vec::new());
    }
    let count = u32_at(&content, 0) as usize;
    // 每次记录固定 29 字节（3×8 加密块 + 5 字节尾部）。
    // 先按文件长度钳死再预分配：TDX 重写 gbbq 期间读到半截文件时头部 count 可能是垃圾大值，
    // 直接 with_capacity(4e9) 会因分配失败 abort（不可捕获），整个进程退出。
    let max_records = content.len().saturating_sub(4) / 29;
    let mut out = Vec::with_capacity(count.min(max_records));
    let mut off = 4usize;

    for idx in 0..count {
        let mut clear: Vec<u8> = Vec::with_capacity(29);
        for _ in 0..3 {
            if off + 8 > content.len() {
                return Err(ZgError::bridge(format!(
                    "gbbq 不可用: 解析失败（记录 {idx} 截断）"
                )));
            }
            decode_block(&content, off, &mut clear);
            off += 8;
        }
        if off + 5 > content.len() {
            return Err(ZgError::bridge(format!(
                "gbbq 不可用: 解析失败（记录 {idx} 尾部截断）"
            )));
        }
        clear.extend_from_slice(&content[off..off + 5]);
        off += 5;

        let market = clear[0];
        let code_raw = &clear[1..8];
        let code_end = code_raw.iter().position(|b| *b == 0).unwrap_or(7);
        let code = String::from_utf8_lossy(&code_raw[..code_end]).to_string();
        let datetime = u32::from_le_bytes([clear[8], clear[9], clear[10], clear[11]]);
        let category = clear[12];
        let f = |o: usize| f32::from_le_bytes([clear[o], clear[o + 1], clear[o + 2], clear[o + 3]]);
        out.push(GbbqRecord {
            market,
            code,
            datetime,
            category,
            hongli_panqianliutong: f(13),
            peigujia_qianzongguben: f(17),
            songgu_qianzongguben: f(21),
            peigu_houzongguben: f(25),
        });
    }
    Ok(out)
}
