//! 整面帧的共享内存区域：宿主建临时文件并铺槽位头，插件映射后写槽、只经信封报告槽位坐标；
//! 宿主按序号校验后回读解码，撕裂或错位的帧整帧丢弃。

use std::sync::atomic::{fence, Ordering};

use serde::{Deserialize, Serialize};

/// 区域头魔数（8 字节）。
pub const REGION_MAGIC: [u8; 8] = *b"ATLXFRM\0";
/// 区域布局版本：字段含义变化即递增，双端不一致按不可用报错。
pub const REGION_VERSION: u32 = 1;
/// 区域头长度。
pub const REGION_HEADER_SIZE: usize = 64;
/// 槽头长度。
pub const SLOT_HEADER_SIZE: usize = 16;
/// 槽数：写入端同时只占一槽，其余留作宿主读取与紧邻两次提交的隔离。
pub const SLOT_COUNT: u32 = 4;
/// 槽容量（字节）：帧只装绘制指令与图片来源，不装图片像素。
pub const SLOT_CAPACITY: u32 = 256 * 1024;
/// 槽容量上限：超出即报可读错误，不再扩容。
pub const MAX_SLOT_CAPACITY: u32 = 16 * 1024 * 1024;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const SLOT_COUNT_OFFSET: usize = 12;
const SLOT_CAPACITY_OFFSET: usize = 16;
const GENERATION_OFFSET: usize = 20;
const HEADER_SIZE_OFFSET: usize = 28;
const SLOT_STRIDE_OFFSET: usize = 32;
const SLOT_SEQ_OFFSET: usize = 0;
const SLOT_LEN_OFFSET: usize = 8;

fn align8(value: usize) -> usize {
    (value + 7) & !7
}

/// 一个共享内存区域的描述（握手应答与 `frame.region` 扩容应答同形）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRegionInfo {
    /// 区域文件绝对路径（宿主建在系统临时目录；插件按此映射）。
    pub path: String,
    /// 代次：每次扩容或新会话换新文件并递增；提交时原样带回，宿主据此拒绝过期映射。
    pub generation: u64,
    pub slot_count: u32,
    /// 每槽可写的最大帧字节数。
    pub slot_capacity: u32,
}

impl FrameRegionInfo {
    /// 按槽容量给出一个区域描述；`path` / `generation` 由宿主补齐。
    pub fn new_layout(slot_capacity: u32) -> Self {
        Self {
            path: String::new(),
            generation: 0,
            slot_count: SLOT_COUNT,
            slot_capacity,
        }
    }

    /// 槽头 + 槽容量的对齐步长。
    pub fn slot_stride(&self) -> usize {
        align8(SLOT_HEADER_SIZE + self.slot_capacity as usize)
    }

    /// 区域文件应有的总长度。
    pub fn byte_len(&self) -> usize {
        REGION_HEADER_SIZE + self.slot_count as usize * self.slot_stride()
    }

    /// 第 `slot` 槽的槽头偏移与内容偏移；越界返回 `None`。
    fn slot_offsets(&self, slot: u32) -> Option<(usize, usize)> {
        if slot >= self.slot_count {
            return None;
        }
        let header = REGION_HEADER_SIZE + slot as usize * self.slot_stride();
        Some((header, header + SLOT_HEADER_SIZE))
    }

    fn check_layout(&self) -> Result<(), String> {
        if self.slot_count == 0 || self.slot_count > 64 {
            return Err(format!("槽数越界：{}", self.slot_count));
        }
        if self.slot_capacity == 0 || self.slot_capacity > MAX_SLOT_CAPACITY {
            return Err(format!("槽容量越界：{}", self.slot_capacity));
        }
        Ok(())
    }
}

/// 填区域头：调用方须已把文件扩到 [`FrameRegionInfo::byte_len`]，且槽位从全零起步（新建文件即全零，
/// 槽序号从 2 起递增，全零槽不会被误认）。
///
/// 布局（全部小端）：64 字节区域头（魔数 / 版本 / 槽数 / 槽容量 / 代次 / 头长 / 槽步长）→
/// 每槽 16 字节槽头（序号 u64、长度 u32）+ 定长槽容量。
///
/// 只写区域头，槽区不动；缓冲不足或槽数 / 槽容量越界即报错。
pub fn init_region(map: &mut [u8], info: &FrameRegionInfo) -> Result<(), String> {
    info.check_layout()?;
    let len = info.byte_len();
    if map.len() < len {
        return Err(format!("区域缓冲不足：需要 {len} 字节，实际 {}", map.len()));
    }
    map[..REGION_HEADER_SIZE].fill(0);
    map[MAGIC_OFFSET..MAGIC_OFFSET + REGION_MAGIC.len()].copy_from_slice(&REGION_MAGIC);
    write_u32(map, VERSION_OFFSET, REGION_VERSION);
    write_u32(map, SLOT_COUNT_OFFSET, info.slot_count);
    write_u32(map, SLOT_CAPACITY_OFFSET, info.slot_capacity);
    write_u64(map, GENERATION_OFFSET, info.generation);
    write_u32(map, HEADER_SIZE_OFFSET, REGION_HEADER_SIZE as u32);
    write_u32(map, SLOT_STRIDE_OFFSET, info.slot_stride() as u32);
    Ok(())
}

