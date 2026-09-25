//! 服务器管理员端点：运行状态、全服用户与空间视图、重置密码与吊销会话。
//!
//! 管理员认定：用户数组首位（注册按序追加且无重排，首位即最早注册的账号），派生
//! 判定不落字段。管理员能力只覆盖账号处置与全服只读视图，不放宽空间内部的角色
//! 校验（owner/editor/viewer 照常生效）。账号处置仅「重置密码」与「吊销会话」，
//! 不提供删除/禁用账号与删除空间——数据处置须有完整的连带规则，当前服务端不做。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, AuthUser};
use crate::state::{Persisted, ServerState};
use crate::{ApiError, ApiResult};

/// 调用方是否为服务器管理员（用户数组首位 = 最早注册的账号）。
fn is_admin(p: &Persisted, user_id: &str) -> bool {
    p.users.first().map(|u| u.id == user_id).unwrap_or(false)
}

fn require_admin(state: &ServerState, user: &AuthUser) -> Result<(), ApiError> {
    state.read(|p| {
        if is_admin(p, &user.user_id) {
            Ok(())
        } else {
            Err(ApiError(StatusCode::FORBIDDEN, "需要服务器管理员权限".to_string()))
        }
    })
}

/// 运行状态（登录即可见）：版本、启动时刻、在线连接数、调用方是否管理员。
/// 不含用户/空间计数等构成信息——那些只走管理员端点，避免普通成员探测全服规模。
pub async fn server_status(
    State(state): State<ServerState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let admin = state.read(|p| is_admin(p, &user.user_id));
    // hub 锁与名册锁分开取（先后取，不嵌套）
    let online = state.hub().total_connections();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "startedAt": state.started_at(),
        "onlineConnections": online,
        "isAdmin": admin,
    })))
}

/// 全服用户列表（管理员）：账号信息 + 会话数 + 参与空间数。不含任何哈希与令牌材料。
pub async fn list_users(
    State(state): State<ServerState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &user)?;
    let users = state.read(|p| {
        p.users
            .iter()
            .map(|u| {
                json!({
                    "userId": u.id,
                    "username": u.username,
                    "displayName": u.display_name,
                    "createdAt": u.created_at,
                    "sessionCount": p.sessions.iter().filter(|s| s.user_id == u.id).count(),
                    "spaceCount": p.spaces.iter().filter(|s| s.members.iter().any(|m| m.user_id == u.id)).count(),
                    "isAdmin": is_admin(p, &u.id),
                })
            })
            .collect::<Vec<_>>()
    });
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetPasswordBody {
    new_password: String,
}

/// 重置任意用户密码（管理员）。新密码长度口径与注册一致；重置联动吊销该用户全部
/// 会话（实时频道连接一并断开）——旧凭据若继续有效，重置即失去意义。
pub async fn reset_password(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(user_id): Path<String>,
    Json(body): Json<ResetPasswordBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &user)?;
    let len = body.new_password.len();
    if !(auth::PASSWORD_MIN..=auth::PASSWORD_MAX).contains(&len) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("密码长度须在 {}–{} 字节之间", auth::PASSWORD_MIN, auth::PASSWORD_MAX),
        ));
    }
    let password_hash = auth::hash_password(&body.new_password)?;
    let revoked_sessions = state.mutate(|p| {
        let target = p
            .users
            .iter_mut()
            .find(|u| u.id == user_id)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "用户不存在".to_string()))?;
        target.password_hash = password_hash.clone();
        // 落库前收集被吊销的会话 id（retain 之后就无从查起）
        let ids: Vec<String> = p
            .sessions
            .iter()
            .filter(|s| s.user_id == user_id)
            .map(|s| s.id.clone())
            .collect();
        p.sessions.retain(|s| s.user_id != user_id);
        Ok(ids)
    })?;
    // 吊销只删会话记录，已建立的实时频道连接凭入房时的会话 id 主动断开
    let kicked = state.hub().kick_sessions(&revoked_sessions);
    tracing::info!(target_user = %user_id, sessions = revoked_sessions.len(), kicked, "管理员重置密码（联动吊销其全部会话）");
    Ok(Json(json!({})))
}

/// 吊销某用户的全部设备会话（管理员）：设备丢失 / 人员离场时踢下线，
/// 其实时频道连接一并断开。
pub async fn revoke_user_sessions(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(user_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &user)?;
    let (revoked, session_ids) = state.mutate(|p| {
        if !p.users.iter().any(|u| u.id == user_id) {
            return Err(ApiError(StatusCode::NOT_FOUND, "用户不存在".to_string()));
        }
        // 落库前收集被吊销的会话 id（retain 之后就无从查起）
        let ids: Vec<String> = p
            .sessions
            .iter()
            .filter(|s| s.user_id == user_id)
            .map(|s| s.id.clone())
            .collect();
        p.sessions.retain(|s| s.user_id != user_id);
        Ok((ids.len(), ids))
    })?;
    let kicked = state.hub().kick_sessions(&session_ids);
    tracing::info!(target_user = %user_id, revoked, kicked, "管理员吊销用户全部会话");
    Ok(Json(json!({ "revoked": revoked })))
}

/// 全部空间列表（管理员）：成员数、内容根与目录占用字节。
/// 目录占用是请求时实算的展示数据（目录遍历在名册锁外进行，不阻塞其他请求），
/// 失败按 0 计，不拖垮整个列表。
pub async fn list_spaces(
    State(state): State<ServerState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&state, &user)?;
    let rows = state.read(|p| {
        p.spaces
            .iter()
            .map(|s| {
                let root = match &s.root_path {
                    Some(path) => std::path::PathBuf::from(path),
                    None => state.default_space_root(&s.id),
                };
                let owner_username = p
                    .users
                    .iter()
                    .find(|u| u.id == s.owner_user_id)
                    .map(|u| u.username.clone())
                    .unwrap_or_default();
                (
                    s.id.clone(),
                    s.name.clone(),
                    s.owner_user_id.clone(),
                    owner_username,
                    s.members.len(),
                    s.created_at,
                    root,
                )
            })
            .collect::<Vec<_>>()
    });
    let spaces = rows
        .into_iter()
        .map(|(id, name, owner_user_id, owner_username, member_count, created_at, root)| {
            json!({
                "spaceId": id,
                "name": name,
                "ownerUserId": owner_user_id,
                "ownerUsername": owner_username,
                "memberCount": member_count,
                "createdAt": created_at,
                "rootPath": root.to_string_lossy(),
                "sizeBytes": dir_size_bytes(&root),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!(spaces)))
}

/// 目录磁盘占用（字节）。符号链接不跟随（防循环），单条目失败按 0 计。
fn dir_size_bytes(root: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            // DirEntry::metadata 不跟随符号链接
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(root, &mut total);
    total
}
