//! `.atb` 表格导出 xlsx：列 = 字段顺序，表头金色底 + 粗体 + 全表细边框 + 冻结首行；
//! image 字段嵌单元格首图（等比缩至 140×90，行高撑开），text 换行、number/duration 写数值、
//! 其余（singleSelect/未知类型）写文本。单元格显示样式不导出。

use std::path::Path;

use base64::Engine;
use rust_xlsxwriter::{Format, FormatBorder, Image, Workbook};

use crate::table_ops::image_entries;
use crate::vault::{TableFile, safe_join};

/// 嵌入图片等比缩放上限与行高（三者联动：图片缩至行高内）。
const EMBED_MAX_W: u32 = 140;
const EMBED_MAX_H: u32 = 90;
const EMBED_ROW_HEIGHT: f64 = 90.0;

/// 导出表格为 xlsx 到目标路径（任意位置可写），返回无法嵌入的图片数。
/// 图片条目按形态解析：dataURL 解码；外置附件路径经仓库根读取（safe_join 校验）。
/// xlsx 仅支持 png/jpeg/gif/bmp，其余格式（如 webp）经重解码转 PNG 后嵌入；
/// 解码与转换失败的图跳过并计入返回值，不阻断导出。
pub fn export_table_xlsx(table: &TableFile, root: Option<&Path>, target_path: &Path) -> Result<usize, String> {
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

    for (c, field) in table.fields.iter().enumerate() {
        let col = c as u16;
        let _ = sheet.write_string_with_format(0, col, &field.name, &header_fmt);
        let _ = sheet.set_column_width(col, column_width_of(&field.field_type));
    }
    if !table.fields.is_empty() {
        let _ = sheet.set_freeze_panes(1, 0);
    }

    let mut skipped = 0usize;
    for (r, row) in table.rows.iter().enumerate() {
        let excel_row = (r + 1) as u32;
        let mut has_image = false;
        for (c, field) in table.fields.iter().enumerate() {
            let col = c as u16;
            let Some(value) = row.values.get(&field.id) else { continue };
            match field.field_type.as_str() {
                // 多图单元格只导首图
                "image" => {
                    if let Some(url) = image_entries(value).first().copied() {
                        match resolve_image_bytes(root, url).ok().as_deref().and_then(native_or_png) {
                            Some(img) => {
                                let img = img.set_scale_to_size(EMBED_MAX_W, EMBED_MAX_H, true);
                                if sheet.insert_image(excel_row, col, &img).is_ok() {
                                    has_image = true;
                                } else {
                                    skipped += 1;
                                }
                            }
                            None => skipped += 1,
                        }
                    }
                }
                "text" => {
                    if let Some(s) = value.as_str() {
                        let _ = sheet.write_string_with_format(excel_row, col, s, &wrap_fmt);
                    }
                }
                "number" | "duration" => {
                    if let Some(n) = value.as_f64() {
                        let _ = sheet.write_number_with_format(excel_row, col, n, &plain_fmt);
                    }
                }
                _ => {
                    if let Some(s) = value.as_str() {
                        let _ = sheet.write_string_with_format(excel_row, col, s, &plain_fmt);
                    }
                }
            }
        }
        if has_image {
            let _ = sheet.set_row_height(excel_row, EMBED_ROW_HEIGHT);
        }
    }

    workbook
        .save(target_path)
        .map_err(|e| format!("导出失败：{e}"))?;
    Ok(skipped)
}

/// 图片字节 → 可嵌入 xlsx 的 Image：原生支持格式直读；其余（webp 等）重解码转 PNG。
fn native_or_png(bytes: &[u8]) -> Option<Image> {
    match Image::new_from_buffer(bytes) {
        Ok(img) => Some(img),
        // 失败多为 xlsx 不支持的格式（webp 等）：经 image crate 解码转 PNG 后重试
        Err(_) => {
            let img = image::load_from_memory(bytes).ok()?;
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Image::new_from_buffer(out.into_inner().as_slice()).ok()
        }
    }
}

