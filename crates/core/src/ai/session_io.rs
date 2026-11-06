//! 会话真源后端：本地仓库目录与协作空间用户元数据两种落点，向写链暴露同一套同步读写接口。
//!
//! 空间落点是**用户元数据**（`/api/spaces/:id/meta/me`）——会话归本人私有、随账号跨设备，
//! 不写进空间共享文件树。键位：`chat/messages/<会话 id>` 存该会话 JSONL 全文、
//! `chat/sessions/<会话 id>` 存会话元数据、`chat/editor-meta` 存面板级覆盖。
//!
//! 同步接口是写链的要求（写盘轮在专用线程上串行执行）；空间实现因此用阻塞 HTTP，
//! **不得在 tokio 线程上调用**——装载与写盘都必须落在自建线程 / 写链线程上。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::net_guard::{self, HostPolicy};
use crate::space_client::{normalize_server_url, urlencode_component};
use crate::vault::{
    self, ChatMetaFile, ChatSessionMeta, ChatSessionRow, CHAT_HISTORY_DIR, CHAT_META_EXT,
    CHAT_MESSAGE_EXT, EDITOR_CHATS_META_SCHEMA,
};

/// 会话私有数据的用户元数据键（前缀 + 会话 id）。
const META_MESSAGES_PREFIX: &str = "chat/messages/";
const META_SESSIONS_PREFIX: &str = "chat/sessions/";
const META_EDITOR: &str = "chat/editor-meta";

/// 空间请求总超时（秒）：写链线程上同步执行，超时避免线程被网络挂死。
const SPACE_TIMEOUT_SECS: u64 = 20;

/// 会话真源的读写出口（写链线程与装载都只经它做 I/O）。
///
/// 实现约定：失败必须返回 `Err`（不得静默降级）；`write_messages` 落盘后 `read_messages`
/// 必须读回同一份内容（写链的全量回落与追加基线判定都依赖这一往返一致）。
pub trait SessionIo: Send + Sync {
    /// 真源身份键（换根去重与快照出线）：同一真源恒同键，不同真源必不同。
    fn vault_key(&self) -> String;

    /// 列出全部会话（以消息正文的存在性为真相）。
    fn list_sessions(&self) -> Result<Vec<ChatSessionRow>, String>;

    /// 读会话消息正文；不存在返回 `Err`（调用方按空消息降级）。
    fn read_messages(&self, file: &str) -> Result<String, String>;

    /// 全量写会话消息正文（落盘形状与本地一致：正文 + 尾随换行）。
    fn write_messages(&self, file: &str, content: &str) -> Result<(), String>;

    /// 追加消息：仅当当前内容与 `base` 一致时才落盘（不一致返回 `Err`，由调用方回落全量重写）。
    fn append_messages(&self, file: &str, base: &str, tail: &str) -> Result<(), String>;

    fn read_session_meta(&self, file: &str) -> Result<Option<ChatSessionMeta>, String>;

    fn write_session_meta(&self, file: &str, meta: &ChatSessionMeta) -> Result<(), String>;

    /// 删会话的两个侧文件（消息 + 元数据）；不存在视为成功（幂等）。
    fn delete_session_files(&self, file: &str) -> Result<(), String>;

    fn read_overrides(&self) -> Result<ChatMetaFile, String>;

    fn write_overrides(&self, file: &ChatMetaFile) -> Result<(), String>;
}

/// 会话消息文件 `.atelyx/对话历史/<id>.jsonl` → 会话 id。
fn message_file_id(file: &str) -> Result<&str, String> {
    let prefix = format!("{CHAT_HISTORY_DIR}/");
    file.strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(CHAT_MESSAGE_EXT))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("非法会话消息路径：{file}"))
}

/// 会话元数据文件 `.atelyx/对话历史/<id>.meta.json` → 会话 id。
fn meta_file_id(file: &str) -> Result<&str, String> {
    let prefix = format!("{CHAT_HISTORY_DIR}/");
    file.strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(CHAT_META_EXT))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("非法会话元数据路径：{file}"))
}

