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
use sha2::{Digest, Sha256};

use crate::access::{join_err, member_root, write_root};
use crate::auth::AuthUser;
use crate::fsops::{
    atomic_write, blocking, file_mtime_secs, read_dir_filtered, read_dir_including_hidden, JoinError,
    SpaceRoot, RESERVED_MEDIA_DIR,
};
use crate::state::{bad_request, max_file_bytes, not_found, ServerState};
use crate::{ApiError, ApiResult};

/// 单文件字节上限（局域网传输与内存呈现的合理上限；过大单文件拖累服务端内存与同步）。
pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;

/// 目录递归深度上限：异常目录结构（超深嵌套 / 目录符号链接环）不得把递归撑成栈溢出。
pub(crate) const MAX_TREE_DEPTH: usize = 64;

/// 内容版本号（条件写的 If-Match 值）：文件字节的 sha256 十六进制。
/// 与 mtime 不同，同一内容恒得同一版本、覆盖写后必变，重启与多副本下也成立。
fn content_version(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 文件树查询参数。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TreeQuery {
    /// 是否列出 `.` 开头的隐藏项（插件仓库能力面；缺省 = 文件面板口径）。
    #[serde(default, deserialize_with = "de_flag")]
    include_hidden: bool,
    /// 只列该子目录（仓库相对路径；缺省 = 仓库根）。
    #[serde(default)]
    dir: Option<String>,
}

/// 查询串布尔：兼容 `1`/`true`（反序列化默认只认 `true`/`false`）。
fn de_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    match raw.as_str() {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        other => Err(serde::de::Error::custom(format!("非法布尔值：{other}"))),
    }
}

/// 全仓库文件树（嵌套；隐藏目录与 `.tmp` 原子写产物默认不出现，与客户端文件面板同过滤）。
/// `includeHidden=1` 时放行隐藏项（`.tmp` 仍挡）；`dir` 只列该子目录（插件按目录列举时
/// 不必拉整棵树）。
pub async fn tree(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<TreeQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    // 团队层排除文件夹：任意层级同名目录不进树（与个人仓库 `excludeFolders` 同语义）
    let exclude = crate::meta::space_exclusions(&state, &space_id).await;
    let dir = query.dir.unwrap_or_default();
    let root_path = root.0.clone();
    let include_hidden = query.include_hidden;
    let check_dir = dir.clone();
    let root_for_check = root_path.clone();
    let nodes = blocking("文件树读取", move || {
        if !check_dir.is_empty() {
            let path = SpaceRoot(root_for_check.clone()).join(&check_dir, false).map_err(join_err)?;
            if !path.is_dir() {
                return Err(not_found("目录不存在"));
            }
        }
        list_tree_in(&root_for_check, &check_dir, &exclude, include_hidden)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))
    })
    .await?;
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

fn list_tree_in(
    root: &std::path::Path,
    rel: &str,
    exclude_folders: &[String],
    include_hidden: bool,
) -> Result<Vec<TreeNode>, String> {
    list_tree_at(root, rel, exclude_folders, include_hidden, 0)
}

fn list_tree_at(
    root: &std::path::Path,
    rel: &str,
    exclude_folders: &[String],
    include_hidden: bool,
    depth: usize,
) -> Result<Vec<TreeNode>, String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!("目录层级超过上限 {MAX_TREE_DEPTH}：{rel}"));
    }
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    let entries = match include_hidden {
        true => read_dir_including_hidden(&dir, rel, exclude_folders),
        false => read_dir_filtered(&dir, rel, exclude_folders),
    }?;
    let mut nodes: Vec<TreeNode> = vec![];
    for (child_rel, is_dir) in entries {
        let path = root.join(&child_rel);
        let children = if is_dir {
            list_tree_at(root, &child_rel, exclude_folders, include_hidden, depth + 1)?
        } else {
            vec![]
        };
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
    /// （附件/媒体二进制写入口）。字节限额按解码后的磁盘字节数计。
    #[serde(default)]
    encoding: Option<String>,
    /// 只允许新建：写锁内发现目标已存在即 409，不覆盖。缺省 = 覆盖写。
    /// 供「读到不存在 → 建空文件 → 补丁上行」的调用方消除读到写之间的窗口。
    #[serde(default)]
    create_only: bool,
    /// 条件写：调用方读到的内容版本（空串 = 期待目标仍不存在）。写锁内版本不符即 409，
    /// 挡住「读到写之间他人已落盘」的整文覆盖；缺省 = 最后写入者胜（旧客户端兼容）。
    #[serde(default)]
    if_match: Option<String>,
}