/// 遗留 dataURL 条目解码（`data:<mime>;base64,<payload>` → 字节；非该形态 = None）。
/// 视图加载与 xlsx 导出共用该兼容口径。
pub fn decode_data_url(entry: &str) -> Option<Vec<u8>> {
    let payload = entry.strip_prefix("data:").and_then(|u| u.split_once(','))?.1;
    base64::engine::general_purpose::STANDARD.decode(payload).ok()
}

/// 图片条目字节：内嵌 dataURL → base64 解码；外置路径引用 → 读仓库附件（safe_join 校验）。
/// `root` 为 None（无本地仓库根）时路径引用无法解析，返回错误由调用方跳过该图。
fn resolve_image_bytes(root: Option<&Path>, entry: &str) -> Result<Vec<u8>, String> {
    if entry.starts_with("data:") {
        return decode_data_url(entry).ok_or_else(|| "图片数据解码失败".to_string());
    }
    let root = root.ok_or("图片为仓库路径引用且当前无本地仓库根，无法读取")?;
    let path = safe_join(root, entry, false)?;
    std::fs::read(&path).map_err(|e| format!("读取图片失败：{} ({e})", entry))
}

/// 工作表名净化：禁止 `[]:*?/\` 字符 + 长度 ≤ 31 字符（Excel 限制，超长/非法名会拒绝）。
pub fn sheet_name_of(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    // 按字符而非字节截断：31 不是多字节字符的边界（String::truncate 会 panic）
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
    fn sheet_name_keeps_short_titles() {
        assert_eq!(sheet_name_of("销售表"), "销售表");
        assert_eq!(sheet_name_of("Q3 plan"), "Q3 plan");
    }

    #[test]
    fn sheet_name_replaces_illegal_chars() {
        assert_eq!(sheet_name_of("a[b]:c*d?e/f\\g"), "a_b__c_d_e_f_g");
        assert_eq!(sheet_name_of("[]:*?/\\"), "_______");
    }

    #[test]
    fn sheet_name_falls_back_when_blank() {
        assert_eq!(sheet_name_of(""), "表格");
        assert_eq!(sheet_name_of("   "), "表格");
    }

    #[test]
    fn sheet_name_caps_at_31_chars() {
        // 13 个汉字 = 39 字节：按字节截断会落在字符中间 panic
        let cjk = "第三季度市场推广投放计划表";
        assert_eq!(sheet_name_of(cjk), cjk);
        assert_eq!(sheet_name_of(&"汉".repeat(32)), "汉".repeat(31));
    }

    #[test]
    fn export_writes_xlsx_and_skips_missing_images() {
        let tmp = crate::vault::test_support::TempDir::new("xlsx-export");
        let field_text = crate::vault::TableField {
            id: "f1".into(),
            name: "名称".into(),
            field_type: "text".into(),
            options: None,
            width: None,
            calc_type: None,
        };
        let field_image = crate::vault::TableField {
            id: "f2".into(),
            name: "图".into(),
            field_type: "image".into(),
            options: None,
            width: None,
            calc_type: None,
        };
        let mut values = serde_json::Map::new();
        values.insert("f1".into(), serde_json::json!("单元值"));
        values.insert("f2".into(), serde_json::json!({ "images": ["缺失.png"] }));
        let row = crate::vault::TableRow { id: "r1".into(), values, height: None, styles: None };
        let table = crate::vault::TableFile {
            schema: crate::vault::TABLE_SCHEMA.into(),
            id: "t1".into(),
            title: "导出表".into(),
            fields: vec![field_text, field_image],
            rows: vec![row],
            created_at: 0,
            updated_at: 0,
        };
        let out = tmp.join("out.xlsx");
        let skipped = export_table_xlsx(&table, Some(&tmp), &out).unwrap();
        assert_eq!(skipped, 1, "缺失的附件图应计入 skipped");
        let meta = std::fs::metadata(&out).unwrap();
        assert!(meta.len() > 0);
    }
}
