//! 空间内容 CRUD（真源 = 服务器文件树）：树 / 读 / 写 / 重命名（跨目录移动）/ 复制 /
//! 建文件夹 / 删文件 / 删文件夹。所有端点先过成员校验，写操作 editor 与 owner 均可。
//! 写 = 原子写（防半截文件）；路径安全由 `fsops::SpaceRoot` 统一校验（防穿越/越界）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::fsops::{atomic_write, file_mtime_secs, read_dir_filtered, JoinError, SpaceRoot};
use crate::state::ServerState;
use crate::{ApiError, ApiResult};

/// 成员校验 + 内容根。写端点统一走这里（读端点角色要求相同——两档角色均有读权）。
pub(crate) fn member_root(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<SpaceRoot, ApiError> {
    state.read(|p| {
        let space = p
            .spaces
            .iter()
            .find(|s| s.id == space_id)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "空间不存在".to_string()))?;
        space
            .members
            .iter()
            .find(|m| m.user_id == user.user_id)
            .ok_or_else(|| ApiError(StatusCode::FORBIDDEN, "不是该空间成员".to_string()))?;
        let root = match &space.root_path {
            Some(p) => std::path::PathBuf::from(p),
            None => state.default_space_root(space_id),
        };
        Ok(SpaceRoot(root))
    })
}

fn bad_request(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_string())
}

fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.to_string())
}

fn join_err(e: JoinError) -> ApiError {
    match e {
        JoinError::Invalid(m) => bad_request(&m),
        JoinError::NotFound(m) => not_found(&m),
    }
}

/// 全仓库文件树（嵌套；隐藏目录与 `.tmp` 原子写产物不出现，与客户端文件面板同过滤）。
pub async fn tree(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let nodes = list_tree_in(&root.0, "").map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::to_value(nodes).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeNode {
    name: String,
    path: String,
    is_dir: bool,
    updated_at: i64,
    children: Vec<TreeNode>,
}

fn list_tree_in(root: &std::path::Path, rel: &str) -> Result<Vec<TreeNode>, String> {
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    let mut nodes: Vec<TreeNode> = vec![];
    for (child_rel, is_dir) in read_dir_filtered(&dir, rel)? {
        let path = root.join(&child_rel);
        let children = if is_dir { list_tree_in(root, &child_rel)? } else { vec![] };
        nodes.push(TreeNode {
            name: child_rel.rsplit('/').next().unwrap_or(&child_rel).to_string(),
            path: child_rel,
            is_dir,
            updated_at: file_mtime_secs(&path),
            children,
        });
    }
    nodes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.is_dir.cmp(&b.is_dir)));
    Ok(nodes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePathQuery {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBody {
    path: String,
    content: String,
}

/// 读文本文件（非 UTF-8 字节按替换字符容错）。
pub async fn read_file(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let path = root.join(&query.path, false).map_err(join_err)?;
    if !path.is_file() {
        return Err(not_found("文件不存在"));
    }
    let bytes = std::fs::read(&path).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("读取失败：{e}")))?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Json(json!({ "content": content, "updatedAt": file_mtime_secs(&path) })))
}

/// 写文本文件（原子写；自动建父目录；路径已存在目录则拒绝）。
pub async fn write_file(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<WriteBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let path = root.join(&body.path, true).map_err(join_err)?;
    if path.is_dir() {
        return Err(bad_request("目标已是目录"));
    }
    atomic_write(&path, body.content.as_bytes())
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    tracing::info!(space_id = %space_id, path = %body.path, bytes = body.content.len(), "内容写入");
    Ok(Json(json!({ "updatedAt": file_mtime_secs(&path) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameBody {
    old_path: String,
    new_path: String,
}

/// 重命名 / 移动（文件与文件夹通用；目标父目录自动创建；目标已存在即拒绝，不静默覆盖）。
pub async fn rename(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<RenameBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let from = root.join(&body.old_path, false).map_err(join_err)?;
    if !from.exists() {
        return Err(not_found("源路径不存在"));
    }
    let to = root.join(&body.new_path, true).map_err(join_err)?;
    if to.exists() {
        return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{}", body.new_path)));
    }
    std::fs::rename(&from, &to)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("重命名失败：{e}")))?;
    tracing::info!(space_id = %space_id, from = %body.old_path, to = %body.new_path, "内容重命名");
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyBody {
    from_path: String,
    to_path: String,
}

/// 复制（文件或整文件夹递归；副本与原件互不关联，目标已存在即拒绝）。
pub async fn copy(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<CopyBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let from = root.join(&body.from_path, false).map_err(join_err)?;
    if !from.exists() {
        return Err(not_found("源路径不存在"));
    }
    let to = root.join(&body.to_path, true).map_err(join_err)?;
    if to.exists() {
        return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{}", body.to_path)));
    }
    copy_recursive(&from, &to)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("复制失败：{e}")))?;
    tracing::info!(space_id = %space_id, from = %body.from_path, to = %body.to_path, "内容复制");
    Ok(Json(json!({})))
}

fn copy_recursive(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// 删除文件（文件夹走 delete_folder）。
pub async fn delete_file(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let path = root.join(&query.path, false).map_err(join_err)?;
    if !path.exists() {
        return Err(not_found("文件不存在"));
    }
    if path.is_dir() {
        return Err(bad_request("目标是目录，请走文件夹删除"));
    }
    std::fs::remove_file(&path).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败：{e}")))?;
    tracing::info!(space_id = %space_id, path = %query.path, "内容删除");
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFolderBody {
    path: String,
}

pub async fn create_folder(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<CreateFolderBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let path = root.join(&body.path, true).map_err(join_err)?;
    if path.exists() {
        return Err(ApiError(StatusCode::CONFLICT, format!("路径已存在：{}", body.path)));
    }
    std::fs::create_dir_all(&path).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("创建目录失败：{e}")))?;
    Ok(Json(json!({ "path": body.path })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFolderBody {
    path: String,
    #[serde(default)]
    force: bool,
}

/// 删文件夹：非空且未 force 时返回 `needsConfirm`（由调用方二次确认后带 force 重发）。
pub async fn delete_folder(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<DeleteFolderBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    if body.path.is_empty() {
        return Err(bad_request("不能删除空间根目录"));
    }
    let path = root.join(&body.path, false).map_err(join_err)?;
    if !path.is_dir() {
        return Err(not_found("文件夹不存在"));
    }
    let empty = std::fs::read_dir(&path).map(|mut d| d.next().is_none()).unwrap_or(false);
    if !empty && !body.force {
        return Ok(Json(json!({ "needsConfirm": true })));
    }
    let result = if empty { std::fs::remove_dir(&path) } else { std::fs::remove_dir_all(&path) };
    result.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败：{e}")))?;
    tracing::info!(space_id = %space_id, path = %body.path, "文件夹删除");
    Ok(Json(json!({ "deleted": true })))
}
