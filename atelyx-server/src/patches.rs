//! 空间内容增量补丁端点（画布 `.atlx` / 表格 `.atb`）：客户端补丁按稳定 id 合并进服务端真源
//! （合并语义与客户端本地保存一致），落地后向空间房间广播补丁帧。
//! 合并语义见 `apply_canvas_patch` / `apply_table_patch`；并发模型与整文件写共用每路径异步锁（见 `state::PathLocks`）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::access::{join_err, write_root};
use crate::fsops::{atomic_write, file_mtime_secs, is_windows_reserved_name};
use crate::state::{bad_request, internal, now_secs};
use crate::{ApiError, ServerState};

/// `.atlx` schema 版本号（私有格式保护：不符即拒绝解析，防外部工具/手改误写）。
const CANVAS_SCHEMA: &str = "atelyx-canvas/v1";
/// `.atb` schema 版本号。
const TABLE_SCHEMA: &str = "atelyx-table/v1";

// ===== .atlx 文件结构（与客户端 types/canvas.ts 同形状；data 不耦合业务字段）=====

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFile {
    #[serde(default)]
    pub schema: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub nodes: Vec<CanvasFileNode>,
    #[serde(default)]
    pub edges: Vec<CanvasFileEdge>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFileNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub x: f64,
    pub y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    pub data: serde_json::Value,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CanvasFileEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_handle: Option<String>,
    /// false = 关联边（无消费语义）；缺省 = 数据流边。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directed: Option<bool>,
    /// 关联边的箭头模式（"none" | "single" | "double"，仅 directed: false 生效；缺省 = 无向）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mode: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

/// 画布增量补丁（与客户端 types/canvas.ts 的 CanvasPatch 同形状）：只含变化/新增/删除的实体。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CanvasPatch {
    /// 画布 id（防串文件守卫）。
    pub id: String,
    /// 标题变化时更新（title 变更 = 同目录改文件名，写盘后返回新相对路径）。
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub upsert_nodes: Vec<CanvasFileNode>,
    #[serde(default)]
    pub removed_node_ids: Vec<String>,
    #[serde(default)]
    pub upsert_edges: Vec<CanvasFileEdge>,
    #[serde(default)]
    pub removed_edge_ids: Vec<String>,
}

// ===== .atb 表格文件结构（与客户端 types/table.ts 同形状；values 不耦合单元格业务）=====

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TableFile {
    #[serde(default)]
    pub schema: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub fields: Vec<TableField>,
    #[serde(default)]
    pub rows: Vec<TableRow>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TableField {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    /// singleSelect 的选项列表（其他类型无此字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// 用户拖拽调整后的列宽（px；缺省 = 前端按字段名自适应）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// 状态栏列自动计算类型（sum/avg/max/min/count；缺省 = 无计算）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calc_type: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct TableRow {
    pub id: String,
    /// 单元格值按字段 id 存（缺 key = 空单元格）。
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
    /// 用户拖拽调整后的行高（px；缺省 = 前端按内容自然撑开）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// 单元格显示样式按字段 id 存（与值正交；缺 key = 默认样式）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<serde_json::Map<String, serde_json::Value>>,
}

/// 表格增量补丁（与客户端 types/table.ts 的 TablePatch 同形状）。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TablePatch {
    /// 表格 id（防串文件守卫）。
    pub id: String,
    /// 标题变化时更新（title 变更 = 同目录改文件名）。
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub upsert_fields: Vec<TableField>,
    #[serde(default)]
    pub removed_field_ids: Vec<String>,
    #[serde(default)]
    pub upsert_rows: Vec<TableRow>,
    #[serde(default)]
    pub removed_row_ids: Vec<String>,
    /// 字段 id 全序（与上次落盘序列不同时携带；排序是数组属性，id 合并无法表达，须显式重排）。
    #[serde(default)]
    pub field_order: Option<Vec<String>>,
    /// 行 id 全序（同上）。
    #[serde(default)]
    pub row_order: Option<Vec<String>>,
}

/// 按 id 顺序重排数组（稳定排序）：order 未出现的 id（同批删除/对端新增）保持相对顺序置于末尾。
fn reorder_by<T>(items: &mut Vec<T>, order: &[String], key: impl Fn(&T) -> &str) {
    if items.len() <= 1 {
        return;
    }
    let rank: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
    items.sort_by_key(|item| rank.get(key(item)).copied().unwrap_or(usize::MAX));
}

