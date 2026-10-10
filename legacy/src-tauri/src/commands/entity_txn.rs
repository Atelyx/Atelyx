//! 画布/表格文件事务的公共骨架：write/patch/rename/create 四联命令按类型槽位参数化，
//! 事务顺序统一为「冲突守卫 → 写盘 → 引用同步（漂移时）→ 旧文件清理 → 缓存 → 广播」。
//! 对外 Tauri 命令名与载荷不变；画布无引用方（不被 .atlx 引用），经 `sync_refs = false` 跳过引用同步。
use std::path::{Path, PathBuf};

use chrono::Utc;
use tauri::{State, WebviewWindow};

use super::content_broadcast::{broadcast_content_changes, ContentChange};
use super::vault::{
    collect_ref_updates, ensure_no_id_conflict, flush_canvas_updates, remove_replaced_file,
    PatchWriteResult,
};
use crate::vault::{
    cache_evict_canvas, cache_evict_table, cache_put_canvas, cache_put_table,
    read_canvas_file, read_canvas_file_cached, read_table_file, read_table_file_cached,
    rel_with_new_title, same_physical_file, safe_join, sanitize_filename, write_canvas_file,
    write_table_file, CanvasFile, CanvasPatch, TableFile, TablePatch, VaultState,
};

/// 实体公共字段的访问口（id/title/时间戳；画布与表格同构）。
pub(crate) trait VaultEntity: Clone {
    fn id(&self) -> &str;
    fn title(&self) -> &str;
    fn set_title(&mut self, title: String);
    fn created_at(&self) -> i64;
    fn set_created_at(&mut self, v: i64);
    fn set_updated_at(&mut self, v: i64);
}

impl VaultEntity for CanvasFile {
    fn id(&self) -> &str { &self.id }
    fn title(&self) -> &str { &self.title }
    fn set_title(&mut self, title: String) { self.title = title; }
    fn created_at(&self) -> i64 { self.created_at }
    fn set_created_at(&mut self, v: i64) { self.created_at = v; }
    fn set_updated_at(&mut self, v: i64) { self.updated_at = v; }
}

impl VaultEntity for TableFile {
    fn id(&self) -> &str { &self.id }
    fn title(&self) -> &str { &self.title }
    fn set_title(&mut self, title: String) { self.title = title; }
    fn created_at(&self) -> i64 { self.created_at }
    fn set_created_at(&mut self, v: i64) { self.created_at = v; }
    fn set_updated_at(&mut self, v: i64) { self.updated_at = v; }
}

/// 补丁的公共取值口（id 必带；title 可选）。补丁合并按实体域各自实现
/// （画布 = 节点/边集合，表格 = 字段/行集合 + 全序重排），由调用方以 apply 闭包传入。
pub(crate) trait EntityPatch {
    fn patch_id(&self) -> &str;
    fn patch_title(&self) -> Option<&str>;
}

impl EntityPatch for CanvasPatch {
    fn patch_id(&self) -> &str { &self.id }
    fn patch_title(&self) -> Option<&str> { self.title.as_deref() }
}

impl EntityPatch for TablePatch {
    fn patch_id(&self) -> &str { &self.id }
    fn patch_title(&self) -> Option<&str> { self.title.as_deref() }
}

/// 类型槽位：实体读写、缓存槽、种类文案与扩展名、是否同步 .atlx 引用。
pub(crate) struct EntityIo<E> {
    pub kind: &'static str,
    pub ext: &'static str,
    pub read_file: fn(&Path) -> Result<E, String>,
    pub write_file: fn(&Path, &E) -> Result<(), String>,
    pub cached_read: fn(&VaultState, &Path, &str) -> Result<(PathBuf, E), String>,
    pub cache_evict: fn(&VaultState, &str),
    pub cache_put: fn(&VaultState, &Path, &str, &E),
    pub sync_refs: bool,
}

pub(crate) fn canvas_io() -> EntityIo<CanvasFile> {
    EntityIo {
        kind: "画布",
        ext: "atlx",
        read_file: read_canvas_file,
        write_file: write_canvas_file,
        cached_read: read_canvas_file_cached,
        cache_evict: cache_evict_canvas,
        cache_put: cache_put_canvas,
        sync_refs: false,
    }
}

pub(crate) fn table_io() -> EntityIo<TableFile> {
    EntityIo {
        kind: "表格",
        ext: "atb",
        read_file: read_table_file,
        write_file: write_table_file,
        cached_read: read_table_file_cached,
        cache_evict: cache_evict_table,
        cache_put: cache_put_table,
        sync_refs: true,
    }
}

