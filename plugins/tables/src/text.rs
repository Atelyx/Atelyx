//! 表格正文排版：字号 / 字重 / 颜色口径与断行缓存。量算走宿主 `query.measure`（可带逐行行段），
//! 缺项时先按字符估算出帧，量算落地后重建；行高、绘制与编辑浮层共用同一份行段。

use std::collections::HashMap;

use atelyx_format::table::ops::CellStyle;
use atelyx_format::table::TableField;
use atelyx_plugin_protocol::paint::{PaintColor, TextFamily, TextWeight};


/// 正文行高倍数（绘制、断行与编辑浮层同源）。
pub const LINE_FACTOR: f32 = 1.45;

/// 绘制行：源文本切片与实测行宽（下划线与删除线取此宽）。
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub width: f32,
}

/// 字体口径：字号 / 字重 / 字族 / 显式族名 / 斜体 / 颜色（单元格样式叠加后的最终形态）。
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub size: f32,
    pub weight: Option<TextWeight>,
    pub family: Option<TextFamily>,
    /// 用户选的系统字体族名（给了就以它为准，宿主未装该族时回落 `family`）。
    pub family_name: Option<String>,
    pub italic: bool,
    pub color: PaintColor,
}

impl FontSpec {
    /// 单元格样式 + 字段类型 → 字体口径（未缩放字号按缩放换算）。
    pub fn of(style: &CellStyle, field: &TableField, zoom: f32) -> Self {
        let base = match field.field_type.as_str() {
            "number" | "duration" => crate::theme::font_caption(),
            _ => crate::theme::font_ui(),
        };
        let size = style.size.unwrap_or(base as f64) as f32 * zoom;
        let (family, family_name) = style
            .font
            .as_deref()
            .filter(|name| !name.is_empty())
            .map(font_face)
            .unwrap_or((Some(TextFamily::Ui), None));
        Self {
            size,
            weight: style.b.then_some(TextWeight::Bold),
            family,
            family_name,
            italic: style.i,
            color: style_color(style),
        }
    }
}

/// 样式字体值 → 绘制族口径：`mono` 走系统等宽族；`serif` 与 hei/song/kai/fang 预设键取系统字体族名；
/// 其余值按已装族名解析（宿主未装时回落界面字体族）。
fn font_face(name: &str) -> (Option<TextFamily>, Option<String>) {
    match name {
        "mono" => (Some(TextFamily::Mono), None),
        "serif" => (Some(TextFamily::Ui), Some(SERIF_FAMILY.to_string())),
        "hei" => (Some(TextFamily::Ui), Some("Microsoft YaHei".to_string())),
        "song" => (Some(TextFamily::Ui), Some("SimSun".to_string())),
        "kai" => (Some(TextFamily::Ui), Some("KaiTi".to_string())),
        "fang" => (Some(TextFamily::Ui), Some("FangSong".to_string())),
        other => (Some(TextFamily::Ui), Some(other.to_string())),
    }
}

/// 绘制指令只有界面 / 等宽两族，通用衬线族按平台取一个已装族名（缺装时回落界面族）。
#[cfg(target_os = "windows")]
const SERIF_FAMILY: &str = "Times New Roman";
#[cfg(not(target_os = "windows"))]
const SERIF_FAMILY: &str = "DejaVu Serif";

/// 单元格字色：样式里的 hex 优先，缺省用正文主色。
pub fn style_color(style: &CellStyle) -> PaintColor {
    match style.color.as_deref().and_then(parse_hex) {
        Some(rgba) => PaintColor::Rgba { rgba },
        None => PaintColor::Token { name: atelyx_plugin_protocol::paint::ColorToken::Text },
    }
}