/// 读文本文件（非 UTF-8 字节按替换字符容错）。
pub async fn read_file(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let encoding = query.encoding.clone();
    let rel = query.path.clone();
    let (bytes, updated_at) = blocking("文件读取", move || {
        let path = root.join(&rel, false).map_err(join_err)?;
        if !path.is_file() {
            return Err(not_found("文件不存在"));
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("读取失败：{e}")))?;
        // updatedAt = 文件 mtime 秒（元数据；不再参与任何冲突判定）
        Ok((bytes, file_mtime_secs(&path)))
    })
    .await?;
    // 版本随内容一并回给调用方：条件写把它当 If-Match，挡住读到写之间的整文覆盖
    let version = content_version(&bytes);
    match encoding.as_deref() {
        None => {
            let content = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Json(json!({ "content": content, "updatedAt": updated_at, "version": version })))
        }
        Some("base64") => Ok(Json(json!({
            "content": BASE64.encode(&bytes),
            "updatedAt": updated_at,
            "encoding": "base64",
            "version": version,
        }))),
        Some(other) => Err(bad_request(&format!("不支持的编码：{other}"))),
    }
}

/// 路径存在性（插件仓库能力面：隐藏段可见；只判文件，不读内容）。
pub async fn stat_path(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<FilePathQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    if query.path.is_empty() {
        return Err(bad_request("路径不能为空"));
    }
    let root = member_root(&state, &space_id, &user)?;
    let rel = query.path.clone();
    let exists = blocking("路径检查", move || match root.join(&rel, false) {
        Ok(path) => Ok(path.is_file()),
        // 父目录不存在 = 文件不存在（与读取同口径）
        Err(JoinError::NotFound(_)) => Ok(false),
        Err(e @ JoinError::Invalid(_)) => Err(bad_request(&e.to_string())),
    })
    .await?;
    Ok(Json(json!({ "exists": exists })))
}

