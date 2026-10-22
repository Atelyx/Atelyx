//! 笔记协作帧编解码（`note-sync` 通道载荷）与 lib0 基元。
//! 字节布局与 lib0 / y-protocols 完全一致，由 golden vector 测试冻结。
//! 解码对网络载荷不可信：字段不完整 / 未知类型 / 尾随字节一律返回 `None` 不抛错。

use std::cmp::Ordering;

pub const NOTE_FRAME_SYNC: u64 = 0x03;
pub const NOTE_FRAME_BASELINE: u64 = 0x42;
pub const NOTE_FRAME_RESYNC: u64 = 0x43;
pub const NOTE_FRAME_RELOCATE: u64 = 0x44;

/// y-protocols 同步消息头（`SYNC` 帧内嵌载荷首字节）。
pub const SYNC_MSG_STEP1: u64 = 0;
pub const SYNC_MSG_STEP2: u64 = 1;
pub const SYNC_MSG_UPDATE: u64 = 2;

/// 基线标签：`seq` 为提案序号（Lamport），`author` 为会话稳定身份，`id` 为基线正文内容标识。
/// 全序 `seq → author → id` 绝无平局；兼容性只看 `id`（同正文基线字节一致，合并幂等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTag {
    pub seq: u64,
    pub author: String,
    pub id: String,
}

impl DocTag {
    pub fn compare(&self, other: &DocTag) -> Ordering {
        self.seq
            .cmp(&other.seq)
            .then_with(|| self.author.cmp(&other.author))
            .then_with(|| self.id.cmp(&other.id))
    }

    /// 基线正文相同（合并幂等）：只比内容标识。
    pub fn same_content(&self, other: &DocTag) -> bool {
        self.id == other.id
    }
}

/// 帧解码结果（按帧类型区分）。`Sync.msg` 为内嵌 y-protocols 同步消息原样字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteFrame {
    Sync { tag: DocTag, msg: Vec<u8> },
    Baseline { tag: DocTag, baseline_text: String, current_text: String },
    Resync { reason: u64 },
    Relocate { old_path: String, new_path: String },
}

// ---------- lib0 基元 ----------

pub fn write_var_uint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        if v < 0x80 {
            buf.push(v as u8);
            return;
        }
        buf.push(((v & 0x7f) | 0x80) as u8);
        v >>= 7;
    }
}

pub fn write_var_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    write_var_uint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

pub fn write_var_string(buf: &mut Vec<u8>, s: &str) {
    write_var_bytes(buf, s.as_bytes());
}

/// 严格读取：越界或字段不完整返回 `None`。
pub fn read_var_uint(r: &mut Reader<'_>) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *r.rest.first()?;
        r.rest = &r.rest[1..];
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

pub fn read_var_bytes<'a>(r: &mut Reader<'a>) -> Option<&'a [u8]> {
    let len = read_var_uint(r)? as usize;
    if r.rest.len() < len {
        return None;
    }
    let (head, tail) = r.rest.split_at(len);
    r.rest = tail;
    Some(head)
}

pub fn read_var_string<'a>(r: &mut Reader<'a>) -> Option<&'a str> {
    std::str::from_utf8(read_var_bytes(r)?).ok()
}

pub struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { rest: data }
    }

    pub fn has_content(&self) -> bool {
        !self.rest.is_empty()
    }
}

// ---------- 帧编解码 ----------

fn write_tag(buf: &mut Vec<u8>, tag: &DocTag) {
    write_var_uint(buf, tag.seq);
    write_var_string(buf, &tag.author);
    write_var_string(buf, &tag.id);
}

fn read_tag(r: &mut Reader<'_>) -> Option<DocTag> {
    Some(DocTag {
        seq: read_var_uint(r)?,
        author: read_var_string(r)?.to_owned(),
        id: read_var_string(r)?.to_owned(),
    })
}

pub fn encode_note_sync(tag: &DocTag, msg: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    write_var_uint(&mut buf, NOTE_FRAME_SYNC);
    write_tag(&mut buf, tag);
    write_var_bytes(&mut buf, msg);
    buf
}

pub fn encode_note_baseline(tag: &DocTag, baseline_text: &str, current_text: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    write_var_uint(&mut buf, NOTE_FRAME_BASELINE);
    write_tag(&mut buf, tag);
    write_var_string(&mut buf, baseline_text);
    write_var_string(&mut buf, current_text);
    buf
}

pub fn encode_note_resync(reason: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    write_var_uint(&mut buf, NOTE_FRAME_RESYNC);
    write_var_uint(&mut buf, reason);
    buf
}

pub fn encode_note_relocate(old_path: &str, new_path: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    write_var_uint(&mut buf, NOTE_FRAME_RELOCATE);
    write_var_string(&mut buf, old_path);
    write_var_string(&mut buf, new_path);
    buf
}

