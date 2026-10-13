//! 宿主广播事件：宿主与插件向插件方向广播领域事件，插件经 `event.subscribe` 声明感兴趣的名字。
//! 事件名为封闭常量表；payload 按事件名定型，未上表的名字宿主拒绝订阅。

use serde::{Deserialize, Serialize};

/// 方法常量：插件订阅请求与宿主广播通知。
pub const SUBSCRIBE: &str = "event.subscribe";
pub const HOST_PUBLISH: &str = "host:event";

/// 事件名常量。订阅不存在的名字宿主回 `RpcError::FAILED`。
pub mod name {
    /// 当前仓库容器切换（本地仓库 ↔ 协作空间，或换仓库）。payload 见 [`VaultSwitchPayload`]。
    /// 未声明 `keepMountedOnVaultSwitch` 的插件收到它之前就已被宿主重载。
    pub const VAULT_SWITCH: &str = "vault:switch";
}

/// [`name::VAULT_SWITCH`] 的 payload。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VaultSwitchPayload {
    Local { root: String },
    Space { space_id: String },
}

/// 宿主广播的通知 params（[`HOST_PUBLISH`]）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishParams {
    pub name: String,
    /// 按事件名定型的 payload；未知名字宿主不广播。
    pub payload: serde_json::Value,
}

/// 插件订阅请求（[`SUBSCRIBE`]）的 params 与结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    pub subscribed: bool,
}