/// 文件名净化：非法字符换 `_`、去首尾空白、Windows 保留名与尾点/尾空格补偿
/// （服务端数据目录可能落在 Windows 盘，与客户端同口径）。
fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    let stem = trimmed.split('.').next().unwrap_or("");
    if is_windows_reserved_name(stem) {
        format!("_{trimmed}")
    } else if trimmed.ends_with(['.', ' ']) {
        format!("{trimmed}_")
    } else {
        trimmed.to_string()
    }
}

/// title 变更后的同目录新相对路径（旧路径取目录前缀 + 净化标题 + 扩展名）。
fn rel_with_new_title(old_file: &str, new_title: &str, ext: &str) -> String {
    let filename = format!("{}.{}", sanitize_filename(new_title), ext);
    match old_file.rfind('/') {
        Some(i) => format!("{}/{}", &old_file[..i], filename),
        None => filename,
    }
}

/// 两路径是否指向同一物理文件（case-only 重命名在大小写不敏感文件系统上的场景：
/// 写新后删旧会删掉刚写入的文件）。都 canonicalize 成功且相等才豁免；任一失败不豁免。
fn same_physical_file(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 名冲突守卫：新路径已被另一文件占用（解析出的稳定 id 不同）→ 409，防静默覆盖丢失；
/// 现存文件解析失败（损坏）不拦截。仅路径漂移（title 变更）时调用——同名时文件就是本次基底。
fn ensure_no_id_conflict(
    new_path: &Path,
    id: &str,
    existing_id: &dyn Fn(&Path) -> Option<String>,
    kind: &str,
    title: &str,
) -> Result<(), ApiError> {
    if new_path.exists() {
        if let Some(existing) = existing_id(new_path) {
            if existing != id {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    format!("{kind}名冲突：另一{kind}已使用名称「{title}」"),
                ));
            }
        }
    }
    Ok(())
}

/// 路径漂移（title 改名）后的旧文件清理：case-only 重命名指向同一物理文件时不删；
/// 旧文件已不在跳过；其余删除，失败须报错——同 id 双文件会歧义（列表读到旧内容）。
fn remove_replaced_file(old_path: &Path, new_path: &Path, kind: &str) -> Result<(), ApiError> {
    if old_path != new_path && old_path.exists() && !same_physical_file(old_path, new_path) {
        std::fs::remove_file(old_path)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除旧{kind}文件失败：{e}")))?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasPatchBody {
    path: String,
    patch: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePatchBody {
    path: String,
    patch: serde_json::Value,
}

fn parse_patch<T: serde::de::DeserializeOwned>(raw: &serde_json::Value, what: &str) -> Result<T, ApiError> {
    serde_json::from_value(raw.clone())
        .map_err(|e| bad_request(&format!("{what}格式不合法：{e}")))
}

/// 读画布真源文件：schema 不符或解析失败（损坏）→ 400 拒绝覆盖，不静默重建。
fn read_canvas_file(path: &Path) -> Result<CanvasFile, ApiError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("读取失败：{e}")))?;
    let canvas: CanvasFile = serde_json::from_str(&text)
        .map_err(|e| bad_request(&format!("画布文件无法解析（可能已损坏），拒绝覆盖：{e}")))?;
    if canvas.schema != CANVAS_SCHEMA {
        return Err(bad_request(&format!("画布 schema 不匹配：{}", canvas.schema)));
    }
    Ok(canvas)
}

/// 读表格真源文件：schema 不符或解析失败（损坏）→ 400 拒绝覆盖，不静默重建。
fn read_table_file(path: &Path) -> Result<TableFile, ApiError> {
    let text = std::fs::read_to_string(path).map_err(|e| internal(format!("读取失败：{e}")))?;
    let table: TableFile = serde_json::from_str(&text)
        .map_err(|e| bad_request(&format!("表格文件无法解析（可能已损坏），拒绝覆盖：{e}")))?;
    if table.schema != TABLE_SCHEMA {
        return Err(bad_request(&format!("表格 schema 不匹配：{}", table.schema)));
    }
    Ok(table)
}

fn write_canvas_file(path: &Path, canvas: &CanvasFile) -> Result<(), ApiError> {
    let json = serde_json::to_string_pretty(canvas).map_err(internal)?;
    atomic_write(path, json.as_bytes()).map_err(internal)
}

