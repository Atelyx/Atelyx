//! 仓库文件读写命令。
//!
//! 通过 `State<VaultState>` 注入当前仓库根路径。本模块为画布/笔记/附件/配置的唯一文件 I/O 出口。
//!
//! 本模块只做文件 I/O，不耦合业务语义：
//! - text 节点 bodyMd 的剥离/填充在 `services/vault` 层组合；
//! - 因此 `read_canvas_vault`/`write_canvas_vault` 直接读写 .atlx 文件，不操作 笔记/*.md。
//!
//! .atlx 文件命名约定：`<sanitized-title>.atlx`（标题即文件名，无 id 后缀，任意文件夹）
//! - 同名自动加序号（-2、-3）保证唯一性（`sanitize_filename` + 递增循环，见 `vault.rs`）；
//! - rename_canvas 时同时重命名文件；重命名/删除后所有引用它的画布内引用同步更新。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::Utc;
use nanoid::nanoid;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::vault::{
    append_chat_messages_file, cache_put_canvas, collect_md_link_updates,
    copy_folder as copy_folder_impl, create_folder as create_folder_impl,
    delete_folder as delete_folder_impl, delete_vault_file, flush_md_updates,
    init_vault_dirs, list_canvas_files,
    list_vault_tree as list_vault_tree_impl, markdown_link_path, read_canvas_file,
    read_canvas_file_cached, read_file_bytes, read_note as read_note_file,
    read_vault_config_with_backup,
    refresh_wiki_index, rename_folder as rename_folder_impl, rename_note_file, rel_with_new_title,
    resolve_link_target, rewrite_internal_links, safe_join, same_physical_file,
    sanitize_filename, walk_md_in, write_canvas_file, write_note as write_note_file, write_vault_config as write_vault_config_file,
    patch_vault_config,
    aggregate_tag_counts, refresh_tag_index,
    list_chat_sessions_file,
    read_chat_session_meta_file, write_chat_session_meta_file, delete_chat_session_meta_file,
    read_editor_chats_meta_file, write_editor_chats_meta_file,
    read_prompt_notes_file, write_prompt_notes_file,
    read_folder_colors_file, write_folder_colors_file,
    read_agents_file, write_agents_file,
    read_chat_messages_file, write_chat_messages_file,
    delete_chat_messages_file, read_dir_filtered, has_hidden_segment, regenerate_file_id, cache_evict_canvas,
    remap_sideloads as remap_sideloads_impl,
    remap_sideloads_by_dir as remap_sideloads_by_dir_impl,
    BacklinkRow, CanvasFile, CanvasFileRow, CanvasPatch, AgentConfig,
    ChatMessageRecord, ChatMetaFile, ChatSessionMeta, ChatSessionRow, DeleteFolderResult,
    FileTreeNode, VaultConfig, VaultState,
    WikiIndex, CANVAS_SCHEMA, query_wiki_backlinks,
    TagIndex, TagRow,
};

/// read_file 分页窗口的默认/上限参数：
/// - READ_WINDOW_MAX_LINE_CHARS：单行字符上限，超长截断并附后缀
/// - READ_WINDOW_MAX_BYTES：单次返回字节预算，超限停加行并标记 truncated（不硬拒，模型可翻页）
/// - READ_WINDOW_DEFAULT_LINES：默认/单次最大返回行数
const READ_WINDOW_DEFAULT_LINES: usize = 2000;
const READ_WINDOW_MAX_LINE_CHARS: usize = 2000;
const READ_WINDOW_MAX_BYTES: usize = 200_000;
/// edit_file 等内部「整读全文再改」路径的宽松防御上限（防对超大文件整读回填上下文拖死；
/// 非强制拒绝，仅防病态输入）。分页工具 read_file 走 read_vault_file_window，不受此限。
const EDIT_READ_MAX_BYTES: usize = 5_000_000;
use crate::watcher;

/// `open_vault` 返回的仓库信息。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    /// 仓库根绝对路径
    pub root: String,
    /// 仓库名（文件夹名）
    pub name: String,
    /// 仓库稳定 ID（`.atelyx/config.json` 的 vaultId，首次打开生成、之后固定）。
    pub id: String,
    /// 非空 = `config.json` 原文损坏、已按该文件名备份并按空配置继续（前端据此提示用户重新配置）
    pub config_corrupt_backup: Option<String>,
}

/// `read_vault_config` 的返回：仓库配置 + 损坏备份文件名（`None` = 正常读取）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultConfigRead {
    pub config: VaultConfig,
    /// 非空 = `config.json` 原文损坏、已按该文件名备份并退回空配置（前端据此提示用户重新配置）
    pub corrupt_backup: Option<String>,
}

/// 打开仓库：设当前仓库根 + 初始化目录结构 + 启动文件监听 + 返回仓库信息。
/// root 经 dunce::canonicalize 存储（去 Windows `\\?\` 前缀，统一路径格式，与 watcher/路径校验语义一致）。
/// `vaultId`（`.atelyx/config.json`）首次打开生成、之后固定，前端据此识别仓库归属（防跨仓库搞混）。
#[tauri::command]
pub fn open_vault(
    path: String,
    app: AppHandle,
    state: State<'_, VaultState>,
) -> Result<VaultInfo, String> {
    let raw = PathBuf::from(&path);
    if !raw.is_dir() {
        return Err(format!("仓库路径不是文件夹：{}", path));
    }
    let root = dunce::canonicalize(&raw).map_err(|e| format!("仓库路径不可达：{} ({e})", path))?;
    let (vault_id, exclude_folders, corrupt_backup) = activate_vault(&app, &state, &root)?;
    // 反链/标签索引后台预热：把唯一一次全量扫描（读全部 .md 提取引用/标签）塞进「进入仓库」阶段，不阻塞打开；
    // 失败静默——首次查询会懒构建兜底（索引幂等可重建）。锁 poison 在此同样容忍：后台预热非关键路径。
    // 世代与 root 必须成对取得（`session_snapshot` 在同一把 session 锁内读三者）：快速连续切仓库时
    // 若只裸读世代，可能拿到「后一次 open_vault」的世代配本次的 root，预热就会把旧仓库索引装进新会话。
    let (snap_root, _, warm_generation) = state.session_snapshot()?;
    if snap_root != root {
        // 已被另一次 open_vault 接管：本轮不预热（索引由那次或首次查询懒构建）
        return Ok(vault_info_from(root, vault_id, corrupt_backup));
    }
    let warm_app = app.clone();
    let warm_root = root.clone();
    let warm_exclude = exclude_folders.clone();
    std::thread::spawn(move || {
        let Some(st) = warm_app.try_state::<VaultState>() else {
            return;
        };
        // vault 作用域插件目录的超龄残留（崩溃遗留的 .install-*/.bak-* 等）随预热线程顺带清理
        crate::commands::plugin::sweep_plugin_residues(&warm_root.join(".atelyx/plugins"));
        // 未入库附件临时区的兜底回收：清理「画布已不存在」的超龄目录
        // （崩溃/强杀遗留，画布未走正常关闭路径；正常关闭由 cleanup_canvas_temp_attachments 按引用清单个清）
        crate::commands::temp_attachment::sweep_orphan_temp_dirs(&warm_root);
        warm_indexes(&st, &warm_root, &warm_exclude, warm_generation);
    });
    Ok(vault_info_from(root, vault_id, corrupt_backup))
}

/// 后台预热反链与标签索引：**先在锁外构建完整索引，再取锁安装**。
/// 为什么不在锁内边扫描边填：扫描大仓库要数秒，查询侧（`scan_wiki_backlinks`/`scan_vault_tags`）
/// 取的是同一把锁，持锁扫描会让「进仓后立刻点开笔记」同步阻塞到扫描结束。
/// 取锁时若已被并发填充（查询侧懒构建/另一次预热），本轮的观测作废不安装——索引只是内存缓存，
/// 磁盘为真相，丢弃一次预热无任何副作用。`generation` 由 `open_vault` 在起线程前捕获：
/// 预热是后台任务，期间可能已切到别的仓库（同路径 + 同指纹的旧仓库条目会一直用错，见 `with_index`）。
fn warm_indexes(state: &VaultState, root: &Path, exclude: &[String], generation: u64) {
    let fresh = || state.generation.load(std::sync::atomic::Ordering::SeqCst) == generation;
    let needs_wiki = state.wiki.lock().map(|g| g.is_none()).unwrap_or(false);
    if needs_wiki {
        let mut index = WikiIndex::default();
        if refresh_wiki_index(root, exclude, &mut index).is_ok() {
            if let Ok(mut guard) = state.wiki.lock() {
                if guard.is_none() && fresh() {
                    *guard = Some(index);
                }
            }
        }
    }
    let needs_tags = state.tags.lock().map(|g| g.is_none()).unwrap_or(false);
    if needs_tags {
        let mut index = TagIndex::default();
        if refresh_tag_index(root, exclude, &mut index).is_ok() {
            if let Ok(mut guard) = state.tags.lock() {
                if guard.is_none() && fresh() {
                    *guard = Some(index);
                }
            }
        }
    }
}