/// 校验区域头与描述一致（映射端开箱即用的一次把关）。
///
/// 缓冲不足、魔数 / 版本 / 代次不符或槽数与步长和描述不同即报错。
pub fn validate_region(map: &[u8], info: &FrameRegionInfo) -> Result<(), String> {
    info.check_layout()?;
    let len = info.byte_len();
    if map.len() < len {
        return Err(format!("区域文件不足：需要 {len} 字节，实际 {}", map.len()));
    }
    if map[MAGIC_OFFSET..MAGIC_OFFSET + REGION_MAGIC.len()] != REGION_MAGIC {
        return Err("区域魔数不符".into());
    }
    let version = read_u32(map, VERSION_OFFSET);
    if version != REGION_VERSION {
        return Err(format!("区域版本不符：文件 {version}，宿主 {REGION_VERSION}"));
    }
    let slot_count = read_u32(map, SLOT_COUNT_OFFSET);
    let capacity = read_u32(map, SLOT_CAPACITY_OFFSET);
    let generation = read_u64(map, GENERATION_OFFSET);
    let header_size = read_u32(map, HEADER_SIZE_OFFSET);
    let stride = read_u32(map, SLOT_STRIDE_OFFSET);
    if generation != info.generation {
        return Err(format!("区域代次不符：文件 {generation}，描述 {}", info.generation));
    }
    if slot_count != info.slot_count
        || capacity != info.slot_capacity
        || header_size as usize != REGION_HEADER_SIZE
        || stride as usize != info.slot_stride()
    {
        return Err(format!(
            "区域头与描述不一致：槽数 {slot_count}，槽容量 {capacity}，槽头 {header_size}，步长 {stride}"
        ));
    }
    Ok(())
}

/// 把一帧编码后的内容写进槽位：序号必须是大于零的偶数。
///
/// 区域头与描述不符、槽位越界、内容超出槽容量或序号非法即报错，槽内内容不动。
pub fn write_slot(
    map: &mut [u8],
    info: &FrameRegionInfo,
    slot: u32,
    seq: u64,
    payload: &[u8],
) -> Result<(), String> {
    validate_region(map, info)?;
    if seq == 0 || seq % 2 != 0 {
        return Err(format!("槽序号须为非零偶数：{seq}"));
    }
    let (header, content) =
        info.slot_offsets(slot).ok_or_else(|| format!("槽位越界：{slot}"))?;
    if payload.len() > info.slot_capacity as usize {
        return Err(format!(
            "帧超出槽容量：{} > {}",
            payload.len(),
            info.slot_capacity
        ));
    }
    // 先置奇数宣告写入中，再写长度与内容；释放栅栏确保读者看到偶数序号时内容已落地
    write_u64(map, header + SLOT_SEQ_OFFSET, seq - 1);
    fence(Ordering::SeqCst);
    write_u32(map, header + SLOT_LEN_OFFSET, payload.len() as u32);
    map[content..content + payload.len()].copy_from_slice(payload);
    fence(Ordering::Release);
    write_u64(map, header + SLOT_SEQ_OFFSET, seq);
    Ok(())
}