/// 写文本文件（原子写；自动建父目录；路径已存在目录则拒绝）。与补丁端点共用每路径写锁，
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
    let join_root = root.0.clone();
    let join_rel = body.path.clone();
    let path = blocking("路径检查", move || {
        let path = SpaceRoot(join_root).join(&join_rel, true).map_err(join_err)?;
        if path.is_dir() {
            return Err(bad_request("目标已是目录"));
        }
        Ok(path)
    })
    .await?;
    // 同路径写串行化：锁内完成落盘，同路径并发写严格按到达序执行。
    // 取锁前记录存在性、锁内复查：等待锁期间 rename(P→Q)/delete_file/补丁漂移已移走或
    // 删除 P 时无条件写会把旧路径复活（.atb/.atlx 同 id 双文件）——覆盖写（取锁前已存在）
    // 被拒，新建（取锁前不存在）不受影响。delete_folder 与本端点不互斥（目录删除无路径锁
    // 协同）：「删目录 → 并发新建目录内文件重建目录」的复活仍可达，但新建分支会广播
    // fs-changed，各端重拉树后收敛到真实状态，不产生同 id 双文件
    let pre_exists = blocking("路径检查", {
        let p = path.clone();
        move || Ok(p.is_file())
    })
    .await?;
    let _lock = state.path_lock(&space_id, &body.path).await;
    let write_path = path.clone();
    let rel = body.path.clone();
    let create_only = body.create_only;
    let if_match = body.if_match.clone();
    let payload = bytes.to_vec();
    blocking("文件写入", move || {
        if pre_exists && !write_path.is_file() {
            return Err(not_found("文件已被改名或删除，写入被拒绝"));
        }
        // 只新建：存在性判定与落盘同在写锁内，调用方无需再赌「读到不存在」的结论
        if create_only && write_path.is_file() {
            return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{rel}")));
        }
        // 条件写：版本判定与落盘同在写锁内。读到的版本之后若有人落盘，本次整文写会覆盖
        // 对方内容，改为让调用方拿到可读的冲突再重取合并；缺省版本请求维持最后写入者胜
        if let Some(expected) = if_match.as_deref() {
            let current = if write_path.is_file() {
                let current = std::fs::read(&write_path).map_err(|e| {
                    ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("读取失败：{e}"))
                })?;
                content_version(&current)
            } else {
                String::new()
            };
            if expected != current {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "文件已被他人修改，本次写入未落盘，请刷新后重试".to_string(),
                ));
            }
        }
        atomic_write(&write_path, &payload).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))
    })
    .await?;
    // 新建文件 = 结构变更：广播在线协作者刷新文件树（覆盖写不广播，内容级各有专用通道）
    if !pre_exists {
        crate::ws::broadcast_fs_changed(&state.hub(), &format!("space:{space_id}"));
    }
    let stat_path = path.clone();
    let updated_at = blocking("时间戳读取", move || Ok(file_mtime_secs(&stat_path))).await?;
    tracing::info!(space_id = %space_id, path = %body.path, bytes = bytes.len(), "内容写入");
    // `created` 供客户端区分新建 / 覆盖（前者另发结构变更广播）
    Ok(Json(json!({ "updatedAt": updated_at, "created": !pre_exists })).into_response())
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
    let (from, to) = blocking("路径检查", {
        let root = root.0.clone();
        let (old_path, new_path) = (body.old_path.clone(), body.new_path.clone());
        move || {
            let from = SpaceRoot(root.clone()).join(&old_path, false).map_err(join_err)?;
            let to = SpaceRoot(root).join(&new_path, true).map_err(join_err)?;
            Ok((from, to))
        }
    })
    .await?;
    // 同路径改名 = 无操作，必须早于取锁：路径锁按 key 复用同一把 tokio Mutex（不可重入），
    // 对同一路径取两次会永久挂起，且挂起时仍持结构锁，连带阻塞该空间全部改名与补丁
    if body.old_path == body.new_path {
        let existing = from.clone();
        let exists = blocking("路径检查", move || Ok(existing.exists())).await?;
        if !exists {
            return Err(not_found("源路径不存在"));
        }
        return Ok(Json(json!({})));
    }
    // 结构锁 → 双路径锁（按 key 字典序取锁防死锁）：检查到 fs::rename 全程在锁内。
    // 补丁端点的「锁内读 → 合并 → 写」窗口若与改名互不互斥，在途补丁会在改名后落盘，
    // 把旧路径文件原样重建（同 id 双文件，旧文件名复活）；结构锁同时消解补丁漂移场景下
    // 跨路径写/删与改名的取锁顺序问题。
    let _structure = state.structure_lock(&space_id).await;
    let from_is_dir;
    {
        let (lock_from, lock_to) = if body.old_path <= body.new_path {
            (
                state.path_lock(&space_id, &body.old_path).await,
                state.path_lock(&space_id, &body.new_path).await,
            )
        } else {
            (
                state.path_lock(&space_id, &body.new_path).await,
                state.path_lock(&space_id, &body.old_path).await,
            )
        };
        // 检查与改名同在锁内：整段放进阻塞池，锁内不占 async 执行器
        let (move_from, move_to) = (from.clone(), to.clone());
        let new_rel = body.new_path.clone();
        let (renamed, is_dir) = blocking("重命名", move || {
            if !move_from.exists() {
                return Err(not_found("源路径不存在"));
            }
            // 目录判定须在改名之前取（改完旧路径已不存在）
            let is_dir = move_from.is_dir();
            if move_to.exists() {
                return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{new_rel}")));
            }
            std::fs::rename(&move_from, &move_to).map_err(|e| {
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("重命名失败：{e}"))
            })?;
            // 单文件改名换了主名：内嵌标题随文件名一并改写（目录改名不动文件名，无需处理）。
            // 锁内完成，与补丁的读改写互斥；改写失败只记日志——改名本身已落地，不因此回滚。
            if !is_dir {
                crate::patches::retitle_after_rename(&move_to);
            }
            Ok((move_to, is_dir))
        })
        .await?;
        from_is_dir = is_dir;
        drop(renamed);
        drop(lock_to);
        drop(lock_from);
    }
    // 历史侧文件随内容改名 / 移动迁移：服务端知道每次改名，比客户端 watcher 可靠；
    // 迁移在每侧文件锁内进行（与历史追加互斥，防旧名侧文件被并发重建）；
    // 失败静默（历史尽力而为，不阻塞重命名主流程）
    crate::history::remap_after_rename(&state, &space_id, &root, &body.old_path, &body.new_path, from_is_dir).await;
    // 落地后向空间房间广播改名帧（结构锁内发出）；失败只记日志不回滚——真源已落盘
    crate::ws::broadcast_rename(&state.hub(), &format!("space:{space_id}"), &body.old_path, &body.new_path);
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
    // 结构锁 → 目标路径锁：与补丁（会写新删旧）、改名（双路径）互斥，防复制目标被并发改写；
    // 目标存在检查在锁内完成，两个并发 copy 不会都通过检查后互相覆盖
    let _structure = state.structure_lock(&space_id).await;
    let _lock = state.path_lock(&space_id, &body.to_path).await;
    // 路径解析、存在检查与递归复制都是同步 IO（整目录可达秒级），持锁期间整段放进阻塞池
    let (space_root, from_path, to_path) = (root.0.clone(), body.from_path.clone(), body.to_path.clone());
    blocking("复制", move || {
        let root = SpaceRoot(space_root);
        let from = root.join(&from_path, false).map_err(join_err)?;
        if !from.exists() {
            return Err(not_found("源路径不存在"));
        }
        let to = root.join(&to_path, true).map_err(join_err)?;
        if to.exists() {
            return Err(ApiError(StatusCode::CONFLICT, format!("目标已存在：{to_path}")));
        }
        copy_recursive(&from, &to, 0)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("复制失败：{e}")))
    })
    .await?;
    tracing::info!(space_id = %space_id, from = %body.from_path, to = %body.to_path, "内容复制");
    crate::ws::broadcast_fs_changed(&state.hub(), &format!("space:{space_id}"));
    Ok(Json(json!({})))
}

