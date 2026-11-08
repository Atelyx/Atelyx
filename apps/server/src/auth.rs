//! 账号与会话：开放注册 / 登录 / 登出 / 设备会话列表与吊销；日志只记元数据，密码与令牌不入日志。
//! 密码 argon2 哈希；令牌随机 256 位 hex、经 `Authorization: Bearer` 传递，服务端只存 SHA-256 摘要。
//! 会话携带客户端设备 id：同账号同设备再次登录原地换发令牌，无设备 id 的会话在新客户端登录时清理。

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::{now_secs, random_hex, token_hash, Persisted, ServerState, Session, User};
use crate::ws::KickReason;
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
    #[serde(default)]
    device_id: Option<String>,
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
    let (session, token) = new_session(&user.id, body.device_name, body.device_id);
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

fn new_session(user_id: &str, device_name: Option<String>, device_id: Option<String>) -> (Session, String) {
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
        // 设备 id 只作合并键：空串视同未提供，长度截断防恶意超长入盘
        device_id: device_id.filter(|s| !s.is_empty()).map(|d| d.chars().take(64).collect()),
        created_at: now_secs(),
        last_seen_at: now_secs(),
    };
    (session, token)
}

/// 登录会话安装：同账号同 device_id 的既有会话原地换发令牌（沿用条目，返回 true 表示换发）；
/// 无 device_id 的既有会话按旧版残留清理（仅新客户端登录时，避免旧登录无限堆积）。
/// 换发后旧令牌摘要被覆盖，持有旧令牌的在途连接由调用方踢出。
fn install_session(p: &mut Persisted, user_id: &str, session: Session) -> (Session, bool) {
    if session.device_id.is_some() {
        p.sessions.retain(|s| !(s.user_id == user_id && s.device_id.is_none()));
    }
    let Some(device_id) = session.device_id.clone() else {
        p.sessions.push(session.clone());
        return (session, false);
    };
    if let Some(existing) = p.sessions.iter_mut().find(|s| {
        s.user_id == user_id && s.device_id.as_deref() == Some(device_id.as_str())
    }) {
        existing.token_hash = session.token_hash.clone();
        existing.device_name = session.device_name.clone();
        existing.last_seen_at = session.last_seen_at;
        let merged = Session { id: existing.id.clone(), created_at: existing.created_at, ..session };
        return (merged, true);
    }
    p.sessions.push(session.clone());
    (session, false)
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
    let (session, token) = new_session(&user.id, body.device_name, body.device_id);
    let (session, reused) = state.mutate(|p| Ok::<_, ApiError>(install_session(p, &user.id, session)))?;
    let session_id = session.id.clone();
    // 换发令牌 = 旧令牌即刻失效：踢掉仍持旧令牌的在途连接，强制以新令牌重连
    if reused {
        state.hub().kick_sessions(None, std::slice::from_ref(&session_id), KickReason::SessionRevoked);
    }
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