fn write_table_file(path: &Path, table: &TableFile) -> Result<(), ApiError> {
    let json = serde_json::to_string_pretty(table).map_err(internal)?;
    atomic_write(path, json.as_bytes()).map_err(internal)
}

/// 按稳定 id 合并画布补丁（removed 幂等；upsert 覆盖同 id 或追加）。
fn apply_canvas_patch(canvas: &mut CanvasFile, patch: &CanvasPatch) {
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
}

/// 按稳定 id 合并表格补丁 + fieldOrder/rowOrder 全序重排。
fn apply_table_patch(table: &mut TableFile, patch: &TablePatch) {
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
    // 顺序变化（拖拽排序/复制行/左右插列）：已删 id 的下标自然空置，order 未出现的实体（并发新增）置尾
    if let Some(order) = &patch.field_order {
        reorder_by(&mut table.fields, order, |f| f.id.as_str());
    }
    if let Some(order) = &patch.row_order {
        reorder_by(&mut table.rows, order, |r| r.id.as_str());
    }
}

/// 补丁端点的类型槽位（画布/表格同构事务的参数点）。
struct PatchEntityIo<E, P> {
    kind: &'static str,
    ext: &'static str,
    /// parse_patch 的载荷文案（"画布补丁" / "表格补丁"）。
    what: &'static str,
    /// 落地后向空间房间广播的帧类型（"canvas-patch" / "table-patch"）。
    broadcast_kind: &'static str,
    read_file: fn(&Path) -> Result<E, ApiError>,
    write_file: fn(&Path, &E) -> Result<(), ApiError>,
    parse: fn(&serde_json::Value, &str) -> Result<P, ApiError>,
    patch_id: fn(&P) -> &str,
    patch_title: fn(&P) -> Option<&String>,
    apply: fn(&mut E, &P),
    entity_id: fn(&E) -> &str,
    entity_title: fn(&E) -> &str,
    set_title: fn(&mut E, String),
    set_updated_at: fn(&mut E, i64),
}

fn canvas_patch_io() -> PatchEntityIo<CanvasFile, CanvasPatch> {
    PatchEntityIo {
        kind: "画布",
        ext: "atlx",
        what: "画布补丁",
        broadcast_kind: "canvas-patch",
        read_file: read_canvas_file,
        write_file: write_canvas_file,
        parse: parse_patch::<CanvasPatch>,
        patch_id: |p| &p.id,
        patch_title: |p| p.title.as_ref(),
        apply: apply_canvas_patch,
        entity_id: |c| &c.id,
        entity_title: |c| &c.title,
        set_title: |c, t| c.title = t,
        set_updated_at: |c, v| c.updated_at = v,
    }
}

fn table_patch_io() -> PatchEntityIo<TableFile, TablePatch> {
    PatchEntityIo {
        kind: "表格",
        ext: "atb",
        what: "表格补丁",
        broadcast_kind: "table-patch",
        read_file: read_table_file,
        write_file: write_table_file,
        parse: parse_patch::<TablePatch>,
        patch_id: |p| &p.id,
        patch_title: |p| p.title.as_ref(),
        apply: apply_table_patch,
        entity_id: |t| &t.id,
        entity_title: |t| &t.title,
        set_title: |t, title| t.title = title,
        set_updated_at: |t, v| t.updated_at = v,
    }
}

