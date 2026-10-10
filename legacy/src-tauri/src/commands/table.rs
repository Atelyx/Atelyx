//! 多维表格（.atb）文件命令：命名约定同 .atlx——`<sanitized-title>.atb`（标题即文件名，任意文件夹，同名自动加序号）。
//! 重命名/移动会扫描所有 .atlx 更新 table 节点 file 引用（链接维护，复用 `commands/vault.rs`
//! 的 collect/flush 扫描函数，事务模式与 rename_note 对称）。

use std::collections::HashSet;

use base64::Engine;
use chrono::Utc;
use nanoid::nanoid;
use serde::Serialize;
use tauri::{Manager, State, WebviewWindow};

use super::content_broadcast::{broadcast_content_changes, ContentChange};
use super::entity_txn;
use super::temp_attachment::{instance_temp_dir, validate_table_id, TempComponent};
use crate::commands::vault::{
    collect_ref_updates, flush_canvas_updates, mime_from_ext, PatchWriteResult,
};
use crate::vault::{
    delete_vault_file, read_table_file,
    reorder_by, rename_note_file, safe_join,
    sanitize_filename, TableField, TableFile, TablePatch, VaultState,
    TABLE_SCHEMA,
};

/// `create_table_vault` 返回值：id（运行时身份）+ file（磁盘定位）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCreateResult {
    pub id: String,
    pub file: String,
}

/// 新建空表格（自带一个「名称」文本字段，空白表无字段难用），返回 `{ id, file }`。
#[tauri::command]
pub fn create_table_vault(
    title: String,
    dir: String,
    state: State<'_, VaultState>,
) -> Result<TableCreateResult, String> {
    let id = nanoid!();
    let now = Utc::now().timestamp();
    let table = TableFile {
        schema: TABLE_SCHEMA.to_string(),
        id: id.clone(),
        title: title.clone(),
        fields: vec![TableField {
            id: nanoid!(),
            name: "名称".to_string(),
            field_type: "text".to_string(),
            options: None,
            width: None,
            calc_type: None,
        }],
        rows: vec![],
        created_at: now,
        updated_at: now,
    };
    let (id, file) = entity_txn::create_entity(&state, &entity_txn::table_io(), &dir, table)?;
    Ok(TableCreateResult { id, file })
}

/// 读 .atb 文件（按相对仓库根路径，如 `项目A/分镜.atb`）。
#[tauri::command]
pub fn read_table_vault(file: String, state: State<'_, VaultState>) -> Result<TableFile, String> {
    let root = state.root()?;
    let path = safe_join(&root, &file, false)?;
    read_table_file(&path)
}

/// 写 .atb 文件（整体原子写；title 改了会自动重命名文件到同目录新名并同步画布 table 节点引用）。
/// 返回落盘后的 updated_at（前端据此更新本地时间戳）。
/// createdAt 保留走一次带缓存读（指纹校验失效，外部改动即时感知）。
#[tauri::command]
pub fn write_table_vault(
    window: WebviewWindow,
    table: TableFile,
    file: String,
    state: State<'_, VaultState>,
) -> Result<i64, String> {
    entity_txn::write_entity(&window, &state, &entity_txn::table_io(), &file, table)
}

