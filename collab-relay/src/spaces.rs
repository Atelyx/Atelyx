//! 空间 / 成员 / 邀请：建空间、我的空间（可见性隔离）、改名、转让 owner、
//! 成员名册、邀请码（角色 + 过期 + 次数上限）与接受邀请。
//!
//! 可见性隔离：空间列表只返回自己参与的空间；成员名册只对空间内成员可见（viewer 只读可看）。
//! 角色三档：owner（改名/邀请/移除成员/转让）、editor（内容读写）、viewer（内容/索引/团队
//! 元数据只读，自身 user 元数据可写）。空间未提供删除端点。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::state::{
    now_secs, random_hex, Member, ServerState, Space, ROLE_EDITOR, ROLE_OWNER, ROLE_VIEWER,
};
use crate::{ApiError, ApiResult};

const NAME_MAX: usize = 64;

fn bad_request(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_string())
}

fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.to_string())
}

fn forbidden(message: &str) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, message.to_string())
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= NAME_MAX
}

/// 全部角色清单（「任意成员可做」的闸门统一用这份清单；新角色加入时须同步）。
pub(crate) const ALL_ROLES: &[&str] = &[ROLE_OWNER, ROLE_EDITOR, ROLE_VIEWER];

/// 查空间（404 = 空间不存在，不区分「无权限」防探测）。
pub(crate) fn space_of<'a>(p: &'a crate::state::Persisted, space_id: &str) -> Result<&'a Space, ApiError> {
    p.spaces.iter().find(|s| s.id == space_id).ok_or_else(|| not_found("空间不存在"))
}

/// mutate 闭包内取可变空间借用。
fn space_of_mut<'a>(p: &'a mut crate::state::Persisted, space_id: &str) -> Result<&'a mut Space, ApiError> {
    p.spaces.iter_mut().find(|s| s.id == space_id).ok_or_else(|| not_found("空间不存在"))
}

/// 要求调用方是成员且角色在允许清单内（403 = 非成员或角色不足，统一口径）。
pub(crate) fn require_role<'a>(space: &'a Space, user_id: &str, allowed: &[&str]) -> Result<&'a Member, ApiError> {
    let member = space
        .members
        .iter()
        .find(|m| m.user_id == user_id)
        .ok_or_else(|| forbidden("不是该空间成员"))?;
    if allowed.contains(&member.role.as_str()) {
        Ok(member)
    } else {
        Err(forbidden("角色权限不足"))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSpaceBody {
    name: String,
    /// 收编的既有目录（如 NAS 上已按仓库结构组织的文件夹）；缺省 = 数据目录内新建。
    #[serde(default)]
    path: Option<String>,
}

pub async fn create_space(
    State(state): State<ServerState>,
    user: AuthUser,
    Json(body): Json<CreateSpaceBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if !valid_name(&body.name) {
        return Err(bad_request("空间名不能为空且不超过 64 字"));
    }
    // 收编目录：归一化为 canonical 路径存储（大小写/符号链接在创建时定死，后续比较一致）
    let adopted: Option<String> = match body.path.as_deref() {
        None | Some("") => None,
        Some(p) => {
            let pb = std::path::PathBuf::from(p);
            if !pb.is_absolute() {
                return Err(bad_request("内容根必须是绝对路径"));
            }
            if !pb.is_dir() {
                return Err(bad_request(&format!("目录不存在：{p}")));
            }
            let canon = dunce::canonicalize(&pb)
                .map_err(|e| bad_request(&format!("目录不可达：{p} ({e})")))?
                .to_string_lossy()
                .to_string();
            // 内容根不得与数据目录互相嵌套（元数据 JSON 会混进内容树）
            let data_canon = dunce::canonicalize(state.data_dir())
                .map_err(|e| bad_request(&format!("数据目录不可达：{e}")))?;
            let adopted_path = std::path::Path::new(&canon);
            if adopted_path.starts_with(&data_canon) || data_canon.starts_with(adopted_path) {
                return Err(bad_request("内容根不能位于数据目录内（或包含数据目录）"));
            }
            Some(canon)
        }
    };
    let space = Space {
        id: random_hex(16),
        name: body.name.trim().to_string(),
        owner_user_id: user.user_id.clone(),
        created_at: now_secs(),
        members: vec![Member { user_id: user.user_id.clone(), role: ROLE_OWNER.to_string(), joined_at: now_secs() }],
        root_path: adopted.clone(),
    };
    // 默认布局先建内容目录再落名册：目录建不出来直接失败，不留无内容的空壳空间；
    // 收编目录已存在且有内容，不建不碰
    if adopted.is_none() {
        std::fs::create_dir_all(state.default_space_root(&space.id))
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("创建空间目录失败：{e}")))?;
    }
    let id = space.id.clone();
    let name = space.name.clone();
    let adopted_for_lock = adopted.clone();
    state.mutate(|p| {
        // 收编目录与其他空间的内容根不得互相嵌套（嵌套会让同一文件出现在两棵树里）
        if let Some(root) = &adopted_for_lock {
            for other in p.spaces.iter() {
                let Some(other_root) = &other.root_path else { continue };
                if root == other_root {
                    return Err(ApiError(StatusCode::CONFLICT, format!("该目录已被空间「{}」使用", other.name)));
                }
                let a = std::path::Path::new(root);
                let b = std::path::Path::new(other_root);
                if a.starts_with(b) || b.starts_with(a) {
                    return Err(bad_request("目录与其他空间的内容根互相嵌套"));
                }
            }
        }
        p.spaces.push(space.clone());
        Ok(())
    })?;
    tracing::info!(space_id = %id, name = %name, adopted = adopted.is_some(), "空间创建");
    Ok(Json(json!({ "spaceId": id, "name": name, "role": ROLE_OWNER, "rootPath": adopted })))
}