/// 会话元数据文件路径（由消息文件路径派生，两处命名规则只在 `vault` 定义一次）。
fn meta_file_for(message_file: &str) -> Result<String, String> {
    Ok(format!(
        "{CHAT_HISTORY_DIR}/{}{CHAT_META_EXT}",
        message_file_id(message_file)?
    ))
}

/// 写入形状统一：消息正文以换行结尾（本地与空间落盘同一份字节）。
fn with_trailing_newline(content: &str) -> String {
    if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    }
}

/// 追加基线复核：磁盘内容（去尾换行）必须等于内存基线。
pub(crate) fn matches_append_base(existing: &str, base: &str) -> bool {
    if existing.is_empty() {
        base.is_empty()
    } else {
        existing.strip_suffix('\n') == Some(base)
    }
}

/// 追加结果：基线为空时只有新增行，否则按行边界拼接。
pub(crate) fn appended_body(base: &str, tail: &str) -> String {
    if base.is_empty() {
        tail.to_string()
    } else {
        format!("{base}\n{tail}")
    }
}

// ===== 本地仓库目录 =====

/// 本地仓库目录后端。
pub struct LocalSessionIo {
    root: PathBuf,
}

impl LocalSessionIo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SessionIo for LocalSessionIo {
    fn vault_key(&self) -> String {
        format!("local:{}", self.root.to_string_lossy())
    }

    fn list_sessions(&self) -> Result<Vec<ChatSessionRow>, String> {
        vault::list_chat_sessions_file(&self.root)
    }

    fn read_messages(&self, file: &str) -> Result<String, String> {
        vault::read_chat_messages_file(&self.root, file)
    }

    fn write_messages(&self, file: &str, content: &str) -> Result<(), String> {
        vault::write_chat_messages_file(&self.root, file, content)
    }

    fn append_messages(&self, file: &str, base: &str, tail: &str) -> Result<(), String> {
        let existing = vault::read_chat_messages_file(&self.root, file)?;
        if !matches_append_base(&existing, base) {
            return Err("磁盘内容与追加基线不一致".to_string());
        }
        vault::append_chat_messages_raw(&self.root, file, tail)
    }

    fn read_session_meta(&self, file: &str) -> Result<Option<ChatSessionMeta>, String> {
        vault::read_chat_session_meta_file(&self.root, file)
    }

    fn write_session_meta(&self, file: &str, meta: &ChatSessionMeta) -> Result<(), String> {
        vault::write_chat_session_meta_file(&self.root, file, meta)
    }

    fn delete_session_files(&self, file: &str) -> Result<(), String> {
        vault::delete_chat_messages_file(&self.root, file)?;
        vault::delete_chat_session_meta_file(&self.root, &meta_file_for(file)?)
    }

    fn read_overrides(&self) -> Result<ChatMetaFile, String> {
        vault::read_editor_chats_meta_file(&self.root)
    }

    fn write_overrides(&self, file: &ChatMetaFile) -> Result<(), String> {
        vault::write_editor_chats_meta_file(&self.root, file)
    }
}

// ===== 协作空间用户元数据 =====

/// 协作空间会话落点（阻塞 HTTP 的用户元数据读写）。
pub struct SpaceSessionIo {
    base: String,
    token: String,
    space_id: String,
    /// 客户端惰性构建一次（连接池复用）；构建失败不入缓存，下次请求重试。
    http: OnceLock<reqwest::blocking::Client>,
}

impl SpaceSessionIo {
    /// 按服务器地址构建：地址经本机/局域网策略校验（与 `SpaceClient` 同一条 SSRF 防线）。
    pub fn new(server_url: &str, token: String, space_id: String) -> Result<Self, String> {
        let base = net_guard::ensure_local_service_http_url(&normalize_server_url(server_url))?;
        Ok(Self {
            base: base.as_str().trim_end_matches('/').to_string(),
            token,
            space_id,
            http: OnceLock::new(),
        })
    }

    fn client(&self) -> Result<&reqwest::blocking::Client, String> {
        if let Some(client) = self.http.get() {
            return Ok(client);
        }
        let client = net_guard::blocking_client_for(HostPolicy::LocalService, SPACE_TIMEOUT_SECS)?;
        Ok(self.http.get_or_init(|| client))
    }

