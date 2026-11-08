//! 空间访问控制与内容根解析（内容类端点共用的前置关口）。
//! 读关口 = 任意成员；写关口 = owner/editor。内容根优先取登记路径，未收编回落默认布局根；
//! 路径拼装错误统一转 `JoinError` 分型响应。持有方 = `state.rs`（状态与锁），本模块不持状态。

use crate::auth::AuthUser;
use crate::fsops::{JoinError, SpaceRoot};
use crate::spaces::{require_role, space_of, ALL_ROLES};
use crate::state::{bad_request, not_found, ServerState, ROLE_EDITOR, ROLE_OWNER};
use crate::ApiError;

/// 成员校验 + 内容根。读端点用这里（任意成员均可读）。
pub(crate) fn member_root(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<SpaceRoot, ApiError> {
    state.read(|p| {
        let space = space_of(p, space_id)?;
        require_role(space, &user.user_id, ALL_ROLES)?;
        Ok(SpaceRoot(state.space_content_root(space)))
    })
}

/// 成员校验 + 内容根 + 写角色（owner/editor）。写端点统一走这里，只读角色（viewer）被拒。
pub(crate) fn write_root(state: &ServerState, space_id: &str, user: &AuthUser) -> Result<SpaceRoot, ApiError> {
    state.read(|p| {
        let space = space_of(p, space_id)?;
        require_role(space, &user.user_id, &[ROLE_OWNER, ROLE_EDITOR])?;
        Ok(SpaceRoot(state.space_content_root(space)))
    })
}

/// 路径拼装错误转 API 响应（非法目标 = bad_request，越界/不存在 = not_found）。
pub(crate) fn join_err(e: JoinError) -> ApiError {
    match e {
        JoinError::Invalid(m) => bad_request(&m),
        JoinError::NotFound(m) => not_found(&m),
    }
}