pub async fn list_spaces(State(state): State<ServerState>, user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    let spaces = state.read(|p| {
        p.spaces
            .iter()
            .filter_map(|s| {
                let role = s.members.iter().find(|m| m.user_id == user.user_id)?.role.clone();
                Some(json!({
                    "spaceId": s.id,
                    "name": s.name,
                    "role": role,
                    "ownerUserId": s.owner_user_id,
                    "createdAt": s.created_at,
                }))
            })
            .collect::<Vec<_>>()
    });
    Ok(Json(json!(spaces)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameSpaceBody {
    name: String,
}

pub async fn rename_space(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<RenameSpaceBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if !valid_name(&body.name) {
        return Err(bad_request("空间名不能为空且不超过 64 字"));
    }
    let name = body.name.trim().to_string();
    state.mutate(|p| {
        let space = space_of_mut(p, &space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER])?;
        space.name = name.clone();
        Ok(())
    })?;
    tracing::info!(space_id = %space_id, "空间改名");
    Ok(Json(json!({ "spaceId": space_id, "name": name })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferBody {
    to_user_id: String,
}

/// 转让 owner：目标须已是成员；原 owner 降为 editor（角色唯一，保底可继续读写内容）。
pub async fn transfer_ownership(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<TransferBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.to_user_id == user.user_id {
        return Err(bad_request("空间已是你的"));
    }
    let new_owner = body.to_user_id.clone();
    state.mutate(|p| {
        let space = space_of_mut(p, &space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER])?;
        let target = space
            .members
            .iter()
            .find(|m| m.user_id == new_owner)
            .ok_or_else(|| bad_request("目标用户不是该空间成员"))?;
        let _ = target;
        space.owner_user_id = new_owner.clone();
        for m in space.members.iter_mut() {
            if m.user_id == new_owner {
                m.role = ROLE_OWNER.to_string();
            } else if m.user_id == user.user_id {
                m.role = ROLE_EDITOR.to_string();
            }
        }
        Ok(())
    })?;
    tracing::info!(space_id = %space_id, "空间管理权转让");
    Ok(Json(json!({ "spaceId": space_id, "ownerUserId": new_owner })))
}

pub async fn list_members(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let members = state.read(|p| {
        let space = space_of(p, &space_id)?;
        // 成员名册对空间内成员可见（viewer 只读），与「名册只对成员可见」的隔离口径一致
        require_role(space, &user.user_id, ALL_ROLES)?;
        let rows = space
            .members
            .iter()
            .map(|m| {
                let account = p.users.iter().find(|u| u.id == m.user_id);
                json!({
                    "userId": m.user_id,
                    "username": account.map(|u| u.username.clone()).unwrap_or_default(),
                    "displayName": account.map(|u| u.display_name.clone()).unwrap_or_default(),
                    "role": m.role,
                    "joinedAt": m.joined_at,
                })
            })
            .collect::<Vec<_>>();
        Ok::<_, ApiError>(rows)
    })?;
    Ok(Json(json!(members)))
}

/// 移除成员（owner 专属）。`userId` 为 `me` = 自行退出（owner 不可自行退出，先转让）。
pub async fn remove_member(
    State(state): State<ServerState>,
    user: AuthUser,
    Path((space_id, member_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let target = if member_id == "me" { user.user_id.clone() } else { member_id };
    state.mutate(|p| {
        let space = space_of_mut(p, &space_id)?;
        let is_owner = space.owner_user_id == user.user_id;
        let removing_owner = target == space.owner_user_id;
        if removing_owner {
            return Err(bad_request("不能移除空间 owner（先转让管理权）"));
        }
        if target != user.user_id && !is_owner {
            return Err(forbidden("只有 owner 可移除其他成员"));
        }
        if !space.members.iter().any(|m| m.user_id == target) {
            return Err(not_found("成员不存在"));
        }
        space.members.retain(|m| m.user_id != target);
        Ok(())
    })?;
    tracing::info!(space_id = %space_id, "成员移除");
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInviteBody {
    role: String,
    /// 有效时长（小时）；缺省 = 长期有效。范围见 INVITE_MIN_HOURS / INVITE_MAX_HOURS。
    #[serde(default)]
    expires_in_hours: Option<i64>,
    /// 次数上限；缺省 = 不限。
    #[serde(default)]
    max_uses: Option<u32>,
}

/// 邀请码有效时长边界（小时）：下限挡 0 与负数（无意义时长），上限 10 年
/// （同时保证秒数换算不溢出）。
pub const INVITE_MIN_HOURS: i64 = 1;
pub const INVITE_MAX_HOURS: i64 = 10 * 8760;

pub async fn create_invite(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<CreateInviteBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.role != ROLE_EDITOR && body.role != ROLE_VIEWER {
        return Err(bad_request("邀请角色只支持 editor 或 viewer"));
    }
    if let Some(n) = body.max_uses {
        if n == 0 {
            return Err(bad_request("次数上限至少为 1"));
        }
    }
    if let Some(h) = body.expires_in_hours {
        if !(INVITE_MIN_HOURS..=INVITE_MAX_HOURS).contains(&h) {
            return Err(bad_request(&format!(
                "有效时长须在 {}–{} 小时之间",
                INVITE_MIN_HOURS, INVITE_MAX_HOURS
            )));
        }
    }
    let invite = crate::state::Invite {
        code: random_hex(8),
        space_id: space_id.clone(),
        role: body.role,
        created_by: user.user_id.clone(),
        created_at: now_secs(),
        expires_at: body.expires_in_hours.map(|h| now_secs() + h * 3600),
        max_uses: body.max_uses,
        used_count: 0,
    };
    let code = invite.code.clone();
    state.mutate(|p| {
        let space = space_of_mut(p, &space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER])?;
        p.invites.push(invite.clone());
        Ok(())
    })?;
    tracing::info!(space_id = %space_id, "邀请码签发");
    Ok(Json(json!({
        "code": code,
        "role": invite.role,
        "expiresAt": invite.expires_at,
        "maxUses": invite.max_uses,
    })))
}

pub async fn list_invites(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let invites = state.read(|p| {
        let space = space_of(p, &space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER])?;
        Ok::<_, ApiError>(p.invites
            .iter()
            .filter(|i| i.space_id == space_id)
            .map(|i| {
                json!({
                    "code": i.code,
                    "role": i.role,
                    "expiresAt": i.expires_at,
                    "maxUses": i.max_uses,
                    "usedCount": i.used_count,
                    "createdAt": i.created_at,
                })
            })
            .collect::<Vec<_>>())
    })?;
    Ok(Json(json!(invites)))
}

pub async fn revoke_invite(
    State(state): State<ServerState>,
    user: AuthUser,
    Path((space_id, code)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    state.mutate(|p| {
        let space = space_of_mut(p, &space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER])?;
        let before = p.invites.len();
        p.invites.retain(|i| !(i.space_id == space_id && i.code == code));
        if p.invites.len() == before {
            return Err(not_found("邀请码不存在"));
        }
        Ok(())
    })?;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptBody {
    code: String,
}

/// 接受邀请：已是成员时幂等返回当前身份。
pub async fn accept_invite(
    State(state): State<ServerState>,
    user: AuthUser,
    Json(body): Json<AcceptBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let now = now_secs();
    let accepted = state.mutate(|p| {
        // 先把邀请码信息拷出（结束对 invites 的不可变借用），再做空间与计数的可变修改
        let invite = p
            .invites
            .iter()
            .find(|i| i.code == body.code)
            .ok_or_else(|| not_found("邀请码无效"))?;
        if let Some(expires_at) = invite.expires_at {
            if now >= expires_at {
                return Err(ApiError(StatusCode::GONE, "邀请码已过期".to_string()));
            }
        }
        if let Some(max_uses) = invite.max_uses {
            if invite.used_count >= max_uses {
                return Err(ApiError(StatusCode::CONFLICT, "邀请码次数已用尽".to_string()));
            }
        }
        let (space_id, role) = (invite.space_id.clone(), invite.role.clone());
        let already_member = p
            .spaces
            .iter()
            .find(|s| s.id == space_id)
            .and_then(|s| s.members.iter().find(|m| m.user_id == user.user_id))
            .map(|m| m.role.clone());
        if let Some(existing) = already_member {
            // 幂等：已是成员返回当前身份，不消耗次数
            let name = p.spaces.iter().find(|s| s.id == space_id).map(|s| s.name.clone()).unwrap_or_default();
            return Ok(json!({ "spaceId": space_id, "name": name, "role": existing }));
        }
        let space = space_of_mut(p, &space_id)?;
        space.members.push(Member { user_id: user.user_id.clone(), role: role.clone(), joined_at: now });
        let name = space.name.clone();
        if let Some(invite) = p.invites.iter_mut().find(|i| i.code == body.code) {
            invite.used_count += 1;
        }
        Ok(json!({ "spaceId": space_id, "name": name, "role": role }))
    })?;
    tracing::info!(space_id = accepted["spaceId"].as_str().unwrap_or(""), "邀请被接受");
    Ok(Json(accepted))
}