fn copy_recursive(from: &std::path::Path, to: &std::path::Path, depth: usize) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!("目录层级超过上限 {MAX_TREE_DEPTH}：{}", from.display()));
    }
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()), depth + 1)?;
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
    let join_root = root.0.clone();
    let join_rel = query.path.clone();
    let path = blocking("路径检查", move || SpaceRoot(join_root).join(&join_rel, false).map_err(join_err)).await?;
    // 结构锁 → 被删路径锁：补丁的「锁内读 → 合并 → 写」若与删除互不互斥，
    // 在途补丁会把已删文件原样写回（删除被静默复活）；结构锁同时消解补丁按 title
    // 漂移到第二条路径的写/删与删除的取锁顺序问题。存在检查在锁内完成。
    let _structure = state.structure_lock(&space_id).await;
    let _lock = state.path_lock(&space_id, &query.path).await;
    let remove_path = path.clone();
    blocking("文件删除", move || {
        if !remove_path.exists() {
            return Err(not_found("文件不存在"));
        }
        if remove_path.is_dir() {
            return Err(bad_request("目标是目录，请走文件夹删除"));
        }
        std::fs::remove_file(&remove_path)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败：{e}")))
    })
    .await?;
    crate::ws::broadcast_fs_changed(&state.hub(), &format!("space:{space_id}"));
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
    let space_root = root.0.clone();
    let rel = body.path.clone();
    let conflict_rel = body.path.clone();
    blocking("创建目录", move || {
        let path = SpaceRoot(space_root).join(&rel, true).map_err(join_err)?;
        if path.exists() {
            return Err(ApiError(StatusCode::CONFLICT, format!("路径已存在：{conflict_rel}")));
        }
        std::fs::create_dir_all(&path)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("创建目录失败：{e}")))
    })
    .await?;
    crate::ws::broadcast_fs_changed(&state.hub(), &format!("space:{space_id}"));
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
    // 结构锁：目录树内逐路径取锁不可行，改为与补丁（会写新删旧）、改名（双路径）
    // 全量互斥——这两类是「在途写把已删条目复活」的来源；write_file 只持单路径锁
    // 且建父目录在取锁前，与删除并发可能得到 ENOENT 500（安全失败），无复活/损坏
    let _structure = state.structure_lock(&space_id).await;
    let space_root = root.0.clone();
    let rel = body.path.clone();
    let force = body.force;
    // 条目清点与删除都是同步递归 IO（大目录可达秒级），持结构锁期间放进阻塞池：
    // 不占 async 执行器，也不让该空间补丁/改名的等待方饿死
    let (item_count, removed) = blocking("目录删除", move || {
        let path = SpaceRoot(space_root).join(&rel, false).map_err(join_err)?;
        if !path.is_dir() {
            return Err(not_found("文件夹不存在"));
        }
        let item_count = count_dir_items(&path, 0);
        // 非空且未 force：只回条目数供调用方二次确认，不动磁盘
        if item_count > 0 && !force {
            return Ok((item_count, false));
        }
        let result =
            if item_count == 0 { std::fs::remove_dir(&path) } else { std::fs::remove_dir_all(&path) };
        result.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败：{e}")))?;
        Ok((item_count, true))
    })
    .await?;
    if !removed {
        return Ok(Json(json!({ "needsConfirm": true, "itemCount": item_count })));
    }
    crate::ws::broadcast_fs_changed(&state.hub(), &format!("space:{space_id}"));
    tracing::info!(space_id = %space_id, path = %body.path, "文件夹删除");
    Ok(Json(json!({ "deleted": true, "itemCount": item_count })))
}