/// 取某索引并把刷新放在锁外：`std::mem::take` 移出索引 → 锁外刷指纹 → 放回。
/// 取锁间隙其他请求看到 `None` 会各自重建/刷新，索引幂等故无副作用；代价是极小概率重复扫描一次，
/// 换来的是「刷新期间不持锁」，查询不再互相排队。
///
/// `generation` 由调用方与 root 一起原子取得（`VaultState::session_snapshot`）：刷新（大仓库数秒）
/// 期间可能已切换仓库，`set` 清空并递增世代后旧仓库的索引不得再装回来（同路径 + 同指纹的条目会
/// 一直用错，直到文件被改动）。世代必须在取 root 的同一步拿到——晚一步就会出现「读完 root 才读世代」
/// 的窗口，那期间切仓库同样会把旧仓库索引装进新会话。
fn with_index<I: Default, O>(
    lock: &std::sync::Mutex<Option<I>>,
    state: &VaultState,
    generation: u64,
    refresh: impl FnOnce(&mut I) -> Result<(), String>,
    f: impl FnOnce(&I) -> O,
) -> Result<O, String> {
    let mut index = {
        let mut guard = lock.lock().map_err(|e| e.to_string())?;
        guard.take().unwrap_or_default()
    };
    refresh(&mut index)?;
    let out = f(&index);
    let mut guard = lock.lock().map_err(|e| e.to_string())?;
    if state.generation.load(std::sync::atomic::Ordering::SeqCst) == generation {
        *guard = Some(index);
    }
    Ok(out)
}

/// 反链索引查询（刷新在锁外，见 `with_index`）。
fn with_wiki_index<O>(
    state: &VaultState,
    root: &Path,
    exclude: &[String],
    generation: u64,
    f: impl FnOnce(&WikiIndex) -> O,
) -> Result<O, String> {
    with_index(&state.wiki, state, generation, |index| refresh_wiki_index(root, exclude, index), f)
}

/// 标签索引查询（刷新在锁外，见 `with_index`）。
fn with_tag_index<O>(
    state: &VaultState,
    root: &Path,
    exclude: &[String],
    generation: u64,
    f: impl FnOnce(&TagIndex) -> O,
) -> Result<O, String> {
    with_index(&state.tags, state, generation, |index| refresh_tag_index(root, exclude, index), f)
}

/// 枚举当前仓库的画布列表（递归扫描全仓库 .atlx，按 updatedAt 倒序）。
#[tauri::command]
pub fn list_canvases_vault(state: State<'_, VaultState>) -> Result<Vec<CanvasFileRow>, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    list_canvas_files(&root, &exclude)
}

/// 读 .atlx 文件（按相对仓库根路径，如 `项目A/方案.atlx`）。
#[tauri::command]
pub fn read_canvas_vault(file: String, state: State<'_, VaultState>) -> Result<CanvasFile, String> {
    let root = state.root()?;
    let path = safe_join(&root, &file, false)?;
    read_canvas_file(&path)
}

/// 写 .atlx 文件（整体原子写；title 改了会自动重命名文件到同目录新名）。
/// 自动更新 updated_at；保留原 created_at（从磁盘读，新画布用 now）。
/// `file`：画布相对仓库根路径（前端持有，画布任意文件夹存放）。
/// `base_updated_at`：前端基于的磁盘版本（加载时的 updatedAt）。磁盘版本更新则拒绝写
/// （乐观并发，防多用户/外部同步静默覆盖丢更新）；None = 不检查。
/// 乐观锁检查与 createdAt 保留共走一次带缓存读（指纹校验失效，外部改动即时感知）。
#[tauri::command]
pub fn write_canvas_vault(
    mut canvas: CanvasFile,
    file: String,
    base_updated_at: Option<i64>,
    state: State<'_, VaultState>,
) -> Result<i64, String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    // 目标路径 = 同目录 + <sanitized-title>.atlx（title 变更 = 同目录改文件名，路径不漂移）
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atlx", sanitize_filename(&canvas.title)));
    // 新路径已存在且 id 不同（前端 dedupe 被绕过/同步盘合并）：拒绝覆盖，防静默丢失另一画布
    ensure_no_id_conflict(
        &new_path,
        &canvas.id,
        &|p| read_canvas_file(p).ok().map(|c| c.id),
        "画布",
        &canvas.title,
    )?;
    let now = Utc::now().timestamp();
    // 乐观并发 + createdAt 保留共读一次（缓存命中免重读；文件缺失 = 新画布用 now）
    if old_path.exists() {
        let (_, disk) = read_canvas_file_cached(&state, &root, &file)
            .map_err(|e| format!("磁盘画布文件损坏，无法保存：{} ({e})", old_path.display()))?;
        if let Some(base) = base_updated_at {
            if disk.updated_at > base {
                return Err("画布已被外部修改，请重载后再编辑".to_string());
            }
        }
        canvas.created_at = disk.created_at;
    } else {
        canvas.created_at = now;
    }
    canvas.updated_at = now;
    write_canvas_file(&new_path, &canvas)?;
    remove_replaced_file(&old_path, &new_path, "画布")?;
    let new_rel = rel_with_new_title(&file, &canvas.title, "atlx");
    cache_evict_canvas(&state, &file);
    cache_put_canvas(&state, &new_path, &new_rel, &canvas);
    // 返回写入的 updated_at，前端保存成功后同步乐观锁基准（防下次保存误判冲突）
    Ok(now)
}

/// 增量保存 .atlx（自动保存主路径）：只写变化/新增/删除的实体（前端按引用 diff 计算补丁），
/// 按稳定 id 合并到磁盘全量文件——乐观锁 / createdAt 保留 / title 重命名 / 原子写语义
/// 与 write_canvas_vault 完全一致，IPC 载荷从整画布缩到变化实体。
/// 返回 (updatedAt, 写盘后的相对路径)——title 变更重命名文件时前端按新路径更新 canvasFile。
/// 冲突策略（与表格的 force「保留本地」不对称，属有意设计）：画布冲突一律报错，由前端提示
/// 「重载（丢本地）或合并（mergeFromDisk 三方合并）」——画布是拓扑+消息的复合体，
/// 无表格行级 LWW 的明确语义，不做静默覆盖。
#[tauri::command]
pub fn patch_canvas_vault(
    patch: CanvasPatch,
    file: String,
    base_updated_at: Option<i64>,
    state: State<'_, VaultState>,
) -> Result<(i64, String), String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    // 磁盘文件缺失（外部删除）：补丁只有变化实体，重建会丢未变化部分——拒绝并回退全量写
    if !old_path.exists() {
        return Err("画布文件不存在（已从磁盘删除）".to_string());
    }
    let (_, mut canvas) = read_canvas_file_cached(&state, &root, &file)?;
    // 防串文件守卫：补丁属于另一画布（陈旧保存回调）→ 拒绝，防跨文件混写
    if patch.id != canvas.id {
        return Err("画布身份不匹配，已中止保存".to_string());
    }
    if let Some(title) = &patch.title {
        canvas.title = title.clone();
    }
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atlx", sanitize_filename(&canvas.title)));
    let new_rel = rel_with_new_title(&file, &canvas.title, "atlx");
    // 名冲突守卫仅路径漂移（title 变更）时检查——同名时文件就是本次基底，id 必然一致，免每次保存全量重读
    if old_path != new_path {
        ensure_no_id_conflict(
            &new_path,
            &canvas.id,
            &|p| read_canvas_file(p).ok().map(|c| c.id),
            "画布",
            &canvas.title,
        )?;
    }
    let now = Utc::now().timestamp();
    // 乐观并发：磁盘版本比前端基准新 → 拒绝覆盖（缓存已按指纹保证磁盘最新）
    if let Some(base) = base_updated_at {
        if canvas.updated_at > base {
            return Err("画布已被外部修改，请重载后再编辑".to_string());
        }
    }
    // 按稳定 id 合并（removed 幂等；upsert 覆盖同 id 或追加）
    let removed_nodes: HashSet<&String> = patch.removed_node_ids.iter().collect();
    canvas.nodes.retain(|n| !removed_nodes.contains(&n.id));
    for n in &patch.upsert_nodes {
        match canvas.nodes.iter_mut().find(|x| x.id == n.id) {
            Some(existing) => *existing = n.clone(),
            None => canvas.nodes.push(n.clone()),
        }
    }
    let removed_edges: HashSet<&String> = patch.removed_edge_ids.iter().collect();
    canvas.edges.retain(|e| !removed_edges.contains(&e.id));
    for e in &patch.upsert_edges {
        match canvas.edges.iter_mut().find(|x| x.id == e.id) {
            Some(existing) => *existing = e.clone(),
            None => canvas.edges.push(e.clone()),
        }
    }
    canvas.updated_at = now;
    write_canvas_file(&new_path, &canvas)?;
    remove_replaced_file(&old_path, &new_path, "画布")?;
    cache_evict_canvas(&state, &file);
    cache_put_canvas(&state, &new_path, &new_rel, &canvas);
    Ok((now, new_rel))
}

