//! 协作空间服务端 HTTP 客户端：全部端点类型化收口，Bearer 令牌鉴权。
//! 错误体 `{"error":"<信息>"}` 按状态码映射 `SpaceError`；401 统一判会话过期由调用方收口。

use crate::net_guard::{self, ClientTimeout, HostPolicy};
use crate::vault::{CanvasPatch, TablePatch};
use base64::Engine;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 请求超时：连接 10s；读超时按次读取计（大文件传输不受总时长钳制）。
const CONNECT_SECS: u64 = 10;
const READ_SECS: u64 = 60;

/// 服务端角色三档（与磁盘口径一致的小写字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceRole {
    Owner,
    Editor,
    Viewer,
}

impl SpaceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceRole::Owner => "owner",
            SpaceRole::Editor => "editor",
            SpaceRole::Viewer => "viewer",
        }
    }

    /// 写能力裁决（服务端同口径：owner/editor 可写，viewer 一律 403）。
    pub fn can_write(self) -> bool {
        self != SpaceRole::Viewer
    }
}

/// 空间角色字段解析（服务端以小写字符串存角色，未知值按只读处理）。
fn parse_role(raw: &str) -> SpaceRole {
    match raw {
        "owner" => SpaceRole::Owner,
        "editor" => SpaceRole::Editor,
        _ => SpaceRole::Viewer,
    }
}

#[derive(Debug, Clone, Error)]
pub enum SpaceError {
    /// 401：令牌失效或被吊销（登录路径为凭据错误），信息取服务端原文。
    #[error("{0}")]
    Unauthorized(String),
    /// 403：非成员或角色不足。
    #[error("权限不足（{0}）")]
    Forbidden(String),
    #[error("内容不存在")]
    NotFound,
    /// 409：目标已存在等冲突。
    #[error("{0}")]
    Conflict(String),
    /// 400：参数非法或超限（含服务端返回的信息）。
    #[error("{0}")]
    BadRequest(String),
    /// 410：邀请码过期。
    #[error("邀请码已过期或已失效")]
    Gone,
    /// 5xx：服务端错误（原文带回，调用方按可重试处理）。
    #[error("服务端错误：{0}")]
    Server(String),
    #[error("网络错误：{0}")]
    Network(String),
}

/// 登录 / 注册响应（令牌原文仅此一次出现，调用方须立即交密钥链保存）。
/// Debug 手写脱敏：令牌与字段原文不进日志。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfo {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub token: String,
    pub session_id: String,
}

impl std::fmt::Debug for AuthInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthInfo").field("username", &self.username).finish_non_exhaustive()
    }
}

/// 设备会话条目（`current` = 本机当前会话）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEntry {
    pub id: String,
    pub device_name: String,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub current: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceSummary {
    pub space_id: String,
    pub name: String,
    #[serde(deserialize_with = "role_from_string")]
    pub role: SpaceRole,
    pub owner_user_id: String,
    pub created_at: i64,
}

fn role_from_string<'de, D>(deserializer: D) -> Result<SpaceRole, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(parse_role(&raw))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInfo {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    #[serde(deserialize_with = "role_from_string")]
    pub role: SpaceRole,
    pub joined_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteInfo {
    pub code: String,
    #[serde(deserialize_with = "role_from_string")]
    pub role: SpaceRole,
    /// 过期时间（unix 秒）；`None` = 长期有效。
    pub expires_at: Option<i64>,
    pub max_uses: Option<u32>,
    pub used_count: u32,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptAck {
    pub space_id: String,
    pub name: String,
    #[serde(deserialize_with = "role_from_string")]
    pub role: SpaceRole,
}

/// 服务端文件树节点（按名排序、目录优先；隐藏 `.` 目录不出现）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub updated_at: i64,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchAck {
    pub updated_at: i64,
    /// 标题漂移后的新相对路径（未改名 = 原路径）。
    pub file: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinkEntry {
    pub file: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagEntry {
    pub tag: String,
    pub count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobResult {
    pub paths: Vec<String>,
    pub total: u64,
    pub capped: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,
    pub total: u64,
    pub capped: bool,
}

/// 文件夹删除回执：非空且未带 force 时 `needs_confirm = true`（附条目数），否则已删除。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderDeleteAck {
    /// 仅确认询问响应携带；落地响应缺省（serde default 防误判失败）。
    #[serde(default)]
    pub needs_confirm: bool,
    /// 服务端仅在删除落地的响应携带；needsConfirm 响应缺省。
    #[serde(default)]
    pub deleted: bool,
    pub item_count: u64,
}

/// 团队 / 个人层元数据键值（value 为 UTF-8 文本原文）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaValues {
    #[serde(default)]
    pub values: std::collections::HashMap<String, String>,
}

/// 历史记录入参（判据与意图交服务端，在其每路径串行锁内判重 / 合并 / 剪枝）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecordReq {
    pub kind: String,
    pub file: String,
    pub content: String,
    pub action: String,
    pub author: HistoryAuthorReq,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesce_edit_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_versions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAuthorReq {
    pub id: String,
    pub name: String,
    pub device: String,
}

/// 历史聚合条目（不含全文）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFeedEntry {
    pub file: String,
    pub kind: String,
    pub ts: i64,
    pub author_id: String,
    pub author_name: String,
    pub author_device: String,
    pub action: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAggregate {
    pub entries: Vec<HistoryFeedEntry>,
    pub timestamps: Vec<i64>,
}

/// 规整用户输入的服务器地址：缺协议补 `http://`，去尾斜杠；显式的非 http/https 协议
/// 原样保留，交地址校验拒绝。
pub fn normalize_server_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

/// 协作空间服务端客户端（可克隆共享）。
/// Debug 输出脱敏：令牌不进日志（`AuthInfo` 同）。
pub struct SpaceClient {
    base: Url,
    http: reqwest::Client,
    token: String,
}

impl std::fmt::Debug for SpaceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceClient").field("base", &self.base).finish_non_exhaustive()
    }
}

