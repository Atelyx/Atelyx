//! 空间内容 CRUD（真源 = 服务器文件树）：树 / 读 / 写 / 重命名（跨目录移动）/ 复制 /
//! 建文件夹 / 删文件 / 删文件夹。所有端点先过成员校验，写操作 editor 与 owner 均可。
//! 写 = 原子写（防半截文件）；路径安全由 `fsops::SpaceRoot` 统一校验（防穿越/越界）。

use std::borrow::Cow;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::fsops::{
    atomic_write, file_mtime_secs, read_dir_filtered, JoinError, SpaceRoot, RESERVED_MEDIA_DIR,
};
use crate::state::{max_file_bytes, ServerState, ROLE_EDITOR, ROLE_OWNER};
use crate::{ApiError, ApiResult};

/// 单文件字节上限（局域网传输与内存呈现的合理上限；过大单文件拖累服务端内存与同步）。
pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;

/// 成员校验 + 内容根。读端点用这里（任意成员均可读）。
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

/// 成员校验 + 内容根 + 写角色（owner/editor）。写端点统一走这里，只读角色（viewer）被拒。
pub(crate) fn write_root(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<SpaceRoot, ApiError> {
    state.read(|p| {
        let space = p
            .spaces
            .iter()
            .find(|s| s.id == space_id)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "空间不存在".to_string()))?;
        let member = space
            .members
            .iter()
            .find(|m| m.user_id == user.user_id)
            .ok_or_else(|| ApiError(StatusCode::FORBIDDEN, "不是该空间成员".to_string()))?;
        if member.role != ROLE_OWNER && member.role != ROLE_EDITOR {
            return Err(ApiError(StatusCode::FORBIDDEN, "角色权限不足：仅 owner/editor 可写".to_string()));
        }
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

pub(crate) fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.to_string())
}