/// 读一槽内容：序号对得上且首尾一致才返回 `Some`。
///
/// 写入进行中 / 被后续写入覆盖返回 `None`（按错位丢弃）；区域头与描述不符、序号非法、槽位或
/// 长度越界、槽内长度与提交不符即报错。
pub fn read_slot(
    map: &[u8],
    info: &FrameRegionInfo,
    slot: u32,
    seq: u64,
    len: u32,
) -> Result<Option<Vec<u8>>, String> {
    validate_region(map, info)?;
    if seq == 0 || seq % 2 != 0 {
        return Err(format!("槽序号须为非零偶数：{seq}"));
    }
    let (header, content) =
        info.slot_offsets(slot).ok_or_else(|| format!("槽位越界：{slot}"))?;
    if len > info.slot_capacity {
        return Err(format!("帧长度越界：{len} > {}", info.slot_capacity));
    }
    let before = read_u64(map, header + SLOT_SEQ_OFFSET);
    if before != seq || before % 2 != 0 {
        return Ok(None);
    }
    fence(Ordering::Acquire);
    let stored_len = read_u32(map, header + SLOT_LEN_OFFSET);
    let payload = map[content..content + len as usize].to_vec();
    fence(Ordering::Acquire);
    let after = read_u64(map, header + SLOT_SEQ_OFFSET);
    if after != before {
        // 读的过程中被后续写入整槽覆盖：内容已不可信，按错位丢弃（不是插件错误）
        return Ok(None);
    }
    if stored_len != len {
        return Err(format!("槽内长度与提交不符：槽内 {stored_len}，提交 {len}"));
    }
    Ok(Some(payload))
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&buf[offset..offset + 4]);
    u32::from_le_bytes(raw)
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&buf[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(capacity: u32) -> FrameRegionInfo {
        FrameRegionInfo { path: "region.bin".into(), generation: 7, ..FrameRegionInfo::new_layout(capacity) }
    }

    #[test]
    fn layout_sizes_and_alignment_are_stable() {
        let info = layout(SLOT_CAPACITY);
        assert_eq!(info.slot_stride(), 16 + SLOT_CAPACITY as usize);
        assert_eq!(info.byte_len(), REGION_HEADER_SIZE + 4 * info.slot_stride());
        let odd = layout(1000);
        assert_eq!(odd.slot_stride() % 8, 0);
        assert!(odd.slot_stride() >= 1000 + SLOT_HEADER_SIZE);
    }

    #[test]
    fn header_roundtrip_and_mismatch_is_rejected() {
        let info = layout(SLOT_CAPACITY);
        let mut map = vec![0u8; info.byte_len()];
        init_region(&mut map, &info).unwrap();
        validate_region(&map, &info).unwrap();

        let wrong = FrameRegionInfo { generation: 8, ..info.clone() };
        assert!(validate_region(&map, &wrong).unwrap_err().contains("代次不符"));
        let mut corrupt = map.clone();
        corrupt[0] = b'X';
        assert!(validate_region(&corrupt, &info).unwrap_err().contains("魔数"));
        let short = vec![0u8; info.byte_len() - 1];
        assert!(validate_region(&short, &info).unwrap_err().contains("不足"));
    }

    #[test]
    fn slot_write_and_read_roundtrip() {
        let info = layout(1024);
        let mut map = vec![0u8; info.byte_len()];
        init_region(&mut map, &info).unwrap();
        write_slot(&mut map, &info, 1, 4, b"hello").unwrap();
        assert_eq!(read_slot(&map, &info, 1, 4, 5).unwrap().as_deref(), Some(&b"hello"[..]));
        // 序号不符 = 被后续写入覆盖：整帧丢弃而不是读出坏内容
        assert_eq!(read_slot(&map, &info, 1, 2, 5).unwrap(), None);
        // 序号一致而长度不符 = 插件谎报：可读错误
        assert!(read_slot(&map, &info, 1, 4, 6).unwrap_err().contains("长度与提交不符"));
        // 槽位与长度越界是可读错误
        assert!(read_slot(&map, &info, 9, 4, 5).unwrap_err().contains("槽位越界"));
        assert!(read_slot(&map, &info, 1, 4, 2048).unwrap_err().contains("长度越界"));
        assert!(write_slot(&mut map, &info, 0, 3, b"x").unwrap_err().contains("偶数"));
        assert!(write_slot(&mut map, &info, 0, 2, &vec![0u8; 2048]).unwrap_err().contains("槽容量"));
        // 序号非法与写入端同一口径：可读错误，不当成正常丢帧
        assert!(read_slot(&map, &info, 1, 0, 5).unwrap_err().contains("非零偶数"));
        assert!(read_slot(&map, &info, 1, 3, 5).unwrap_err().contains("非零偶数"));
    }

    #[test]
    fn odd_sequence_is_treated_as_in_progress() {
        let info = layout(64);
        let mut map = vec![0u8; info.byte_len()];
        init_region(&mut map, &info).unwrap();
        let (header, _) = info.slot_offsets(0).unwrap();
        write_u64(&mut map, header + SLOT_SEQ_OFFSET, 5);
        // 读端拿的是提交里的偶数序号（这一笔要写到 6）：槽内还是奇数即写入中
        assert_eq!(read_slot(&map, &info, 0, 6, 0).unwrap(), None);
    }
}
