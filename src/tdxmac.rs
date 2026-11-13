//! 通达信 MAC（扩展行情）协议最小客户端 —— K 线（分钟/日）+ 板块/概念分类。
//!
//! 为什么不用标准协议：标准协议的 K 线命令在公共行情服务器上返回空体
//! （`声明 800 条记录但响应体仅 2 字节`），easytdx / pytdx / tdxrs 全部实测为空；
//! MAC 协议同一批服务器正常供数（1 分钟线带成交额、服务端支持前复权）。
//!
//! 协议要点（字节级复现自 easy-tdx-plus 的 MAC 实现，手工按字节对真实服务器验证过）：
//! - 连接后必须按序发 3 条握手命令，每条读完响应丢弃；
//! - 请求帧 = 10 字节头 `<BIBHH>`（head_flag=0x1C、customize=0、version=1、zipsize、unzipsize）
//!   + msg_id(u16 LE) + 命令体；
//! - K 线命令 msg_id=0x122E，命令体 46 字节 `<H22sHHIHHbbb bH4s>`：
//!   market(u16) + code(22B GBK，右侧补 0) + period(u16) + times(u16) + start(u32)
//!   + count(u16) + fq(u16) + 1 + 1 + 0 + 1 + 0(u16) + 4B 空；
//! - 响应 = 16 字节头 `<IIIHH>`（zipsize@12、unzipsize@14）+ body；
//!   MAC 的 K 线响应**未压缩**（zipsize == unzipsize），不等则直接报错而不是硬解；
//! - K 线 body = 33 字节头（count=u16@27、start=u32@29）+ count × 36 字节 `<II7f>`：
//!   ymd(u32)、time(u32，HH*3600+MM*60)、OHLC、amount(元)、vol(股)、流通股本(万股)。
//!
//! 口径：MAC 的 1 分钟线是**通达信自己的轴**（09:31–11:30 / 13:01–15:00，单日 240 根，
//! 无集合竞价伪行），与本地 `.lc1` 完全同轴；未复权价与本地 `.lc1` 原始值逐位一致。
//!
//! 板块/概念命令（2026-11-02 接入，实测字节级验证）：
//! - `0x1218` 个股所属板块 —— **head_flag=1**（不是 0x1C，这是该命令的特例）；
//!   请求体 `<H8s16x21s` = market + code(8B GBK) + 16B 填充 + "Stock_GLHQ"；
//!   响应 = 27 字节头 + **GBK 编码的 JSON 数组**，每行 9 或 13 个元素：
//!   [板块类型, 板块市场, 板块代码, 板块名, 板块指数, 前收, ...]（前 4 个是**字符串**）；
//! - `0x1231` 板块列表（head_flag=0x1C）：请求体 `<HHBBHH8x` = 页长 + 板块类型 +
//!   排序列 + 排序向 + 起始偏移 + 标志 + 8B 填充；响应 = count_all(u16) + total(u16)
//!   再跟记录，每条 160 字节 `<H6s16s44sfffH6s16s44sfff`（板块 + 领涨股各一份）；
//!   **count_all = 2 × 实际条数**（板块/领涨股各一份），实际条数 = count_all / 2；
//! - `0x122C` 板块成分股（head_flag=0x1C）：请求体 = `<I9xHIHBB`（板块码、排序、偏移、
//!   页长）+ 16 字节字段位图（全 0 = 不要附加字段）+ 4 字节控制区；
//!   响应 = 20 字节位图 + total(u32) + row_count(u16) + 行，**行宽 = 68 + 4 × 位图置位数**；
//! - **板块码要转换**：协议码 = 用户可见码 - 880000 + 20000（880564 → 20564），
//!   直接传 880564 服务端返回 0 行（实测）。
//! - 三个命令的响应**均未压缩**（zipsize == unzipsize），与 K 线响应一致。
//!
//! 板块类型（`0x1218` 行首字段，**不是** easy_tdx 的 BoardType 枚举，两套编号不同）：
//! 3=地区、4=概念、5=风格、12=行业（已用板块列表交叉验证：概念块落在 GN 分类集合内，
//! 地区块落在 DQ 集合内）。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// MAC 行情主机（easy-tdx-plus 的 MAC_HOSTS）+ 默认端口。
///
/// 顺序有讲究：前三台实测对同一标的同一根 bar 给出的**日汇总完全一致**
/// （600519 2026-10-28 日成交额 5,187,324,5xx 元、成交量 3,880,500 股，与本地 `.lc1` 一致），
/// 但**桶级分桶有微差**（tick 归桶时序不同，个别 5 分钟桶差 1~2 手）。
/// 因此固定「按此顺序取第一台连通的」，避免刷新时主机漂移导致同一根 bar 数值跳变。
pub const MAC_HOSTS: [&str; 3] = ["121.36.248.138", "123.60.47.136", "121.37.207.165"];
pub const MAC_PORT: u16 = 7709;