/// 增量保存 .atb（自动保存主路径）：只写变化/新增/删除的字段与行（前端按引用 diff 计算补丁），
/// 按稳定 id 合并到磁盘全量文件——createdAt 保留 / title 重命名（同步画布引用）/
/// 原子写语义与 write_table_vault 一致，IPC 载荷从整表缩到变化行/字段（image dataURL 不重传）。
/// 返回 (updatedAt, 写盘后的相对路径)——title 变更重命名文件时前端按新路径更新 tableFile。
#[tauri::command]
pub fn patch_table_vault(
    window: WebviewWindow,
    patch: TablePatch,
    file: String,
    state: State<'_, VaultState>,
) -> Result<PatchWriteResult, String> {
    entity_txn::patch_entity(&window, &state, &entity_txn::table_io(), &file, &patch, |table, patch| {
        // 按稳定 id 合并（removed 幂等；upsert 覆盖同 id 或追加）
        let removed_fields: HashSet<&String> = patch.removed_field_ids.iter().collect();
        table.fields.retain(|f| !removed_fields.contains(&f.id));
        for f in &patch.upsert_fields {
            match table.fields.iter_mut().find(|x| x.id == f.id) {
                Some(existing) => *existing = f.clone(),
                None => table.fields.push(f.clone()),
            }
        }
        let removed_rows: HashSet<&String> = patch.removed_row_ids.iter().collect();
        table.rows.retain(|r| !removed_rows.contains(&r.id));
        for r in &patch.upsert_rows {
            match table.rows.iter_mut().find(|x| x.id == r.id) {
                Some(existing) => *existing = r.clone(),
                None => table.rows.push(r.clone()),
            }
        }
        // 顺序变化（拖拽排序/复制行/左右插列）：按补丁携带的 id 全序重排——
        // 已删 id 的下标自然空置，order 未出现的实体（并发新增）保持相对顺序置尾
        if let Some(order) = &patch.field_order {
            reorder_by(&mut table.fields, order, |f| f.id.as_str());
        }
        if let Some(order) = &patch.row_order {
            reorder_by(&mut table.rows, order, |r| r.id.as_str());
        }
    })
}

/// 重命名表格：更新 .atb 内 title + 同目录重命名文件 + 扫描所有 .atlx 更新 table 节点引用。
/// 事务模式同 rename_note：预扫描 → 改名 → 统一写回，写回失败回滚文件。
#[tauri::command]
pub fn rename_table_vault(
    window: WebviewWindow,
    file: String,
    new_title: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    entity_txn::rename_entity(&window, &state, &entity_txn::table_io(), &file, new_title)
}

/// 移动表格文件到新路径（跨目录，拖动文件到文件夹用）+ 扫描所有 .atlx 更新 table 节点引用
/// （与 rename_table_vault 对称；rename_note_file 复用通用移动：路径校验 + 防覆盖）。
#[tauri::command]
pub fn move_table_vault(
    window: WebviewWindow,
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    let pending = collect_ref_updates(&root, &old_file, &new_file)?;
    rename_note_file(&root, &old_file, &new_file)?;
    if let Err(e) = flush_canvas_updates(&pending) {
        let _ = rename_note_file(&root, &new_file, &old_file);
        return Err(format!("更新画布引用失败，移动已回滚（请重试）：{e}"));
    }
    broadcast_content_changes(
        &window,
        &root.to_string_lossy(),
        vec![ContentChange::rename(&old_file, &new_file)],
    );
    Ok(())
}

