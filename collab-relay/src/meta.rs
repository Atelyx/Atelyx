//! 空间配置落点（键值元数据）：space 与 user 两种 scope。
//!
//! 存储：按键分文件散列在数据目录下 `<data>/meta/<space_id>/{space,user/<user_id>}/<key 编码>.json`，
//! 文件内容即 value 原文（UTF-8 文本，非 JSON 包装）；扩展名 `.json` 仅为与数据目录其他文件一致。
//! 原子写复用 fsops::atomic_write（temp + rename），避免半截文件；目录不存在即空 map。
//! 权限：space scope 任意成员可读，仅 owner/editor 可写；user scope 任意成员可读写自己那份。
//! 非成员一律拒绝（与内容端点同口径）。

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::fsops::{atomic_write, walk_files};
use crate::state::{max_meta_value_bytes, ServerState};
use crate::spaces::{require_role, space_of, ALL_ROLES};
use crate::{ApiError, ApiResult};

fn bad_request(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_string())
}

fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.to_string())
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// 要求调用方是该空间成员（任意角色；404 = 空间不存在，不区分无权限防探测）。
fn require_member(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<(), ApiError> {
    state.read(|p| {
        let space = space_of(p, space_id)?;
        require_role(space, &user.user_id, ALL_ROLES)?;
        Ok(())
    })
}

/// 要求成员角色在写清单内（owner/editor）；viewer 返回明确错误。
fn require_writer(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<(), ApiError> {
    state.read(|p| {
        let space = space_of(p, space_id)?;
        require_role(space, &user.user_id, &[crate::state::ROLE_OWNER, crate::state::ROLE_EDITOR])?;
        Ok(())
    })
}

/// Windows 保留设备名段（大小写不敏感）：这类名字作文件名时系统会做保留名处理，
/// key → 磁盘路径的映射会与回读错位。
const WINDOWS_RESERVED_SEGS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 校验 key：允许字母数字、`-`、`_`、`.`、`/`（层级）；逐段查空段 / `.` / `..` / 以 `.` 开头 /
/// 以 `.` 结尾 / Windows 保留设备名 / 非法字符。
/// 防路径穿越：段规则与内容路径校验同口径（拒绝 `..` / 绝对路径段 / 隐藏段）；
/// 尾点与保留名是 Windows 文件名变形（尾点被剥离、保留名被改写），会让回读与原 key 错位。
fn validate_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() {
        return Err(bad_request("键不能为空"));
    }
    if key.starts_with('/') || key.ends_with('/') {
        return Err(bad_request("键不能以 / 开头或结尾"));
    }
    for seg in key.split('/') {
        if seg.is_empty() {
            return Err(bad_request("键含空段（连续 /）"));
        }
        if seg == "." || seg == ".." {
            return Err(bad_request("键含非法段：. 或 .."));
        }
        if seg.starts_with('.') {
            return Err(bad_request("键段不能以 . 开头（隐藏段）"));
        }
        if seg.ends_with('.') {
            return Err(bad_request("键段不能以 . 结尾（Windows 会剥离尾点致回读错位）"));
        }
        if WINDOWS_RESERVED_SEGS.contains(&seg.to_ascii_lowercase().as_str()) {
            return Err(bad_request(
                "键段不能使用 Windows 保留设备名（con/prn/aux/nul/com1-9/lpt1-9）",
            ));
        }
        if !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(bad_request("键含非法字符（仅允许字母数字 - _ . /）"));
        }
    }
    Ok(())
}

/// key → 磁盘路径：key 的分段直接映射为相对路径（与内容路径同等编码），末段加 `.json` 扩展名。
/// 末段以 `{seg}.json` 拼接（非 set_extension），避免 key 自带扩展名被替换导致回读错位。
fn key_to_path(root: &FsPath, key: &str) -> Result<PathBuf, ApiError> {
    validate_key(key)?;
    let mut path = root.to_path_buf();
    let segs: Vec<&str> = key.split('/').collect();
    for seg in &segs[..segs.len() - 1] {
        path.push(seg);
    }
    path.push(format!("{}.json", segs[segs.len() - 1]));
    Ok(path)
}

/// space scope 根目录。
fn space_scope_root(data_dir: &FsPath, space_id: &str) -> PathBuf {
    data_dir.join("meta").join(space_id).join("space")
}

/// user scope 根目录（按成员 id 隔离）。
fn user_scope_root(data_dir: &FsPath, space_id: &str, user_id: &str) -> PathBuf {
    data_dir.join("meta").join(space_id).join("user").join(user_id)
}

/// 读某 scope 全量键值（目录不存在 = 空 map）。遍历复用 fsops::walk_files（屏蔽隐藏项与 `.tmp` 临时件）。
fn list_scope(root: &FsPath) -> Result<HashMap<String, String>, ApiError> {
    let mut out: HashMap<String, String> = HashMap::new();
    if !root.exists() {
        return Ok(out);
    }
    let mut files: Vec<(String, i64)> = vec![];
    walk_files(root, "", &mut files).map_err(internal)?;
    for (rel, _) in files {
        // 文件均以为 .json 结尾；剥掉后缀回得原始 key（内容不强制 JSON，后缀仅作标记）
        let key = rel.strip_suffix(".json").unwrap_or(&rel);
        let value = std::fs::read_to_string(root.join(&rel)).map_err(internal)?;
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

/// 写单键：先建父目录，原子写 value 原文（temp + rename，不留半截文件）。
fn write_key(root: &FsPath, key: &str, value: &str) -> Result<(), ApiError> {
    let path = key_to_path(root, key)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| internal(format!("创建目录失败：{e}")))?;
    }
    atomic_write(&path, value.as_bytes()).map_err(internal)
}

