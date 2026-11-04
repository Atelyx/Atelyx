//! 宿主广播事件：宿主与插件向插件方向广播领域事件，插件经 `event.subscribe` 声明感兴趣的名字。
//! 事件名为封闭常量表；payload 按事件名定型，未上表的名字宿主拒绝订阅。

use serde::{Deserialize, Serialize};

/// 方法常量：插件订阅请求、插件发布域事件与宿主广播通知。
pub const SUBSCRIBE: &str = "event.subscribe";
/// 插件发布一条它自己拥有的域事件（例如表格插件发布 `table:changed`）：宿主校验事件名与
/// 归属后按普通广播发给订阅方。
pub const EMIT: &str = "event.emit";
pub const HOST_PUBLISH: &str = "host:event";

/// [`EMIT`] 的 params：事件名 + 按该事件名定型的载荷。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEmitParams {
    pub name: String,
    pub payload: serde_json::Value,
}

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
    /// 历史回滚把某个版本交回当前画布：payload 见 [`crate::method::CanvasContentParams`]
    /// （订阅方按自己打开的路径匹配；`content` 是 `.atlx` 全文）。
    pub const CANVAS_RESTORE: &str = "canvas:restore";
    /// 画布内容已结算（本插件落盘 / 关闭）：payload 见 [`CanvasChangedPayload`]。
    pub const CANVAS_CHANGED: &str = "canvas:changed";
    /// 宿主拦截的画布快捷键命令（撤销 / 复制 / 粘贴等，按用户自定义键位触发）：
    /// payload 见 [`crate::method::CanvasCommandParams`]。
    pub const CANVAS_COMMAND: &str = "canvas:command";
    /// 画布是否正展示在前台（后台标签页不算）：payload 见 [`CanvasVisiblePayload`]。
    pub const CANVAS_VISIBLE: &str = "canvas:visible";
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
    /// 当前仓库的可检索文件清单重建（进 / 换仓库、宿主刷新文件清单时）。payload 为空对象；
    /// 订阅方重取 `vault.files` 拿整份清单（条数与顺序都随重建变化）。插件经 `vault` 新建的
    /// 文件不在其列，要等下一次清单重建才出现。
    pub const VAULT_FILES_CHANGED: &str = "vault:files.changed";
    /// 协作空间团队元数据的某个键变更（任何成员经 `collab.metaPatch` 或宿主写入）。
    /// payload 见 [`SpaceMetaChangedPayload`]；订阅方按 `key` 过滤后重取该键。
    pub const SPACE_META_CHANGED: &str = "space:meta.changed";
    /// 空间的连接态 / 在线设备 / 各端打开文件变化（连接建立、名册刷新、在场更新、断开）。
    /// payload 为空对象；订阅方重取 `space.room`。
    pub const SPACE_PEERS_CHANGED: &str = "space:peers.changed";
    /// 空间成员名册缓存变化（拉取开始 / 成功 / 失败、离开空间清空）。payload 为空对象；
    /// 订阅方重取 `space.members`。
    pub const SPACE_MEMBERS_CHANGED: &str = "space:members.changed";
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
    /// 当前容器的最近打开清单变化（打开文件确认成功、失效条目被剔除）。payload 为空对象；
    /// 订阅方重取 `recent.list` 拿整份清单（条数与顺序都随记录变化）。
    pub const RECENT_CHANGED: &str = "recent:changed";
    /// 当前容器的历史版本集合变化（新版本记入侧文件、回滚落盘）。payload 为空对象；
    /// 订阅方重取 `history.groups` / `history.day`（宿主已丢弃聚合缓存）。
    pub const HISTORY_CHANGED: &str = "history:changed";
    /// 拖拽会话的落点状态（发起方收到）：落点 / 拒绝原因 / 悬停自动展开。
    /// payload 见 [`crate::method::DragStateParams`]。
    pub const DRAG_STATE: &str = "drag:state";
    /// 当前打开的文件集合变化（打开 / 切换 / 关闭笔记 / 表格 / 画布）。payload 为空对象；
    /// 订阅方重取 `editor.openFiles` 拿当前集合（笔记 / 表格 / 画布各一个，至多 3 个）。
    pub const EDITOR_OPEN_CHANGED: &str = "editor:open.changed";
    /// 历史回滚把某个版本交回当前表格：payload 见 [`crate::method::TableContentParams`]
    /// （订阅方按自己打开的路径匹配；`content` 是 `.atb` 全文）。
    pub const TABLE_RESTORE: &str = "table:restore";
    /// 别的成员改动了空间里的表格：payload 见 [`crate::method::SpaceFilePatchPayload`]；
    /// 订阅方按 `path` 与自己打开的文件匹配后合入。
    pub const SPACE_FILE_PATCH: &str = "space:file.patch";
    /// 宿主请面板插件立即落盘：payload 见 [`crate::method::PanelFlushParams`]，
    /// 结果经 `panel.flushDone` 按 `token` 回执。
    pub const PANEL_FLUSH: &str = "panel:flush";
    /// 用户点了面板头部声明的菜单项：payload 见 [`crate::method::PanelHeaderActionParams`]。
    pub const PANEL_HEADER_ACTION: &str = "panel:header.action";
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
            | name::CANVAS_RESTORE
            | name::CANVAS_CHANGED
            | name::CANVAS_COMMAND
            | name::CANVAS_VISIBLE
            | name::AI_CHAT_CHANGED
            | name::AI_CHAT_RELOAD
            | name::AI_CHAT_PERSIST
            | name::VAULT_FILE_CHANGED
            | name::VAULT_FILES_CHANGED
            | name::SPACE_META_CHANGED
            | name::SPACE_PEERS_CHANGED
            | name::SPACE_MEMBERS_CHANGED
            | name::WINDOW_FILE_DROP
            | name::DOC_CHANGED
            | name::DOC_PRESENCE
            | name::EDITOR_PREFS_CHANGED
            | name::RECENT_CHANGED
            | name::HISTORY_CHANGED
            | name::DRAG_STATE
            | name::EDITOR_OPEN_CHANGED
            | name::TABLE_RESTORE
            | name::SPACE_FILE_PATCH
            | name::PANEL_FLUSH
            | name::PANEL_HEADER_ACTION
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

/// [`name::CANVAS_CHANGED`] 的 payload：当前打开画布的路径 / 落盘状态 / 选中卡属性 / 统计。
/// 关闭画布时 `path` 缺省、`selection` 缺省、统计全零。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasChangedPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<crate::method::CanvasSelectionInfo>,
    #[serde(default)]
    pub stats: crate::method::CanvasStats,
}

/// [`name::CANVAS_VISIBLE`] 的 payload：画布是否展示在前台。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasVisiblePayload {
    pub visible: bool,
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
