//! 账号与会话：开放注册 / 登录 / 登出 / 设备会话列表与吊销。
//!
//! 密码 argon2 哈希；令牌随机 256 位 hex，服务端只存 SHA-256 摘要。令牌经
//! `Authorization: Bearer` 传递；一次登录 = 一个设备会话，同账号多会话并存、可逐个吊销。
//! 日志只记用户名等元数据，密码与令牌一律不入日志。

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::{now_secs, random_hex, token_hash, ServerState, Session, User};
use crate::{ApiError, ApiResult};

const USERNAME_MAX: usize = 32;
pub(crate) const PASSWORD_MIN: usize = 6;
pub(crate) const PASSWORD_MAX: usize = 128;

/// 用户名：1–32 位 ASCII 字母 / 数字 / `_` / `-`（表名与 URL 片段友好，不含空白）。
fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= USERNAME_MAX
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("密码哈希失败：{e}")))
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

fn unauthorized() -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "未认证或会话已失效".to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    username: String,
    password: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
}

/// 创建用户 + 首个设备会话，返回令牌原文（仅此一次出现在响应中）。
async fn create_account(state: ServerState, body: Credentials) -> ApiResult<axum::response::Response> {
    if !valid_username(&body.username) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "用户名须为 1–32 位字母/数字/_/-".to_string()));
    }
    let password_len = body.password.len();
    if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&password_len) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("密码长度须在 {PASSWORD_MIN}–{PASSWORD_MAX} 字节之间"),
        ));
    }
    let display_name = body
        .display_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| body.username.clone())
        .chars()
        .take(64)
        .collect::<String>();
    let password_hash = hash_password(&body.password)?;
    let user = User {
        id: random_hex(6),
        username: body.username.clone(),
        password_hash,
        display_name,
        created_at: now_secs(),
    };
    let (session, token) = new_session(&user.id, body.device_name);
    state.mutate(|p| {
        if p.users.iter().any(|u| u.username == body.username) {
            return Err(ApiError(StatusCode::CONFLICT, "用户名已被占用".to_string()));
        }
        p.users.push(user.clone());
        p.sessions.push(session.clone());
        Ok(())
    })?;
    tracing::info!(username = %user.username, "用户注册");
    Ok(Json(json!({
        "userId": user.id,
        "username": user.username,
        "displayName": user.display_name,
        "token": token,
        "sessionId": session.id,
    }))
    .into_response())
}

fn new_session(user_id: &str, device_name: Option<String>) -> (Session, String) {
    let token = random_hex(32);
    let session = Session {
        id: random_hex(8),
        user_id: user_id.to_string(),
        token_hash: token_hash(&token),
        device_name: device_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "未知设备".to_string())
            .chars()
            .take(64)
            .collect(),
        created_at: now_secs(),
        last_seen_at: now_secs(),
    };
    (session, token)
}

pub async fn register(State(state): State<ServerState>, Json(body): Json<Credentials>) -> ApiResult<axum::response::Response> {
    create_account(state, body).await
}

pub async fn login(State(state): State<ServerState>, Json(body): Json<Credentials>) -> ApiResult<axum::response::Response> {
    let user = state.read(|p| p.users.iter().find(|u| u.username == body.username).cloned());
    let Some(user) = user else {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "用户名或密码错误".to_string()));
    };
    if !verify_password(&body.password, &user.password_hash) {
        // 用户名存在与否统一口径（不给枚举账号的线索）
        return Err(ApiError(StatusCode::UNAUTHORIZED, "用户名或密码错误".to_string()));
    }
    let (session, token) = new_session(&user.id, body.device_name);
    let session_id = session.id.clone();
    state.mutate(|p| {
        p.sessions.push(session.clone());
        Ok(())
    })?;
    tracing::info!(username = %user.username, device = %session.device_name, "用户登录");
    Ok(Json(json!({
        "userId": user.id,
        "username": user.username,
        "displayName": user.display_name,
        "token": token,
        "sessionId": session_id,
    }))
    .into_response())
}

/// 鉴权后的调用方身份（user_id 归属人，session_id 归属设备会话）。
#[derive(Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub session_id: String,
}

impl FromRequestParts<ServerState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &ServerState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let hash = token_hash(header);
        state
            .authenticate(&hash)
            .ok_or_else(unauthorized)
    }
}

pub async fn logout(State(state): State<ServerState>, user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    state.mutate(|p| {
        p.sessions.retain(|s| s.id != user.session_id);
        Ok(())
    })?;
    Ok(Json(json!({})))
}

pub async fn list_devices(State(state): State<ServerState>, user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    let now = now_secs();
    let devices = state.read(|p| {
        p.sessions
            .iter()
            .filter(|s| s.user_id == user.user_id)
            .map(|s| {
                json!({
                    "id": s.id,
                    "deviceName": s.device_name,
                    "createdAt": s.created_at,
                    "lastSeenAt": if s.last_seen_at == 0 { s.created_at } else { s.last_seen_at },
                    "current": s.id == user.session_id,
                })
            })
            .collect::<Vec<_>>()
    });
    // 摸一下内存里的 last_seen（随下次结构变更落盘）；read 的闭包不可变，这里单独刷新
    state.touch_session(&user.session_id, now);
    Ok(Json(json!(devices)))
}

pub async fn revoke_device(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let removed = state.mutate(|p| {
        let before = p.sessions.len();
        p.sessions.retain(|s| !(s.user_id == user.user_id && s.id == session_id));
        if p.sessions.len() == before {
            return Err(ApiError(StatusCode::NOT_FOUND, "会话不存在".to_string()));
        }
        Ok(())
    })?;
    let _ = removed;
    Ok(Json(json!({})))
}