impl SpaceClient {
    /// 按服务器地址构建客户端：地址经本机/局域网策略校验（SSRF 防线收口）。
    pub fn new(server_url: &str, token: String) -> Result<Self, String> {
        let base = net_guard::ensure_local_service_http_url(&normalize_server_url(server_url))?;
        let http = net_guard::client_for(HostPolicy::LocalService, ClientTimeout::ConnectRead {
            connect: CONNECT_SECS,
            read: READ_SECS,
        })?;
        Ok(SpaceClient { base, http, token })
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        // base 序列化恒带尾斜杠（http://host/），直接拼接路径段会产生双斜杠（路由 404）
        let base = self.base.as_str().trim_end_matches('/');
        format!("{base}{path}")
    }

    /// 统一请求出口：挂鉴权头，响应按状态码映射 `SpaceError`，错误体信息一并带回。
    async fn send(&self, method: reqwest::Method, path: &str) -> Result<reqwest::Response, SpaceError> {
        let resp = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| SpaceError::Network(e.to_string()))?;
        check_status(resp).await
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response, SpaceError> {
        self.send(reqwest::Method::GET, path).await
    }

    async fn send_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, SpaceError> {
        let req = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&self.token);
        let req = match body {
            Some(b) => req.json(b),
            None => req,
        };
        let resp = req.send().await.map_err(|e| SpaceError::Network(e.to_string()))?;
        let resp = check_status(resp).await?;
        resp.json::<T>().await.map_err(|e| SpaceError::Network(format!("响应解析失败：{e}")))
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SpaceError> {
        self.send_json(reqwest::Method::POST, path, Some(body)).await
    }

    // ---------- 账号 ----------

    pub async fn register(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
        device_name: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<AuthInfo, SpaceError> {
        self.post_json(
            "/api/auth/register",
            &serde_json::json!({
                "username": username,
                "password": password,
                "displayName": display_name,
                "deviceName": device_name,
                "deviceId": device_id,
            }),
        )
        .await
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
        device_name: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<AuthInfo, SpaceError> {
        self.post_json(
            "/api/auth/login",
            &serde_json::json!({
                "username": username,
                "password": password,
                "deviceName": device_name,
                "deviceId": device_id,
            }),
        )
        .await
    }

    pub async fn logout(&self) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(reqwest::Method::POST, "/api/auth/logout", None)
            .await
            .map(|_| ())
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceEntry>, SpaceError> {
        self.send_json(reqwest::Method::GET, "/api/auth/devices", None::<&serde_json::Value>).await
    }

    pub async fn revoke_device(&self, session_id: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::DELETE,
            &format!("/api/auth/devices/{session_id}"),
            None,
        )
        .await
        .map(|_| ())
    }

    // ---------- 空间与成员 ----------

    pub async fn list_spaces(&self) -> Result<Vec<SpaceSummary>, SpaceError> {
        self.send_json(reqwest::Method::GET, "/api/spaces", None::<&serde_json::Value>).await
    }

    pub async fn create_space(&self, name: &str) -> Result<SpaceSummary, SpaceError> {
        self.post_json("/api/spaces", &serde_json::json!({ "name": name }))
            .await
    }

    pub async fn rename_space(&self, space_id: &str, name: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::PATCH,
            &format!("/api/spaces/{space_id}"),
            Some(&serde_json::json!({ "name": name })),
        )
        .await
        .map(|_| ())
    }