    fn meta_url(&self, key: Option<&str>) -> String {
        match key {
            Some(key) => format!(
                "{}/api/spaces/{}/meta/me?key={}",
                self.base,
                self.space_id,
                urlencode_component(key)
            ),
            None => format!("{}/api/spaces/{}/meta/me", self.base, self.space_id),
        }
    }

    /// 读该用户层的全部键值（会话清单与逐键读共用一次请求）。
    fn meta_values(&self) -> Result<HashMap<String, String>, String> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawMeta {
            #[serde(default)]
            values: HashMap<String, String>,
        }
        let resp = self
            .client()?
            .get(self.meta_url(None))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| format!("空间请求失败：{e}"))?;
        let raw: RawMeta = check_status(resp)?
            .json()
            .map_err(|e| format!("空间响应解析失败：{e}"))?;
        Ok(raw.values)
    }

    fn meta_get(&self, key: &str) -> Result<Option<String>, String> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawMeta {
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            values: HashMap<String, String>,
        }
        let resp = self
            .client()?
            .get(self.meta_url(Some(key)))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| format!("空间请求失败：{e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let raw: RawMeta = check_status(resp)?
            .json()
            .map_err(|e| format!("空间响应解析失败：{e}"))?;
        Ok(raw.value.or_else(|| raw.values.get(key).cloned()))
    }

    fn meta_put(&self, key: &str, value: &str) -> Result<(), String> {
        let body = serde_json::json!({ "values": { key: value } });
        let resp = self
            .client()?
            .patch(self.meta_url(None))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .map_err(|e| format!("空间请求失败：{e}"))?;
        check_status(resp).map(|_| ())
    }

    /// 删键：键不存在（404）与服务端幂等删除同语义，视为成功。
    fn meta_delete(&self, key: &str) -> Result<(), String> {
        let resp = self
            .client()?
            .delete(self.meta_url(Some(key)))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| format!("空间请求失败：{e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        check_status(resp).map(|_| ())
    }
}

/// 状态码 → 结果：2xx 放行；其余取服务端 `error` 原文（超长截断，防代理页淹没提示）。
fn check_status(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(format!("空间请求被拒（{status}）：{message}"))
}

impl SessionIo for SpaceSessionIo {
    fn vault_key(&self) -> String {
        format!("{}/{}", self.base, self.space_id)
    }