/// 握手命令（顺序固定，来自 pytdx 的 setup1/2/3，已在真实服务器验证）。
const SETUP_HEX: [&str; 3] = [
    "0c0218930001030003000d0001",
    "0c0218940001030003000d0002",
    "0c031899000120002000db0fd5d0c9ccd6a4a8af0000008fc22540130000d500c9ccbdf0d7ea00000002",
];

const MSG_ID_KLINE: u16 = 0x122E;
/// 个股所属板块（head_flag=1，见模块注释）。
const MSG_ID_BELONG_BOARD: u16 = 0x1218;
/// 板块列表。
const MSG_ID_BOARD_LIST: u16 = 0x1231;
/// 板块成分股。
const MSG_ID_BOARD_MEMBERS: u16 = 0x122C;
const HEAD_FLAG_MAC: u8 = 0x1C;
/// `0x1218` 专用的 head_flag（与 MAC 其余命令不同）。
const HEAD_FLAG_BELONG_BOARD: u8 = 0x01;
const RESP_HEADER_SIZE: usize = 16;
const BAR_HEADER_SIZE: usize = 33;
const BAR_SIZE: usize = 36;

/// `0x1218` 响应的板块类型（wire type，与 easy_tdx 的 BoardType 编号不同）。
pub const BOARD_TYPE_REGION: u16 = 3;
pub const BOARD_TYPE_CONCEPT: u16 = 4;
pub const BOARD_TYPE_STYLE: u16 = 5;
pub const BOARD_TYPE_INDUSTRY: u16 = 12;

/// `0x1231` 的板块分类过滤（与 easy_tdx BoardType 一致）。
pub const BOARD_CATEGORY_GN: u16 = 3;
pub const BOARD_CATEGORY_FG: u16 = 4;
pub const BOARD_CATEGORY_DQ: u16 = 5;

/// MAC 周期码（与 easy-tdx 的 Period 一致）。
pub const PERIOD_MIN_5: u16 = 0;
pub const PERIOD_MIN_15: u16 = 1;
pub const PERIOD_MIN_30: u16 = 2;
pub const PERIOD_MIN_60: u16 = 3;
pub const PERIOD_DAY: u16 = 4;
pub const PERIOD_MIN_1: u16 = 7;

/// 前复权（服务端算；不复权时传 0）。
pub const FQ_QFQ: u16 = 1;
/// 不复权（服务端返回原始成交价；MAC 的不复权日线与本地 `.day` 逐位一致）。
pub const FQ_NONE: u16 = 0;

/// 一根 K 线（价格元、成交量股、成交额元）。
#[derive(Clone, Debug)]
pub struct MacBar {
    /// `YYYY-MM-DD HH:MM`（日线只有日期部分）。
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// 成交额（元）。
    pub amount: f64,
    /// 成交量（股）。
    pub vol: f64,
}

/// 一个板块（`0x1218` 个股所属板块 / `0x1231` 板块列表）。
#[derive(Clone, Debug, PartialEq)]
pub struct MacBoard {
    /// 板块类型（3 地区 / 4 概念 / 5 风格 / 12 行业）。
    pub board_type: u16,
    /// 板块所在市场（88xxx 板块恒为 1）。
    pub market: u16,
    /// 板块代码（用户可见码，如 `880564`）。
    pub code: String,
    /// 板块名（GBK 解码后的中文）。
    pub name: String,
}