pub fn decode_note_payload(payload: &[u8]) -> Option<NoteFrame> {
    let mut r = Reader::new(payload);
    if !r.has_content() {
        return None;
    }
    let kind = read_var_uint(&mut r)?;
    let frame = match kind {
        NOTE_FRAME_SYNC => NoteFrame::Sync {
            tag: read_tag(&mut r)?,
            msg: read_var_bytes(&mut r)?.to_vec(),
        },
        NOTE_FRAME_BASELINE => NoteFrame::Baseline {
            tag: read_tag(&mut r)?,
            baseline_text: read_var_string(&mut r)?.to_owned(),
            current_text: read_var_string(&mut r)?.to_owned(),
        },
        NOTE_FRAME_RESYNC => NoteFrame::Resync { reason: read_var_uint(&mut r)? },
        NOTE_FRAME_RELOCATE => NoteFrame::Relocate {
            old_path: read_var_string(&mut r)?.to_owned(),
            new_path: read_var_string(&mut r)?.to_owned(),
        },
        _ => return None,
    };
    if r.has_content() {
        return None;
    }
    Some(frame)
}

// ---------- 基线内容标识与偏移换算 ----------

fn base36(mut v: u64) -> String {
    if v == 0 {
        return "0".to_owned();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while v > 0 {
        out.push(DIGITS[(v % 36) as usize]);
        v /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 字符恒为 ASCII")
}

/// 基线正文内容标识：双 32 位 FNV-1a 散列逐 UTF-16 code unit（与 yjs 侧 charCodeAt/imul 对齐），
/// 格式 `{utf16长度.base36}-{h1.base36}-{h2.base36}`。
pub fn baseline_id_of(text: &str) -> String {
    baseline_id_of_utf16(text.encode_utf16())
}

/// 同 `baseline_id_of`，直接对 UTF-16 code unit 序列计算（覆盖 JS 侧可能出现的孤立代理单元）。
pub fn baseline_id_of_utf16(units: impl Iterator<Item = u16>) -> String {
    let mut h1: u32 = 0x811c_9dc5;
    let mut h2: u32 = 0x9e37_79b9;
    let mut len: u64 = 0;
    for unit in units {
        h1 = (h1 ^ u32::from(unit)).wrapping_mul(0x0100_0193);
        h2 = (h2 ^ u32::from(unit)).wrapping_mul(0x85eb_ca6b);
        len += 1;
    }
    format!("{}-{}-{}", base36(len), base36(u64::from(h1)), base36(u64::from(h2)))
}

/// JS UTF-16 code unit 偏移 → Rust 字节偏移（落在字符中间时取所在字符起点）。
pub fn utf16_to_byte_offset(text: &str, u16off: usize) -> usize {
    let mut byte = 0usize;
    let mut units = 0usize;
    for ch in text.chars() {
        if units >= u16off {
            break;
        }
        units += ch.len_utf16();
        byte += ch.len_utf8();
    }
    byte
}

/// Rust 字节偏移 → JS UTF-16 code unit 偏移（byte 落在字符内部时取该字符起点）。
pub fn byte_to_utf16_offset(text: &str, byte: usize) -> usize {
    let mut units = 0usize;
    let mut start = 0usize;
    for ch in text.chars() {
        if start >= byte || start + ch.len_utf8() > byte {
            break;
        }
        units += ch.len_utf16();
        start += ch.len_utf8();
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 基线内容标识与_yjs_侧字节一致() {
        // golden vector 由 yjs 侧 baselineIdOf（charCodeAt + Math.imul）实测产出
        let cases = [
            ("", "0-ztntfp-17wdrqx"),
            ("a", "1-1r9wi7g-1wzaxxk"),
            ("ab", "2-lekqmi-192e1e6"),
            ("ba", "2-gp0ybw-mri6m0"),
            ("a\u{0}b", "3-4pbs2a-1wvfum"),
            ("😀", "2-1kdnhdk-d1ty04"),
            ("😀😀", "4-1e58txp-1u8kdap"),
            ("一段基线正文\n第二行\n", "b-13pppth-wcwcap"),
            ("# 标题\n\n正文 with `code` 和 **强调**。\n", "v-1mf0z0j-16sg4dr"),
        ];
        for (text, id) in cases {
            assert_eq!(baseline_id_of(text), id, "text = {text:?}");
        }
        // JS 侧孤立代理单元 "\ud83d"（charCodeAt 逐单元可出现，Rust String 无法直接表达）
        assert_eq!(baseline_id_of_utf16([0xd83d].into_iter()), "1-fegh9k-1pu6mng");
    }

    #[test]
    fn 帧编解码与_yjs_侧字节一致() {
        let tag = DocTag {
            seq: 7,
            author: "设备A#ab12".to_owned(),
            id: baseline_id_of("一段基线正文\n第二行\n"),
        };
        let inner = unhex("0101b2f21900040104746578741de4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a00");
        assert_eq!(
            hex(encode_note_sync(&tag, &inner)),
            "03070ce8aebee5a48741236162313210622d313370707074682d7763776361702c0101b2f21900040104746578741de4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a00"
        );
        assert_eq!(
            hex(encode_note_baseline(&tag, "一段基线正文\n第二行\n", "一段基线正文\n第二行\n\n当前追加")),
            "42070ce8aebee5a48741236162313210622d313370707074682d7763776361701de4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a2ae4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a0ae5bd93e5898de8bfbde58aa0"
        );
        assert_eq!(
            hex(encode_note_baseline(
                &DocTag { seq: 0, author: String::new(), id: baseline_id_of("") },
                "",
                ""
            )),
            "42000010302d7a746e7466702d313777647271780000"
        );
        assert_eq!(hex(encode_note_resync(1)), "4301");
        assert_eq!(
            hex(encode_note_relocate("笔记/旧 名.md", "子/新名.md")),
            "4411e7ac94e8aeb02fe697a720e5908d2e6d640de5ad902fe696b0e5908d2e6d64"
        );
    }

    #[test]
    fn 帧解码还原与坏帧拒绝() {
        let tag = DocTag { seq: 7, author: "设备A#ab12".to_owned(), id: "b-13pppth-wcwcap".to_owned() };
        let inner = vec![0, 1, 2, 250, 255];
        assert_eq!(
            decode_note_payload(&encode_note_sync(&tag, &inner)),
            Some(NoteFrame::Sync { tag: tag.clone(), msg: inner })
        );
        assert_eq!(
            decode_note_payload(&encode_note_baseline(&tag, "", "追加")),
            Some(NoteFrame::Baseline {
                tag: tag.clone(),
                baseline_text: String::new(),
                current_text: "追加".to_owned()
            })
        );
        assert_eq!(decode_note_payload(&encode_note_resync(1)), Some(NoteFrame::Resync { reason: 1 }));
        assert_eq!(
            decode_note_payload(&encode_note_relocate("笔记/旧 名.md", "子/新名.md")),
            Some(NoteFrame::Relocate {
                old_path: "笔记/旧 名.md".to_owned(),
                new_path: "子/新名.md".to_owned()
            })
        );
        // 空载荷 / 未知类型 / 尾随字节 / 截断
        assert_eq!(decode_note_payload(&[]), None);
        let mut unknown = Vec::new();
        write_var_uint(&mut unknown, 0x7f);
        assert_eq!(decode_note_payload(&unknown), None);
        let mut trailing = encode_note_resync(0);
        trailing.push(9);
        assert_eq!(decode_note_payload(&trailing), None);
        let full = encode_note_baseline(&tag, "完整正文", "完整正文");
        assert_eq!(decode_note_payload(&full[..full.len() - 2]), None);
        let sync = encode_note_sync(&tag, &[1, 2, 3]);
        assert_eq!(decode_note_payload(&sync[..3]), None);
    }

    #[test]
    fn 标签全序三段无平局() {
        let t = |seq: u64, author: &str, id: &str| DocTag {
            seq,
            author: author.to_owned(),
            id: id.to_owned(),
        };
        assert_eq!(t(1, "b", "x").compare(&t(2, "a", "y")), Ordering::Less);
        assert_eq!(t(2, "a", "x").compare(&t(2, "b", "y")), Ordering::Less);
        assert_eq!(t(2, "b", "x").compare(&t(2, "b", "y")), Ordering::Less);
        assert_eq!(t(3, "a", "x").compare(&t(2, "z", "y")), Ordering::Greater);
        assert_eq!(t(2, "b", "x").compare(&t(2, "b", "x")), Ordering::Equal);
        assert!(t(1, "甲", "i1").same_content(&t(9, "乙", "i1")));
        assert!(!t(1, "甲", "i1").same_content(&t(1, "甲", "i2")));
    }

    #[test]
    fn utf16与字节偏移换算() {
        let text = "a😀中文\n行";
        // 'a' 1 unit 1 byte；😀 2 units 4 bytes；中文各 1 unit 3 bytes；\n 1；行 1
        assert_eq!(byte_to_utf16_offset(text, 0), 0);
        assert_eq!(byte_to_utf16_offset(text, 1), 1); // a 之后
        assert_eq!(byte_to_utf16_offset(text, 3), 1); // 😀 中间向下取整
        assert_eq!(byte_to_utf16_offset(text, 5), 3); // 😀 之后
        assert_eq!(byte_to_utf16_offset(text, 8), 4); // 中 之后
        assert_eq!(utf16_to_byte_offset(text, 0), 0);
        assert_eq!(utf16_to_byte_offset(text, 1), 1);
        assert_eq!(utf16_to_byte_offset(text, 3), 5);
        assert_eq!(utf16_to_byte_offset(text, 4), 8);
        assert_eq!(utf16_to_byte_offset(text, 99), text.len());
        // 往返（仅字符边界字节可逆；mid-char 向下取整后回映到所在字符起点）
        for byte in [0usize, 1, 5, 8, 11, 12, text.len()] {
            assert_eq!(utf16_to_byte_offset(text, byte_to_utf16_offset(text, byte)), byte.min(text.len()));
        }
        assert_eq!(byte_to_utf16_offset(text, 14), 6); // mid-行 → 前一字符边界
        assert_eq!(utf16_to_byte_offset(text, 6), 12);
    }

    fn hex(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("测试用例恒为合法 hex"))
            .collect()
    }
}