pub(crate) fn join_err(e: JoinError) -> ApiError {
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
    // 团队层排除文件夹：任意层级同名目录不进树（与个人仓库 `excludeFolders` 同语义）
    let exclude = crate::meta::space_exclusions(state.data_dir(), &space_id);
    let nodes = list_tree_in(&root.0, "", &exclude).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
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

fn list_tree_in(root: &std::path::Path, rel: &str, exclude_folders: &[String]) -> Result<Vec<TreeNode>, String> {
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    let mut nodes: Vec<TreeNode> = vec![];
    for (child_rel, is_dir) in read_dir_filtered(&dir, rel, exclude_folders)? {
        let path = root.join(&child_rel);
        let children = if is_dir { list_tree_in(root, &child_rel, exclude_folders)? } else { vec![] };
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
    /// 缺省 = 文本（非 UTF-8 字节按替换字符容错）；`base64` = 原始字节按标准 base64 返回。
    /// 附件/媒体等二进制内容必须走 base64，文本读会损坏字节。
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBody {
    path: String,
    content: String,
    /// 缺省 = content 按文本落盘；`base64` = content 为标准 base64，服务端解码后按字节落盘
    /// （附件/媒体二进制写入口）。字节限额与乐观锁均按解码后的磁盘字节数计。
    #[serde(default)]
    encoding: Option<String>,
    /// 乐观锁基准（上次读到的 updatedAt = 服务端内容版本号）；提供且服务端版本更新 → 409
    /// 拒绝覆盖（防多端/外部同步静默丢更新）；缺省 = 无条件写（笔记整文件保存语义不变）。
    /// 判据用版本号而非裸 mtime：mtime 只有整秒精度，同秒内他人写入后 mtime == 基准会漏判
    /// （见 state::content_version）。
    #[serde(default)]
    base_updated_at: Option<i64>,
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
    // updatedAt 返回内容版本号（与写/补丁的冲突判据同源闭环），不是裸 mtime
    let version = state.content_version(&space_id, &query.path, &path);
    match query.encoding.as_deref() {
        None => {
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Json(json!({ "content": content, "updatedAt": version })))
        }
        Some("base64") => Ok(Json(json!({
            "content": BASE64.encode(&bytes),
            "updatedAt": version,
            "encoding": "base64",
        }))),
        Some(other) => Err(bad_request(&format!("不支持的编码：{other}"))),
    }
}

/// 写文本文件（原子写；自动建父目录；路径已存在目录则拒绝）。
/// 乐观锁基准过期返回 409（body 带当前 updatedAt）。与补丁端点共用每路径写锁，
/// 同路径并发写严格按到达序落地。
pub async fn write_file(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<WriteBody>,
) -> Result<Response, ApiError> {
    let limit = max_file_bytes();
    // base64 写在此解码：限额按解码后的磁盘字节数计（与文本写同套语义），解码失败明确拒绝不静默
    let bytes: Cow<'_, [u8]> = match body.encoding.as_deref() {
        None => Cow::Borrowed(body.content.as_bytes()),
        Some("base64") => Cow::Owned(
            BASE64
                .decode(body.content.as_bytes())
                .map_err(|_| bad_request("content 不是合法 base64"))?,
        ),
        Some(other) => return Err(bad_request(&format!("不支持的编码：{other}"))),
    };
    if bytes.len() > limit {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("文件过大：单文件上限 {}MB", limit / 1024 / 1024),
        ));
    }
    let root = write_root(&state, &space_id, &user)?;
    let path = root.join(&body.path, true).map_err(join_err)?;
    if path.is_dir() {
        return Err(bad_request("目标已是目录"));
    }
    // 同路径写串行化：锁内完成乐观锁判定与落盘（判定基于锁内的磁盘事实）
    let _lock = state.path_lock(&space_id, &body.path).await;
    let current = state.content_version(&space_id, &body.path, &path);
    if let Some(base) = body.base_updated_at {
        if path.exists() && base < current {
            return Ok(crate::patches::conflict(current));
        }
    }
    atomic_write(&path, &bytes)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let version = state.advance_content_version(&space_id, &body.path, &path, current);
    tracing::info!(space_id = %space_id, path = %body.path, bytes = bytes.len(), "内容写入");
    Ok(Json(json!({ "updatedAt": version })).into_response())
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
    let root = write_root(&state, &space_id, &user)?;
    let from = root.join(&body.old_path, false).map_err(join_err)?;
    if !from.exists() {
        return Err(not_found("源路径不存在"));
    }
    // 目录判定须在改名之前取（改完旧路径已不存在）
    let from_is_dir = from.is_dir();
    let to = root.join(&body.new_path, true).map_err(join_err)?;
    if to.exists() {
        return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{}", body.new_path)));
    }
    std::fs::rename(&from, &to)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("重命名失败：{e}")))?;
    // 历史侧文件随内容改名 / 移动迁移：服务端知道每次改名，比客户端 watcher 可靠；
    // 迁移在每侧文件锁内进行（与历史追加互斥，防旧名侧文件被并发重建）；
    // 失败静默（历史尽力而为，不阻塞重命名主流程）
    crate::history::remap_after_rename(&state, &space_id, &root, &body.old_path, &body.new_path, from_is_dir).await;
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
    let root = write_root(&state, &space_id, &user)?;
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
    let root = write_root(&state, &space_id, &user)?;
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
    let root = write_root(&state, &space_id, &user)?;
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
/// 两者都带 `itemCount` 递归条目数（含隐藏项，与客户端 `count_dir_items` 同口径）——
/// 删除确认弹窗按它显示「包含 N 个文件/文件夹」。
pub async fn delete_folder(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<DeleteFolderBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = write_root(&state, &space_id, &user)?;
    if body.path.is_empty() {
        return Err(bad_request("不能删除空间根目录"));
    }
    let path = root.join(&body.path, false).map_err(join_err)?;
    if !path.is_dir() {
        return Err(not_found("文件夹不存在"));
    }
    let item_count = count_dir_items(&path);
    let empty = item_count == 0;
    if !empty && !body.force {
        return Ok(Json(json!({ "needsConfirm": true, "itemCount": item_count })));
    }
    let result = if empty { std::fs::remove_dir(&path) } else { std::fs::remove_dir_all(&path) };
    result.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败：{e}")))?;
    tracing::info!(space_id = %space_id, path = %body.path, "文件夹删除");
    Ok(Json(json!({ "deleted": true, "itemCount": item_count })))
}

