//! `.atb` 表格导出 xlsx：列 = 字段顺序，表头金色底 + 粗体 + 全表细边框 + 冻结首行；
//! image 字段嵌单元格首图（等比缩至 140×90，行高撑开），text 换行、number/duration 写数值、
//! 其余（singleSelect/未知类型）写文本。单元格显示样式不导出。

use std::collections::HashMap;
use std::path::Path;

use atelyx_format::table::ops;
use atelyx_format::table::TableFile;
use rust_xlsxwriter::{Format, FormatBorder, Image, Workbook};

/// 嵌入图片等比缩放上限与行高（三者联动：图片缩至行高内）。
const EMBED_MAX_W: u32 = 140;
const EMBED_MAX_H: u32 = 90;
const EMBED_ROW_HEIGHT: f64 = 90.0;

/// 导出输入：表格 + 已读好的图片字节（附件路径 → 字节）。
pub struct ExportInput<'a> {
    pub table: &'a TableFile,
    pub images: &'a HashMap<String, Vec<u8>>,
    /// 空间模式：外置附件图片在服务端、无法内嵌（与宿主原导出口径一致）。
    pub space: bool,
    pub target: &'a Path,
}

/// 导出表格为 xlsx 到目标路径，返回无法嵌入的图片数。
pub fn export(input: &ExportInput<'_>) -> Result<usize, String> {
    let table = input.table;
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .set_name(sheet_name_of(&table.title))
        .map_err(|e| format!("设置工作表名失败：{e}"))?;

    let header_fmt = Format::new()
        .set_bold()
        .set_background_color("E0A94E")
        .set_border(FormatBorder::Thin);
    let wrap_fmt = Format::new().set_border(FormatBorder::Thin).set_text_wrap();
    let plain_fmt = Format::new().set_border(FormatBorder::Thin);

    for (index, field) in table.fields.iter().enumerate() {
        let col = index as u16;
        let _ = sheet.write_string_with_format(0, col, &field.name, &header_fmt);
        let _ = sheet.set_column_width(col, column_width_of(&field.field_type));
    }
    if !table.fields.is_empty() {
        let _ = sheet.set_freeze_panes(1, 0);
    }

    let mut skipped = 0usize;
    for (row_index, row) in table.rows.iter().enumerate() {
        let excel_row = (row_index + 1) as u32;
        let mut has_image = false;
        for (index, field) in table.fields.iter().enumerate() {
            let col = index as u16;
            let Some(value) = row.values.get(&field.id) else { continue };
            match field.field_type.as_str() {
                // 多图单元格只导首图
                "image" => {
                    let Some(entry) = ops::image_entries(value).first().copied() else {
                        continue;
                    };
                    match resolve_image(input.images, input.space, entry).and_then(native_or_png) {
                        Some(image) => {
                            let image = image.set_scale_to_size(EMBED_MAX_W, EMBED_MAX_H, true);
                            if sheet.insert_image(excel_row, col, &image).is_ok() {
                                has_image = true;
                            } else {
                                skipped += 1;
                            }
                        }
                        None => skipped += 1,
                    }
                }
                "text" => {
                    if let Some(text) = value.as_str() {
                        let _ = sheet.write_string_with_format(excel_row, col, text, &wrap_fmt);
                    }
                }
                "number" | "duration" => {
                    if let Some(number) = value.as_f64() {
                        let _ = sheet.write_number_with_format(excel_row, col, number, &plain_fmt);
                    }
                }
                _ => {
                    if let Some(text) = value.as_str() {
                        let _ = sheet.write_string_with_format(excel_row, col, text, &plain_fmt);
                    }
                }
            }
        }
        if has_image {
            let _ = sheet.set_row_height(excel_row, EMBED_ROW_HEIGHT);
        }
    }

    workbook.save(input.target).map_err(|e| format!("导出失败：{e}"))?;
    Ok(skipped)
}

/// 图片条目字节：内嵌 dataURL → base64 解码；仓库路径引用 → 用预读好的字节；
/// 空间模式下的外置附件读不到（与宿主原导出一致）。
fn resolve_image(images: &HashMap<String, Vec<u8>>, space: bool, entry: &str) -> Option<Vec<u8>> {
    if entry.starts_with("data:") {
        return decode_data_url(entry);
    }
    if space {
        return None;
    }
    images.get(entry).cloned()
}

/// 图片字节 → 可嵌入 xlsx 的 Image：原生支持格式直读；其余（webp 等）重解码转 PNG。
fn native_or_png(bytes: Vec<u8>) -> Option<Image> {
    match Image::new_from_buffer(&bytes) {
        Ok(image) => Some(image),
        Err(_) => {
            let image = image::load_from_memory(&bytes).ok()?;
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Image::new_from_buffer(out.into_inner().as_slice()).ok()
        }
    }
}

/// dataURL 条目解码（`data:<mime>;base64,<payload>` → 字节）。
pub fn decode_data_url(entry: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let payload = entry.strip_prefix("data:").and_then(|url| url.split_once(','))?.1;
    base64::engine::general_purpose::STANDARD.decode(payload).ok()
}

/// 工作表名净化：禁止 `[]:*?/\` 字符 + 按字符截到 31 个（Excel 限制）。
pub fn sheet_name_of(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|ch| match ch {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            _ => ch,
        })
        .collect();
    let trimmed = cleaned.trim();
    let base = if trimmed.is_empty() { "表格" } else { trimmed };
    base.chars().take(31).collect()
}

/// 列宽按字段类型预设（Excel 字符宽度单位）。
fn column_width_of(field_type: &str) -> f64 {
    match field_type {
        "text" => 40.0,
        "number" => 12.0,
        "duration" => 10.0,
        "singleSelect" => 14.0,
        _ => 20.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_name_sanitizes_and_caps() {
        assert_eq!(sheet_name_of("销售表"), "销售表");
        assert_eq!(sheet_name_of("a[b]:c*d?e/f\\g"), "a_b__c_d_e_f_g");
        assert_eq!(sheet_name_of(""), "表格");
        assert_eq!(sheet_name_of(&"汉".repeat(32)), "汉".repeat(31));
    }
}