/// 全量校验一批键值（key 合法性 + value 大小），任一不合规即拒绝、不落半态。
fn validate_values(values: &HashMap<String, String>) -> Result<(), ApiError> {
    let limit = max_meta_value_bytes();
    for (key, value) in values {
        validate_key(key)?;
        if value.as_bytes().len() > limit {
            return Err(bad_request(&format!(
                "配置值过大：单键上限 {}MB",
                limit / 1024 / 1024
            )));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaValuesBody {
    values: HashMap<String, String>,
}

/// meta 读端点查询参数：`key` 缺省 = 全量 `{values}`；带 `key` = 单键 `{value}`。
#[derive(Deserialize)]
pub struct MetaReadQuery {
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    scope: Option<String>,
    key: String,
}

#[derive(Deserialize)]
pub struct DeleteMeQuery {
    key: String,
}

// ===== space scope =====

/// 单键读：键名校验与写路径同口径；键不存在 404。返回 `{ "value": <v> }`。
fn read_single_key(root: &FsPath, key: &str) -> Result<serde_json::Value, ApiError> {
    let path = key_to_path(root, key)?;
    if !path.exists() {
        return Err(not_found("键不存在"));
    }
    let value = std::fs::read_to_string(&path).map_err(internal)?;
    Ok(json!({ "value": value }))
}

/// GET /api/spaces/{space_id}/meta — 任意成员可读；带 `?key=` 返回单键 `{value}`，否则全量 `{values}`。
pub async fn get_space_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<MetaReadQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_member(&state, &space_id, &user)?;
    let root = space_scope_root(state.data_dir(), &space_id);
    if let Some(key) = &query.key {
        return Ok(Json(read_single_key(&root, key)?));
    }
    let values = list_scope(&root)?;
    Ok(Json(json!({ "values": values })))
}

/// PATCH /api/spaces/{space_id}/meta — 按键合并写入；仅 owner/editor。
pub async fn patch_space_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<MetaValuesBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_writer(&state, &space_id, &user)?;
    validate_values(&body.values)?;
    let root = space_scope_root(state.data_dir(), &space_id);
    for (key, value) in &body.values {
        write_key(&root, key, value)?;
    }
    tracing::info!(space_id = %space_id, keys = body.values.len(), "空间元信息写入");
    Ok(Json(json!({ "updated": body.values.len() })))
}

/// DELETE /api/spaces/{space_id}/meta?scope=space&key=<key> — 删单键；仅 owner/editor。
pub async fn delete_space_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(scope) = &query.scope {
        if scope != "space" {
            return Err(bad_request("scope 仅支持 space"));
        }
    }
    require_writer(&state, &space_id, &user)?;
    validate_key(&query.key)?;
    let root = space_scope_root(state.data_dir(), &space_id);
    let path = key_to_path(&root, &query.key)?;
    if !path.exists() {
        return Err(not_found("键不存在"));
    }
    std::fs::remove_file(&path).map_err(|e| internal(format!("删除失败：{e}")))?;
    Ok(Json(json!({ "deleted": true })))
}

// ===== user scope（本人） =====

/// GET /api/spaces/{space_id}/meta/me — 本人键值（任意成员可读写自己那份）；
/// 带 `?key=` 返回单键 `{value}`，否则全量 `{values}`。
pub async fn get_user_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<MetaReadQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_member(&state, &space_id, &user)?;
    let root = user_scope_root(state.data_dir(), &space_id, &user.user_id);
    if let Some(key) = &query.key {
        return Ok(Json(read_single_key(&root, key)?));
    }
    let values = list_scope(&root)?;
    Ok(Json(json!({ "values": values })))
}

/// PATCH /api/spaces/{space_id}/meta/me — 合并写本人键值。
pub async fn patch_user_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<MetaValuesBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_member(&state, &space_id, &user)?;
    validate_values(&body.values)?;
    let root = user_scope_root(state.data_dir(), &space_id, &user.user_id);
    for (key, value) in &body.values {
        write_key(&root, key, value)?;
    }
    tracing::info!(space_id = %space_id, user_id = %user.user_id, keys = body.values.len(), "用户元信息写入");
    Ok(Json(json!({ "updated": body.values.len() })))
}

/// DELETE /api/spaces/{space_id}/meta/me?key=<key> — 删本人单键。
pub async fn delete_user_meta(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<DeleteMeQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_member(&state, &space_id, &user)?;
    validate_key(&query.key)?;
    let root = user_scope_root(state.data_dir(), &space_id, &user.user_id);
    let path = key_to_path(&root, &query.key)?;
    if !path.exists() {
        return Err(not_found("键不存在"));
    }
    std::fs::remove_file(&path).map_err(|e| internal(format!("删除失败：{e}")))?;
    Ok(Json(json!({ "deleted": true })))
}