/// 删除表格 .atb 文件（不更新 .atlx 引用，画布 table 节点断链降级「文件缺失」）。
/// 图片附件目录按 tableId 划分、随表私有：删除前读表拿 id，删文件后随删附件目录
/// （读不到 id——文件损坏/已被外部删除——则跳过，残留目录不拦截删除本身）。
#[tauri::command]
pub fn delete_table_vault(file: String, state: State<'_, VaultState>) -> Result<(), String> {
    let root = state.root()?;
    let table_id = read_table_file(&safe_join(&root, &file, false)?)
        .ok()
        .map(|t| t.id);
    delete_vault_file(&root, &file)?;
    if let Some(id) = table_id {
        if let Ok(dir) = safe_join(&root, &instance_temp_dir(TempComponent::Table, &id), false) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    Ok(())
}

/// 图片单元格条目（双形态兼容）：新形态 `{ images: [...] }` / 旧形态 `string[]` → 字符串列表
/// （路径引用或遗留内嵌 dataURL）。其他形态（空/缺省）→ 空列表。
pub(crate) fn image_cell_entries(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .or_else(|| value.get("images").and_then(|v| v.as_array()))
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// 图片条目字节：内嵌 dataURL → base64 解码；外置路径引用 → 读仓库附件（safe_join 校验）。
/// `root` 为 None（协作空间会话无本地仓库根）时路径引用无法解析，返回错误由调用方跳过该图。
/// 导出前端已把路径引用就地换成 dataURL（见 `tableStore.inlineImageCellsForExport`），
/// 所以纯空间会话亦能带图导出；路径分支只服务旧快照/未内联的调用方。
fn resolve_table_image_bytes(root: Option<&std::path::Path>, entry: &str) -> Result<Vec<u8>, String> {
    if entry.starts_with("data:") {
        data_url_to_bytes(entry).ok_or_else(|| "图片数据解码失败".to_string())
    } else {
        let root = root.ok_or_else(|| "图片为仓库路径引用且当前无本地仓库根，无法读取".to_string())?;
        let path = safe_join(root, entry, false)?;
        std::fs::read(&path).map_err(|e| format!("读取图片失败：{} ({e})", entry))
    }
}

/// 把本机图片（前端读好的 base64 字节 + 文件名）落为表格附件，返回相对仓库根路径。
/// 文件名 = `img-<nanoid>.<ext>`（每次导入唯一：删除后重导不覆盖旧文件、不撞缓存/撤销引用；
/// 与迁移的确定性命名 `img-<rowId>-<fieldId>-<idx>` 前缀不同，互不冲突）。
/// 表格图片落为附件（前端读为 base64 的字节 + 文件名），返回唯一相对路径供单元格引用
/// （每次导入新文件，删除后重导不覆盖旧文件、不撞显示缓存）。
/// 落 `temp/tables/<tableId>/`（temp 组件目录，按引用回收）；字节由前端经 IPC 传输而非后端
/// 读本机路径：协作空间后端同样消费前端 base64（路径对服务端不可达），两侧调用形状保持一致。
#[tauri::command]
pub fn import_table_image_vault(
    file_name: String,
    data: String,
    table_id: String,
    state: State<'_, VaultState>,
) -> Result<String, String> {
    validate_table_id(&table_id)?;
    let root = state.root()?;
    let ext = mime_from_ext(&file_name)
        .and_then(ext_from_mime)
        .ok_or_else(|| format!("非图片文件：{}", file_name))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("图片数据解码失败：{e}"))?;
    let rel = format!("{}/img-{}.{}", instance_temp_dir(TempComponent::Table, &table_id), nanoid!(), ext);
    let dest = safe_join(&root, &rel, true)?;
    std::fs::write(&dest, bytes).map_err(|e| format!("写入图片失败：{e}"))?;
    Ok(rel)
}

/// 导出表格为 .xlsx（目标路径来自系统保存对话框，任意位置可写）。
/// 列 = 字段顺序，表头 = 字段名（金色底 + 粗体 + 全表边框 + 冻结首行）；
/// image 字段嵌入单元格首图（等比缩至 140x90），text 换行，number/duration 数值，其余文本。
/// 图片条目两种形态：dataURL 直接解码；外置附件路径按本地仓库根读取——协作空间会话无本地
/// 仓库根，故不在此硬要求 root（前端导出前已把路径引用换成 dataURL）。
#[tauri::command]
pub fn export_table_xlsx(
    table: TableFile,
    target_path: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    use rust_xlsxwriter::{Format, FormatBorder, Image, Workbook};

    let root = state.root().ok();
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

    for (r, row) in table.rows.iter().enumerate() {
        let excel_row = (r + 1) as u32;
        let mut has_image = false;
        for (c, field) in table.fields.iter().enumerate() {
            let col = c as u16;
            let Some(value) = row.values.get(&field.id) else { continue };
            match field.field_type.as_str() {
                // 多图单元格只导首图；dataURL/附件路径 → 字节 → 等比缩至 140x90 嵌入（行高撑开）
                "image" => {
                    if let Some(url) = image_cell_entries(value).first().copied() {
                        if let Ok(bytes) = resolve_table_image_bytes(root.as_deref(), url) {
                            if let Ok(mut img) = Image::new_from_buffer(&bytes) {
                                img = img.set_scale_to_size(140, 90, true);
                                if sheet.insert_image(excel_row, col, &img).is_ok() {
                                    has_image = true;
                                }
                            }
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
            let _ = sheet.set_row_height(excel_row, 90);
        }
    }

    workbook
        .save(&target_path)
        .map_err(|e| format!("导出失败：{e}"))
}

/// 保存 dataURL 图片到系统 Downloads 文件夹（放大预览右键「下载」用）。
/// 文件名 = `sanitize_filename` 净化后的基础名 + 按 mime 推的扩展名（png/jpg/webp/gif），
/// 重名自动追加 ` (1)` ` (2)` 序号（不覆盖已有文件）。
/// 目录用 OS 标准 Downloads（Windows FOLDERID_Downloads / Linux XDG），失败兜底用户主目录。
/// 数 MB 解码/写盘放 spawn_blocking：async 命令虽不占 UI 线程，但阻塞 tokio worker
/// 会影响同 runtime 的其他 async 任务（搜索代理等）。
#[tauri::command]
pub async fn save_image_to_downloads(
    file_name: String,
    data_url: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // dataURL 前缀 `data:<mime>;base64,` → mime；非 dataURL 直接报错
        let prefix = data_url.split_once(',').map(|(p, _)| p).ok_or("非图片数据")?;
        let mime = prefix
            .strip_prefix("data:")
            .and_then(|p| p.split(';').next())
            .ok_or("非图片数据")?;
        let ext = ext_from_mime(mime).ok_or("不支持的图片格式")?;
        let bytes = data_url_to_bytes(&data_url).ok_or("图片数据解码失败")?;
        let dir = app
            .path()
            .download_dir()
            .or_else(|_| app.path().home_dir())
            .map_err(|e| format!("无法定位下载目录：{e}"))?;
        let base = sanitize_filename(&file_name);
        // 重名自动加序号（`name (1).ext`、`name (2).ext`…），不覆盖已有文件
        let mut path = dir.join(format!("{base}.{ext}"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{base} ({n}).{ext}"));
            n += 1;
        }
        std::fs::write(&path, &bytes).map_err(|e| format!("写入失败：{e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("保存线程失败：{e}"))?
}

/// dataURL mime → 文件扩展名（与 `mime_from_ext` 的图片集合对称）。
fn ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// 工作表名净化：禁止 `[]:*?/\` 字符 + 长度 ≤ 31 **字符**（Excel 限制，超长/非法名会拒绝）。
fn sheet_name_of(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    // 按字符而非字节截断：31 不是多字节字符的边界（`String::truncate` 会 panic），
    // 且 Excel 的 31 上限本就是字符数
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
        "image" => 20.0,
        _ => 20.0,
    }
}

/// dataURL（`data:<mime>;base64,...`）→ 字节；非 dataURL/解码失败返回 None。
fn data_url_to_bytes(url: &str) -> Option<Vec<u8>> {
    let b64 = url.split_once(",")?.1;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

#[cfg(test)]
mod tests {
    use super::sheet_name_of;

    #[test]
    fn sheet_name_keeps_short_titles() {
        assert_eq!(sheet_name_of("销售表"), "销售表");
        assert_eq!(sheet_name_of("Q3 plan"), "Q3 plan");
    }

    #[test]
    fn sheet_name_replaces_illegal_chars() {
        assert_eq!(sheet_name_of("a[b]:c*d?e/f\\g"), "a_b__c_d_e_f_g");
        // 全是非法字符：替换后非空，不回落到「表格」
        assert_eq!(sheet_name_of("[]:*?/\\"), "_______");
    }

    #[test]
    fn sheet_name_falls_back_when_blank() {
        assert_eq!(sheet_name_of(""), "表格");
        assert_eq!(sheet_name_of("   "), "表格");
    }

    #[test]
    fn sheet_name_truncates_by_chars_not_bytes() {
        // 13 个汉字 = 39 字节：按字节截到 31 会落在字符中间 panic
        let cjk = "第三季度市场推广投放计划表";
        assert_eq!(cjk.chars().count(), 13);
        assert_eq!(cjk.len(), 39);
        assert_eq!(sheet_name_of(cjk), cjk);

        let ascii31 = "x".repeat(31);
        assert_eq!(sheet_name_of(&ascii31), ascii31);
    }

    #[test]
    fn sheet_name_caps_at_31_chars() {
        for title in [
            "汉".repeat(31),
            "汉".repeat(32),
            "汉".repeat(100),
            "🙂".repeat(40), // 4 字节字符
            "ab🙂".repeat(20),
        ] {
            let name = sheet_name_of(&title);
            assert!(
                name.chars().count() <= 31,
                "期望 ≤ 31 字符，实际 {}（{name}）",
                name.chars().count()
            );
            assert!(!name.is_empty());
        }
        assert_eq!(sheet_name_of(&"汉".repeat(32)).chars().count(), 31);
        assert_eq!(sheet_name_of(&"汉".repeat(32)), "汉".repeat(31));
    }
}
