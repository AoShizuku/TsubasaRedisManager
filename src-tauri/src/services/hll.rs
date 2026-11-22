//! HyperLogLog（HLL）子形态的解析。
//!
//! HLL 不是第七种类型：`TYPE` 返回的就是 string，它只是带固定头部的字节串。
//! 16 字节头 = `"HYLL"` 魔数(4) + encoding(1，0=dense/1=sparse) + 保留(3，恒零)
//! + 基数缓存(8，u64 小端，**最高 bit 是失效标记**)，其后是寄存器数据。
//!
//! 6-bit 解包与 sparse 的 ZERO/XZERO/VAL 操作码是 Redis 实现知识，
//! 按 `doc/architecture.md`「前后端分层」收在 Rust —— 前端拿到的是
//! 展开成 16,384 个值的规范数组，dense 与 sparse 在界面上因此没有分支。

use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;

use crate::models::HllMeta;

pub const HLL_HEADER_LEN: usize = 16;
pub const HLL_REGISTERS: usize = 16384;
/// dense 寄存器区的字节数：16384 × 6 bit = 12288 B，总长恒为 12304 B。
pub const HLL_DENSE_REGISTERS_LEN: usize = HLL_REGISTERS * 6 / 8;

const MAGIC: &[u8; 4] = b"HYLL";
const ENCODING_DENSE: u8 = 0;
const ENCODING_SPARSE: u8 = 1;

/// 头部解析结果。`cached_card` 已剥掉失效标记位。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HllHeaderInfo {
    /// "dense" | "sparse"
    pub encoding: String,
    pub cached_card: u64,
    /// 最高 bit 为 1 表示缓存自上次写入后未重算，读它得不到可信基数。
    pub cache_stale: bool,
}

/// `redis_inspect_hll` 的返回。所有字段都按"能解析到哪一层"降级：
/// 伪魔数 → `header` 为 None；sparse 操作码损坏 → `registers_base64` 为 None；
/// dense 被截断 → 数组仍是 16,384 长（缺失段补零），`valid_registers` 说明边界。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HllInspect {
    pub header: Option<HllHeaderInfo>,
    /// 展开后的寄存器，一字节一个值（0–63），恒为 16,384 长，base64 编码。
    pub registers_base64: Option<String>,
    /// 可信的寄存器个数。dense 截断时小于 16,384，前端把缺失段画成占位。
    pub valid_registers: u32,
    pub total_registers: u32,
    /// 服务端 `STRLEN` 报告的实际字节数（可能大于本次取回的窗口）。
    pub actual_bytes: u64,
    /// 该 encoding 的期望总字节数。只有 dense 是定长的；sparse 为 None。
    pub expected_bytes: Option<u64>,
}

fn parse_header(bytes: &[u8]) -> Option<(u8, HllHeaderInfo)> {
    if bytes.len() < HLL_HEADER_LEN || &bytes[..4] != MAGIC {
        return None;
    }
    let encoding = bytes[4];
    // 比服务端严一点：保留字节非零就当普通 string，压低碰巧以 HYLL 开头的误报。
    if encoding > ENCODING_SPARSE || bytes[5..8] != [0, 0, 0] {
        return None;
    }
    let card = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    Some((
        encoding,
        HllHeaderInfo {
            encoding: if encoding == ENCODING_DENSE {
                "dense".into()
            } else {
                "sparse".into()
            },
            cached_card: card & !(1 << 63),
            cache_stale: card >> 63 == 1,
        },
    ))
}

/// `get_key_meta` 的识别入口：喂 `GETRANGE key 0 15` 的结果。
pub fn detect(bytes: &[u8]) -> Option<HllMeta> {
    parse_header(bytes).map(|(_, header)| HllMeta {
        encoding: header.encoding,
    })
}

/// 取第 `index` 个 dense 寄存器。6 bit 一个、LSB 优先，跨字节的部分从下一字节补；
/// 截断的尾巴用 0 补足由调用方通过 `valid_registers` 表达，这里不越界。
fn dense_register(registers: &[u8], index: usize) -> u8 {
    let bit = index * 6;
    let byte = bit / 8;
    let shift = (bit & 7) as u32;
    let low = registers.get(byte).copied().unwrap_or(0) as u32 >> shift;
    let high = if shift > 2 {
        (registers.get(byte + 1).copied().unwrap_or(0) as u32) << (8 - shift)
    } else {
        0
    };
    ((low | high) & 63) as u8
}