/// `#rgb` / `#rrggbb` / `#rrggbbaa` → RGBA 字节；只接受 ASCII 十六进制。
pub fn parse_hex(hex: &str) -> Option<[u8; 4]> {
    let hex = hex.trim().strip_prefix('#')?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    match hex.len() {
        3 => {
            let mut out = [255u8; 4];
            for (index, slot) in out.iter_mut().take(3).enumerate() {
                let value = u8::from_str_radix(&hex[index..index + 1], 16).ok()?;
                *slot = value * 17;
            }
            Some(out)
        }
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// RGBA → `#rrggbb` / `#rrggbbaa`。
pub fn to_hex([r, g, b, a]: [u8; 4]) -> String {
    if a == u8::MAX {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// 断行缓存键：字号 / 字重 / 字族 / 斜体按位取等，与尺寸无关。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapKey {
    pub text: String,
    pub size_bits: u32,
    pub weight: Option<TextWeight>,
    pub family: Option<TextFamily>,
    pub italic: bool,
    /// 换行宽（bits）：同一段文字在不同列宽下是不同的行段。
    pub wrap_bits: u32,
    /// 显式字体族名（换族即换行段）。
    pub family_name: Option<String>,
}

impl std::hash::Hash for WrapKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
        self.size_bits.hash(state);
        weight_code(self.weight).hash(state);
        family_code(self.family).hash(state);
        self.italic.hash(state);
        self.wrap_bits.hash(state);
        self.family_name.hash(state);
    }
}

/// 字重编码（协议枚举不实现 `Hash`，按判别值入键）。
fn weight_code(weight: Option<TextWeight>) -> u8 {
    match weight {
        None => 0,
        Some(TextWeight::Normal) => 1,
        Some(TextWeight::Medium) => 2,
        Some(TextWeight::SemiBold) => 3,
        Some(TextWeight::Bold) => 4,
    }
}

/// 字族编码（同上）。
fn family_code(family: Option<TextFamily>) -> u8 {
    match family {
        None => 0,
        Some(TextFamily::Ui) => 1,
        Some(TextFamily::Mono) => 2,
    }
}

impl WrapKey {
    pub fn of(text: &str, font: &FontSpec, wrap_w: f32) -> Self {
        Self {
            text: text.to_string(),
            size_bits: font.size.to_bits(),
            weight: font.weight,
            family: font.family,
            italic: font.italic,
            family_name: font.family_name.clone(),
            wrap_bits: wrap_w.to_bits(),
        }
    }
}

struct WrapEntry {
    pass: u64,
    lines: Vec<TextLine>,
    /// 是否已由宿主量算过（false = 当前是估算值，等量算补齐）。
    measured: bool,
}

/// 断行结果缓存：随布局重建滚动，只保留本轮用过的键；未量算的键登记为待量算。
#[derive(Default)]
pub struct WrapCache {
    entries: HashMap<WrapKey, WrapEntry>,
    misses: Vec<WrapKey>,
    pass: u64,
}

impl WrapCache {
    pub fn begin_pass(&mut self) {
        self.pass += 1;
        self.misses.clear();
    }

    pub fn end_pass(&mut self) {
        let pass = self.pass;
        self.entries.retain(|_, entry| entry.pass == pass);
    }

    /// 取整段正文的行段（无键则按字符估算并登记待量算）。
    pub fn lines(&mut self, text: &str, font: &FontSpec, wrap_w: f32) -> &[TextLine] {
        let key = WrapKey::of(text, font, wrap_w);
        let pass = self.pass;
        let entry = self.entries.entry(key.clone()).or_insert_with(|| {
            self.misses.push(key.clone());
            WrapEntry { pass, lines: estimate_lines_of(text, font.size, wrap_w), measured: false }
        });
        entry.pass = pass;
        &entry.lines
    }

    /// 只读行段（绘制用；没有就返回 None，由调用方退回估算）。
    pub fn peek(&self, text: &str, font: &FontSpec, wrap_w: f32) -> Option<&[TextLine]> {
        self.entries
            .get(&WrapKey::of(text, font, wrap_w))
            .map(|entry| entry.lines.as_slice())
    }


    /// 取出本轮要量算的键（含换行宽；同一帧内去重）。
    /// 未量算成功的键留在待办里等下一帧重试，直到量算落地或本轮布局弃用。
    pub fn take_misses(&mut self) -> Vec<(WrapKey, f32)> {
        let entries = &self.entries;
        self.misses
            .retain(|key| entries.get(key).is_some_and(|entry| !entry.measured));
        self.misses
            .iter()
            .map(|key| (key.clone(), f32::from_bits(key.wrap_bits)))
            .collect()
    }

    /// 量算结果落地：按源文本与宽度重算行段（宿主已按同样口径折行，这里只切行）。
    pub fn apply(&mut self, key: WrapKey, lines: Vec<TextLine>) {
        let pass = self.pass;
        self.entries
            .insert(key, WrapEntry { pass, lines, measured: true });
    }
}

/// 整段正文的估算行段（量算落地前用）：按硬换行分段、逐字贪心。
pub fn estimate_lines_of(text: &str, size: f32, wrap_w: f32) -> Vec<TextLine> {
    let mut out = Vec::new();
    for segment in text.split('\n') {
        let mut current = String::new();
        let mut width = 0.0f32;
        for ch in segment.chars() {
            let advance = estimate_char(ch, size);
            if !current.is_empty() && width + advance > wrap_w {
                out.push(TextLine { text: std::mem::take(&mut current), width });
                width = 0.0;
            }
            current.push(ch);
            width += advance;
        }
        out.push(TextLine { text: current, width });
    }
    out
}

/// 估算行数（拖列宽在途用，不写缓存）。
pub fn estimate_lines(text: &str, size: f32, wrap_w: f32) -> usize {
    estimate_lines_of(text, size, wrap_w).len().max(1)
}

/// 单字估算字宽：ASCII 0.55 字宽，其余 1 字宽（与宿主量算落地前的估算同口径）。
fn estimate_char(ch: char, size: f32) -> f32 {
    size * if ch.is_ascii() { 0.55 } else { 1.0 }
}

/// 单元格文本可否直接起稿（文本 / 数字 / 时长；单选与图片走各自的交互）。
pub fn can_type_into(field: &TableField) -> bool {
    matches!(field.field_type.as_str(), "text" | "number" | "duration")
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn parse_hex_accepts_shorthand_and_rejects_non_ascii() {
        assert_eq!(parse_hex("#fff"), Some([255, 255, 255, 255]));
        assert_eq!(parse_hex("#00000000"), Some([0, 0, 0, 0]));
        // 字节长恰为 6 但含多字节字符：按字符边界切会 panic，这里必须安全返回 None
        assert_eq!(parse_hex("#aééb"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }
}