/// 递归统计目录内条目数（含隐藏文件与子目录；删除确认弹窗文案用）。
/// 遍历失败按已数到的部分返回（确认文案尽力而为，不因个别不可读子项挡住删除流程）。
fn count_dir_items(dir: &std::path::Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in rd.flatten() {
        count += 1;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            count += count_dir_items(&entry.path());
        }
    }
    count
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaListQuery {
    /// 缺省 = 枚举整个保留目录
    #[serde(default)]
    path: Option<String>,
}

/// 递归枚举目录下全部文件（相对 `rel` 前缀的 `/` 分隔路径 + 字节大小）。
/// 不做隐藏项 / `.tmp` 过滤：保留目录整体已在内容树之外，附件回收需要看到其中全部文件
/// （含客户端以隐藏段命名的中间产物）；不跟随目录符号链接（`file_type` 不解引用，与树遍历同口径）。
fn list_media_files(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, u64)>) -> Result<(), String> {
    let mut entries: Vec<(String, bool)> = vec![];
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((name, is_dir));
    }
    // 按名排序保证顺序确定（read_dir 顺序由文件系统决定）
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, is_dir) in entries {
        let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if is_dir {
            list_media_files(&dir.join(&name), &child_rel, out)?;
        } else {
            let size = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
            out.push((child_rel, size));
        }
    }
    Ok(())
}

/// 枚举保留媒体目录（`.space-media/`）下的文件（递归、只列文件），供客户端临时附件回收。
/// 成员即可读（与内容读一致）。`path` 省略 = 枚举整个保留目录；提供时解析后必须仍落在
/// 保留目录内（首段前缀先判 + `SpaceRoot::join` 安全校验 + canonicalize 前缀复核，
/// 防穿越与符号链接逃逸）；空目录 / 不存在返回空 entries。
pub async fn media_list(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<MediaListQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let media_root = root.0.join(RESERVED_MEDIA_DIR);
    let list_dir = match query.path.as_deref() {
        None | Some("") => media_root.clone(),
        Some(rel) => {
            // 逐段前缀判定在 join 之前：不存在的路径无法 canonicalize，
            // 首段不等于保留目录名即越出保留区（含 `\` 分隔与保留目录名的相似前缀）
            if rel.split('/').next() != Some(RESERVED_MEDIA_DIR) {
                return Err(bad_request(&format!("路径越出保留媒体目录：{rel}")));
            }
            match root.join(rel, false) {
                Ok(joined) => joined,
                // 保留目录内不存在的路径与空目录同口径：空 entries
                Err(JoinError::NotFound(_)) => return Ok(Json(json!({ "entries": [] }))),
                Err(e) => return Err(join_err(e)),
            }
        }
    };
    if !list_dir.is_dir() {
        return Ok(Json(json!({ "entries": [] })));
    }
    // 已存在的枚举点解出真实落点后必须仍在保留目录内（防符号链接把枚举点引出保留区）
    let media_canon = dunce::canonicalize(&media_root)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("保留目录不可达：{e}")))?;
    let list_canon = dunce::canonicalize(&list_dir)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("路径不可达：{e}")))?;
    if !list_canon.starts_with(&media_canon) {
        return Err(bad_request("路径越出保留媒体目录"));
    }
    let mut files: Vec<(String, u64)> = vec![];
    list_media_files(&list_dir, "", &mut files)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Json(json!({
        "entries": files
            .iter()
            .map(|(name, size)| json!({ "name": name, "size": size }))
            .collect::<Vec<_>>(),
    })))
}
