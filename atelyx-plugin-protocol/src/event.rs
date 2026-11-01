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
    /// 当前打开表格的选区变更（本端点选 / 拖选 / 键盘移动，协作者进入 / 移动 / 离场）。
    /// 内容不变、只有选区移动时也发。payload 见 [`TableSelectionChangedPayload`]。
    pub const TABLE_SELECTION_CHANGED: &str = "table:selection.changed";
    /// 会话语义变更（流式增量不广播）。payload 见 [`ChatChangedPayload`]。
    pub const AI_CHAT_CHANGED: &str = "ai:chat.changed";
    /// 会话真源整体重载完成（换仓 / 空间装载），订阅方应整体重取。
    /// payload 见 [`ChatReloadedPayload`]。
    pub const AI_CHAT_RELOAD: &str = "ai:chat.reload";
    /// 会话落盘状态变化（失败置位 / 恢复）。payload 见 [`ChatPersistChangedPayload`]。
    pub const AI_CHAT_PERSIST: &str = "ai:chat.persist";
    /// 仓库内文件变更（宿主保存 / 新建 / 删除 / 改名 / 插件经 `vault` 写入）。只覆盖宿主发起的
    /// 写入，外部程序直接改磁盘不发。payload 见 [`FileChangedPayload`]。
    pub const VAULT_FILE_CHANGED: &str = "vault:file.changed";
    /// 协作空间团队元数据的某个键变更（任何成员经 `collab.metaPatch` 或宿主写入）。
    /// payload 见 [`SpaceMetaChangedPayload`]；订阅方按 `key` 过滤后重取该键。
    pub const SPACE_META_CHANGED: &str = "space:meta.changed";
    /// 外部文件拖入窗口（宿主只转发绝对路径，读取由订阅方经 `fs` 自理）。
    /// payload 见 [`FileDropPayload`]；未挂载界面的插件应忽略。
    pub const WINDOW_FILE_DROP: &str = "window:file.drop";
    /// 当前文档会话的正文 / 落盘状态变化（协作远端合入、保存成功或失败、属性面板改写）。
    /// payload 见 [`DocChangedPayload`]。
    pub const DOC_CHANGED: &str = "doc:changed";
    /// 当前文档的远端协作者光标（进入 / 移动 / 离场都会发；无协作者 = 空清单）。
    /// payload 见 [`DocPresencePayload`]。
    pub const DOC_PRESENCE: &str = "doc:presence";
    /// 全局编辑偏好变更（软换行 / 页面内标题 / 正文宽度）。payload 同
    /// `editor.prefs` 的 result（[`crate::method::EditorPrefs`]）。
    pub const EDITOR_PREFS_CHANGED: &str = "editor:prefs.changed";
}

/// [`name::DOC_PRESENCE`] 的 payload：当前文档路径 + 远端协作者光标（不含本端）。
/// `anchor` / `head` 与 `doc.open` 回执的 `text` 同一坐标系（全文字节偏移）；缺省 = 该成员没有光标。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPresencePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cursors: Vec<DocRemoteCursor>,
}

/// 一位远端协作者的光标（`color` 为 `#rrggbb` 或 `#rrggbbaa`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRemoteCursor {
    pub name: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<usize>,
}

/// 订阅事件名是否在封闭表内（宿主另允许 `ai::HOST_CHAT_EVENT`，见插件运行时）。
pub fn is_known(name: &str) -> bool {
    matches!(
        name,
        name::VAULT_SWITCH
            | name::TABLE_CHANGED
            | name::TABLE_SELECTION_CHANGED
            | name::AI_CHAT_CHANGED
            | name::AI_CHAT_RELOAD
            | name::AI_CHAT_PERSIST
            | name::VAULT_FILE_CHANGED
            | name::SPACE_META_CHANGED
            | name::WINDOW_FILE_DROP
            | name::DOC_CHANGED
            | name::DOC_PRESENCE
            | name::EDITOR_PREFS_CHANGED
    )
}

/// [`name::DOC_CHANGED`] 的 payload：`text` 只在正文被宿主改写时给出（保存成功回填合流结果、
/// 远端合入、属性面板改写）；`dirty` / `saveError` 反映当前落盘状态。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocChangedPayload {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
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

/// [`name::TABLE_SELECTION_CHANGED`] 的 payload：当前表格路径 + 本端与远端选区。
/// 无打开表格 = 全缺省。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableSelectionChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::method::TableSelection>,
    /// 其他协作者的当前选区（不含本端）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_selections: Vec<crate::method::TableRemoteSelection>,
}

/// [`name::AI_CHAT_CHANGED`] 的 payload。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatChangedPayload {
    pub session_id: String,
    pub kind: ChatChangeKind,
}

/// [`name::AI_CHAT_RELOAD`] 的 payload：当前无字段，保留结构体便于扩展。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatReloadedPayload {}

/// [`name::AI_CHAT_PERSIST`] 的 payload：当前落盘失败（`None` = 已恢复）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPersistChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<crate::chat::ChatPersistError>,
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

/// [`name::WINDOW_FILE_DROP`] 的 payload：本次拖入的绝对路径（一次事件可含多张图）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileDropPayload {
    pub paths: Vec<String>,
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
    fn chat_events_carry_their_own_payload() {
        // 真源重载无字段；落盘恢复 = 空对象，插件按「无 persistError」清掉提示
        assert_eq!(
            serde_json::to_value(ChatReloadedPayload::default()).unwrap(),
            serde_json::json!({})
        );
        let recovered: ChatPersistChangedPayload = serde_json::from_str("{}").unwrap();
        assert!(recovered.persist_error.is_none(), "缺字段 = 已恢复");
        let failed: ChatPersistChangedPayload = serde_json::from_value(serde_json::json!({
            "persistError": { "message": "磁盘只读", "at": 1 }
        }))
        .unwrap();
        assert_eq!(failed.persist_error.map(|error| error.message).as_deref(), Some("磁盘只读"));
        assert_eq!(
            serde_json::to_value(ChatChangedPayload {
                session_id: "s1".into(),
                kind: ChatChangeKind::Meta,
            })
            .unwrap(),
            serde_json::json!({ "sessionId": "s1", "kind": "meta" })
        );
    }

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

    #[test]
    fn table_selection_payload_carries_local_and_remote_ranges() {
        let payload = TableSelectionChangedPayload {
            path: Some("plan.atb".into()),
            selection: Some(crate::method::TableSelection::Row { row_id: "r1".into() }),
            remote_selections: vec![crate::method::TableRemoteSelection {
                name: "鲍勃".into(),
                color: "#3d7be0".into(),
                selection: crate::method::TableSelection::Cell {
                    row_id: "r2".into(),
                    field_id: "f1".into(),
                },
            }],
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            serde_json::json!({
                "path": "plan.atb",
                "selection": { "kind": "row", "rowId": "r1" },
                "remoteSelections": [{
                    "name": "鲍勃",
                    "color": "#3d7be0",
                    "selection": { "kind": "cell", "rowId": "r2", "fieldId": "f1" }
                }]
            })
        );
        assert_eq!(
            serde_json::to_value(TableSelectionChangedPayload::default()).unwrap(),
            serde_json::json!({}),
            "无打开表格 = 全缺省"
        );
    }
}