    fn list_sessions(&self) -> Result<Vec<ChatSessionRow>, String> {
        let values = self.meta_values()?;
        let mut rows: Vec<ChatSessionRow> = values
            .keys()
            .filter_map(|key| key.strip_prefix(META_MESSAGES_PREFIX).filter(|id| !id.is_empty()))
            .map(|id| ChatSessionRow {
                id: id.to_string(),
                file: format!("{CHAT_HISTORY_DIR}/{id}{CHAT_MESSAGE_EXT}"),
                meta: values
                    .get(&format!("{META_SESSIONS_PREFIX}{id}"))
                    .and_then(|raw| serde_json::from_str::<ChatSessionMeta>(raw).ok()),
            })
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    fn read_messages(&self, file: &str) -> Result<String, String> {
        let id = message_file_id(file)?;
        self.meta_get(&format!("{META_MESSAGES_PREFIX}{id}"))?
            .ok_or_else(|| format!("会话消息不存在：{file}"))
    }

    fn write_messages(&self, file: &str, content: &str) -> Result<(), String> {
        let id = message_file_id(file)?;
        self.meta_put(&format!("{META_MESSAGES_PREFIX}{id}"), &with_trailing_newline(content))
    }

    fn append_messages(&self, file: &str, base: &str, tail: &str) -> Result<(), String> {
        let id = message_file_id(file)?;
        let key = format!("{META_MESSAGES_PREFIX}{id}");
        let existing = self.meta_get(&key)?.unwrap_or_default();
        if !matches_append_base(&existing, base) {
            // 基线漂移（多为另一设备对同一会话追加了消息）：按约定回落全量重写，
            // 以本进程内存视图为准（写链对 Err 的统一处置）；服务端无原子追加契约，无法两全
            return Err("空间内容与追加基线不一致".to_string());
        }
        // 元数据无追加语义：整值写回「基线 + 新增行」，落盘形状与本地追加一致
        self.meta_put(&key, &appended_body(base, tail))
    }

    fn read_session_meta(&self, file: &str) -> Result<Option<ChatSessionMeta>, String> {
        let id = meta_file_id(file)?;
        Ok(self
            .meta_get(&format!("{META_SESSIONS_PREFIX}{id}"))?
            .and_then(|raw| serde_json::from_str::<ChatSessionMeta>(&raw).ok()))
    }

    fn write_session_meta(&self, file: &str, meta: &ChatSessionMeta) -> Result<(), String> {
        let id = meta_file_id(file)?;
        let value = serde_json::to_string(meta).map_err(|e| e.to_string())?;
        self.meta_put(&format!("{META_SESSIONS_PREFIX}{id}"), &value)
    }

    fn delete_session_files(&self, file: &str) -> Result<(), String> {
        let id = message_file_id(file)?;
        // 先删消息键（存在性真相），再删元数据键；两者都幂等
        self.meta_delete(&format!("{META_MESSAGES_PREFIX}{id}"))?;
        self.meta_delete(&format!("{META_SESSIONS_PREFIX}{id}"))
    }

    fn read_overrides(&self) -> Result<ChatMetaFile, String> {
        let Some(raw) = self.meta_get(META_EDITOR)? else {
            return Ok(ChatMetaFile::default());
        };
        let file: ChatMetaFile = serde_json::from_str(&raw).unwrap_or_default();
        if file.schema != EDITOR_CHATS_META_SCHEMA {
            return Ok(ChatMetaFile::default());
        }
        Ok(file)
    }

    fn write_overrides(&self, file: &ChatMetaFile) -> Result<(), String> {
        let value = serde_json::to_string(file).map_err(|e| e.to_string())?;
        self.meta_put(META_EDITOR, &value)
    }
}

// ===== 构造入口 =====

/// 本地仓库真源。
pub fn local_io(root: impl Into<PathBuf>) -> Arc<dyn SessionIo> {
    Arc::new(LocalSessionIo::new(root))
}

/// 协作空间真源（用户元数据）。
pub fn space_io(
    server_url: &str,
    token: String,
    space_id: String,
) -> Result<Arc<dyn SessionIo>, String> {
    Ok(Arc::new(SpaceSessionIo::new(server_url, token, space_id)?))
}

/// 会话元数据文件路径（由消息文件路径派生）。
pub fn session_meta_file(message_file: &str) -> Result<String, String> {
    meta_file_for(message_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_derived_from_the_canonical_paths() {
        let message_file = format!("{CHAT_HISTORY_DIR}/s1{CHAT_MESSAGE_EXT}");
        let meta_file = format!("{CHAT_HISTORY_DIR}/s1{CHAT_META_EXT}");
        assert_eq!(message_file_id(&message_file).unwrap(), "s1");
        assert_eq!(meta_file_id(&meta_file).unwrap(), "s1");
        assert_eq!(session_meta_file(&message_file).unwrap(), meta_file);
        // 会话文件只允许出现在对话历史目录下（防越权读写）
        assert!(message_file_id(&format!("notes/s1{CHAT_MESSAGE_EXT}")).is_err());
        assert!(meta_file_id(&format!("{CHAT_HISTORY_DIR}/s1.md")).is_err());
    }

    #[test]
    fn append_shapes_match_the_local_primitive() {
        assert!(matches_append_base("", ""));
        assert!(matches_append_base("a\n", "a"));
        assert!(!matches_append_base("b\n", "a"));
        assert!(!matches_append_base("", "a"));
        assert_eq!(appended_body("", "x\n"), "x\n");
        assert_eq!(appended_body("a", "x\n"), "a\nx\n");
        assert_eq!(with_trailing_newline("a"), "a\n");
        assert_eq!(with_trailing_newline("a\n"), "a\n");
    }
}