/// 板块成分股（`0x122C`）。
#[derive(Clone, Debug)]
pub struct MacMember {
    pub market: u16,
    pub code: String,
    pub name: String,
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i + 1 < b.len() {
        let hi = (b[i] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (b[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((hi << 4) | lo);
        i += 2;
    }
    out
}

/// 市场码：0=深 1=沪 2=北。
pub fn market_code(mkt: &str) -> u16 {
    match mkt {
        "sh" => 1,
        "bj" => 2,
        _ => 0,
    }
}

/// 周期名（`1`/`5`/`15`/`30`/`60`/`day`）→ MAC 周期码。
pub fn period_code(period: &str) -> Option<u16> {
    match period {
        "1" => Some(PERIOD_MIN_1),
        "5" => Some(PERIOD_MIN_5),
        "15" => Some(PERIOD_MIN_15),
        "30" => Some(PERIOD_MIN_30),
        "60" => Some(PERIOD_MIN_60),
        "day" => Some(PERIOD_DAY),
        _ => None,
    }
}

/// MAC 连接（已握手）。
pub struct MacConn {
    stream: TcpStream,
    host: String,
}

impl MacConn {
    /// 连上第一个能用的主机并完成握手。
    pub fn connect_any(timeout: Duration) -> Result<Self, String> {
        let mut errs: Vec<String> = Vec::new();
        for host in MAC_HOSTS.iter() {
            match Self::connect(host, MAC_PORT, timeout) {
                Ok(c) => return Ok(c),
                Err(e) => errs.push(format!("{host}: {e}")),
            }
        }
        Err(format!("MAC 全部主机不可用（{}）", errs.join("；")))
    }

    /// 实际连上的主机（对表脚本必须连同一台，否则桶级微差会被误判成实现不一致）。
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self, String> {
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("解析 {host}:{port} 失败: {e}"))?
            .next()
            .ok_or_else(|| format!("{host}:{port} 无地址"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
        stream.set_read_timeout(Some(timeout)).ok();
        stream.set_write_timeout(Some(timeout)).ok();
        stream.set_nodelay(true).ok();
        let mut conn = MacConn {
            stream,
            host: host.to_string(),
        };
        conn.handshake()?;
        Ok(conn)
    }

    /// 3 条握手命令，每条读完响应丢弃。任一条读失败即判定该主机不可用（避免帧错位）。
    fn handshake(&mut self) -> Result<(), String> {
        for hex in SETUP_HEX.iter() {
            let cmd = hex_bytes(hex);
            self.stream
                .write_all(&cmd)
                .map_err(|e| format!("握手写入失败: {e}"))?;
            // 握手响应只丢弃，不解码（其 body 可能是压缩帧，与 K 线响应不同）
            self.read_frame_raw().map_err(|e| format!("握手响应读取失败: {e}"))?;
        }
        Ok(())
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| format!("读取 {n} 字节失败: {e}"))?;
        Ok(buf)
    }

    /// 读一帧：16 字节头 + body，不做压缩判断（握手用）。
    fn read_frame_raw(&mut self) -> Result<Vec<u8>, String> {
        let hdr = self.read_exact(RESP_HEADER_SIZE)?;
        let zipsize = u16::from_le_bytes([hdr[12], hdr[13]]) as usize;
        self.read_exact(zipsize)
    }

    /// 读一帧：16 字节头 + body。MAC 的 K 线响应未压缩，压缩则直接报错。
    fn read_frame(&mut self) -> Result<Vec<u8>, String> {
        let hdr = self.read_exact(RESP_HEADER_SIZE)?;
        let zipsize = u16::from_le_bytes([hdr[12], hdr[13]]) as usize;
        let unzipsize = u16::from_le_bytes([hdr[14], hdr[15]]) as usize;
        let body = self.read_exact(zipsize)?;
        if zipsize != unzipsize {
            return Err(format!(
                "MAC 响应体被压缩（zipsize={zipsize} unzipsize={unzipsize}），本实现只处理未压缩帧"
            ));
        }
        Ok(body)
    }

    /// 取 K 线。`start` 是偏移（0 = 最新一根），`count` 服务端可能自行截断（实测上限 700）。
    pub fn bars(
        &mut self,
        market: u16,
        code: &str,
        period: u16,
        start: u32,
        count: u16,
        fq: u16,
    ) -> Result<Vec<MacBar>, String> {
        // 命令体：market + code(22B GBK) + period + times + start + count + fq + 4B + 0(u16) + 4B
        let mut code_field = [0u8; 22];
        let (encoded, _, _) = encoding_rs::GBK.encode(code);
        let n = encoded.len().min(22);
        code_field[..n].copy_from_slice(&encoded[..n]);
        let mut body: Vec<u8> = Vec::with_capacity(46);
        body.extend_from_slice(&market.to_le_bytes());
        body.extend_from_slice(&code_field);
        body.extend_from_slice(&period.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes()); // times
        body.extend_from_slice(&start.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        body.extend_from_slice(&fq.to_le_bytes());
        body.extend_from_slice(&[1u8, 1u8, 0u8, 1u8]);
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&[0u8; 4]);

        let resp = self.request(MSG_ID_KLINE, &body, HEAD_FLAG_MAC)?;
        parse_bars(&resp, period)
    }

    /// 个股所属板块（`0x1218`，head_flag=1）。
    ///
    /// 返回该股所属的全部板块（地区/概念/风格/行业混在一起，按 `board_type` 区分）。
    pub fn belong_board(&mut self, market: u16, code: &str) -> Result<Vec<MacBoard>, String> {
        // <H8s16x21s = market + code(8B GBK) + 16B 填充 + "Stock_GLHQ"(21B，右侧补 0)
        let mut code_field = [0u8; 8];
        let (encoded, _, _) = encoding_rs::GBK.encode(code);
        let n = encoded.len().min(8);
        code_field[..n].copy_from_slice(&encoded[..n]);
        let mut body: Vec<u8> = Vec::with_capacity(47);
        body.extend_from_slice(&market.to_le_bytes());
        body.extend_from_slice(&code_field);
        body.extend_from_slice(&[0u8; 16]);
        let mut tag = [0u8; 21];
        let src = b"Stock_GLHQ";
        tag[..src.len()].copy_from_slice(src);
        body.extend_from_slice(&tag);

        let resp = self.request(MSG_ID_BELONG_BOARD, &body, HEAD_FLAG_BELONG_BOARD)?;
        parse_belong_board(&resp)
    }

    /// 板块列表（`0x1231`）。`board_type` 用 `BOARD_CATEGORY_*`，`start` 是偏移。
    ///
    /// 返回 (本页板块, 服务端总条数)；服务端每页最多 150 条，返回条数可能少于请求页长（末页）。
    pub fn board_list(
        &mut self,
        board_type: u16,
        start: u32,
        page: u16,
    ) -> Result<(Vec<MacBoard>, u16), String> {
        // <HHBBHH8x = 页长 + 板块类型 + 排序列 + 排序向 + 起始偏移(u16) + 标志 + 8B 填充
        if start > u16::MAX as u32 {
            return Err(format!("板块列表起始偏移过大: {start}"));
        }
        let mut body: Vec<u8> = Vec::with_capacity(18);
        body.extend_from_slice(&page.to_le_bytes());
        body.extend_from_slice(&board_type.to_le_bytes());
        body.push(0); // 排序列：0 = 涨速
        body.push(0); // 排序向
        body.extend_from_slice(&(start as u16).to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes()); // 标志
        body.extend_from_slice(&[0u8; 8]);

        let resp = self.request(MSG_ID_BOARD_LIST, &body, HEAD_FLAG_MAC)?;
        parse_board_list(&resp, board_type)
    }

    /// 板块成分股（`0x122C`）。`board_code` 是**协议码**（见 `board_protocol_code`）。
    ///
    /// 返回 (本页成分股, 服务端总只数)。
    pub fn board_members(
        &mut self,
        board_code: u32,
        start: u32,
        page: u16,
    ) -> Result<(Vec<MacMember>, u32), String> {
        // <I9xHIHBB = 板块码 + 9B 填充 + 排序类型 + 起始偏移 + 页长 + 排序向 + 填充
        let mut body: Vec<u8> = Vec::with_capacity(43);
        body.extend_from_slice(&board_code.to_le_bytes());
        body.extend_from_slice(&[0u8; 9]);
        body.extend_from_slice(&3u16.to_le_bytes()); // 排序类型：涨速
        body.extend_from_slice(&start.to_le_bytes());
        body.extend_from_slice(&page.to_le_bytes());
        body.push(0); // 排序向
        body.push(0); // 填充
        body.extend_from_slice(&[0u8; 16]); // 字段位图：全 0 = 不取附加字段
        body.extend_from_slice(&[0u8, 0u8, 0u8, 1u8]); // 控制区

        let resp = self.request(MSG_ID_BOARD_MEMBERS, &body, HEAD_FLAG_MAC)?;
        parse_board_members(&resp)
    }

    /// 发一帧请求并读回响应体（未压缩）。
    fn request(&mut self, msg_id: u16, body: &[u8], head_flag: u8) -> Result<Vec<u8>, String> {
        let mut inner: Vec<u8> = Vec::with_capacity(2 + body.len());
        inner.extend_from_slice(&msg_id.to_le_bytes());
        inner.extend_from_slice(body);

        let mut frame: Vec<u8> = Vec::with_capacity(10 + inner.len());
        frame.push(head_flag);
        frame.extend_from_slice(&0u32.to_le_bytes()); // customize
        frame.push(0x01); // version
        frame.extend_from_slice(&(inner.len() as u16).to_le_bytes());
        frame.extend_from_slice(&(inner.len() as u16).to_le_bytes());
        frame.extend_from_slice(&inner);

        self.stream
            .write_all(&frame)
            .map_err(|e| format!("MAC 请求写入失败: {e}"))?;
        self.read_frame()
    }
}

/// 板块码：用户可见码 → 协议码（`880564` → `20564`）。
///
/// 直传 `880564` 服务端返回 0 行（实测），必须按 opentdx 的规则换算。
pub fn board_protocol_code(code: &str) -> u32 {
    let s = code.trim();
    let n: u32 = s.parse().unwrap_or(0);
    if s.len() == 6 {
        if s.starts_with("88") {
            return n - 880_000 + 20_000;
        }
        if s.starts_with("399") {
            return n - 399_000 + 30_000;
        }
        if s.starts_with("899") {
            return n - 899_000 + 32_000;
        }
        if s.starts_with("000") {
            return 31_000 + n;
        }
    }
    n
}

/// 解析 `0x1218` 响应：27 字节头 + GBK JSON 数组。
///
/// 每行 9 或 13 个元素，前 4 个是**字符串**：类型、市场、板块代码、板块名。
fn parse_belong_board(body: &[u8]) -> Result<Vec<MacBoard>, String> {
    if body.len() < 27 {
        return Err(format!("MAC 所属板块响应过短: {} 字节", body.len()));
    }
    let text = encoding_rs::GBK
        .decode(&body[27..])
        .0
        .into_owned();
    let rows: Vec<Vec<serde_json::Value>> = serde_json::from_str(&text)
        .map_err(|e| format!("MAC 所属板块 JSON 解析失败: {e}（前 80 字: {}）", &text[..text.len().min(80)]))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        if row.len() != 9 && row.len() != 13 {
            continue;
        }
        let Some(board_type) = json_u16(&row[0]) else { continue };
        let market = json_u16(&row[1]).unwrap_or(0);
        let code = json_str(&row[2]);
        let name = json_str(&row[3]);
        if code.is_empty() || name.is_empty() {
            continue;
        }
        out.push(MacBoard {
            board_type,
            market,
            code,
            name,
        });
    }
    Ok(out)
}

/// 解析 `0x1231` 响应：count_all(u16) + total(u16) + 160 字节/条。
///
/// `count_all = 2 × 实际条数`（板块 + 领涨股各一份），实际条数 = count_all / 2。
/// 返回的 `board_type` 是**请求的分类**（`BOARD_CATEGORY_*`），与 `belong_board`
/// 的 wire type 不是同一套编号，别混用。
fn parse_board_list(body: &[u8], board_type: u16) -> Result<(Vec<MacBoard>, u16), String> {
    if body.len() < 4 {
        return Err(format!("MAC 板块列表响应过短: {} 字节", body.len()));
    }
    let count_all = u16::from_le_bytes([body[0], body[1]]) as usize;
    let total = u16::from_le_bytes([body[2], body[3]]);
    let count = count_all / 2;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = 4 + i * 160;
        if off + 160 > body.len() {
            break;
        }
        let market = u16::from_le_bytes([body[off], body[off + 1]]);
        let code = gbk_str(&body[off + 2..off + 8]);
        let name = gbk_str(&body[off + 24..off + 68]);
        if code.is_empty() || name.is_empty() {
            continue;
        }
        out.push(MacBoard {
            board_type,
            market,
            code,
            name,
        });
    }
    Ok((out, total))
}

/// 解析 `0x122C` 响应：20 字节位图 + total(u32) + row_count(u16) + 行。
///
/// 行宽 = 68 + 4 × 位图置位数（位图全 0 时就是 68）。
fn parse_board_members(body: &[u8]) -> Result<(Vec<MacMember>, u32), String> {
    if body.len() < 26 {
        return Err(format!("MAC 成分股响应过短: {} 字节", body.len()));
    }
    let total = u32::from_le_bytes([body[20], body[21], body[22], body[23]]);
    let field_count = body[0..16].iter().map(|b| b.count_ones() as usize).sum::<usize>();
    let row_count = u16::from_le_bytes([body[24], body[25]]) as usize;
    let row_len = 68 + field_count * 4;
    let mut out = Vec::with_capacity(row_count);
    for i in 0..row_count {
        let rs = 26 + i * row_len;
        if rs + 68 > body.len() {
            break;
        }
        let market = u16::from_le_bytes([body[rs], body[rs + 1]]);
        let code = gbk_str(&body[rs + 2..rs + 24]);
        let name = gbk_str(&body[rs + 24..rs + 68]);
        if code.is_empty() {
            continue;
        }
        out.push(MacMember { market, code, name });
    }
    Ok((out, total))
}

/// JSON 里的数字（服务端给的是字符串，如 `"3"`）。
fn json_u16(v: &serde_json::Value) -> Option<u16> {
    match v {
        serde_json::Value::String(s) => s.trim().parse::<u16>().ok(),
        serde_json::Value::Number(n) => n.as_u64().map(|x| x as u16),
        _ => None,
    }
}

/// JSON 里的字符串（数字也接受，转成文本）。
fn json_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// GBK 字节串 → 去掉尾部 NUL 的文本。
fn gbk_str(b: &[u8]) -> String {
    encoding_rs::GBK
        .decode(b)
        .0
        .into_owned()
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

/// 解析 K 线响应体（33 字节头 + count × 36 字节）。
fn parse_bars(body: &[u8], period: u16) -> Result<Vec<MacBar>, String> {
    if body.len() < BAR_HEADER_SIZE {
        return Err(format!("MAC 响应体过短: {} 字节", body.len()));
    }
    let count = u16::from_le_bytes([body[27], body[28]]) as usize;
    let n = count.min((body.len() - BAR_HEADER_SIZE) / BAR_SIZE);
    let intraday = period < PERIOD_DAY || period == PERIOD_MIN_1;
    let mut out: Vec<MacBar> = Vec::with_capacity(n);
    for i in 0..n {
        let off = BAR_HEADER_SIZE + i * BAR_SIZE;
        let ymd = u32::from_le_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        if !(19900101..=20991231).contains(&ymd) {
            continue;
        }
        let t = u32::from_le_bytes([body[off + 4], body[off + 5], body[off + 6], body[off + 7]]);
        let f = |k: usize| -> f64 {
            let p = off + 8 + k * 4;
            f32::from_le_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]) as f64
        };
        let date = if intraday && t > 0 {
            let hh = t / 3600;
            let mm = (t % 3600) / 60;
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                ymd / 10000,
                (ymd % 10000) / 100,
                ymd % 100,
                hh,
                mm
            )
        } else {
            format!("{:04}-{:02}-{:02}", ymd / 10000, (ymd % 10000) / 100, ymd % 100)
        };
        out.push(MacBar {
            date,
            open: f(0),
            high: f(1),
            low: f(2),
            close: f(3),
            amount: f(4),
            vol: f(5),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses() {
        assert_eq!(hex_bytes("0c02"), vec![0x0c, 0x02]);
        assert_eq!(hex_bytes("ff"), vec![0xff]);
    }

    #[test]
    fn market_and_period_mapping() {
        assert_eq!(market_code("sh"), 1);
        assert_eq!(market_code("sz"), 0);
        assert_eq!(market_code("bj"), 2);
        assert_eq!(period_code("1"), Some(PERIOD_MIN_1));
        assert_eq!(period_code("5"), Some(PERIOD_MIN_5));
        assert_eq!(period_code("day"), Some(PERIOD_DAY));
        assert_eq!(period_code("week"), None);
    }

    /// 响应体结构：33 字节头 + 36 字节/根（手工构造，验证字段偏移与时间还原）。
    #[test]
    fn parses_bar_layout() {
        let mut body = vec![0u8; BAR_HEADER_SIZE + BAR_SIZE];
        body[27] = 1; // count = 1
        let off = BAR_HEADER_SIZE;
        let ymd: u32 = 20261029;
        let t: u32 = 35100; // 09:45
        body[off..off + 4].copy_from_slice(&ymd.to_le_bytes());
        body[off + 4..off + 8].copy_from_slice(&t.to_le_bytes());
        let vals = [20.89f32, 20.92, 20.87, 20.91, 10_236_384.0, 490_000.0, 495_639.0];
        for (k, v) in vals.iter().enumerate() {
            let p = off + 8 + k * 4;
            body[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let bars = parse_bars(&body, PERIOD_MIN_1).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].date, "2026-10-29 09:45");
        assert!((bars[0].close - 20.91).abs() < 1e-6);
        assert!((bars[0].amount - 10_236_384.0).abs() < 1e-3);
        assert!((bars[0].vol - 490_000.0).abs() < 1e-3);
    }

    /// 板块码换算：880xxx → 20xxx（直传 880564 服务端返 0 行）。
    #[test]
    fn board_protocol_code_converts() {
        assert_eq!(board_protocol_code("880564"), 20_564);
        assert_eq!(board_protocol_code("881218"), 21_218);
        assert_eq!(board_protocol_code(" 880732 "), 20_732);
        // 非 88 开头保持原值
        assert_eq!(board_protocol_code("600519"), 600_519);
    }

    /// `0x1218` 响应：27 字节头 + GBK JSON（类型是字符串）。
    #[test]
    fn parses_belong_board_json() {
        let json = r#"[["3","1","880229","贵州板块","7501.51","7271.26","0","0","0"],["4","1","880564","白酒概念","1","2","3","4","5"]]"#;
        let mut body = vec![0u8; 27];
        let (enc, _, _) = encoding_rs::GBK.encode(json);
        body.extend_from_slice(&enc);
        let boards = parse_belong_board(&body).unwrap();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].board_type, BOARD_TYPE_REGION);
        assert_eq!(boards[0].code, "880229");
        assert_eq!(boards[0].name, "贵州板块");
        assert_eq!(boards[1].board_type, BOARD_TYPE_CONCEPT);
        assert_eq!(boards[1].code, "880564");
        assert_eq!(boards[1].name, "白酒概念");
    }

