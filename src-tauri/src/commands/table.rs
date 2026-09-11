//! 多维表格（.atb）文件命令。
//!
//! 命名约定同 .atlx：`<sanitized-title>.atb`（标题即文件名，任意文件夹，同名自动加序号）。
//! 重命名/移动会扫描所有 .atlx 更新 table 节点 file 引用（链接维护，
//! 复用 `commands/vault.rs` 的 collect/flush 扫描函数，事务模式与 rename_note 对称）。

use std::collections::HashSet;

use base64::Engine;
use chrono::Utc;
use nanoid::nanoid;
use serde::Serialize;
use tauri::{Manager, State};

use crate::commands::vault::{
    collect_ref_updates, ensure_no_id_conflict, flush_canvas_updates, mime_from_ext,
    remove_replaced_file,
};
use crate::vault::{
    cache_evict_table, cache_put_table, delete_vault_file, read_table_file, read_table_file_cached,
    reorder_by, rename_note_file, rel_with_new_title, safe_join, same_physical_file,
    sanitize_filename, write_table_file, TableField, TableFile, TablePatch, VaultState,
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
    let root = state.root()?;
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
    let filename = format!("{}.atb", sanitize_filename(&title));
    let rel = if dir.is_empty() {
        filename
    } else {
        format!("{dir}/{filename}")
    };
    let path = safe_join(&root, &rel, true)?;
    // 后端兜底：同名表格已存在则拒绝（前端 dedupe 是正常路径，此处防绕过/同步盘合并覆盖）
    if path.exists() {
        return Err(format!("表格名冲突：{}", title));
    }
    write_table_file(&path, &table)?;
    Ok(TableCreateResult { id, file: rel })
}

/// 读 .atb 文件（按相对仓库根路径，如 `项目A/分镜.atb`）。
#[tauri::command]
pub fn read_table_vault(file: String, state: State<'_, VaultState>) -> Result<TableFile, String> {
    let root = state.root()?;
    let path = safe_join(&root, &file, false)?;
    read_table_file(&path)
}

/// 写 .atb 文件（整体原子写；title 改了会自动重命名文件到同目录新名并同步画布 table 节点引用）。
/// `base_updated_at`：乐观并发基准（加载时的磁盘 updatedAt），磁盘版本更新则拒绝写（None = 不检查）。
/// 返回写入的 updated_at，前端保存成功后同步乐观锁基准。
/// 乐观锁检查与 createdAt 保留共走一次带缓存读（指纹校验失效，外部改动即时感知）。
#[tauri::command]
pub fn write_table_vault(
    mut table: TableFile,
    file: String,
    base_updated_at: Option<i64>,
    state: State<'_, VaultState>,
) -> Result<i64, String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atb", sanitize_filename(&table.title)));
    // 新路径已存在且 id 不同（前端 dedupe 被绕过/同步盘合并）：拒绝覆盖
    ensure_no_id_conflict(
        &new_path,
        &table.id,
        &|p| read_table_file(p).ok().map(|t| t.id),
        "表格",
        &table.title,
    )?;
    let now = Utc::now().timestamp();
    // 乐观并发 + createdAt 保留共读一次（缓存命中免重读；文件缺失 = 新表用 now）
    if old_path.exists() {
        let (_, disk) = read_table_file_cached(&state, &root, &file)
            .map_err(|e| format!("磁盘表格文件损坏，无法保存：{} ({e})", old_path.display()))?;
        if let Some(base) = base_updated_at {
            if disk.updated_at > base {
                return Err("表格已被外部修改，请重载后再编辑".to_string());
            }
        }
        table.created_at = disk.created_at;
    } else {
        table.created_at = now;
    }
    table.updated_at = now;
    let new_rel = rel_with_new_title(&file, &table.title, "atb");
    write_table_file(&new_path, &table)?;
    // title 变更导致路径漂移：table 节点按 file 路径引用，须同步全部 .atlx（防断链）
    if old_path != new_path {
        let pending = collect_ref_updates(&root, &file, &new_rel)?;
        if let Err(e) = flush_canvas_updates(&pending) {
            // 回滚：删新文件（旧文件未动、引用未刷，保持原名），防改名后引用断裂
            let _ = std::fs::remove_file(&new_path);
            return Err(format!("更新画布引用失败：{e}"));
        }
        remove_replaced_file(&old_path, &new_path, "表格")?;
    }
    cache_evict_table(&state, &file);
    cache_put_table(&state, &new_path, &new_rel, &table);
    Ok(now)
}