/// 重命名画布：更新 .atlx 内 title + 同目录重命名文件（按当前文件路径）。
#[tauri::command]
pub fn rename_canvas_vault(
    file: String,
    new_title: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    let old_path = safe_join(&root, &file, false)?;
    let mut canvas = read_canvas_file(&old_path)?;
    canvas.title = new_title;
    canvas.updated_at = Utc::now().timestamp();
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.atlx", sanitize_filename(&canvas.title)));
    // 目标已被另一画布占用（内部 id 不同，文件名与 title 脱钩时前端 dedupe 防不住）时拒绝覆盖，
    // 防静默丢失（同 write_canvas_vault 的保存守卫；同物理文件 = case-only 改名豁免）
    if !same_physical_file(&old_path, &new_path) {
        ensure_no_id_conflict(
            &new_path,
            &canvas.id,
            &|p| read_canvas_file(p).ok().map(|c| c.id),
            "画布",
            &canvas.title,
        )?;
    }
    // 先写新文件再删旧文件，保证不丢数据
    write_canvas_file(&new_path, &canvas)?;
    remove_replaced_file(&old_path, &new_path, "画布")?;
    // 重命名路径变化：清旧缓存键 + 新路径按新指纹入缓存
    if new_path != old_path {
        cache_evict_canvas(&state, &file);
        let new_rel = rel_with_new_title(&file, &canvas.title, "atlx");
        cache_put_canvas(&state, &new_path, &new_rel, &canvas);
    }
    Ok(())
}

/// 移动画布文件到新路径（跨目录，拖动文件到文件夹用）。画布不被其他文件引用，
/// 无需更新任何 .atlx；前端负责同步 canvases 列表 / currentCanvasFile 路径。
#[tauri::command]
pub fn move_canvas_vault(
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    rename_note_file(&root, &old_file, &new_file)?;
    // 移动路径变化：清旧路径缓存键（新路径下次读盘自然入缓存）
    cache_evict_canvas(&state, &old_file);
    Ok(())
}

/// 删除画布 .atlx 文件（不删 笔记/附件，文件可跨画布共享）。
#[tauri::command]
pub fn delete_canvas_vault(file: String, state: State<'_, VaultState>) -> Result<(), String> {
    let root = state.root()?;
    let path = safe_join(&root, &file, false)?;
    if !path.exists() {
        return Err(format!("文件不存在：{}", file));
    }
    if path.is_dir() {
        return Err(format!("目标是目录，仅支持删除文件：{}", file));
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    cache_evict_canvas(&state, &file);
    Ok(())
}

/// 读 .md 笔记（按相对仓库根路径，如 `笔记/xxx.md`）。
#[tauri::command]
pub fn read_note(file: String, state: State<'_, VaultState>) -> Result<String, String> {
    let root = state.root()?;
    read_note_file(&root, &file)
}

/// 查询反链（`[[note_name]]` 或 `[label](基于仓库的路径)` 两种写法；索引缓存 + 指纹增量刷新）。
/// 只扫 .md 不扫 .atlx；与文件树同过滤（跳过隐藏/排除目录，`.atelyx/对话历史` 会话正文不算反链）。
/// 索引懒构建 + 后台预热（open_vault），切仓库随 set 清空——纯内存缓存，磁盘为真相（外部编辑自愈）。
#[tauri::command]
pub fn scan_wiki_backlinks(
    note_name: String,
    note_file: String,
    state: State<'_, VaultState>,
) -> Result<Vec<BacklinkRow>, String> {
    let (root, exclude, generation) = state.session_snapshot()?;
    with_wiki_index(&state, &root, &exclude, generation, |index| {
        query_wiki_backlinks(index, &note_name, &note_file)
    })
}

/// 全仓库标签词汇表（frontmatter `tags` + 正文内联 `#标签`；索引缓存 + 指纹增量刷新）。
/// 只扫 .md 不扫 .atlx；与文件树同过滤（跳过隐藏/排除目录，`.atelyx/对话历史` 不算）。
/// 返回出现次数降序 + 名称升序、上限 1000；纯内存缓存，磁盘为真相（外部编辑自愈）。
#[tauri::command]
pub fn scan_vault_tags(state: State<'_, VaultState>) -> Result<Vec<TagRow>, String> {
    let (root, exclude, generation) = state.session_snapshot()?;
    with_tag_index(&state, &root, &exclude, generation, aggregate_tag_counts)
}

/// 重建内部链接的结果统计。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildLinksResult {
    /// 扫描的 .md 文件数
    pub scanned: u32,
    /// 实际写回修改的文件数
    pub modified: u32,
    /// 改写的链接处数
    pub links: u32,
}

/// 一键重建内部链接（设置 → 编辑器）：全仓库 .md 统一规范为标准 Markdown 写法
/// `[名](基于仓库的规范路径)`；目标笔记不存在 → `[名]()`（点击可快捷新建）。
/// 只改写链接跨度（跳过 frontmatter/代码块/行内代码/图片链接/外部链接/非 .md 相对路径），
/// 其余内容字节级原样保留；只写有变化的文件（原子写）。单文件失败跳过不阻塞其余。
#[tauri::command]
pub fn rebuild_internal_links(
    state: State<'_, VaultState>,
) -> Result<RebuildLinksResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    // 收集全部 .md 相对路径 + 绝对路径（walk_md_in 回调已给 root.join(rel)，来源可信无需再 safe_join 重算）
    let mut files: Vec<(String, PathBuf)> = vec![];
    walk_md_in(&root, "", &exclude, &mut |rel, path| {
        files.push((rel.to_string(), path.to_path_buf()));
        Ok(())
    })?;
    let exact: std::collections::HashSet<String> =
        files.iter().map(|(rel, _)| rel.clone()).collect();
    let mut by_basename: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (rel, _) in &files {
        let base = rel.rsplit('/').next().unwrap_or(rel).to_string();
        by_basename.entry(base).or_default().push(rel.clone());
    }
    let resolve =
        |name: &str| resolve_link_target(name, &exact, &by_basename);
    let mut modified = 0u32;
    let mut links = 0u32;
    for (rel, path) in &files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // 不可读跳过
        };
        let (next, n) = rewrite_internal_links(&content, &resolve);
        if n > 0 {
            links += n as u32;
            if let Err(e) = write_note_file(&root, rel, &next) {
                eprintln!("重建内部链接写回失败（跳过该文件）：{rel}: {e}");
                continue;
            }
            modified += 1;
        }
    }
    Ok(RebuildLinksResult {
        scanned: files.len() as u32,
        modified,
        links,
    })
}

/// 写 .md 笔记（原子写，自动建父目录）。
#[tauri::command]
pub fn write_note(
    file: String,
    content: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    write_note_file(&root, &file, &content)
}

/// 读仓库内任意文本文件全文（安全边界 = 仓库根，safe_join 校验；非 UTF-8 返回替换字符容错）。
/// edit_file / 笔记历史等「整读全文再改」路径的后端。超过 EDIT_READ_MAX_BYTES 拒绝，
/// 防对超大文件整读回填上下文拖死前端。分页查看走 read_vault_file_window。
/// 隐藏目录屏蔽由前端工具 validate 层强制（内部能力如历史/任务清单为刻意豁免），本命令层不做路径过滤。
#[tauri::command]
pub fn read_vault_file(file: String, state: State<'_, VaultState>) -> Result<String, String> {
    let root = state.root()?;
    let content = read_note_file(&root, &file)?;
    if content.len() > EDIT_READ_MAX_BYTES {
        return Err(format!(
            "文件过大（>{}MB），拒绝整读回填；如需查看请用分页读取",
            EDIT_READ_MAX_BYTES / 1_000_000
        ));
    }
    Ok(content)
}

/// 写仓库内任意文本文件（指定相对路径；原子写 + 自动建父目录）。
/// AI write_file/edit_file/append_file 与任务清单等通用路径的后端。不做字节硬上限
/// （内容受模型输出 token 天然约束；路径安全由 safe_join 保障）。
/// 隐藏目录屏蔽由前端工具 validate 层强制（内部能力如任务清单为刻意豁免），本命令层不做路径过滤。
#[tauri::command]
pub fn write_vault_file(
    file: String,
    content: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    write_note_file(&root, &file, &content)
}

