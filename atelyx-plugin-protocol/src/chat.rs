//! 会话真源的跨库契约：读面快照与通用提交批次（`chat.session.*`）。
//!
//! 批次形状与宿主磁盘模型一致：插件提交「要做什么」，宿主负责校验、写盘链与空间后端。
//! 消息本身是自由 JSON（`role` / `content` / `steps` 等字段由 AI 域约定），批次只描述
//! 创建、追加、补丁、截断、删除与覆盖项。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 双层 Option 的 JSON 语义：字段缺省 = `None`（不动），显式 `null` = `Some(None)`（清除）。
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 新会话登记。`file` 缺省由宿主按会话 id 补齐（插件不需要知道磁盘目录）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDef {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
}

/// 既有会话的元数据字段更新（缺省字段不动）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaPatch {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 双层 Option：外层缺省 = 不动，内层 `None` = 清除（切回默认 Agent）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub agent_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendBatch {
    pub session_id: String,
    pub messages: Vec<Value>,
}

/// 消息内容补丁：`rev` 单调，迟到的旧补丁不覆盖新内容。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchEntry {
    pub session_id: String,
    pub message_id: String,
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateEntry {
    pub session_id: String,
    pub keep_count: usize,
}

/// 会话删除：`removeFiles = false` 只摘除内存登记（磁盘文件已在外部消失的对账路径）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteEntry {
    pub session_id: String,
    #[serde(default = "default_true")]
    pub remove_files: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRemoval {
    pub session_id: String,
    pub message_id: String,
}

fn default_true() -> bool {
    true
}

/// 覆盖项更新（双层 Option：外层缺省 = 不动，内层 `None` = 清除覆盖）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverridesPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub model_override: Option<Option<Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub effort_override: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub agent_override: Option<Option<String>>,
}

/// 一次提交：登记 / 元数据 / 消息追加 / 内容补丁 / 截断 / 删除 / 覆盖项。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitBatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<SessionDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metas: Vec<MetaPatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub appends: Vec<AppendBatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncations: Vec<TruncateEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deletes: Vec<DeleteEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_removals: Vec<MessageRemoval>,
    #[serde(default)]
    pub overrides: OverridesPatch,
}

/// [`crate::method::chat::SESSION_READ`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: String,
}

/// 单会话读面：元数据 + 消息数组（消息形态与磁盘格式同源）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
}

/// 落盘失败信息（`message` + 时间戳）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPersistError {
    pub message: String,
    pub at: i64,
}

/// 全仓库读面快照（进仓 / 切仓 / 覆盖层引导）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStoreSnapshot {
    #[serde(default)]
    pub session_vault_key: String,
    #[serde(default)]
    pub sessions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<ChatPersistError>,
}