/// 增量保存 .atb（自动保存主路径）：只写变化/新增/删除的字段与行（前端按引用 diff 计算补丁），
/// 按稳定 id 合并到磁盘全量文件——乐观锁 / createdAt 保留 / title 重命名（同步画布引用）/
/// 原子写语义与 write_table_vault 一致，IPC 载荷从整表缩到变化行/字段（image dataURL 不重传）。
/// `force` = 保留本地（绕过乐观锁强制覆盖，冲突条「保留本地并保存」用）。
/// 返回 (updatedAt, 写盘后的相对路径)——title 变更重命名文件时前端按新路径更新 tableFile。
#[tauri::command]
pub fn patch_table_vault(
    patch: TablePatch,
    file: String,
    base_updated_at: Option<i64>,
    force: bool,
    state: State<'_, VaultState>,
) -> Result<(i64, String), String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    // 磁盘文件缺失（外部删除）：补丁只有变化实体，重建会丢未变化部分——拒绝并回退全量写
    if !old_path.exists() {
        return Err("表格文件不存在（已从磁盘删除）".to_string());
    }
    let (_, mut table) = read_table_file_cached(&state, &root, &file)?;
    // 防串文件守卫：补丁属于另一表格（陈旧保存回调）→ 拒绝，防跨文件混写
    if patch.id != table.id {
        return Err("表格身份不匹配，已中止保存".to_string());
    }
    if let Some(title) = &patch.title {
        table.title = title.clone();
    }
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atb", sanitize_filename(&table.title)));
    let new_rel = rel_with_new_title(&file, &table.title, "atb");
    // 名冲突守卫仅路径漂移（title 变更）时检查——同名时文件就是本次基底，id 必然一致，免每次保存全量重读
    if old_path != new_path {
        ensure_no_id_conflict(
            &new_path,
            &table.id,
            &|p| read_table_file(p).ok().map(|t| t.id),
            "表格",
            &table.title,
        )?;
    }
    let now = Utc::now().timestamp();
    // 乐观并发：磁盘版本比前端基准新 → 拒绝覆盖（force = 保留本地强制覆盖）；缓存已按指纹保证磁盘最新
    if !force {
        if let Some(base) = base_updated_at {
            if table.updated_at > base {
                return Err("表格已被外部修改，请重载后再编辑".to_string());
            }
        }
    }
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
    table.updated_at = now;
    write_table_file(&new_path, &table)?;
    // title 变更导致路径漂移：table 节点按 file 路径引用，须同步全部 .atlx（防断链）
    if old_path != new_path {
        let pending = collect_ref_updates(&root, &file, &new_rel)?;
        if let Err(e) = flush_canvas_updates(&pending) {
            // 回滚：删新文件（旧文件未动、引用未刷，保持原名），防改名后引用断裂
            let _ = std::fs::remove_file(&new_path);
            return Err(format!("更新画布引用失败：{e}"));
        }
        remove_replaced_file(&old_path, &new_path, "表格")?;
    }
    cache_put_table(&state, &new_path, &new_rel, &table);
    Ok((now, new_rel))
}