// ===== AI read_file 分页窗口 =====

/// read_file 分页窗口的一行（number = 文件内 1-based 绝对行号）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWindowLine {
    pub number: usize,
    pub text: String,
}

/// read_file 分页窗口结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWindowResult {
    pub lines: Vec<ReadWindowLine>,
    /// 文件精确总行数（页脚引导翻页用，即便未读全）。
    pub total_lines: usize,
    /// 单次返回是否命中字节预算被截断（模型应继续分页/缩小窗口）。
    pub truncated: bool,
}

/// 分页读取参数（offset 1-based 默认 1；limit 默认/上限 READ_WINDOW_DEFAULT_LINES）。
struct ReadWindowRequest {
    offset: usize,
    limit: usize,
}

/// 读仓库内任意文本文件的分页窗口（安全边界 = 仓库根，safe_join 校验；非 UTF-8 返回替换字符容错）。
/// AI read_file 工具后端：按 offset/limit 取带绝对行号的窗口，单行按字符上限截断、单次返回受字节
/// 预算约束并在超限时标记 truncated；恒返回精确 totalLines 供模型翻页——不硬拒大文件。
#[tauri::command]
pub fn read_vault_file_window(
    file: String,
    offset: Option<usize>,
    limit: Option<usize>,
    state: State<'_, VaultState>,
) -> Result<ReadWindowResult, String> {
    let root = state.root()?;
    let content = read_note_file(&root, &file)?;
    let req_offset = offset.unwrap_or(1).max(1);
    let request = ReadWindowRequest {
        offset: req_offset,
        limit: limit.unwrap_or(READ_WINDOW_DEFAULT_LINES).min(READ_WINDOW_DEFAULT_LINES),
    };
    let result = build_read_window(
        &content,
        request,
        READ_WINDOW_MAX_LINE_CHARS,
        READ_WINDOW_MAX_BYTES,
    );
    // offset 越界显式报错（区别于「文件本来就短/读完」）：模型据此自纠，而不是误判内容为空
    if offset_out_of_range(req_offset, result.total_lines) {
        return Err(format!(
            "offset {} 超出文件范围（文件共 {} 行）",
            req_offset, result.total_lines
        ));
    }
    Ok(result)
}

/// offset 是否越界：超出总行数即越界；空文件（0 行）读 offset=1 是合法空读（不误报）。
fn offset_out_of_range(offset: usize, total_lines: usize) -> bool {
    offset > total_lines && !(total_lines == 0 && offset == 1)
}

/// 纯函数：从整文构建分页窗口（命令与单测共用）。逐行处理：剥离尾 `\r`、统计精确 totalLines、
/// 超长单行按 line_chars 截断并附后缀、累积字节超 max_bytes 停并标 truncated。offset 越界返回空窗口。
fn build_read_window(
    content: &str,
    request: ReadWindowRequest,
    line_chars: usize,
    max_bytes: usize,
) -> ReadWindowResult {
    let mut lines: Vec<ReadWindowLine> = Vec::new();
    let mut total_lines = 0usize;
    let mut out_bytes = 0usize;
    let mut truncated = false;

    // split_terminator：按 '\n' 切但省略尾部终止符产生的空段——'a\nb\n' 与 'a\nb' 都精确为 2 行，
    // 空文件 0 行，杜绝把尾部换行数成幽灵行。
    for raw in content.split_terminator('\n') {
        total_lines += 1;
        if truncated || total_lines < request.offset || lines.len() >= request.limit {
            continue;
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let text = if line.chars().count() > line_chars {
            let head: String = line.chars().take(line_chars).collect();
            format!("{head}... (line truncated to {line_chars} chars)")
        } else {
            line.to_string()
        };
        // +1 为换行符本身；文本行不含换行，页脚按行文本估算尺寸
        let bytes = text.len() + 1;
        if out_bytes + bytes > max_bytes {
            truncated = true;
            continue;
        }
        out_bytes += bytes;
        lines.push(ReadWindowLine {
            number: total_lines,
            text,
        });
    }

    ReadWindowResult {
        lines,
        total_lines,
        truncated,
    }
}

// ===== AI list_dir 单层列目 =====

/// 单层列目上限（AI list_dir 工具；前端 LIST_DIR_MAX_ENTRIES 与此对齐）。
const LIST_DIR_MAX_ENTRIES: usize = 200;

/// list_dir 单层条目：kind = "dir" | "file"；size 仅文件（字节）；children 仅目录（直接子项数）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDirEntry {
    pub name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<usize>,
}

/// list_dir 结果（entries/total/capped 词汇与 glob_vault/grep_vault 一致）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDirResult {
    pub entries: Vec<ListDirEntry>,
    /// 目录真实条目总数（可能大于 entries.len()，超上限时提示收窄）。
    pub total: usize,
    /// 是否因超上限被截断（total > entries.len()）。
    pub capped: bool,
}

/// 单层列出目录条目（AI list_dir 工具后端；安全边界 = 仓库根，safe_join 校验）。
/// 完全屏蔽 `.` 开头隐藏项（.atelyx/.git 等对 AI 不可见）；目录在前、按名称升序；
/// 子目录带直接子项数、文件带字节大小；超上限截断并标 capped（total 恒为真实总数）。
#[tauri::command]
pub fn list_vault_dir(dir: String, state: State<'_, VaultState>) -> Result<ListDirResult, String> {
    let root = state.root()?;
    list_dir_entries(&list_dir_target(&root, &dir)?, LIST_DIR_MAX_ENTRIES)
}

/// 解析列目目标：空串 = 仓库根，其余走 safe_join 并要求确实为目录；指向隐藏目录（. 开头段）拒绝。
fn list_dir_target(root: &Path, dir: &str) -> Result<PathBuf, String> {
    if has_hidden_segment(&normalize_list_dir(dir)) {
        return Err("路径位于隐藏目录（. 开头段），AI 工具不可访问".to_string());
    }
    let path = if dir.is_empty() {
        root.to_path_buf()
    } else {
        safe_join(root, dir, false)?
    };
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{}", dir));
    }
    Ok(path)
}

/// 列表目录入参归一化（仅用于隐藏段判定：`\`→`/`、去尾斜杠）。
fn normalize_list_dir(dir: &str) -> String {
    let mut s = dir.replace('\\', "/");
    while s.ends_with('/') {
        s.pop();
    }
    s
}

/// 子目录的直接可见子项数（跳过 `.` 开头隐藏项，与顶层 list_dir 展示口径一致）。
fn count_visible_children(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
        .unwrap_or(0)
}

/// 单层列目核心（max 参数化供测试注入小上限）：目录在前、按名称升序；跳过 `.` 开头隐藏项。
fn list_dir_entries(dir: &Path, max: usize) -> Result<ListDirResult, String> {
    let mut entries: Vec<ListDirEntry> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        // 非 UTF-8 文件名无法经 JSON 传递，跳过（常规文件系统上不出现）
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        // 隐藏项（. 开头，含 .gitignore 类隐藏文件）对 AI 完全屏蔽
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let (size, children) = if is_dir {
            (None, Some(count_visible_children(&entry.path())))
        } else {
            (entry.metadata().ok().map(|md| md.len()), None)
        };
        entries.push(ListDirEntry {
            name,
            kind: if is_dir { "dir" } else { "file" },
            size,
            children,
        });
    }
    // "dir" < "file" 字典序，升序即目录在前；同级按名称字节序
    entries.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.name.cmp(&b.name)));
    let total = entries.len();
    let capped = total > max;
    if capped {
        entries.truncate(max);
    }
    Ok(ListDirResult { entries, total, capped })
}

/// 链接维护的副作用报告：本次被改写的 `.md` 相对路径清单。
/// 前端据此作废这些笔记的正文缓存（它们的 watcher 回波落在自写抑制窗口内，拿不到变更事件）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRewriteResult {
    pub rewritten: Vec<String>,
}