    pub async fn transfer_space(&self, space_id: &str, to_user_id: &str) -> Result<(), SpaceError> {
        self.post_json::<serde_json::Value, serde_json::Value>(
            &format!("/api/spaces/{space_id}/transfer"),
            &serde_json::json!({ "toUserId": to_user_id }),
        )
        .await
        .map(|_| ())
    }

    pub async fn list_members(&self, space_id: &str) -> Result<Vec<MemberInfo>, SpaceError> {
        self.send_json(reqwest::Method::GET, &format!("/api/spaces/{space_id}/members"), None::<&serde_json::Value>)
            .await
    }

    pub async fn remove_member(&self, space_id: &str, user_id: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::DELETE,
            &format!("/api/spaces/{space_id}/members/{user_id}"),
            None,
        )
        .await
        .map(|_| ())
    }

    /// 自行退出空间（服务端 `me` 语义）。
    pub async fn leave_space(&self, space_id: &str) -> Result<(), SpaceError> {
        self.remove_member(space_id, "me").await
    }

    pub async fn list_invites(&self, space_id: &str) -> Result<Vec<InviteInfo>, SpaceError> {
        self.send_json(reqwest::Method::GET, &format!("/api/spaces/{space_id}/invites"), None::<&serde_json::Value>)
            .await
    }

    pub async fn create_invite(
        &self,
        space_id: &str,
        role: SpaceRole,
        expires_in_hours: Option<i64>,
        max_uses: Option<u32>,
    ) -> Result<InviteInfo, SpaceError> {
        self.post_json(
            &format!("/api/spaces/{space_id}/invites"),
            &serde_json::json!({
                "role": role.as_str(),
                "expiresInHours": expires_in_hours,
                "maxUses": max_uses,
            }),
        )
        .await
    }

    pub async fn revoke_invite(&self, space_id: &str, code: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::DELETE,
            &format!("/api/spaces/{space_id}/invites/{code}"),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn accept_invite(&self, code: &str) -> Result<AcceptAck, SpaceError> {
        self.post_json("/api/invites/accept", &serde_json::json!({ "code": code }))
            .await
    }

    // ---------- 内容 ----------

    pub async fn tree(&self, space_id: &str) -> Result<Vec<TreeNode>, SpaceError> {
        self.send_json(reqwest::Method::GET, &format!("/api/spaces/{space_id}/tree"), None::<&serde_json::Value>)
            .await
    }

    /// 读文本文件（非 UTF-8 字节按替换字符容错；encoding 缺省即文本，服务端无 text 值）。
    pub async fn get_file_text(&self, space_id: &str, path: &str) -> Result<String, SpaceError> {
        let resp = self
            .get(&format!(
                "/api/spaces/{space_id}/file?path={}",
                urlencode_component(path)
            ))
            .await?;
        let text = resp.text().await.map_err(|e| SpaceError::Network(e.to_string()))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| SpaceError::Network(format!("响应解析失败：{e}")))?;
        Ok(parsed
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }

    /// 读二进制文件（`encoding=base64`，服务端返回标准 base64，此处解码为原始字节）。
    pub async fn get_file_bytes(&self, space_id: &str, path: &str) -> Result<Vec<u8>, SpaceError> {
        let resp = self
            .get(&format!(
                "/api/spaces/{space_id}/file?path={}&encoding=base64",
                urlencode_component(path)
            ))
            .await?;
        #[derive(Deserialize)]
        struct FileBody {
            content: String,
        }
        let body: FileBody = resp.json().await.map_err(|e| SpaceError::Network(format!("响应解析失败：{e}")))?;
        base64::engine::general_purpose::STANDARD
            .decode(body.content)
            .map_err(|e| SpaceError::Network(format!("base64 解码失败：{e}")))
    }

    /// 写文本文件（原子写，自动建父目录；单文件 50MB 限额由服务端强制）。
    pub async fn put_file_text(&self, space_id: &str, path: &str, content: &str) -> Result<(), SpaceError> {
        self.put_file_raw(space_id, path, content.as_bytes(), false).await
    }

    /// 写二进制文件（base64 编码上行）。
    pub async fn put_file_bytes(&self, space_id: &str, path: &str, bytes: &[u8]) -> Result<(), SpaceError> {
        self.put_file_raw(space_id, path, bytes, true).await
    }

    async fn put_file_raw(&self, space_id: &str, path: &str, bytes: &[u8], base64_enc: bool) -> Result<(), SpaceError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WriteAck {
            #[allow(dead_code)]
            updated_at: i64,
        }
        let content = if base64_enc {
            base64::engine::general_purpose::STANDARD.encode(bytes)
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        self.send_json::<WriteAck, serde_json::Value>(
            reqwest::Method::PUT,
            &format!("/api/spaces/{space_id}/file"),
            Some(&match base64_enc {
                // 二进制走 base64；文本不传 encoding（服务端缺省即文本，无 text 值）
                true => serde_json::json!({ "path": path, "content": content, "encoding": "base64" }),
                false => serde_json::json!({ "path": path, "content": content }),
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_file(&self, space_id: &str, path: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::DELETE,
            &format!("/api/spaces/{space_id}/file?path={}", urlencode_component(path)),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn rename_file(&self, space_id: &str, old_path: &str, new_path: &str) -> Result<(), SpaceError> {
        self.post_json::<serde_json::Value, serde_json::Value>(
            &format!("/api/spaces/{space_id}/rename"),
            &serde_json::json!({ "oldPath": old_path, "newPath": new_path }),
        )
        .await
        .map(|_| ())
    }

    pub async fn copy_file(&self, space_id: &str, from_path: &str, to_path: &str) -> Result<(), SpaceError> {
        self.post_json::<serde_json::Value, serde_json::Value>(
            &format!("/api/spaces/{space_id}/copy"),
            &serde_json::json!({ "fromPath": from_path, "toPath": to_path }),
        )
        .await
        .map(|_| ())
    }

    pub async fn create_folder(&self, space_id: &str, path: &str) -> Result<(), SpaceError> {
        self.post_json::<serde_json::Value, serde_json::Value>(
            &format!("/api/spaces/{space_id}/folder"),
            &serde_json::json!({ "path": path }),
        )
        .await
        .map(|_| ())
    }

    /// 删文件夹：`force = false` 时非空目录返回 `needs_confirm = true` 与条目数。
    pub async fn delete_folder(&self, space_id: &str, path: &str, force: bool) -> Result<FolderDeleteAck, SpaceError> {
        self.send_json(
            reqwest::Method::DELETE,
            &format!("/api/spaces/{space_id}/folder"),
            Some(&serde_json::json!({ "path": path, "force": force })),
        )
        .await
    }

    /// 表格增量补丁：服务端按稳定 id 合并、title 漂移算新路径；补丁 id 与文件 id 不符 → 400。
    pub async fn table_patch(&self, space_id: &str, path: &str, patch: &TablePatch) -> Result<PatchAck, SpaceError> {
        self.post_json(
            &format!("/api/spaces/{space_id}/patches/table"),
            &serde_json::json!({ "path": path, "patch": patch }),
        )
        .await
    }

    /// 画布增量补丁：服务端按稳定 id 合并、title 漂移算新路径；补丁 id 与文件 id 不符 → 400。
    pub async fn canvas_patch(&self, space_id: &str, path: &str, patch: &CanvasPatch) -> Result<PatchAck, SpaceError> {
        self.post_json(
            &format!("/api/spaces/{space_id}/patches/canvas"),
            &serde_json::json!({ "path": path, "patch": patch }),
        )
        .await
    }

    // ---------- 索引与检索 ----------

    pub async fn glob(&self, space_id: &str, pattern: &str, path: Option<&str>) -> Result<GlobResult, SpaceError> {
        self.post_json(
            &format!("/api/spaces/{space_id}/glob"),
            &serde_json::json!({ "pattern": pattern, "path": path }),
        )
        .await
    }

    pub async fn grep(
        &self,
        space_id: &str,
        pattern: &str,
        path: Option<&str>,
        include: Option<&str>,
    ) -> Result<GrepResult, SpaceError> {
        self.post_json(
            &format!("/api/spaces/{space_id}/grep"),
            &serde_json::json!({ "pattern": pattern, "path": path, "include": include }),
        )
        .await
    }

    pub async fn backlinks(&self, space_id: &str, note_name: &str, note_file: &str) -> Result<Vec<BacklinkEntry>, SpaceError> {
        let resp = self
            .get(&format!(
                "/api/spaces/{space_id}/backlinks?noteName={}&noteFile={}",
                urlencode_component(note_name),
                urlencode_component(note_file)
            ))
            .await?;
        parse_json(resp).await
    }

    pub async fn tags(&self, space_id: &str) -> Result<Vec<TagEntry>, SpaceError> {
        let resp = self.get(&format!("/api/spaces/{space_id}/tags")).await?;
        parse_json(resp).await
    }

    // ---------- 历史 ----------

    pub async fn history_record(&self, space_id: &str, req: &HistoryRecordReq) -> Result<(), SpaceError> {
        self.post_json::<serde_json::Value, HistoryRecordReq>(
            &format!("/api/spaces/{space_id}/history/record"),
            req,
        )
        .await
        .map(|_| ())
    }

    pub async fn history_aggregate(&self, space_id: &str) -> Result<HistoryAggregate, SpaceError> {
        self.send_json(reqwest::Method::GET, &format!("/api/spaces/{space_id}/history/aggregate"), None::<&serde_json::Value>)
            .await
    }

    /// 读历史版本侧文件（与服务端同一落点与编码约定；缺失 / 损坏 = 空列表，历史尽力而为）。
    pub async fn history_versions(
        &self,
        space_id: &str,
        kind: crate::history::HistoryKind,
        file: &str,
    ) -> Result<Vec<crate::history::HistoryVersion>, SpaceError> {
        let side_path = crate::history::history_path_for(kind, file);
        match self.get_file_text(space_id, &side_path).await {
            Ok(raw) => {
                #[derive(Deserialize)]
                struct HistoryFile {
                    #[serde(default)]
                    versions: Vec<crate::history::HistoryVersion>,
                }
                Ok(serde_json::from_str::<HistoryFile>(&raw)
                    .map(|f| f.versions)
                    .unwrap_or_default())
            }
            Err(SpaceError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    // ---------- 元数据 ----------

    async fn meta_read(&self, space_id: &str, layer: &str, key: Option<&str>) -> Result<MetaValues, SpaceError> {
        let query = match key {
            Some(k) => format!("?key={}", urlencode_component(k)),
            None => String::new(),
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawMeta {
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            values: std::collections::HashMap<String, String>,
        }
        let raw: RawMeta = self
            .send_json(reqwest::Method::GET, &format!("/api/spaces/{space_id}/meta{layer}{query}"), None::<&serde_json::Value>)
            .await?;
        let values = match raw.value {
            Some(v) => [(key.unwrap_or_default().to_string(), v)].into_iter().collect(),
            None => raw.values,
        };
        Ok(MetaValues { values })
    }

    async fn meta_write(&self, space_id: &str, layer: &str, values: &[(String, String)]) -> Result<(), SpaceError> {
        let map: serde_json::Map<String, serde_json::Value> = values
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::PATCH,
            &format!("/api/spaces/{space_id}/meta{layer}"),
            Some(&serde_json::json!({ "values": map })),
        )
        .await
        .map(|_| ())
    }

    async fn meta_delete(&self, space_id: &str, layer: &str, key: &str) -> Result<(), SpaceError> {
        self.send_json::<serde_json::Value, serde_json::Value>(
            reqwest::Method::DELETE,
            &format!("/api/spaces/{space_id}/meta{layer}?key={}", urlencode_component(key)),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn team_meta_get(&self, space_id: &str, key: Option<&str>) -> Result<MetaValues, SpaceError> {
        self.meta_read(space_id, "", key).await
    }

    pub async fn team_meta_patch(&self, space_id: &str, values: &[(String, String)]) -> Result<(), SpaceError> {
        self.meta_write(space_id, "", values).await
    }

    pub async fn team_meta_delete(&self, space_id: &str, key: &str) -> Result<(), SpaceError> {
        self.meta_delete(space_id, "", key).await
    }

    pub async fn my_meta_get(&self, space_id: &str, key: Option<&str>) -> Result<MetaValues, SpaceError> {
        self.meta_read(space_id, "/me", key).await
    }

    pub async fn my_meta_patch(&self, space_id: &str, values: &[(String, String)]) -> Result<(), SpaceError> {
        self.meta_write(space_id, "/me", values).await
    }

    pub async fn my_meta_delete(&self, space_id: &str, key: &str) -> Result<(), SpaceError> {
        self.meta_delete(space_id, "/me", key).await
    }
}

/// 状态码 → `SpaceError`：2xx 原样返回；错误体 `{"error"}` 信息带回（超长截断，防网关 HTML 淹没提示）。
async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, SpaceError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let info = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&info)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or(info);
    let msg = msg.chars().take(200).collect::<String>();
    Err(match status.as_u16() {
        401 => SpaceError::Unauthorized(if msg.is_empty() { "会话已失效".to_string() } else { msg }),
        403 => SpaceError::Forbidden(msg),
        404 => SpaceError::NotFound,
        409 => SpaceError::Conflict(msg),
        410 => SpaceError::Gone,
        code if code >= 500 => SpaceError::Server(msg),
        _ => SpaceError::BadRequest(msg),
    })
}

async fn parse_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, SpaceError> {
    resp.json::<T>().await.map_err(|e| SpaceError::Network(format!("响应解析失败：{e}")))
}

/// 查询参数段转义：非 RFC 3986 unreserved 字符按字节 `%XX` 编码。
pub(crate) fn urlencode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单请求 JSON 假服务端：读完整请求头后按给定响应体回 200（回执解码回归用）。
    fn spawn_json_server(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("回环端口可绑");
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("客户端会连入");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let mut stream = reader.into_inner();
            use std::io::Write as _;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).expect("响应写出");
        });
        (format!("http://{addr}"), server)
    }

    #[test]
    fn 地址规整补协议去尾斜杠() {
        assert_eq!(normalize_server_url("192.168.1.5:11224"), "http://192.168.1.5:11224");
        assert_eq!(normalize_server_url("http://a.b/"), "http://a.b");
        assert_eq!(normalize_server_url(" https://s.io "), "https://s.io");
    }

    #[test]
    fn 角色写能力与解析() {
        assert!(SpaceRole::Owner.can_write() && SpaceRole::Editor.can_write());
        assert!(!SpaceRole::Viewer.can_write());
        assert_eq!(parse_role("editor"), SpaceRole::Editor);
        assert_eq!(parse_role("unknown"), SpaceRole::Viewer);
    }

    #[test]
    fn 客户端构建拒绝非法地址() {
        // 链路本地（云元数据）必须被拒
        assert!(SpaceClient::new("http://169.254.169.254:11224", String::new()).is_err());
        assert!(SpaceClient::new("ftp://nope", String::new()).is_err());
    }

    #[test]
    fn 客户端构建放行回环与公网字面量() {
        assert!(SpaceClient::new("127.0.0.1:11224", String::new()).is_ok());
        assert!(SpaceClient::new("https://space.example.com", String::new()).is_ok());
    }

    /// 鉴权头回归：`bearer_auth` 自带 Bearer 前缀，令牌不得再手工拼一次
    /// （双重前缀会被服务端剥层后当作令牌原文，所有鉴权端点 401）。
    #[tokio::test]
    async fn 鉴权头为单层bearer() {
        // 回环监听：收一条请求，读出 Authorization 头后即关闭
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("回环端口可绑");
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("客户端会连入");
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("Authorization") {
                        return value.trim().to_string();
                    }
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            String::new()
        });
        let client = SpaceClient::new(&format!("http://{addr}"), "tok123".to_string()).expect("回环放行");
        let _ = client.list_spaces().await;
        let header = server.join().expect("抓头线程不 panic");
        assert_eq!(header, "Bearer tok123");
    }

    /// 写文件回执按服务端真实形状（camelCase）解码：字段名不匹配会把落盘成功当失败上报。
    #[tokio::test]
    async fn 写文件回执按服务端键名解码() {
        let (base, server) = spawn_json_server(r#"{"updatedAt": 123}"#);
        let client = SpaceClient::new(&base, "tok".to_string()).expect("回环放行");
        client
            .put_file_text("s1", "notes/a.md", "正文")
            .await
            .expect("camelCase 回执应可解码");
        server.join().expect("服务端线程不 panic");
    }

    /// 删文件夹落地回执不含 needsConfirm（仅确认询问响应携带）：缺省按 false 解码。
    #[tokio::test]
    async fn 删文件夹落地回执缺确认键可解码() {
        let (base, server) = spawn_json_server(r#"{"deleted": true, "itemCount": 2}"#);
        let client = SpaceClient::new(&base, "tok".to_string()).expect("回环放行");
        let ack = client
            .delete_folder("s1", "notes", true)
            .await
            .expect("落地回执应可解码");
        assert!(!ack.needs_confirm);
        assert!(ack.deleted);
        assert_eq!(ack.item_count, 2);
        server.join().expect("服务端线程不 panic");
    }
}