/// 重命名表格：更新 .atb 内 title + 同目录重命名文件 + 扫描所有 .atlx 更新 table 节点引用。
/// 事务模式同 rename_note：预扫描 → 改名 → 统一写回，写回失败回滚文件。
#[tauri::command]
pub fn rename_table_vault(
    file: String,
    new_title: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atb", sanitize_filename(&new_title)));
    let new_rel = rel_with_new_title(&file, &new_title, "atb");
    let old_table = read_table_file(&old_path)?;
    let same_file = same_physical_file(&old_path, &new_path);
    // 目标已存在且非同文件：读现存 id 比对，异表拒绝覆盖（同 write_table_vault，防同步盘合并/
    // 隐藏/排除目录绕过前端 dedupe 时静默覆盖另一表格）
    if !same_file {
        ensure_no_id_conflict(
            &new_path,
            &old_table.id,
            &|p| read_table_file(p).ok().map(|t| t.id),
            "表格",
            &new_title,
        )?;
    }
    let pending = collect_ref_updates(&root, &file, &new_rel)?;
    let mut table = old_table.clone();
    table.title = new_title;
    table.updated_at = Utc::now().timestamp();
    // 先写新文件再删旧文件，保证不丢数据
    write_table_file(&new_path, &table)?;
    remove_replaced_file(&old_path, &new_path, "表格")?;
    if let Err(e) = flush_canvas_updates(&pending) {
        // 尽力回滚：恢复旧文件（旧标题），清理新文件
        let _ = write_table_file(&old_path, &old_table);
        if old_path != new_path {
            let _ = std::fs::remove_file(&new_path);
        }
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    Ok(())
}

/// 移动表格文件到新路径（跨目录，拖动文件到文件夹用）+ 扫描所有 .atlx 更新 table 节点引用
/// （与 rename_table_vault 对称；rename_note_file 复用通用移动：路径校验 + 防覆盖）。
#[tauri::command]
pub fn move_table_vault(
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
    Ok(())
}

/// 删除表格 .atb 文件（不更新 .atlx 引用，画布 table 节点断链降级「文件缺失」）。
/// 附件目录按 tableId 划分、随表私有：删除前读表拿 id，删文件后随删整个附件目录
/// （读不到 id——文件损坏/已被外部删除——则跳过，残留目录不拦截删除）。
#[tauri::command]
pub fn delete_table_vault(file: String, state: State<'_, VaultState>) -> Result<(), String> {
    let root = state.root()?;
    let table_id = read_table_file(&safe_join(&root, &file, false)?)
        .ok()
        .map(|t| t.id);
    delete_vault_file(&root, &file)?;
    if let Some(id) = table_id {
        if let Ok(dir) = safe_join(&root, &table_attachments_rel(&id), false) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    Ok(())
}

/// 回收表格孤儿图片附件：删除 `.atelyx/attachments/<tableId>/` 下未被 .atb 任一 image
/// 单元格引用的文件。文件名每次导入唯一（`img-<nanoid>.<ext>`）——未被引用即确定孤儿，
/// 无共享文件、无其他消费者（目录按 tableId 私有）。
/// 会话内不调用（删除后 Ctrl+Z 需能恢复引用）；切表/关闭表格时调用（该表撤销栈已清、
/// 显示缓存已清，无跨会话恢复路径）。读盘失败（文件损坏/已被外部删除）返回 0 保守不清理——
/// 引用集合未知，防误删引用中的文件。返回删除文件数。
#[tauri::command]
pub fn cleanup_table_attachments_vault(
    file: String,
    state: State<'_, VaultState>,
) -> Result<usize, String> {
    let root = state.root()?;
    let table = match read_table_file(&safe_join(&root, &file, false)?) {
        Ok(t) => t,
        Err(_) => return Ok(0),
    };
    // 收集全部 image 单元格的路径引用（遗留 `data:` 条目非路径，不计入）
    let mut referenced: HashSet<String> = HashSet::new();
    for field in &table.fields {
        if field.field_type != "image" {
            continue;
        }
        for row in &table.rows {
            let Some(value) = row.values.get(&field.id) else { continue };
            for item in image_cell_entries(value) {
                if !item.starts_with("data:") {
                    referenced.insert(item.to_string());
                }
            }
        }
    }
    // 目录不存在 = 无附件可清
    let Ok(dir) = safe_join(&root, &table_attachments_rel(&table.id), false) else {
        return Ok(0);
    };
    if !dir.is_dir() {
        return Ok(0);
    }
    // 只删顶层普通文件（附件均为单层存放，不递归——防误删子目录）
    let mut removed = 0;
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!("{}/{}", table_attachments_rel(&table.id), name);
        if referenced.contains(&rel) {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 图片单元格条目（双形态兼容）：新形态 `{ images: [...] }` / 旧形态 `string[]` → 字符串列表
/// （路径引用或遗留内嵌 dataURL）。其他形态（空/缺省）→ 空列表。
fn image_cell_entries(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .or_else(|| value.get("images").and_then(|v| v.as_array()))
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// 表格附件目录（相对仓库根）：`.atelyx/attachments/<tableId>/`。隐藏目录（`.` 开头）——
/// watcher / 文件树 / 全仓库扫描天然跳过，图片写盘零回波、零树噪声。
/// 图片字节不随 .atb 内嵌，单元格只存路径引用（大表多图免每次保存全量序列化图片）。
fn table_attachments_rel(table_id: &str) -> String {
    format!(".atelyx/attachments/{table_id}")
}

/// 图片条目字节：遗留内嵌 dataURL → base64 解码；外置路径引用 → 读仓库附件（safe_join 校验）。
/// xlsx 导出用；非 dataURL 条目按相对仓库根路径解析。
fn resolve_table_image_bytes(root: &std::path::Path, entry: &str) -> Result<Vec<u8>, String> {
    if entry.starts_with("data:") {
        data_url_to_bytes(entry).ok_or_else(|| "图片数据解码失败".to_string())
    } else {
        let path = safe_join(root, entry, false)?;
        std::fs::read(&path).map_err(|e| format!("读取图片失败：{} ({e})", entry))
    }
}

/// 把系统文件选择器选中的图片复制为表格附件，返回相对仓库根路径。
/// 文件名 = `img-<nanoid>.<ext>`（每次导入唯一：删除后重导不覆盖旧文件、不撞缓存/撤销引用；
/// 与迁移的确定性命名 `img-<rowId>-<fieldId>-<idx>` 前缀不同，互不冲突）。
/// 路径来自 OS 对话框（用户显式选择），非仓库内路径不走 safe_join（同旧 read_external_image_data_url）。
#[tauri::command]
pub fn import_table_image_vault(
    src: String,
    table_id: String,
    state: State<'_, VaultState>,
) -> Result<String, String> {
    let root = state.root()?;
    let src_path = std::path::Path::new(&src);
    if !src_path.is_file() {
        return Err(format!("文件不存在：{}", src));
    }
    let ext = mime_from_ext(&src)
        .and_then(ext_from_mime)
        .ok_or_else(|| format!("非图片文件：{}", src))?;
    let rel = format!("{}/img-{}.{}", table_attachments_rel(&table_id), nanoid!(), ext);
    let dest = safe_join(&root, &rel, true)?;
    // 唯一名不可能已存在；直接复制
    std::fs::copy(&src_path, &dest).map_err(|e| format!("复制图片失败：{e}"))?;
    Ok(rel)
}

/// 导出表格为 .xlsx（目标路径来自系统保存对话框，任意位置可写）。
/// 列 = 字段顺序，表头 = 字段名（金色底 + 粗体 + 全表边框 + 冻结首行）；
/// image 字段嵌入单元格首图（等比缩至 140x90），text 换行，number/duration 数值，其余文本。
/// 图片条目兼容两种形态：遗留内嵌 dataURL 直接解码；外置附件路径按仓库根读取。
#[tauri::command]
pub fn export_table_xlsx(
    table: TableFile,
    target_path: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    use rust_xlsxwriter::{Format, FormatBorder, Image, Workbook};

    let root = state.root()?;
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .set_name(sheet_name_of(&table.title))
        .map_err(|e| format!("设置工作表名失败：{e}"))?;

    let header_fmt = Format::new()
        .set_bold()
        .set_background_color("D4AF37")
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
                        if let Ok(bytes) = resolve_table_image_bytes(&root, url) {
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