/// 重命名 .md 笔记 + 扫描所有 .atlx 更新 text 节点 file 引用 + 扫描所有 .md 更新内部链接（链接维护）。
/// 预扫描 → 改名 → 统一写回：任一写回失败时回滚重命名（避免「已改名但引用未更新、
/// 重试报源文件不存在」的半完成态）。
/// .md 链接维护只认规范路径形态（`[x](旧路径)`）——编码等非规范形态由「重建内部链接」归一后纳入维护。
#[tauri::command]
pub fn rename_note(
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<LinkRewriteResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    let pending = collect_ref_updates(&root, &old_file, &new_file)?;
    let pending_md = collect_md_link_updates(&root, &exclude, &mut |span: &str| {
        let Some(path) = markdown_link_path(span) else {
            return span.to_string();
        };
        if path != old_file {
            return span.to_string();
        }
        let label_end = span.find("](").unwrap_or(span.len() - 1);
        format!("{}]({new_file})", &span[..label_end])
    })?;
    // 收集发生在重命名前：被重命名笔记自身的旧路径自引用条目，写回目标必须是新路径（防旧路径重建文件）
    let pending_md: Vec<(String, String)> = pending_md
        .into_iter()
        .map(|(rel, content)| {
            if rel == old_file {
                (new_file.clone(), content)
            } else {
                (rel, content)
            }
        })
        .collect();
    rename_note_file(&root, &old_file, &new_file)?;
    if let Err(e) = flush_canvas_updates(&pending) {
        // 尽力回滚：恢复源文件原名（部分画布引用可能已写回，错误信息如实说明）
        let _ = rename_note_file(&root, &new_file, &old_file);
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    if let Err(e) = flush_md_updates(&root, &pending_md) {
        let _ = rename_note_file(&root, &new_file, &old_file);
        return Err(format!("更新笔记内部链接失败，重命名已回滚（请重试）：{e}"));
    }
    Ok(LinkRewriteResult {
        rewritten: pending_md.into_iter().map(|(rel, _)| rel).collect(),
    })
}

/// 枚举仓库文件树（文件面板全仓库树；跳过隐藏/排除目录与 `.tmp`）。
#[tauri::command]
pub fn list_vault_tree(state: State<'_, VaultState>) -> Result<Vec<FileTreeNode>, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    list_vault_tree_impl(&root, &exclude)
}

/// 新建文件夹（相对仓库根路径，如 `项目A/素材`），自动建父目录；返回相对路径。
#[tauri::command]
pub fn create_folder(dir: String, state: State<'_, VaultState>) -> Result<String, String> {
    let root = state.root()?;
    create_folder_impl(&root, &dir)
}

/// 删除 .md 笔记（不更新 .atlx 引用，断链降级由前端 TextNode 显示「文件缺失」提示）。
#[tauri::command]
pub fn delete_note(file: String, state: State<'_, VaultState>) -> Result<(), String> {
    let root = state.root()?;
    delete_vault_file(&root, &file)
}

/// 删除附件（同 delete_note，不更新 .atlx 引用）。
#[tauri::command]
pub fn delete_attachment(file: String, state: State<'_, VaultState>) -> Result<(), String> {
    let root = state.root()?;
    delete_vault_file(&root, &file)
}

/// 复制仓库内文件为同目录副本（纯字节复制；新路径由前端 dedupe 防重名）。
/// `.atlx`/`.atb` 副本重新生成 id（与 copy_folder 的 regenerate_ids_in 同语义，
/// 防同 id 双文件歧义：画布标签按 id 去重、协作合并按 id 身份）；title 保持原样。
#[tauri::command]
pub fn copy_vault_file(
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    let src = safe_join(&root, &old_file, false)?;
    let dst = safe_join(&root, &new_file, true)?;
    if !src.is_file() {
        return Err(format!("源不是文件：{}", old_file));
    }
    // 目标已存在拒绝（防覆盖丢数据；前端 dedupe 已防重名，此处兜底并发/外部创建）
    if dst.exists() {
        return Err(format!("目标文件已存在：{}", new_file));
    }
    std::fs::copy(&src, &dst).map_err(|e| format!("复制文件失败：{e}"))?;
    regenerate_file_id(&dst)?;
    Ok(())
}

/// 复制文件夹为同父目录副本（递归复制全部内容；新路径由前端 dedupe 防重名）。
#[tauri::command]
pub fn copy_vault_folder(
    old_dir: String,
    new_dir: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    copy_folder_impl(&root, &old_dir, &new_dir)
}

/// 删除文件夹（相对仓库根路径）。force=false 空目录直接删，非空返回 needsConfirm 供前端弹窗；
/// 确认后以 force=true 递归删除全部内容。仓库根（空路径）拒绝。
#[tauri::command]
pub fn delete_folder(
    dir: String,
    force: bool,
    state: State<'_, VaultState>,
) -> Result<DeleteFolderResult, String> {
    let root = state.root()?;
    delete_folder_impl(&root, &dir, force)
}

/// 重命名文件夹：移动整个目录 + 扫描所有 .atlx 更新位于该目录下文件的引用
/// （text `file` / media `file` / conversation `systemPromptFile` 按 `old_dir/` 前缀替换）
/// + 扫描所有 .md 更新内部链接（`](old_dir/` 前缀替换为 `](new_dir/`，链接维护）。
/// 事务模式与 rename_note 对称：先移动目录（旧路径已不存在），再扫描新树收集引用更新并统一写回；
/// 扫描或写回失败时回滚目录移动（部分画布引用可能已写回，错误信息如实说明）。
#[tauri::command]
pub fn rename_folder(
    old_dir: String,
    new_dir: String,
    state: State<'_, VaultState>,
) -> Result<LinkRewriteResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    rename_folder_impl(&root, &old_dir, &new_dir)?;
    let pending = match collect_canvas_updates(&root, &mut |canvas| {
        remap_dir_refs_in_canvas(canvas, &old_dir, &new_dir)
    }) {
        Ok(p) => p,
        Err(e) => {
            // 目录已移动：扫描失败也回滚，防「目录已改名但画布引用仍指向旧路径」的半完成态
            let _ = rename_folder_impl(&root, &new_dir, &old_dir);
            return Err(format!("扫描画布引用失败，重命名已回滚（请重试）：{e}"));
        }
    };
    let old_prefix = format!("{old_dir}/");
    let new_prefix = format!("{new_dir}/");
    let pending_md = match collect_md_link_updates(&root, &exclude, &mut |span: &str| {
        let Some(path) = markdown_link_path(span) else {
            return span.to_string();
        };
        let Some(rest) = path.strip_prefix(&old_prefix) else {
            return span.to_string();
        };
        let label_end = span.find("](").unwrap_or(span.len() - 1);
        format!("{}]({new_prefix}{rest})", &span[..label_end])
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = rename_folder_impl(&root, &new_dir, &old_dir);
            return Err(format!("扫描笔记内部链接失败，重命名已回滚（请重试）：{e}"));
        }
    };
    if let Err(e) = flush_canvas_updates(&pending) {
        // 尽力回滚：目录移回旧名（部分画布引用可能已写回，错误信息如实说明）
        let _ = rename_folder_impl(&root, &new_dir, &old_dir);
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    if let Err(e) = flush_md_updates(&root, &pending_md) {
        let _ = rename_folder_impl(&root, &new_dir, &old_dir);
        return Err(format!("更新笔记内部链接失败，重命名已回滚（请重试）：{e}"));
    }
    Ok(LinkRewriteResult {
        rewritten: pending_md.into_iter().map(|(rel, _)| rel).collect(),
    })
}

/// 迁移单个文件的全部候选历史侧文件到新编码路径（笔记/表格/画布重命名/移动后调用）。
/// 源不存在静默跳过、目标已存在跳过，单文件失败不阻断（历史尽力而为，不阻塞重命名主流程）。
#[tauri::command]
pub fn remap_sideloads(
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    remap_sideloads_impl(&root, &old_file, &new_file);
    Ok(())
}

/// 迁移某文件夹下全部历史侧文件到新目录前缀（文件夹重命名后调用，语义同 remap_sideloads）。
#[tauri::command]
pub fn remap_sideloads_by_dir(
    old_dir: String,
    new_dir: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    remap_sideloads_by_dir_impl(&root, &old_dir, &new_dir);
    Ok(())
}

/// 重命名附件 + 扫描所有 .atlx 更新 media 节点 file 引用（链接维护，与 rename_note 对称）。
#[tauri::command]
pub fn rename_attachment(
    old_file: String,
    new_file: String,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    let pending = collect_ref_updates(&root, &old_file, &new_file)?;
    rename_note_file(&root, &old_file, &new_file)?;
    if let Err(e) = flush_canvas_updates(&pending) {
        let _ = rename_note_file(&root, &new_file, &old_file);
        return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
    }
    Ok(())
}

/// 读仓库内附件为 dataURL（`data:<mime>;base64,...`），供 media 节点缩略图/预览与未入库附件读回。
/// 图片扩展名推具体 mime，其余按 `application/octet-stream`（调用方按内容自行消费为文本或二进制）。
#[tauri::command]
pub fn read_attachment_data_url(
    file: String,
    state: State<'_, VaultState>,
) -> Result<String, String> {
    let root = state.root()?;
    let bytes = read_file_bytes(&root, &file)?;
    let mime = mime_from_ext(&file).unwrap_or("application/octet-stream");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// 按扩展名推图片 mime（仅图片，其他返回 None）。
pub(crate) fn mime_from_ext(file: &str) -> Option<&'static str> {
    let ext = Path::new(file)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())?;
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// 读仓库级配置（.atelyx/config.json，不存在返回空覆盖）。
#[tauri::command]
pub fn read_vault_config(state: State<'_, VaultState>) -> Result<VaultConfigRead, String> {
    let root = state.root()?;
    let (config, corrupt_backup) = read_vault_config_with_backup(&root)?;
    Ok(VaultConfigRead { config, corrupt_backup })
}

/// 以字段级合并补丁写仓库级配置（`.atelyx/config.json`）：只影响补丁里出现的字段，
/// 其余字段保留磁盘当前值（防撕裂窗口用陈旧内存副本整文件覆盖主窗口刚写入的配置）。
/// 补丁值为 `null` = 删除该键；值为对象 = 与磁盘同名键递归合并；数组/标量 = 整体替换。
/// 合并结果经 `VaultConfig` 往返，未知字段（旧版本/手工添加）会被丢弃——写盘后的文件只含当前已知字段。
/// 返回损坏原文的备份文件名（`null` = 未发生损坏；非空 = 已备份并提示用户）。
#[tauri::command]
pub fn vault_config_patch(
    patch: serde_json::Value,
    state: State<'_, VaultState>,
) -> Result<Option<String>, String> {
    let root = state.root()?;
    patch_vault_config(&root, &patch)
}

/// 读系统提示词标记列表（.atelyx/prompt-notes.json，不存在/损坏返回空）。
#[tauri::command]
pub fn read_prompt_notes(state: State<'_, VaultState>) -> Result<Vec<String>, String> {
    let root = state.root()?;
    read_prompt_notes_file(&root)
}

/// 写系统提示词标记列表（原子写 .atelyx/prompt-notes.json，独立于 config.json）。
#[tauri::command]
pub fn write_prompt_notes(
    files: Vec<String>,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    write_prompt_notes_file(&root, &files)
}

/// 读 Agent 配置列表（.atelyx/agents.json，不存在/损坏返回空）。
#[tauri::command]
pub fn read_agents(state: State<'_, VaultState>) -> Result<Vec<AgentConfig>, String> {
    let root = state.root()?;
    read_agents_file(&root)
}

/// 写 Agent 配置列表（原子写 .atelyx/agents.json，独立于 config.json）。
#[tauri::command]
pub fn write_agents(
    agents: Vec<AgentConfig>,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    write_agents_file(&root, &agents)
}

/// 读文件夹图标颜色映射（.atelyx/folder-colors.json，不存在/损坏返回空）。
#[tauri::command]
pub fn read_folder_colors(
    state: State<'_, VaultState>,
) -> Result<HashMap<String, String>, String> {
    let root = state.root()?;
    read_folder_colors_file(&root)
}

/// 写文件夹图标颜色映射（原子写 .atelyx/folder-colors.json，独立于 config.json）。
#[tauri::command]
pub fn write_folder_colors(
    colors: HashMap<String, String>,
    state: State<'_, VaultState>,
) -> Result<(), String> {
    let root = state.root()?;
    write_folder_colors_file(&root, &colors)
}

/// 扫 .atelyx/对话历史/ 列出全部会话（消息 .jsonl + 可选元数据侧车；会话清单 = 扫目录，无整文件索引）。
#[tauri::command]
pub fn list_chat_sessions(state: State<'_, VaultState>) -> Result<Vec<ChatSessionRow>, String> {
    let root = state.root()?;
    list_chat_sessions_file(&root)
}

/// 读会话元数据侧车（.atelyx/对话历史/<会话 id>.meta.json；不存在返回 None）。
#[tauri::command]
pub fn read_chat_session_meta(
    state: State<'_, VaultState>,
    file: String,
) -> Result<Option<ChatSessionMeta>, String> {
    let root = state.root()?;
    read_chat_session_meta_file(&root, &file)
}

/// 写会话元数据侧车（原子写；路径已校验）。
#[tauri::command]
pub fn write_chat_session_meta(
    state: State<'_, VaultState>,
    file: String,
    meta: ChatSessionMeta,
) -> Result<(), String> {
    let root = state.root()?;
    write_chat_session_meta_file(&root, &file, &meta)
}

/// 删会话元数据侧车（幂等；删除会话时调用）。
#[tauri::command]
pub fn delete_chat_session_meta(state: State<'_, VaultState>, file: String) -> Result<(), String> {
    let root = state.root()?;
    delete_chat_session_meta_file(&root, &file)
}

/// 读面板级覆盖（.atelyx/editor-chats-meta.json，不存在/损坏返回默认）。
#[tauri::command]
pub fn read_editor_chats_meta(state: State<'_, VaultState>) -> Result<ChatMetaFile, String> {
    let root = state.root()?;
    read_editor_chats_meta_file(&root)
}

/// 写面板级覆盖（原子写 .atelyx/editor-chats-meta.json）。
#[tauri::command]
pub fn write_editor_chats_meta(
    state: State<'_, VaultState>,
    file: ChatMetaFile,
) -> Result<(), String> {
    let root = state.root()?;
    write_editor_chats_meta_file(&root, &file)
}

/// 读会话消息正文 .jsonl（.atelyx/对话历史/ 下，路径已校验；文件缺失报错由前端 catch 降级）。
#[tauri::command]
pub fn read_chat_messages(state: State<'_, VaultState>, file: String) -> Result<String, String> {
    let root = state.root()?;
    read_chat_messages_file(&root, &file)
}

/// 写会话消息正文 .jsonl（自动建目录 + 原子写；路径已校验）。
#[tauri::command]
pub fn write_chat_messages(
    state: State<'_, VaultState>,
    file: String,
    content: String,
) -> Result<(), String> {
    let root = state.root()?;
    write_chat_messages_file(&root, &file, &content)
}

/// 追加式写会话消息正文 .jsonl（消息增长场景：前端只传新增记录，每记录一行紧凑 JSON，省全量重拼与 IPC 载荷；
/// 文件缺失报错由前端回落全量重写；截断场景仍走 write_chat_messages 全量重写）。
#[tauri::command]
pub fn append_chat_messages(
    state: State<'_, VaultState>,
    file: String,
    records: Vec<ChatMessageRecord>,
) -> Result<(), String> {
    let root = state.root()?;
    append_chat_messages_file(&root, &file, &records)
}

/// 删会话消息正文 .jsonl（不存在视为成功——幂等）。
#[tauri::command]
pub fn delete_chat_messages(state: State<'_, VaultState>, file: String) -> Result<(), String> {
    let root = state.root()?;
    delete_chat_messages_file(&root, &file)
}

/// 确保默认仓库已打开（首启 bootstrap：无最近仓库时建默认仓库并打开）。
///
/// 流程：已打开则返回 → 否则建 `app_data_dir/default-vault` → 初始化目录 →
/// 设为当前仓库 + 启动文件监听。
#[tauri::command]
pub fn ensure_default_vault(
    app_handle: AppHandle,
    state: State<'_, VaultState>,
) -> Result<VaultInfo, String> {
    // 已打开则直接返回（重新读配置拿 id；缺失时补生成，与 open_vault 语义一致）
    if let Ok(root) = state.root() {
        let (vault_id, _, corrupt_backup) = ensure_vault_id_on_disk(&root)?;
        return Ok(vault_info_from(root, vault_id, corrupt_backup));
    }
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let default_root = app_data_dir.join("default-vault");
    if !default_root.exists() {
        std::fs::create_dir_all(&default_root).map_err(|e| e.to_string())?;
    }
    // 与 open_vault 一致：dunce::canonicalize → 先启监听再切 state，保证存/回传格式统一
    let default_root = dunce::canonicalize(&default_root)
        .map_err(|e| format!("默认仓库路径不可达：{} ({e})", default_root.display()))?;
    let (vault_id, _, corrupt_backup) = activate_vault(&app_handle, &state, &default_root)?;
    Ok(vault_info_from(default_root, vault_id, corrupt_backup))
}

/// 新建空画布，返回 `{ id, file }`（file = 相对仓库根路径，前端打开/保存用）。
#[tauri::command]
pub fn create_canvas_vault(
    title: String,
    dir: String,
    state: State<'_, VaultState>,
) -> Result<CanvasCreateResult, String> {
    let root = state.root()?;
    let id = nanoid!();
    let now = Utc::now().timestamp();
    let canvas = CanvasFile {
        schema: CANVAS_SCHEMA.to_string(),
        id: id.clone(),
        title: title.clone(),
        nodes: vec![],
        edges: vec![],
        created_at: now,
        updated_at: now,
    };
    // 目标路径：dir（相对仓库根，空 = 根目录）+ <sanitized-title>.atlx
    let filename = format!("{}.atlx", sanitize_filename(&title));
    let rel = if dir.is_empty() {
        filename
    } else {
        format!("{dir}/{filename}")
    };
    let path = safe_join(&root, &rel, true)?;
    // 后端兜底：同名画布已存在则拒绝（前端 dedupe 是正常路径，此处防绕过/同步盘合并覆盖）
    if path.exists() {
        return Err(format!("画布名冲突：{}", title));
    }
    write_canvas_file(&path, &canvas)?;
    Ok(CanvasCreateResult { id, file: rel })
}

/// `create_canvas_vault` 返回值：id（运行时身份）+ file（磁盘定位）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasCreateResult {
    pub id: String,
    pub file: String,
}