/// 补丁端点公共骨架：存在检查 → 解析补丁 → 读盘 → id 守卫 → title 应用 → 漂移目标锁 →
/// 漂移冲突守卫 → 合并 → 原子写 → 旧文件清理 → 广播帧。结构锁与请求路径锁由 handler 持有后调用；
/// title 漂移产生的第二条落盘路径在本函数内补取路径锁（见下）。
async fn patch_entity<E: serde::de::DeserializeOwned, P>(
    state: &ServerState,
    space_id: &str,
    path: &Path,
    body_path: &str,
    raw_patch: &serde_json::Value,
    io: &PatchEntityIo<E, P>,
) -> Result<Response, ApiError> {
    // 磁盘文件缺失：补丁只有变化实体，重建会丢未变化部分——拒绝
    if !path.is_file() {
        return Err(crate::state::not_found(&format!(
            "{}文件不存在（已从磁盘删除）",
            io.kind
        )));
    }
    let patch = (io.parse)(raw_patch, io.what)?;
    let mut entity = (io.read_file)(path)?;
    // 防串文件守卫：补丁属于另一实体（陈旧保存回调）→ 拒绝，防跨文件混写
    if (io.patch_id)(&patch) != (io.entity_id)(&entity) {
        return Err(bad_request(&format!(
            "补丁与文件不匹配：补丁属于另一{}，已中止保存",
            io.kind
        )));
    }
    if let Some(title) = (io.patch_title)(&patch) {
        (io.set_title)(&mut entity, title.clone());
    }
    let parent = path
        .parent()
        .ok_or_else(|| bad_request(&format!("非法路径：{}", body_path)))?;
    let new_path = parent.join(format!("{}.{}", sanitize_filename((io.entity_title)(&entity)), io.ext));
    let new_rel = rel_with_new_title(body_path, (io.entity_title)(&entity), io.ext);
    // 漂移目标路径锁：目标路径上的并发 write_file（单路径锁，不进结构锁）可与本端点的
    // 「写新删旧」交错——不持锁时对方的整文件写会被随后的删旧静默吞掉。补丁与改名是仅有的
    // 双路径锁持有者且已被结构锁互斥，单锁持有方不会等待第二把锁，持一取二无死锁环。
    let _drift_lock = if new_path != path {
        Some(state.path_lock(space_id, &new_rel).await)
    } else {
        None
    };
    if new_path != path {
        ensure_no_id_conflict(
            &new_path,
            (io.entity_id)(&entity),
            &|p: &Path| {
                std::fs::read_to_string(p)
                    .ok()
                    .and_then(|t| serde_json::from_str::<E>(&t).ok())
                    .map(|e| (io.entity_id)(&e).to_string())
            },
            io.kind,
            (io.entity_title)(&entity),
        )?;
    }
    (io.apply)(&mut entity, &patch);
    (io.set_updated_at)(&mut entity, now_secs());
    (io.write_file)(&new_path, &entity)?;
    remove_replaced_file(path, &new_path, io.kind)?;
    // 落地后向空间房间广播补丁帧（与客户端 WS 透传帧同形状）；失败只记日志不回滚——真源已落盘
    ws_broadcast(state, space_id, io.broadcast_kind, &new_rel, raw_patch.clone());
    tracing::info!(space_id = %space_id, path = %new_rel, "{}补丁落地", io.kind);
    let updated_at = file_mtime_secs(&new_path);
    Ok(Json(json!({ "updatedAt": updated_at, "file": new_rel })).into_response())
}

/// 画布补丁端点：锁内 读 → 校验 → 合并 → 原子写 → 广播。
pub async fn patch_canvas(
    State(state): State<ServerState>,
    user: AuthUser,
    AxumPath(space_id): AxumPath<String>,
    Json(body): Json<CanvasPatchBody>,
) -> Result<Response, ApiError> {
    let root = write_root(&state, &space_id, &user)?;
    let path = root.join(&body.path, false).map_err(join_err)?;
    // 结构锁 → 请求路径锁：与改名端点互斥（改名同样持结构锁 + 双路径锁），防在途改名/补丁
    // 互相重建对方路径的文件；title 漂移目标的第二把路径锁在骨架内补取（漂移点才可知）
    let _structure = state.structure_lock(&space_id).await;
    let _lock = state.path_lock(&space_id, &body.path).await;
    patch_entity(&state, &space_id, &path, &body.path, &body.patch, &canvas_patch_io()).await
}

/// 表格补丁端点：锁内 读 → 校验 → 合并 → 原子写 → 广播（与画布同构）。
pub async fn patch_table(
    State(state): State<ServerState>,
    user: AuthUser,
    AxumPath(space_id): AxumPath<String>,
    Json(body): Json<TablePatchBody>,
) -> Result<Response, ApiError> {
    let root = write_root(&state, &space_id, &user)?;
    let path = root.join(&body.path, false).map_err(join_err)?;
    // 结构锁 → 请求路径锁（与画布补丁同构）；漂移目标锁在骨架内补取
    let _structure = state.structure_lock(&space_id).await;
    let _lock = state.path_lock(&space_id, &body.path).await;
    patch_entity(&state, &space_id, &path, &body.path, &body.patch, &table_patch_io()).await
}

fn ws_broadcast(state: &ServerState, space_id: &str, kind: &'static str, file: &str, patch: serde_json::Value) {
    crate::ws::broadcast_patch(&state.hub(), &format!("space:{space_id}"), kind, file, patch);
}