/// title 决定的落点：同目录新路径（磁盘）与漂移后的相对路径。
fn title_target(old_path: &Path, file: &str, title: &str, ext: &str) -> Result<(PathBuf, String), String> {
    let parent = old_path
        .parent()
        .ok_or_else(|| format!("非法路径：{}", file))?;
    let new_path = parent.join(format!("{}.{}", sanitize_filename(title), ext));
    let new_rel = rel_with_new_title(file, title, ext);
    Ok((new_path, new_rel))
}

/// 引用同步（漂移时）：先刷全部 .atlx 再清理旧文件——失败删新文件回滚（旧文件未动）。
/// case-only 改名时新旧路径指向同一物理文件，删「新文件」即删唯一副本，保留不动。
fn sync_refs_on_drift(
    root: &Path,
    file: &str,
    new_rel: &str,
    old_path: &Path,
    new_path: &Path,
    kind: &str,
) -> Result<(), String> {
    if old_path == new_path {
        return Ok(());
    }
    let pending = collect_ref_updates(root, file, new_rel)?;
    if let Err(e) = flush_canvas_updates(&pending) {
        if !same_physical_file(old_path, new_path) {
            let _ = std::fs::remove_file(new_path);
        }
        return Err(format!("更新画布引用失败：{e}"));
    }
    remove_replaced_file(old_path, new_path, kind)
}

fn broadcast_write(window: &WebviewWindow, root: &Path, file: &str, new_rel: &str) {
    // title 漂移 = 写盘同时改名：接收方先跟路径再对账
    let change = if new_rel == file {
        ContentChange::write(file)
    } else {
        ContentChange::write_drifted(new_rel, file)
    };
    broadcast_content_changes(window, &root.to_string_lossy(), vec![change]);
}

/// 全量写（title 变更 = 同目录改文件名）：createdAt 保留磁盘值（新文件用当前时间），
/// 返回落盘的 updated_at。
pub(crate) fn write_entity<E: VaultEntity>(
    window: &WebviewWindow,
    state: &State<'_, VaultState>,
    io: &EntityIo<E>,
    file: &str,
    mut entity: E,
) -> Result<i64, String> {
    let root = state.root()?;
    let old_path = safe_join(&root, file, false)?;
    let (new_path, new_rel) = title_target(&old_path, file, entity.title(), io.ext)?;
    // 新路径已存在且 id 不同（前端 dedupe 被绕过/同步盘合并）：拒绝覆盖，防静默丢失另一实体
    ensure_no_id_conflict(
        &new_path,
        entity.id(),
        &|p: &Path| (io.read_file)(p).ok().map(|e| e.id().to_string()),
        io.kind,
        entity.title(),
    )?;
    let now = Utc::now().timestamp();
    // createdAt 保留：读一次磁盘（缓存命中免重读；文件缺失 = 新实体用当前时间）
    if old_path.exists() {
        let (_, disk) = (io.cached_read)(&state, &root, file)
            .map_err(|e| format!("磁盘{}文件损坏，无法保存：{} ({e})", io.kind, old_path.display()))?;
        entity.set_created_at(disk.created_at());
    } else {
        entity.set_created_at(now);
    }
    entity.set_updated_at(now);
    (io.write_file)(&new_path, &entity)?;
    if io.sync_refs {
        sync_refs_on_drift(&root, file, &new_rel, &old_path, &new_path, io.kind)?;
    } else {
        remove_replaced_file(&old_path, &new_path, io.kind)?;
    }
    (io.cache_evict)(&state, file);
    (io.cache_put)(&state, &new_path, &new_rel, &entity);
    broadcast_write(window, &root, file, &new_rel);
    Ok(now)
}

/// 增量补丁写：磁盘文件必须存在（补丁只含变化实体，重建丢未变化部分）；补丁 id 与磁盘
/// 实体一致才合并；title 变更走漂移事务。返回 (updated_at, 落盘后的相对路径)。
pub(crate) fn patch_entity<E: VaultEntity, P: EntityPatch>(
    window: &WebviewWindow,
    state: &State<'_, VaultState>,
    io: &EntityIo<E>,
    file: &str,
    patch: &P,
    apply: impl Fn(&mut E, &P),
) -> Result<PatchWriteResult, String> {
    let root = state.root()?;
    let old_path = safe_join(&root, file, false)?;
    // 磁盘文件缺失（外部删除）：补丁只有变化实体，重建会丢未变化部分——拒绝并回退全量写
    if !old_path.exists() {
        return Err(format!("{}文件不存在（已从磁盘删除）", io.kind));
    }
    let (_, mut entity) = (io.cached_read)(&state, &root, file)?;
    // 防串文件守卫：补丁属于另一实体（陈旧保存回调）→ 拒绝，防跨文件混写
    if patch.patch_id() != entity.id() {
        return Err(format!("{}身份不匹配，已中止保存", io.kind));
    }
    if let Some(title) = patch.patch_title() {
        entity.set_title(title.to_string());
    }
    let (new_path, new_rel) = title_target(&old_path, file, entity.title(), io.ext)?;
    // 名冲突守卫仅路径漂移（title 变更）时检查——同名时文件就是本次基底，id 必然一致，免每次保存全量重读
    if old_path != new_path {
        ensure_no_id_conflict(
            &new_path,
            entity.id(),
            &|p: &Path| (io.read_file)(p).ok().map(|e| e.id().to_string()),
            io.kind,
            entity.title(),
        )?;
    }
    apply(&mut entity, patch);
    let now = Utc::now().timestamp();
    entity.set_updated_at(now);
    (io.write_file)(&new_path, &entity)?;
    if io.sync_refs {
        sync_refs_on_drift(&root, file, &new_rel, &old_path, &new_path, io.kind)?;
    } else {
        remove_replaced_file(&old_path, &new_path, io.kind)?;
    }
    (io.cache_evict)(&state, file);
    (io.cache_put)(&state, &new_path, &new_rel, &entity);
    broadcast_write(window, &root, file, &new_rel);
    Ok(PatchWriteResult { updated_at: now, file: new_rel })
}