    /// `0x1231` 响应：count_all = 2 × 实际条数，160 字节/条。
    #[test]
    fn parses_board_list_layout() {
        let mut body = vec![0u8; 4 + 160];
        body[0..2].copy_from_slice(&2u16.to_le_bytes()); // count_all = 2 → 实际 1 条
        body[2..4].copy_from_slice(&269u16.to_le_bytes()); // total
        let (enc, _, _) = encoding_rs::GBK.encode("880732");
        body[4 + 2..4 + 8].copy_from_slice(&enc);
        let (enc, _, _) = encoding_rs::GBK.encode("AI智能体");
        body[4 + 24..4 + 24 + enc.len()].copy_from_slice(&enc);
        let (boards, total) = parse_board_list(&body, BOARD_CATEGORY_GN).unwrap();
        assert_eq!(total, 269);
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].code, "880732");
        assert_eq!(boards[0].name, "AI智能体");
        assert_eq!(boards[0].board_type, BOARD_CATEGORY_GN);
    }

    /// `0x122C` 响应：行宽 68 + 4 × 位图置位数。
    #[test]
    fn parses_board_members_layout() {
        let mut body = vec![0u8; 26 + 68];
        body[20..24].copy_from_slice(&1u32.to_le_bytes()); // total
        body[24..26].copy_from_slice(&1u16.to_le_bytes()); // row_count
        body[26..28].copy_from_slice(&0u16.to_le_bytes()); // market = 0（深）
        let (enc, _, _) = encoding_rs::GBK.encode("000338");
        body[28..28 + enc.len()].copy_from_slice(&enc);
        let (enc, _, _) = encoding_rs::GBK.encode("潍柴动力");
        body[50..50 + enc.len()].copy_from_slice(&enc);
        let (members, total) = parse_board_members(&body).unwrap();
        assert_eq!(total, 1);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].market, 0);
        assert_eq!(members[0].code, "000338");
        assert_eq!(members[0].name, "潍柴动力");
    }
}