// ===== 内部辅助 =====

/// 仓库稳定 ID：读配置的 vaultId（存量仓库缺失则生成 nanoid），保证同一仓库 ID 恒定、仓库间互不混淆。
/// 返回 `(id, 是否新建)`——仅新建时才需写盘，避免每次打开重写 config.json 的 IO 噪声。
fn ensure_vault_id(config: &mut VaultConfig) -> (String, bool) {
    if let Some(id) = &config.vault_id {
        if !id.trim().is_empty() {
            return (id.clone(), false);
        }
    }
    let id = nanoid!();
    config.vault_id = Some(id.clone());
    (id, true)
}

/// 从路径构造 VaultInfo（仓库名经 `vault_display_name`，兼容网络共享根等 `file_name()` 取不到的场景）。
fn vault_info_from(root: PathBuf, id: String, config_corrupt_backup: Option<String>) -> VaultInfo {
    VaultInfo {
        root: root.to_string_lossy().to_string(),
        name: crate::vault::vault_display_name(&root),
        id,
        config_corrupt_backup,
    }
}

/// 仓库稳定 ID 保障 + 生效文件面板配置读取（激活流程与「已打开」快路径共用）：
/// 读配置 → 缺失生成 nanoid → 仅新建时落盘（失败不阻塞打开——下次打开会补写，内存值本轮回调已生效）。
/// 返回 (vault_id, exclude_folders, 损坏备份文件名)。备份名必须往上传：配置损坏时本函数紧接着就会
/// 用「只剩 vaultId」的新配置覆盖原路径，前端若另起一次读命令只会读到这份合法新文件、永远发现不了损坏。
fn ensure_vault_id_on_disk(root: &Path) -> Result<(String, Vec<String>, Option<String>), String> {
    let (mut config, corrupt_backup) = read_vault_config_with_backup(root)?;
    let (vault_id, vault_id_new) = ensure_vault_id(&mut config);
    let exclude_folders = config.exclude_folders.clone().unwrap_or_default();
    if vault_id_new {
        let _ = write_vault_config_file(root, &config);
    }
    Ok((vault_id, exclude_folders, corrupt_backup))
}