/// 重命名：更新实体 title 与 updated_at 后走写盘事务；路径漂移按迁移广播，同名落点按写盘广播。
pub(crate) fn rename_entity<E: VaultEntity>(
    window: &WebviewWindow,
    state: &State<'_, VaultState>,
    io: &EntityIo<E>,
    file: &str,
    new_title: String,
) -> Result<(), String> {
    let root = state.root()?;
    let old_path = safe_join(&root, file, false)?;
    let mut entity = (io.read_file)(&old_path)?;
    entity.set_title(new_title);
    entity.set_updated_at(Utc::now().timestamp());
    let (new_path, new_rel) = title_target(&old_path, file, entity.title(), io.ext)?;
    // 目标已被另一实体占用（内部 id 不同，文件名与 title 脱钩时前端 dedupe 防不住）时拒绝覆盖，
    // 防静默丢失（同全量写的保存守卫；同物理文件 = case-only 改名豁免）
    if !same_physical_file(&old_path, &new_path) {
        ensure_no_id_conflict(
            &new_path,
            entity.id(),
            &|p: &Path| (io.read_file)(p).ok().map(|e| e.id().to_string()),
            io.kind,
            entity.title(),
        )?;
    }
    // 先写新文件再删旧文件，保证不丢数据；引用预扫描在写盘前——扫描失败时磁盘零残留
    //（不产生同 id 双文件），落盘后 flush 失败才删新文件回滚（旧文件未动）
    let pending = if io.sync_refs {
        Some(collect_ref_updates(&root, file, &new_rel)?)
    } else {
        None
    };
    (io.write_file)(&new_path, &entity)?;
    if let Some(pending) = pending {
        if let Err(e) = flush_canvas_updates(&pending) {
            if !same_physical_file(&old_path, &new_path) {
                let _ = std::fs::remove_file(&new_path);
            }
            return Err(format!("更新画布引用失败，重命名已回滚（请重试）：{e}"));
        }
        remove_replaced_file(&old_path, &new_path, io.kind)?;
    } else {
        remove_replaced_file(&old_path, &new_path, io.kind)?;
    }
    (io.cache_evict)(&state, file);
    (io.cache_put)(&state, &new_path, &new_rel, &entity);
    // 路径漂移 = 迁移（接收方跟路径后重读）；同名落点也是内容改写（title/updated_at），按写盘广播
    let change = if new_rel != file {
        ContentChange::rename(file, &new_rel)
    } else {
        ContentChange::write(file)
    };
    broadcast_content_changes(window, &root.to_string_lossy(), vec![change]);
    Ok(())
}

/// 新建：路径 = dir（相对仓库根，空 = 根目录）+ sanitized title。同名已存在则拒绝。
/// 返回 (id, 相对路径)。
pub(crate) fn create_entity<E: VaultEntity>(
    state: &State<'_, VaultState>,
    io: &EntityIo<E>,
    dir: &str,
    entity: E,
) -> Result<(String, String), String> {
    let root = state.root()?;
    let filename = format!("{}.{}", sanitize_filename(entity.title()), io.ext);
    let rel = if dir.is_empty() {
        filename
    } else {
        format!("{dir}/{filename}")
    };
    let path = safe_join(&root, &rel, true)?;
    // 后端兜底：同名实体已存在则拒绝（前端 dedupe 是正常路径，此处防绕过/同步盘合并覆盖）
    if path.exists() {
        return Err(format!("{}名冲突：{}", io.kind, entity.title()));
    }
    (io.write_file)(&path, &entity)?;
    Ok((entity.id().to_string(), rel))
}
