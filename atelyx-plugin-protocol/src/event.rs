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
    /// 当前打开表格的内容变更（打开 / 切换 / 关闭表格、本地编辑结算、协作远端补丁落地）。
    /// payload 见 [`TableChangedPayload`]。
    pub const TABLE_CHANGED: &str = "table:changed";
    /// 会话语义变更（流式增量不广播）。payload 见 [`ChatChangedPayload`]。
    pub const AI_CHAT_CHANGED: &str = "ai:chat.changed";
    /// 仓库内文件变更（宿主保存 / 新建 / 删除 / 改名 / 插件经 `vault` 写入）。只覆盖宿主发起的
    /// 写入，外部程序直接改磁盘不发。payload 见 [`FileChangedPayload`]。
    pub const VAULT_FILE_CHANGED: &str = "vault:file.changed";
    /// 协作空间团队元数据的某个键变更（任何成员经 `collab.metaPatch` 或宿主写入）。
    /// payload 见 [`SpaceMetaChangedPayload`]；订阅方按 `key` 过滤后重取该键。
    pub const SPACE_META_CHANGED: &str = "space:meta.changed";
}

/// [`name::VAULT_SWITCH`] 的 payload（`kind` 与字段名均 camelCase，与协议其余类型同一口径）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum VaultSwitchPayload {
    Local { root: String },
    /// `can_write` = 当前身份在空间内可写（viewer 只读；写操作仍由服务端终审）。
    Space {
        space_id: String,
        #[serde(default)]
        can_write: bool,
    },
}

/// [`name::TABLE_CHANGED`] 的 payload：当前打开表格的仓库相对路径（无打开表格 = 缺省）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// [`name::AI_CHAT_CHANGED`] 的 payload。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatChangedPayload {
    pub session_id: String,
    pub kind: ChatChangeKind,
}

/// 会话变更种类：创建 / 消息列表变化 / 元数据变化 / 删除。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatChangeKind {
    Created,
    Messages,
    Meta,
    Deleted,
}

/// [`name::VAULT_FILE_CHANGED`] 的 payload：仓库相对路径 + 变更种类（改名另带旧路径）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangedPayload {
    pub path: String,
    pub kind: FileChangeKind,
    /// 改名 / 移动前的路径（其余种类缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

/// [`name::SPACE_META_CHANGED`] 的 payload：变更的键名（团队元数据单键）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceMetaChangedPayload {
    pub key: String,
}

/// 文件变更种类：新建 / 内容写入 / 删除 / 改名或移动。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeKind {
    Created,
    Updated,
    Deleted,
    Renamed,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_changed_payload_uses_camel_case_and_omits_missing_old_path() {
        let updated = FileChangedPayload {
            path: "notes/a.md".into(),
            kind: FileChangeKind::Updated,
            old_path: None,
        };
        assert_eq!(
            serde_json::to_value(&updated).unwrap(),
            serde_json::json!({ "path": "notes/a.md", "kind": "updated" })
        );
        let renamed = FileChangedPayload {
            path: "notes/b.md".into(),
            kind: FileChangeKind::Renamed,
            old_path: Some("notes/a.md".into()),
        };
        assert_eq!(
            serde_json::to_value(&renamed).unwrap(),
            serde_json::json!({ "path": "notes/b.md", "kind": "renamed", "oldPath": "notes/a.md" })
        );
    }

    #[test]
    fn space_payloads_carry_write_role_and_meta_key() {
        let space = VaultSwitchPayload::Space { space_id: "space-1".into(), can_write: true };
        assert_eq!(
            serde_json::to_value(&space).unwrap(),
            serde_json::json!({ "kind": "space", "spaceId": "space-1", "canWrite": true })
        );
        let meta = SpaceMetaChangedPayload { key: "calendar".into() };
        assert_eq!(serde_json::to_value(&meta).unwrap(), serde_json::json!({ "key": "calendar" }));
    }
}