/// 仓库激活公共流程（open_vault / ensure_default_vault 共用）：保障 vault_id → init 目录 →
/// 先启监听再切 state（watcher 失败降级为警告，不阻塞打开——大仓库递归监听可能超 OS watch
/// 上限（Linux inotify max_user_watches），仓库仍可打开，实时同步降级为手动刷新）。
/// root 必须已完成 dunce::canonicalize（两调用方均为先归一化再激活，保证存/回传格式统一）。
fn activate_vault(
    app_handle: &AppHandle,
    state: &State<'_, VaultState>,
    root: &Path,
) -> Result<(String, Vec<String>, Option<String>), String> {
    let (vault_id, exclude_folders, corrupt_backup) = ensure_vault_id_on_disk(root)?;
    init_vault_dirs(root)?;
    if let Err(e) = watcher::start(app_handle.clone(), root.to_path_buf(), exclude_folders.clone()) {
        eprintln!("文件监听启动失败（仓库仍可打开，实时同步降级）：{e}");
    }
    state.set(root.to_path_buf(), exclude_folders.clone())?;
    Ok((vault_id, exclude_folders, corrupt_backup))
}

/// 保存名冲突守卫（画布/表格保存命令共用）：新路径已被另一文件占用（解析出的稳定 id 不同，
/// 前端 dedupe 被绕过/同步盘合并/隐藏目录绕过）→ 拒绝覆盖，防静默丢失；现存文件解析失败
/// （损坏）不拦截。调用方按需前置豁免条件（路径未漂移/同物理文件时 id 必然一致或已豁免）。
pub(crate) fn ensure_no_id_conflict(
    new_path: &Path,
    id: &str,
    existing_id: &dyn Fn(&Path) -> Option<String>,
    kind: &str,
    title: &str,
) -> Result<(), String> {
    if new_path.exists() {
        if let Some(existing) = existing_id(new_path) {
            if existing != id {
                return Err(format!("{kind}名冲突：另一{kind}已使用名称「{title}」"));
            }
        }
    }
    Ok(())
}

/// 保存路径漂移（title 改名）后的旧文件清理（画布/表格保存命令共用）：case-only 重命名
/// （Windows 大小写不敏感文件系统）指向同一物理文件时不删（删即丢数据）；旧文件已不在
/// （新建/外部已删）跳过；其余删除，失败须报错——同 id 双文件会歧义（列表读到旧内容）。
pub(crate) fn remove_replaced_file(
    old_path: &Path,
    new_path: &Path,
    kind: &str,
) -> Result<(), String> {
    if old_path != new_path && old_path.exists() && !same_physical_file(old_path, new_path) {
        std::fs::remove_file(old_path).map_err(|e| format!("删除旧{kind}文件失败：{e}"))?;
    }
    Ok(())
}

/// 递归扫描仓库内全部 .atlx，命中更新条件的收集（不写盘，flush_canvas_updates 统一写回）。
/// 全仓库扫描：被引用文件与画布可能位于任意文件夹（含排除/隐藏目录），链接维护不过滤。
/// pub(crate)：table 命令的引用扫描复用（rename/move_table_vault）。
pub(crate) fn collect_canvas_updates(
    root: &Path,
    update: &mut dyn FnMut(&mut CanvasFile) -> bool,
) -> Result<Vec<(PathBuf, CanvasFile)>, String> {
    let mut updates: Vec<(PathBuf, CanvasFile)> = vec![];
    scan_atlx_in(root, "", update, &mut updates)?;
    Ok(updates)
}

fn scan_atlx_in(
    root: &Path,
    rel: &str,
    update: &mut dyn FnMut(&mut CanvasFile) -> bool,
    updates: &mut Vec<(PathBuf, CanvasFile)>,
) -> Result<(), String> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    if !dir.exists() {
        return Ok(());
    }
    // 链接维护不过滤（排除/隐藏目录内的画布也可能引用文件），exclude 传空
    for (child_rel, is_dir) in read_dir_filtered(&dir, rel, &[])? {
        if is_dir {
            scan_atlx_in(root, &child_rel, update, updates)?;
        } else if child_rel.ends_with(".atlx") {
            let path = root.join(&child_rel);
            let mut canvas = match read_canvas_file(&path) {
                Ok(c) => c,
                Err(_) => continue, // 跳过无法解析的文件，不阻塞其他画布的链接维护
            };
            if update(&mut canvas) {
                canvas.updated_at = Utc::now().timestamp();
                updates.push((path, canvas));
            }
        }
    }
    Ok(())
}

/// 收集需更新节点 file / systemPromptFile 引用的画布（不写盘，rename/move 后引用同步用）。
/// 三类引用（笔记 .md / 附件 / 表格 .atb）共用：`update_refs_in_canvas` 按扩展名天然互斥。
pub(crate) fn collect_ref_updates(
    root: &Path,
    old_file: &str,
    new_file: &str,
) -> Result<Vec<(PathBuf, CanvasFile)>, String> {
    collect_canvas_updates(root, &mut |canvas| {
        update_refs_in_canvas(canvas, old_file, new_file)
    })
}

/// 节点类型 → 引用字段名（text/media/table 节点引用独立文件用 `file`；
/// conversation 节点系统提示词引用 .md 用 `systemPromptFile`）。
fn ref_field_of(node_type: &str) -> Option<&'static str> {
    match node_type {
        "text" | "media" | "table" => Some("file"),
        "conversation" => Some("systemPromptFile"),
        _ => None,
    }
}

/// 更新单个 .atlx 内节点引用（内存中）：按节点类型替换 file/systemPromptFile 命中值，返回是否有变更。
/// 三类引用（笔记 .md / 附件 / 表格 .atb）统一走此函数——旧路径按扩展名天然互斥，合并后行为等价。
fn update_refs_in_canvas(canvas: &mut CanvasFile, old_file: &str, new_file: &str) -> bool {
    let mut changed = false;
    for node in &mut canvas.nodes {
        let Some(field) = ref_field_of(&node.node_type) else {
            continue;
        };
        let Some(obj) = node.data.as_object_mut() else {
            continue;
        };
        if obj.get(field).and_then(|v| v.as_str()) == Some(old_file) {
            obj.insert(field.to_string(), serde_json::Value::String(new_file.to_string()));
            changed = true;
        }
    }
    changed
}