/// sparse → 16,384 个寄存器。操作码越界、总数不等于 16,384 都算损坏，返回 None ——
/// 半截 sparse 无从知道哪一段还可信，诚实的做法是只给 raw。
fn expand_sparse(data: &[u8]) -> Option<Vec<u8>> {
    let mut registers = vec![0u8; HLL_REGISTERS];
    let mut index = 0usize;
    let mut cursor = 0usize;
    while cursor < data.len() {
        let opcode = data[cursor];
        if opcode & 0x80 != 0 {
            // VAL: 1vvvvvxx
            let value = ((opcode >> 2) & 0x1f) + 1;
            let run = (opcode & 0x03) as usize + 1;
            if index + run > HLL_REGISTERS {
                return None;
            }
            registers[index..index + run].fill(value);
            index += run;
            cursor += 1;
        } else if opcode & 0x40 != 0 {
            // XZERO: 01xxxxxx yyyyyyyy
            let next = *data.get(cursor + 1)?;
            let run = (((opcode & 0x3f) as usize) << 8 | next as usize) + 1;
            if index + run > HLL_REGISTERS {
                return None;
            }
            index += run;
            cursor += 2;
        } else {
            // ZERO: 00xxxxxx
            let run = (opcode & 0x3f) as usize + 1;
            if index + run > HLL_REGISTERS {
                return None;
            }
            index += run;
            cursor += 1;
        }
    }
    (index == HLL_REGISTERS).then_some(registers)
}