/// 递归统计目录内条目数（含隐藏文件与子目录；删除确认弹窗文案用）。
/// 遍历失败按已数到的部分返回（确认文案尽力而为，不因个别不可读子项挡住删除流程）。
fn count_dir_items(dir: &std::path::Path, depth: usize) -> usize {
    // 超限分支不再下探（只数当前层），确认文案的条目数按已数到的部分返回
    if depth > MAX_TREE_DEPTH {
        return 0;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in rd.flatten() {
        count += 1;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            count += count_dir_items(&entry.path(), depth + 1);
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
fn list_media_files(
    dir: &std::path::Path,
    rel: &str,
    out: &mut Vec<(String, u64)>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!("目录层级超过上限 {MAX_TREE_DEPTH}：{rel}"));
    }
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
            list_media_files(&dir.join(&name), &child_rel, out, depth + 1)?;
        } else {
            let size = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
            out.push((child_rel, size));
        }
    }
    Ok(())
}

/// 枚举保留媒体目录（`.atelyx/temp/`，与个人仓库同构的临时区根）下的文件（递归、只列文件），供客户端临时附件回收。
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
    let (space_root, media_root) = (root.0.clone(), root.0.join(RESERVED_MEDIA_DIR));
    let requested = query.path.clone();
    // 路径校验、canonicalize 复核与递归枚举都是同步 IO，整段放进阻塞池
    let files = blocking("保留目录枚举", move || {
        let root = SpaceRoot(space_root);
        let list_dir = match requested.as_deref() {
            None | Some("") => media_root.clone(),
            Some(rel) => {
                // 逐段前缀判定在 join 之前：不存在的路径无法 canonicalize，路径不落在保留目录
                // 内（含 `\` 分隔与保留目录名的相似前缀，如 `.atelyx/temp-x`）即越出保留区
                if !(rel == RESERVED_MEDIA_DIR || rel.starts_with(&format!("{RESERVED_MEDIA_DIR}/"))) {
                    return Err(bad_request(&format!("路径越出保留媒体目录：{rel}")));
                }
                match root.join(rel, false) {
                    Ok(joined) => joined,
                    // 保留目录内不存在的路径与空目录同口径：空 entries
                    Err(JoinError::NotFound(_)) => return Ok(Vec::new()),
                    Err(e) => return Err(join_err(e)),
                }
            }
        };
        if !list_dir.is_dir() {
            return Ok(Vec::new());
        }
        // 已存在的枚举点解出真实落点后必须仍在保留目录内（防符号链接把枚举点引出保留区）
        let media_canon = dunce::canonicalize(&media_root).map_err(|e| {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("保留目录不可达：{e}"))
        })?;
        let list_canon = dunce::canonicalize(&list_dir)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("路径不可达：{e}")))?;
        if !list_canon.starts_with(&media_canon) {
            return Err(bad_request("路径越出保留媒体目录"));
        }
        let mut files: Vec<(String, u64)> = vec![];
        list_media_files(&list_dir, "", &mut files, 0)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    })
    .await?;
    Ok(Json(json!({
        "entries": files
            .iter()
            .map(|(name, size)| json!({ "name": name, "size": size }))
            .collect::<Vec<_>>(),
    })))
}