/// 统一写回收集的画布更新（rename 成功后才调用；失败由调用方回滚 rename）。
/// pub(crate)：table 命令的引用写回复用。
pub(crate) fn flush_canvas_updates(updates: &[(PathBuf, CanvasFile)]) -> Result<(), String> {
    for (path, canvas) in updates {
        write_canvas_file(path, canvas)?;
    }
    Ok(())
}

/// 更新单个 .atlx 内的目录前缀引用（内存中）：位于 `old_dir/` 下的 text `file`、
/// media `file`、conversation `systemPromptFile` 替换为 `new_dir/` + 剩余部分。
/// 返回是否有变更。前缀含尾斜杠，防误匹配 `a` 命中 `ab/x.md`。
fn remap_dir_refs_in_canvas(canvas: &mut CanvasFile, old_dir: &str, new_dir: &str) -> bool {
    let old_prefix = format!("{}/", old_dir);
    let mut changed = false;
    for node in &mut canvas.nodes {
        let Some(obj) = node.data.as_object_mut() else {
            continue;
        };
        let Some(field) = ref_field_of(&node.node_type) else {
            continue;
        };
        if let Some(path) = obj.get(field).and_then(|v| v.as_str()) {
            if path.starts_with(&old_prefix) {
                obj.insert(
                    field.to_string(),
                    serde_json::Value::String(format!(
                        "{new_dir}/{}",
                        &path[old_prefix.len()..]
                    )),
                );
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod ensure_vault_id_tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    /// 仓库配置损坏时，激活路径必须把备份名带出来（前端据此提示用户），并补出可用的 vaultId。
    /// 这条覆盖的是「open_vault 先消费损坏、前端再读只会读到合法新文件」的时序——只测读命令看不出来。
    #[test]
    fn corrupt_config_is_reported_and_replaced_with_new_vault_id() {
        let root = TempDir::new("ensure-vault-id-corrupt");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{ 这不是 JSON").unwrap();

        let (vault_id, _exclude, backup) = ensure_vault_id_on_disk(&root).unwrap();

        let backup = backup.expect("损坏必须回传备份名");
        assert!(dir.join(&backup).is_file(), "原文要留档");
        assert!(!vault_id.is_empty());
        // 补出的配置含 vaultId，且原路径已让开（后续读路径读到的是这份合法文件）
        let on_disk = crate::vault::read_vault_config(&root).unwrap();
        assert_eq!(on_disk.vault_id.as_deref(), Some(vault_id.as_str()));
    }

    #[test]
    fn healthy_config_reports_no_backup() {
        let root = TempDir::new("ensure-vault-id-ok");
        let dir = root.join(".atelyx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"vaultId":"fixed-id","fileExplorerSort":"name"}"#,
        )
        .unwrap();

        let (vault_id, _exclude, backup) = ensure_vault_id_on_disk(&root).unwrap();

        assert_eq!(vault_id, "fixed-id");
        assert!(backup.is_none());
    }
}

#[cfg(test)]
mod read_window_tests {
    use super::*;

    fn req(offset: usize, limit: usize) -> ReadWindowRequest {
        ReadWindowRequest { offset, limit }
    }

    #[test]
    fn window_selects_offset_and_limit() {
        let r = build_read_window("a\nb\nc\nd", req(2, 2), 2000, 200_000);
        assert_eq!(r.total_lines, 4);
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].number, 2);
        assert_eq!(r.lines[0].text, "b");
        assert_eq!(r.lines[1].number, 3);
        assert_eq!(r.lines[1].text, "c");
        assert!(!r.truncated);
    }

    #[test]
    fn line_numbering_is_absolute_from_offset() {
        let r = build_read_window("x\ny\nz", req(1, 10), 2000, 200_000);
        let nums: Vec<usize> = r.lines.iter().map(|l| l.number).collect();
        assert_eq!(nums, vec![1, 2, 3]);
        assert_eq!(r.total_lines, 3);
    }

    #[test]
    fn truncates_overlong_line_with_suffix() {
        let long = "a".repeat(30);
        let r = build_read_window(&long, req(1, 10), 10, 200_000);
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.lines[0].number, 1);
        assert_eq!(r.lines[0].text, "aaaaaaaaaa... (line truncated to 10 chars)");
        assert_eq!(r.total_lines, 1);
        assert!(!r.truncated);
    }

    #[test]
    fn byte_budget_marks_truncated() {
        // 每行 11 字节（10 字符 + 换行），预算 33 仅容纳 3 行，第 4 行触发 truncated
        let content = ["0123456789"; 6].join("\n");
        let r = build_read_window(&content, req(1, 10), 2000, 33);
        assert_eq!(r.lines.len(), 3);
        assert_eq!(r.total_lines, 6);
        assert!(r.truncated);
        assert_eq!(r.lines[0].number, 1);
        assert_eq!(r.lines[2].number, 3);
    }

    #[test]
    fn strips_trailing_carriage_return() {
        let r = build_read_window("a\r\nb\r\nc", req(1, 10), 2000, 200_000);
        assert_eq!(r.lines[0].text, "a");
        assert_eq!(r.lines[1].text, "b");
        assert_eq!(r.lines[2].text, "c");
        assert_eq!(r.total_lines, 3);
    }

    #[test]
    fn total_lines_counts_with_and_without_trailing_newline() {
        // 尾换行不产生幽灵空行；空文件为 0 行
        assert_eq!(build_read_window("a\nb", req(1, 10), 2000, 200_000).total_lines, 2);
        assert_eq!(build_read_window("a\nb\n", req(1, 10), 2000, 200_000).total_lines, 2);
        assert_eq!(build_read_window("", req(1, 10), 2000, 200_000).total_lines, 0);
    }

    #[test]
    fn offset_beyond_eof_returns_empty_window() {
        let r = build_read_window("a\nb\nc", req(10, 10), 2000, 200_000);
        assert!(r.lines.is_empty());
        assert_eq!(r.total_lines, 3);
        assert!(!r.truncated);
    }

    #[test]
    fn offset_out_of_range_predicate() {
        // 越界 = offset > totalLines；空文件（0 行）读 offset=1 是合法空读，不误报
        assert!(offset_out_of_range(999, 20));
        assert!(offset_out_of_range(4, 3));
        assert!(offset_out_of_range(2, 1));
        assert!(!offset_out_of_range(1, 1));
        assert!(!offset_out_of_range(3, 3));
        assert!(!offset_out_of_range(1, 0));
        assert!(offset_out_of_range(2, 0));
    }
}

#[cfg(test)]
mod list_dir_tests {
    use super::*;

    /// 临时目录（纳秒级命名防碰撞；测试自清理）。
    fn tmp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("atelyx-list-dir-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dirs_first_with_counts_sizes_and_hidden_skipped() {
        let root = tmp_root("basic");
        std::fs::create_dir_all(root.join("子目录/内层")).unwrap();
        std::fs::create_dir_all(root.join("子目录/.git")).unwrap(); // 子目录内隐藏项：不应计入 children
        std::fs::write(root.join("子目录/a.txt"), "x").unwrap();
        std::fs::write(root.join("b.md"), "hello").unwrap();
        std::fs::write(root.join(".隐藏.md"), "h").unwrap();
        std::fs::create_dir_all(root.join(".atelyx")).unwrap();
        let r = list_dir_entries(&root, 200).unwrap();
        // 隐藏项（. 开头段）对 AI 完全屏蔽：.隐藏.md 与 .atelyx 均不出现
        assert_eq!(r.total, 2);
        assert!(!r.capped);
        assert_eq!(r.entries[0].name, "子目录");
        assert_eq!(r.entries[0].kind, "dir");
        // 子目录直接子项数 = 可见项（内层 + a.txt），隐藏的 .git 不计入
        assert_eq!(r.entries[0].children, Some(2));
        assert!(r.entries[0].size.is_none());
        assert_eq!(r.entries[1].name, "b.md");
        assert_eq!(r.entries[1].kind, "file");
        assert_eq!(r.entries[1].size, Some(5));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn list_dir_target_rejects_hidden_directory() {
        let root = tmp_root("hidden");
        std::fs::create_dir_all(root.join(".atelyx")).unwrap();
        assert!(list_dir_target(&root, ".atelyx").is_err());
        assert!(list_dir_target(&root, "a/.hidden").is_err());
        // 普通目录可通过
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        assert!(list_dir_target(&root, "a").is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn caps_at_max_and_reports_real_total() {
        let root = tmp_root("cap");
        for i in 0..5 {
            std::fs::write(root.join(format!("f{i}.txt")), "").unwrap();
        }
        let r = list_dir_entries(&root, 3).unwrap();
        assert_eq!(r.entries.len(), 3);
        assert_eq!(r.total, 5);
        assert!(r.capped);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