/// `bytes` 是取回的窗口（上限见 `values::HLL_INSPECT_FETCH_LIMIT`），
/// `actual_bytes` 是服务端报告的总长 —— 两者不等说明值大到不像 HLL。
pub fn inspect(bytes: &[u8], actual_bytes: u64) -> HllInspect {
    let Some((encoding, header)) = parse_header(bytes) else {
        return HllInspect {
            header: None,
            registers_base64: None,
            valid_registers: 0,
            total_registers: HLL_REGISTERS as u32,
            actual_bytes,
            expected_bytes: None,
        };
    };

    let register_bytes = &bytes[HLL_HEADER_LEN..];
    let complete = actual_bytes == bytes.len() as u64;

    let (registers, valid, expected) = match encoding {
        ENCODING_DENSE => {
            // 截断的 dense 仍能机械展开前缀：每个寄存器的 6 bit 完整落在
            // 取回的字节里才算可信，其余补零、由 valid_registers 划界。
            let valid = (register_bytes.len() * 8 / 6).min(HLL_REGISTERS);
            let mut registers = vec![0u8; HLL_REGISTERS];
            for (index, register) in registers.iter_mut().enumerate().take(valid) {
                *register = dense_register(register_bytes, index);
            }
            (
                Some(registers),
                valid,
                Some((HLL_HEADER_LEN + HLL_DENSE_REGISTERS_LEN) as u64),
            )
        }
        _ => {
            let registers = if complete {
                expand_sparse(register_bytes)
            } else {
                None
            };
            match registers {
                Some(registers) => (Some(registers), HLL_REGISTERS, None),
                None => (None, 0, None),
            }
        }
    };

    HllInspect {
        header: Some(header),
        registers_base64: registers.map(|bytes| general_purpose::STANDARD.encode(bytes)),
        valid_registers: valid as u32,
        total_registers: HLL_REGISTERS as u32,
        actual_bytes,
        expected_bytes: expected,
    }
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose, Engine as _};

    use super::{detect, inspect, HLL_DENSE_REGISTERS_LEN, HLL_HEADER_LEN, HLL_REGISTERS};

    fn header(encoding: u8, card: u64) -> Vec<u8> {
        let mut bytes = b"HYLL".to_vec();
        bytes.push(encoding);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&card.to_le_bytes());
        bytes
    }

    fn decode_registers(inspect: &super::HllInspect) -> Vec<u8> {
        general_purpose::STANDARD
            .decode(inspect.registers_base64.as_deref().unwrap())
            .unwrap()
    }

    #[test]
    fn detects_dense_and_sparse_headers() {
        assert_eq!(detect(&header(0, 0)).unwrap().encoding, "dense");
        assert_eq!(detect(&header(1, 42)).unwrap().encoding, "sparse");
    }

    /// "HYLLO WORLD…"：魔数撞车但第 5 字节不是合法 encoding，必须拒识。
    #[test]
    fn rejects_fake_magic_prefix() {
        assert!(detect(b"HYLLO WORLD, THIS IS TEXT").is_none());
    }

    #[test]
    fn rejects_short_values_and_nonzero_reserved_bytes() {
        assert!(detect(b"HYLL").is_none());
        let mut bytes = header(0, 0);
        bytes[6] = 1;
        assert!(detect(&bytes).is_none());
    }

    #[test]
    fn header_reports_cache_staleness_from_the_top_bit() {
        let fresh = inspect(&header(1, 99), HLL_HEADER_LEN as u64);
        let fresh = fresh.header.unwrap();
        assert_eq!(fresh.cached_card, 99);
        assert!(!fresh.cache_stale);

        let stale = inspect(&header(1, 99 | 1 << 63), HLL_HEADER_LEN as u64);
        let stale = stale.header.unwrap();
        assert_eq!(stale.cached_card, 99);
        assert!(stale.cache_stale);
    }

    #[test]
    fn unpacks_dense_registers_across_byte_boundaries() {
        // 寄存器 0..4 = [3, 5, 63, 0]：bit 序列 (LSB 优先) 000011 101000 111111 000000
        let mut bytes = header(0, 0);
        let mut registers = vec![0u8; HLL_DENSE_REGISTERS_LEN];
        // 手工按 6-bit LSB 打包：reg0=3 → byte0 低 6 bit；reg1=5 → byte0 高 2 bit + byte1 低 4 bit …
        registers[0] = 3 | (5 << 6) as u8;
        registers[1] = (5 >> 2) | (63 << 4) as u8;
        registers[2] = 63 >> 4;
        bytes.extend_from_slice(&registers);

        let result = inspect(&bytes, bytes.len() as u64);
        assert_eq!(result.valid_registers, HLL_REGISTERS as u32);
        let unpacked = decode_registers(&result);
        assert_eq!(&unpacked[..4], &[3, 5, 63, 0]);
        assert_eq!(result.expected_bytes, Some(12304));
    }

    /// 截断的 dense：前缀照常展开，valid_registers 说出边界，而不是整体拒绝。
    #[test]
    fn truncated_dense_degrades_to_a_valid_prefix() {
        let mut bytes = header(0, 0);
        bytes.extend_from_slice(&vec![0b0010_0110_u8; 6000]);

        let result = inspect(&bytes, bytes.len() as u64);
        assert_eq!(result.valid_registers, 6000 * 8 / 6);
        let unpacked = decode_registers(&result);
        assert_eq!(unpacked.len(), HLL_REGISTERS);
        assert_eq!(unpacked[0], 0b100110 & 63);
        // 缺失段补零
        assert_eq!(unpacked[HLL_REGISTERS - 1], 0);
    }

    #[test]
    fn expands_sparse_opcodes() {
        let mut bytes = header(1, 0);
        // VAL: value=7 run=2 → 1vvvvvxx = 1 00110 01；XZERO 覆盖余下 16382 个
        bytes.push(0b1001_1001);
        let remaining = (HLL_REGISTERS - 2 - 1) as u16;
        bytes.push(0x40 | (remaining >> 8) as u8);
        bytes.push((remaining & 0xff) as u8);

        let result = inspect(&bytes, bytes.len() as u64);
        assert_eq!(result.valid_registers, HLL_REGISTERS as u32);
        let unpacked = decode_registers(&result);
        assert_eq!(&unpacked[..3], &[7, 7, 0]);
        assert_eq!(result.expected_bytes, None);
    }

    /// sparse 损坏（计数对不上 16,384）→ 不给寄存器，只降级到头部。
    #[test]
    fn corrupt_sparse_yields_no_registers() {
        let mut bytes = header(1, 0);
        bytes.push(0b00_000011); // ZERO run=4，远不足 16384
        let result = inspect(&bytes, bytes.len() as u64);
        assert!(result.header.is_some());
        assert!(result.registers_base64.is_none());
        assert_eq!(result.valid_registers, 0);
    }

    /// 窗口取不全（值比取回的窗口大）时 sparse 不展开 —— 半截操作码流不可信。
    #[test]
    fn incomplete_sparse_window_is_not_expanded() {
        let mut bytes = header(1, 0);
        bytes.push(0b00_000011);
        let result = inspect(&bytes, bytes.len() as u64 + 100);
        assert!(result.registers_base64.is_none());
    }
}