#[cfg(test)]
mod net {
    use super::*;

    /// 联网冒烟 · 日线（默认跳过）：`cargo test --bin tv tdxmac::net -- --ignored --nocapture`
    ///
    /// 不复权日线必须与本地 `.day` **逐位一致**（tv 的日线替代源就是靠这条性质
    /// 用本地事件法复权到同一基）；这里只验「能取到 + 末根是最近交易日 + 根数够」。
    #[test]
    #[ignore]
    fn live_daily_bars() {
        let mut c = MacConn::connect_any(Duration::from_secs(8)).expect("connect");
        let bars = c
            .bars(1, "600519", PERIOD_DAY, 0, 700, FQ_NONE)
            .expect("bars");
        println!("daily bars={} host={}", bars.len(), c.host());
        assert!(bars.len() >= 300, "日线根数太少: {}", bars.len());
        let last = bars.last().expect("末根");
        println!("  末根 {} close={:.2}", last.date, last.close);
        assert!(last.date.len() == 10, "日线日期应只有日期部分: {}", last.date);
    }

    /// 联网冒烟（默认跳过）：`cargo test --bin tv tdxmac::net -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_minute_bars() {
        let mut c = MacConn::connect_any(Duration::from_secs(8)).expect("connect");
        let bars = c.bars(0, "000338", PERIOD_MIN_1, 0, 700, FQ_QFQ).expect("bars");
        println!("bars={}", bars.len());
        for b in bars.iter().take(3).chain(bars.iter().rev().take(3)) {
            println!("  {} o={:.4} h={:.4} l={:.4} c={:.4} amt={:.0} vol={:.0}", b.date, b.open, b.high, b.low, b.close, b.amount, b.vol);
        }
        assert!(!bars.is_empty());
    }

    /// 联网冒烟 · 个股所属板块（默认跳过）。
    ///
    /// 判据是**双向**的：正向必须命中真概念（000338 有「固态电池」这个 2022 年就有的老概念，
    /// 也有「AI智能体」这种新概念）；反向必须**不**把地区/风格/行业当成概念
    /// （「山东板块」是 type=3、「高分红股」是 type=5、「汽车零部件」是 type=12）。
    #[test]
    #[ignore]
    fn live_belong_board() {
        let mut c = MacConn::connect_any(Duration::from_secs(8)).expect("connect");
        let boards = c.belong_board(0, "000338").expect("belong_board");
        println!("host={} boards={}", c.host(), boards.len());
        for b in boards.iter() {
            println!("  type={} code={} name={}", b.board_type, b.code, b.name);
        }
        assert!(boards.len() >= 5, "板块数太少: {}", boards.len());
        let concepts: Vec<&str> = boards
            .iter()
            .filter(|b| b.board_type == BOARD_TYPE_CONCEPT)
            .map(|b| b.name.as_str())
            .collect();
        assert!(concepts.contains(&"固态电池"), "正向：应命中固态电池概念");
        assert!(concepts.contains(&"AI智能体"), "正向：应命中 AI智能体（近年新概念）");
        // 反向：非概念类型不得混进概念列表
        for name in ["山东板块", "高分红股", "汽车零部件"] {
            assert!(!concepts.contains(&name), "反向：{name} 不应出现在概念列表");
        }
    }

    /// 联网冒烟 · 板块列表 + 成分股（默认跳过）。
    ///
    /// 板块码必须过 `board_protocol_code` 换算（880564 → 20564），直传会返 0 行。
    #[test]
    #[ignore]
    fn live_board_list_and_members() {
        let mut c = MacConn::connect_any(Duration::from_secs(8)).expect("connect");
        let (boards, total) = c
            .board_list(BOARD_CATEGORY_GN, 0, 150)
            .expect("board_list");
        println!("  概念板块总数={total}");
        println!("host={} 概念板块(第1页)={}", c.host(), boards.len());
        assert!(!boards.is_empty());
        let target = boards
            .iter()
            .find(|b| b.name == "AI智能体")
            .expect("概念列表应含 AI智能体");
        println!("  AI智能体 code={} protocol={}", target.code, board_protocol_code(&target.code));
        let (members, mtotal) = c
            .board_members(board_protocol_code(&target.code), 0, 80)
            .expect("board_members");
        println!("  成分股总数={mtotal}");
        println!("  成分股 {} 只", members.len());
        for m in members.iter().take(5) {
            println!("    mkt={} {} {}", m.market, m.code, m.name);
        }
        assert!(!members.is_empty(), "成分股为空（板块码没换算？）");
        assert!(
            members.iter().any(|m| m.code.chars().all(|c| c.is_ascii_digit()) && m.code.len() == 6),
            "成分股代码应为 6 位数字"
        );
    }
}
