//! 会话容器真源：唯一持有容器内存态与写盘链——op 应用、增量产出、seq 广播、
//! 防抖落盘都在本模块。撕裂窗口经快照命令拉基线、apply 命令提交写意图；执行体（主窗口）经
//! commit 提交编排变更；执行 op（发送/重生成/压缩/命名等）经意图转发主窗口执行。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
// 仅生产构建引入：emit 的实例化收敛在 install_app_emitter，测试 exe 的链接闭包
// 因此不携带 Tauri 运行时的 GUI 依赖（tao → comctl32 v6 + manifest 要求）
#[cfg(not(test))]
use tauri::{AppHandle, Emitter};

use crate::vault::{
    append_chat_messages_raw, delete_chat_messages_file, delete_chat_session_meta_file,
    list_chat_sessions_file, read_chat_messages_file, read_editor_chats_meta_file,
    write_chat_messages_file, write_chat_session_meta_file, write_editor_chats_meta_file,
    ChatMetaFile, ChatSessionMeta, EditorChatModelOverride, CHAT_HISTORY_DIR, CHAT_MESSAGE_EXT,
    EDITOR_CHATS_META_SCHEMA,
};

/// 容器增量广播事件名（与前端 wire 常量一致；全窗口订阅，seq 单调去重）。
#[cfg(not(test))]
pub const DELTA_EVENT: &str = "chat-container-delta";
/// 执行 op 意图事件（Rust → 主窗口定向；主窗口执行后经 intent_result 回填）。
#[cfg(not(test))]
pub const INTENT_EVENT: &str = "chat-container-intent";
/// 执行体窗口 label（意图只投递给它，commit 只接受它）。
pub const EXECUTOR_LABEL: &str = "main";
/// 参数生成中的合成工具步 id 前缀（流式引擎合成；收敛时 running 态归一为已中断）。
const PENDING_RUN_ID_PREFIX: &str = "pending:";
/// 写链失败的 persistError 文案（同文案重复失败不重复广播）。
const PERSIST_ERROR_MESSAGE: &str = "会话保存失败，将自动重试";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ===== 线上形状（与 services/chatContainerWire.ts 契约逐字段对齐）=====

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PersistError {
    pub message: String,
    pub at: i64,
}

/// 消息增量：upserts 按 id 原位替换或末尾追加；keepCount = 截断为前 N 条。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MessagesDelta {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub upserts: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_count: Option<usize>,
}

/// 一次变更产出的增量片段（seq 由广播时统一分配）。
/// metas 元素为手工构造的对象：完整片段恒带 title/agentId/compaction（null = 清除，
/// 前端按 undefined/null 区分「未变/清除」），移除片段只有 id + removed。
#[derive(Debug, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Fragments {
    pub metas: Vec<Value>,
    pub messages: Vec<MessagesDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Value>,
    pub message_session_ids: Vec<String>,
    pub meta_session_ids: Vec<String>,
    pub deleted_ids: Vec<String>,
}

/// 宿主 → 全部窗口的增量广播。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ContainerDelta {
    pub seq: u64,
    /// 引起本批变更的请求方 id（镜像据此抑制自身回声的插件事件转发）。
    pub op_owners: Vec<String>,
    #[serde(flatten)]
    pub fragments: Fragments,
}

/// 镜像 boot 拉取的容器基线快照（seq = 已广播的最新一拍）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSnapshot {
    pub seq: u64,
    pub session_vault_key: String,
    pub sessions: Vec<Value>,
    pub streaming: bool,
    pub compacting: Option<String>,
    pub persist_error: Option<PersistError>,
    pub model_override: Option<Value>,
    pub effort_override: Option<String>,
}

/// apply 命令的成功响应（失败走 Err = 前端 invoke 拒绝，与 chatContainerWire 的 ok:false 同语义）。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_session_id: Option<String>,
    pub fragments: Fragments,
}

/// 镜像 → 真源的写意图（15 种容器 op；serde tag = kind，与前端 union 同形）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChatOp {
    Send {
        active_session_id: Option<String>,
        forced_session_id: String,
        draft_agent_id: Option<String>,
        content: String,
        refs: Value,
        pendings: Value,
    },
    Regenerate {
        session_id: String,
    },
    Stop,
    Compact {
        session_id: String,
    },
    Rename {
        session_id: String,
    },
    Rollback {
        session_id: String,
        message_id: String,
    },
    Delete {
        session_id: String,
    },
    SetAgentId {
        session_id: String,
        agent_id: Option<String>,
    },
    SetModelOverride {
        ov: Option<Value>,
    },
    SetEffortOverride {
        effort: Option<String>,
    },
    Import {
        messages: Value,
        opts: Option<Value>,
    },
    Append {
        session_id: String,
        messages: Value,
    },
    Create {
        opts: Option<Value>,
    },
    SetTitle {
        session_id: String,
        title: String,
    },
    DeleteExternal {
        session_id: String,
    },
}

impl ChatOp {
    /// 执行 op：需要主窗口的 AI 编排或 TS 侧消息转换（toPanelMessages），真源只转发意图。
    /// 其余为纯持久 op：真源直接应用。
    fn is_execution(&self) -> bool {
        matches!(
            self,
            ChatOp::Send { .. }
                | ChatOp::Regenerate { .. }
                | ChatOp::Stop
                | ChatOp::Compact { .. }
                | ChatOp::Rename { .. }
                | ChatOp::Import { .. }
                | ChatOp::Append { .. }
        )
    }
}

// ===== 执行体提交批次（commit 命令载荷；编排侧容器变更的声明式表达）=====

/// 新会话登记（id 由调用方先行确定：send 的附件临时目录归属依赖会话 id）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDef {
    pub id: String,
    pub file: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub compaction: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub messages: Vec<Value>,
}

/// 既有会话的元数据字段更新（缺省字段不动）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaPatch {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub compaction: Option<Value>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendBatch {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub messages: Vec<Value>,
}

/// 消息内容补丁（rev 单调防乱序：流式补丁 fire-and-forget 过 IPC，迟到的旧补丁不得覆盖新内容）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub rev: u64,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub steps: Option<Value>,
    /// 非空才写入（失败占位语义：`m.content || "[错误] …"`——流式已有内容不被错误文案回退）。
    #[serde(default)]
    pub content_if_empty: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "keepCount")]
    pub keep_count: usize,
}

/// 执行态字段补丁（缺省 = 不动；compacting 双层 Option：外层缺省不动，内层 null = 清除）。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatusPatch {
    #[serde(default)]
    pub streaming: Option<bool>,
    #[serde(default)]
    pub compacting: Option<Option<String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommitBatch {
    #[serde(default)]
    pub created: Vec<SessionDef>,
    #[serde(default)]
    pub metas: Vec<MetaPatch>,
    #[serde(default)]
    pub appends: Vec<AppendBatch>,
    #[serde(default)]
    pub patches: Vec<PatchEntry>,
    #[serde(default)]
    pub truncations: Vec<TruncateEntry>,
    #[serde(default)]
    pub status: StatusPatch,
    /// 需要重写消息 .jsonl 的会话（写链脏标记）。
    #[serde(default)]
    pub dirty_messages: Vec<String>,
    /// 需要重写元数据侧车的会话。
    #[serde(default)]
    pub dirty_meta: Vec<String>,
}

// ===== 真源内部状态 =====

/// 会话内存态（消息保持前端原样 JSON；payload 运行时缓存随形态保留，出线/落盘时剥离）。
#[derive(Clone)]
struct Session {
    id: String,
    file: String,
    title: Option<String>,
    agent_id: Option<String>,
    compaction: Option<Value>,
    created_at: i64,
    updated_at: i64,
    messages: Vec<Value>,
}

/// 消息 .jsonl 的追加基线：上次成功落盘的内容前缀（确定性序列化，字符串比对判定可追加）。
struct PersistedBase {
    len: usize,
    jsonl: String,
}

struct IntentWindow {
    intent_id: String,
    request_id: String,
    fragments: Vec<Fragments>,
}

/// 执行体回填的意图结果（intent_result 命令载荷；status = ok/err）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum IntentResult {
    Ok {
        #[serde(default)]
        value: Option<Value>,
        #[serde(default)]
        created_session_id: Option<String>,
    },
    Err { error: String },
}

struct Inner {
    loaded_root: Option<PathBuf>,
    sessions: Vec<Session>,
    streaming: bool,
    compacting: Option<String>,
    persist_error: Option<PersistError>,
    model_override: Option<Value>,
    effort_override: Option<String>,
    seq: u64,
    // 写链脏集合与基线
    dirty_messages: HashSet<String>,
    dirty_meta: HashSet<String>,
    overrides_dirty: bool,
    persisted: HashMap<String, PersistedBase>,
    /// 会话变更版本（写在途复核：写前捕获、写后比对，变了就保留脏标记由下轮追平）
    change_versions: HashMap<String, u64>,
    overrides_version: u64,
    // 写链调度
    last_mark: Option<Instant>,
    flush_requested: bool,
    retry_attempt: usize,
    next_retry_at: Option<Instant>,
    pending_flush: Vec<tokio::sync::oneshot::Sender<()>>,
    // 意图窗口（执行 op 在途）：期间全部增量归属该意图并累积进响应
    intent_windows: Vec<IntentWindow>,
    /// 意图 id → 回填通道（executor 登记时整体失败回填；intent_result 逐个移除）
    pending_intents: HashMap<String, tokio::sync::oneshot::Sender<IntentResult>>,
    /// 执行体（主窗口）是否已登记（executor_boot 置位；装载重置不影响）
    executor_ready: bool,
    /// 消息补丁 rev（(sessionId, messageId) → 已应用的最大 rev）
    patch_revs: HashMap<(String, String), u64>,
    /// 真源换根纪元（写在途期间换仓库 → 写回结果整体丢弃，防旧仓库基线污染新真源）
    epoch: u64,
}

impl Inner {
    fn status_view(&self) -> StatusView<'_> {
        StatusView {
            streaming: self.streaming,
            compacting: &self.compacting,
            persist_error: &self.persist_error,
            model_override: &self.model_override,
            effort_override: &self.effort_override,
            vault_key: self.vault_key(),
        }
    }

    fn vault_key(&self) -> String {
        session_vault_key(self.loaded_root.as_deref())
    }

    fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    fn has_dirty(&self) -> bool {
        !self.dirty_messages.is_empty() || !self.dirty_meta.is_empty() || self.overrides_dirty
    }

    /// 写链脏标记（每次容器内容变化都必须伴随调用，否则变更只存在于内存）。
    fn mark_session_messages(&mut self, id: &str) {
        self.dirty_messages.insert(id.to_string());
        self.bump_session(id);
    }

    fn mark_session_meta(&mut self, id: &str) {
        self.dirty_meta.insert(id.to_string());
        self.bump_session(id);
    }

    fn bump_session(&mut self, id: &str) {
        let v = self.change_versions.entry(id.to_string()).or_insert(0);
        *v += 1;
        self.last_mark = Some(Instant::now());
    }

    fn mark_overrides(&mut self) {
        self.overrides_dirty = true;
        self.overrides_version += 1;
        self.last_mark = Some(Instant::now());
    }
}

fn session_vault_key(root: Option<&Path>) -> String {
    match root {
        Some(root) => format!("local:{}", root.to_string_lossy()),
        None => String::new(),
    }
}

/// 状态面快照（差分与状态补丁的比对基准；vault_key 为计算值，按值持有）。
struct StatusView<'a> {
    streaming: bool,
    compacting: &'a Option<String>,
    persist_error: &'a Option<PersistError>,
    model_override: &'a Option<Value>,
    effort_override: &'a Option<String>,
    vault_key: String,
}

/// 写链配置（测试注入零延迟；生产 = 500ms 防抖 + 指数退避）。
#[derive(Clone)]
pub struct WriteChainConfig {
    pub debounce: Duration,
    pub retries: Vec<Duration>,
}

impl Default for WriteChainConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(500),
            retries: vec![
                Duration::from_millis(500),
                Duration::from_millis(2000),
                Duration::from_millis(8000),
                Duration::from_millis(30000),
            ],
        }
    }
}

/// 广播事件：delta 为容器增量（全窗口广播），intent 为执行 op 意图（定向投递执行体窗口）。
#[cfg_attr(not(test), allow(dead_code))]
pub enum EmitterEvent {
    Delta(ContainerDelta),
    /// 意图事件载荷：{ intentId, requestId, op }
    Intent(Value),
}

/// 广播出口：事件 → 出口回调。生产侧闭包包装 Tauri emit（全窗口 delta / 定向 intent），
/// 测试侧为收集器。
#[cfg_attr(not(test), allow(dead_code))]
type EmitterFn = Box<dyn Fn(&EmitterEvent) + Send + Sync>;

pub struct ChatContainerState {
    inner: Mutex<Inner>,
    sched: Condvar,
    emitter: Mutex<Option<EmitterFn>>,
    config: WriteChainConfig,
}

impl ChatContainerState {
    pub fn new(config: WriteChainConfig) -> Self {
        Self {
            inner: Mutex::new(Inner {
                loaded_root: None,
                sessions: Vec::new(),
                streaming: false,
                compacting: None,
                persist_error: None,
                model_override: None,
                effort_override: None,
                seq: 0,
                dirty_messages: HashSet::new(),
                dirty_meta: HashSet::new(),
                overrides_dirty: false,
                persisted: HashMap::new(),
                change_versions: HashMap::new(),
                overrides_version: 0,
                last_mark: None,
                flush_requested: false,
                retry_attempt: 0,
                next_retry_at: None,
                pending_flush: Vec::new(),
                intent_windows: Vec::new(),
                pending_intents: HashMap::new(),
                executor_ready: false,
                patch_revs: HashMap::new(),
                epoch: 0,
            }),
            sched: Condvar::new(),
            emitter: Mutex::new(None),
            config,
        }
    }

    /// 安装生产广播出口（Tauri 事件）：delta 全窗口广播、intent 定向投递执行体窗口。
    /// 仅生产构建编译：emit 的单态化实现只在此闭包内被引用——若进测试 exe 的链接闭包，
    /// 会拉入 tao/comctl32 v6 依赖，而测试二进制无 manifest，loader 绑到 v5 缺入口点启动即失败。
    #[cfg(not(test))]
    pub fn install_app_emitter(&self, app: AppHandle) {
        let app = app.clone();
        *self.emitter.lock().unwrap_or_else(poison) =
            Some(Box::new(move |ev: &EmitterEvent| match ev {
                EmitterEvent::Delta(delta) => {
                    let _ = app.emit(DELTA_EVENT, delta);
                }
                EmitterEvent::Intent(payload) => {
                    let _ = app.emit_to(EXECUTOR_LABEL, INTENT_EVENT, payload);
                }
            }));
    }

    #[cfg(test)]
    fn install_collector(&self, f: impl Fn(&EmitterEvent) + Send + Sync + 'static) {
        *self.emitter.lock().unwrap_or_else(poison) = Some(Box::new(f));
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(poison)
    }

    /// seq 单调推进 + 广播（空片段不广播）；片段同时累积进全部在途意图窗口（响应读己之写），
    /// opOwners 并入在途意图的 requestId（发起窗口据此抑制自身回声）。
    fn publish(&self, inner: &mut Inner, fragments: Fragments, request_id: Option<&str>) {
        if fragments_empty(&fragments) {
            return;
        }
        let mut owners: Vec<String> = request_id.map(|r| vec![r.to_string()]).unwrap_or_default();
        for w in inner.intent_windows.iter_mut() {
            if !owners.iter().any(|o| o == &w.request_id) {
                owners.push(w.request_id.clone());
            }
            w.fragments.push(fragments.clone());
        }
        inner.seq += 1;
        let delta = ContainerDelta {
            seq: inner.seq,
            op_owners: owners,
            fragments,
        };
        let emitter = self.emitter.lock().unwrap_or_else(poison);
        if let Some(e) = emitter.as_ref() {
            e(&EmitterEvent::Delta(delta));
        }
    }

    // ===== 快照 / 装载 =====

    pub fn snapshot(&self) -> ContainerSnapshot {
        let inner = self.lock();
        snapshot_of(&inner)
    }

    /// 装载真源（主窗口进仓/切仓调用）：同根幂等；换根先落盘旧仓库在途写，再读盘重建，
    /// 并以全量差分广播重置（旧会话移除 + 新会话登记 + 状态含 sessionVaultKey）。
    pub fn load(&self, root: &Path) -> Result<ContainerSnapshot, String> {
        {
            let inner = self.lock();
            if inner.loaded_root.as_deref() == Some(root) {
                return Ok(snapshot_of(&inner));
            }
        }
        // 换根前先落旧仓库在途写（失败不阻塞切换：错误可见 + 退避重试，与 flush 命令同语义）
        if self.lock().has_dirty() {
            self.run_write_cycle();
        }
        let mut inner = self.lock();
        if inner.loaded_root.as_deref() == Some(root) {
            return Ok(snapshot_of(&inner)); // 并发 load 已完成
        }
        let sessions = scan_sessions(root)?;
        let editor_meta = read_editor_chats_meta_file(root)?;
        let prev = view_clone(&inner);
        inner.epoch += 1;
        inner.loaded_root = Some(root.to_path_buf());
        inner.sessions = sessions;
        inner.streaming = false;
        inner.compacting = None;
        inner.persist_error = None;
        inner.dirty_messages.clear();
        inner.dirty_meta.clear();
        inner.overrides_dirty = false;
        inner.persisted = inner
            .sessions
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    PersistedBase {
                        len: s.messages.len(),
                        jsonl: serialize_messages_jsonl(&s.messages),
                    },
                )
            })
            .collect();
        inner.change_versions.clear();
        inner.patch_revs.clear();
        inner.model_override = editor_meta
            .model_override
            .and_then(|o| serde_json::to_value(o).ok());
        inner.effort_override = editor_meta.effort_override;
        inner.last_mark = None;
        inner.next_retry_at = None;
        inner.retry_attempt = 0;
        let fragments = diff_views(
            &prev.sessions,
            &prev.status,
            &inner.sessions,
            &inner.status_view(),
        );
        self.publish(&mut inner, fragments, None);
        Ok(snapshot_of(&inner))
    }

    // ===== 写意图（apply）=====

    /// 容器 op 统一入口：纯持久 op 直接应用；执行 op 转发主窗口并等待结果（无超时——
    /// 压缩/命名是模型请求；执行体重启时挂起意图被失败回填，不悬挂）。
    pub async fn apply(
        self: &Arc<Self>,
        request_id: String,
        expected_root: String,
        op: ChatOp,
    ) -> Result<ApplyOk, String> {
        if !op.is_execution() {
            return self.apply_pure(&request_id, &expected_root, op);
        }
        let rx = self.begin_intent(&request_id, &expected_root, op)?;
        let result = rx
            .await
            .map_err(|_| "执行体未响应（意图通道已关闭）".to_string())?;
        let mut inner = self.lock();
        let window = match inner
            .intent_windows
            .iter()
            .position(|w| w.request_id == request_id)
        {
            Some(i) => Some(inner.intent_windows.remove(i)),
            None => None,
        };
        match result {
            IntentResult::Ok {
                value,
                created_session_id,
            } => {
                let fragments = window
                    .map(|w| merge_fragment_list(&w.fragments))
                    .unwrap_or_default();
                Ok(ApplyOk {
                    value,
                    created_session_id,
                    fragments,
                })
            }
            IntentResult::Err { error } => Err(error),
        }
    }

    fn begin_intent(
        self: &Arc<Self>,
        request_id: &str,
        expected_root: &str,
        op: ChatOp,
    ) -> Result<tokio::sync::oneshot::Receiver<IntentResult>, String> {
        self.guard_expected_root(expected_root)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut inner = self.lock();
            if !inner.executor_ready {
                return Err("执行体未就绪（主窗口未完成装载）".to_string());
            }
            inner.intent_windows.push(IntentWindow {
                intent_id: uuid_v4(),
                request_id: request_id.to_string(),
                fragments: Vec::new(),
            });
            // 意图与 oneshot 一一对应：intent_id 在 intent_result 时定位 sender
            let intent_id = inner.intent_windows.last().unwrap().intent_id.clone();
            inner.pending_intents.insert(intent_id.clone(), tx);
            let payload = json!({ "intentId": intent_id, "requestId": request_id, "op": op });
            drop(inner);
            self.emit_to_executor(&payload);
        }
        Ok(rx)
    }

    fn emit_to_executor(&self, payload: &Value) {
        let emitter = self.emitter.lock().unwrap_or_else(poison);
        if let Some(e) = emitter.as_ref() {
            e(&EmitterEvent::Intent(payload.clone()));
        }
    }

    /// 主窗口回填执行结果（intentId 定位挂起意图）。回填通道已关闭（apply 调用方取消，
    /// invoke future 被 drop）时同步清理对应意图窗口，防僵尸窗口持续累积增量。
    pub fn intent_result(
        &self,
        intent_id: &str,
        result: IntentResult,
        _executor_label: &str,
    ) -> Result<(), String> {
        let tx = {
            let mut inner = self.lock();
            inner.pending_intents.remove(intent_id)
        };
        let Some(tx) = tx else {
            return Err(format!("意图不存在或已完结：{intent_id}"));
        };
        if tx.send(result).is_err() {
            let mut inner = self.lock();
            inner
                .intent_windows
                .retain(|w| w.intent_id != intent_id);
        }
        Ok(())
    }

    /// 执行体（重）启动登记：失败全部挂起意图（执行体重启 = 在途编排已死）并清空意图
    /// 窗口，复位执行态（streaming/compacting）后广播。
    pub fn executor_boot(&self, executor_label: &str) -> Result<(), String> {
        if executor_label != EXECUTOR_LABEL {
            return Err("仅主窗口可登记执行体".to_string());
        }
        let pending: Vec<_> = {
            let mut inner = self.lock();
            inner.executor_ready = true;
            inner.intent_windows.clear();
            inner.pending_intents.drain().collect()
        };
        for (_, tx) in pending {
            let _ = tx.send(IntentResult::Err {
                error: "执行体重启，操作未完成，请重试".to_string(),
            });
        }
        let mut inner = self.lock();
        let mut status = json!({});
        if inner.streaming {
            status["streaming"] = json!(false);
        }
        if inner.compacting.is_some() {
            status["compacting"] = Value::Null;
        }
        inner.streaming = false;
        inner.compacting = None;
        let fragments = fragments_from_status(status);
        self.publish(&mut inner, fragments, None);
        Ok(())
    }

    // ===== 纯持久 op 应用 =====

    fn apply_pure(
        self: &Arc<Self>,
        request_id: &str,
        expected_root: &str,
        op: ChatOp,
    ) -> Result<ApplyOk, String> {
        let mut inner = self.lock();
        Self::guard_expected_root_locked(&inner, expected_root)?;
        let mut acc = DiffAccumulator::default();
        let value = match &op {
            ChatOp::Create { opts } => {
                let (title, agent_id) = create_opts(opts);
                let id = uuid_v4();
                let now = now_ms();
                let session = Session {
                    id: id.clone(),
                    file: chat_message_file(&id),
                    title,
                    agent_id,
                    compaction: None,
                    created_at: now,
                    updated_at: now,
                    messages: Vec::new(),
                };
                if inner.session(&id).is_some() {
                    return Err(format!("会话已存在：{id}"));
                }
                acc.created(&session);
                inner.mark_session_meta(&id);
                inner.sessions.push(session);
                json!({ "id": id })
            }
            ChatOp::SetTitle { session_id, title } => {
                let session = inner
                    .session_mut(session_id)
                    .ok_or_else(|| format!("会话不存在：{session_id}"))?;
                session.title = Some(title.clone());
                acc.meta_patch(session_id, &MetaPatch {
                    id: session_id.clone(),
                    title: Some(title.clone()),
                    agent_id: None,
                    compaction: None,
                    updated_at: None,
                });
                inner.mark_session_meta(session_id);
                Value::Null
            }
            ChatOp::SetAgentId {
                session_id,
                agent_id,
            } => {
                // 会话缺失静默跳过（与前端 setSessionAgent 的 map 语义一致）
                if let Some(session) = inner.session_mut(session_id) {
                    session.agent_id = agent_id.clone();
                    acc.meta_patch(session_id, &MetaPatch {
                        id: session_id.clone(),
                        title: None,
                        agent_id: agent_id.clone(),
                        compaction: None,
                        updated_at: None,
                    });
                    inner.mark_session_meta(session_id);
                }
                Value::Null
            }
            ChatOp::SetModelOverride { ov } => {
                inner.model_override = ov.clone();
                inner.mark_overrides();
                Value::Null
            }
            ChatOp::SetEffortOverride { effort } => {
                inner.effort_override = effort.clone();
                inner.mark_overrides();
                Value::Null
            }
            ChatOp::Rollback {
                session_id,
                message_id,
            } => {
                // 守卫与前端 rollbackSession 一致：流式/压缩中或目标位置非法时静默不动作
                if !inner.streaming && inner.compacting.is_none() {
                    let Some(session) = inner.session(session_id) else {
                        return Ok(empty_apply());
                    };
                    let idx = session
                        .messages
                        .iter()
                        .position(|m| message_id_of(m).as_deref() == Some(message_id.as_str()));
                    let Some(idx) = idx else {
                        return Ok(empty_apply());
                    };
                    if idx == session.messages.len() - 1 {
                        return Ok(empty_apply());
                    }
                    let keep = idx + 1;
                    let session = inner.session_mut(session_id).unwrap();
                    session.messages.truncate(keep);
                    session.updated_at = now_ms();
                    acc.truncated(session_id, keep);
                    acc.meta_updated_at(session_id, session.updated_at);
                    inner.mark_session_messages(session_id);
                }
                Value::Null
            }
            ChatOp::Delete { .. } | ChatOp::DeleteExternal { .. } => {
                let session_id = match &op {
                    ChatOp::Delete { session_id } | ChatOp::DeleteExternal { session_id } => {
                        session_id.clone()
                    }
                    _ => unreachable!(),
                };
                // 会话缺失静默跳过（幂等；与前端 deleteSession 对不存在 id 的行为一致）
                if let Some(pos) = inner.sessions.iter().position(|s| s.id == session_id) {
                    let session = inner.sessions.remove(pos);
                    inner.dirty_messages.remove(&session_id);
                    inner.dirty_meta.remove(&session_id);
                    inner.persisted.remove(&session_id);
                    inner.change_versions.remove(&session_id);
                    acc.removed(&session_id);
                    drop(inner);
                    // 文件清理即删即落（删除 = 删文件；失败仅影响磁盘残留，下次进仓兜底）
                    if let Some(root) = self.lock().loaded_root.clone() {
                        if let Err(e) = delete_chat_messages_file(&root, &session.file) {
                            eprintln!("删除会话消息文件失败：{e}");
                        }
                        if let Err(e) = delete_chat_session_meta_file(&root, &session.file) {
                            eprintln!("删除会话元数据文件失败：{e}");
                        }
                    }
                    inner = self.lock();
                }
                Value::Null
            }
            _ => return Err("op 须由执行体处理".to_string()),
        };
        let fragments = acc.finish(&inner);
        self.publish(&mut inner, fragments.clone(), Some(request_id));
        let created_session_id = match &op {
            ChatOp::Create { .. } => value.get("id").and_then(Value::as_str).map(str::to_string),
            _ => None,
        };
        Ok(ApplyOk {
            value: Some(value),
            created_session_id,
            fragments,
        })
    }

    fn guard_expected_root(&self, expected_root: &str) -> Result<(), String> {
        let inner = self.lock();
        Self::guard_expected_root_locked(&inner, expected_root)
    }

    /// 锁内复核：guard 与变更分两次取锁之间可能换仓，变更路径须在落锁后复验根。
    fn guard_expected_root_locked(inner: &Inner, expected_root: &str) -> Result<(), String> {
        if inner.loaded_root.is_none() {
            return Err("会话容器真源未就绪，请稍后重试".to_string());
        }
        if inner.vault_key() != expected_root {
            return Err("仓库已切换，本次操作已丢弃".to_string());
        }
        Ok(())
    }

    // ===== 执行体提交（commit）=====

    /// 编排侧容器变更的统一入口：登记会话 / 元数据补丁 / 消息追加 / 内容补丁 / 截断 /
    /// 执行态 / 脏标记，一次应用 + 差分广播。仅执行体窗口可调用。
    pub fn commit(
        &self,
        request_id: &str,
        executor_label: &str,
        expected_root: &str,
        batch: CommitBatch,
    ) -> Result<Fragments, String> {
        if executor_label != EXECUTOR_LABEL {
            return Err("仅执行体窗口可提交容器变更".to_string());
        }
        let mut inner = self.lock();
        Self::guard_expected_root_locked(&inner, expected_root)?;
        let mut acc = DiffAccumulator::default();
        for def in batch.created {
            if inner.session(&def.id).is_some() {
                return Err(format!("会话已存在：{}", def.id));
            }
            let session = Session {
                id: def.id.clone(),
                file: def.file,
                title: def.title,
                agent_id: def.agent_id,
                compaction: def.compaction,
                created_at: def.created_at,
                updated_at: def.updated_at,
                messages: def.messages,
            };
            acc.created(&session);
            inner.mark_session_meta(&def.id);
            if !session.messages.is_empty() {
                inner.mark_session_messages(&def.id);
            }
            inner.sessions.push(session);
        }
        for patch in batch.metas {
            if let Some(session) = inner.session_mut(&patch.id) {
                if patch.title.is_some() {
                    session.title = patch.title.clone();
                }
                if patch.agent_id.is_some() {
                    session.agent_id = patch.agent_id.clone();
                }
                if patch.compaction.is_some() {
                    session.compaction = patch.compaction.clone();
                }
                if let Some(at) = patch.updated_at {
                    session.updated_at = at;
                }
                acc.meta_patch(&patch.id, &patch);
                inner.mark_session_meta(&patch.id);
            }
        }
        for append in batch.appends {
            let Some(session) = inner.session_mut(&append.session_id) else {
                continue; // 会话已删：丢弃（与前端 patchSessionMessage 的存在性守卫一致）
            };
            session.messages.extend(append.messages.iter().cloned());
            acc.appended(&append.session_id, &append.messages);
            inner.mark_session_messages(&append.session_id);
        }
        for patch in batch.patches {
            let key = (patch.session_id.clone(), patch.message_id.clone());
            let applied_rev = inner.patch_revs.get(&key).copied().unwrap_or(0);
            if patch.rev < applied_rev {
                continue; // 迟到的旧补丁（fire-and-forget 过 IPC 乱序）不得覆盖新内容
            }
            let Some(idx) = inner
                .sessions
                .iter()
                .position(|s| s.id == patch.session_id)
            else {
                continue;
            };
            let Some(mi) = inner.sessions[idx]
                .messages
                .iter()
                .position(|m| message_id_of(m).as_deref() == Some(patch.message_id.as_str()))
            else {
                continue;
            };
            let message = &mut inner.sessions[idx].messages[mi];
            let mut changed = false;
            if let Some(content) = &patch.content {
                message["content"] = json!(content);
                changed = true;
            }
            if let Some(steps) = &patch.steps {
                message["steps"] = steps.clone();
                changed = true;
            }
            if let Some(fallback) = &patch.content_if_empty {
                let empty = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty();
                if empty {
                    message["content"] = json!(fallback);
                    changed = true;
                }
            }
            if changed {
                inner.patch_revs.insert(key, patch.rev);
                let message = &inner.sessions[idx].messages[mi];
                acc.patched(&patch.session_id, message);
                inner.mark_session_messages(&patch.session_id);
            }
        }
        for trunc in batch.truncations {
            let Some(session) = inner.session_mut(&trunc.session_id) else {
                continue;
            };
            if trunc.keep_count >= session.messages.len() {
                continue;
            }
            session.messages.truncate(trunc.keep_count);
            session.updated_at = now_ms();
            acc.truncated(&trunc.session_id, trunc.keep_count);
            acc.meta_updated_at(&trunc.session_id, session.updated_at);
            inner.mark_session_messages(&trunc.session_id);
        }
        let mut status = json!({});
        if let Some(streaming) = batch.status.streaming {
            status["streaming"] = json!(streaming);
            inner.streaming = streaming;
        }
        if let Some(compacting) = &batch.status.compacting {
            status["compacting"] = compacting
                .as_ref()
                .map(|id| json!(id))
                .unwrap_or(Value::Null);
            inner.compacting = compacting.clone();
        }
        for id in &batch.dirty_messages {
            if inner.session(id).is_some() {
                inner.mark_session_messages(id);
            }
        }
        for id in &batch.dirty_meta {
            if inner.session(id).is_some() {
                inner.mark_session_meta(id);
            }
        }
        acc.status(status);
        let fragments = acc.finish(&inner);
        self.publish(&mut inner, fragments.clone(), Some(request_id));
        Ok(fragments)
    }

    // ===== 写盘链 =====

    /// 立即执行一轮写盘（flush 命令与换根装载调用；写失败置 persistError 并安排退避重试）。
    pub fn run_write_cycle(&self) {
        let work = {
            let mut inner = self.lock();
            if !inner.has_dirty() {
                self.ack_flush(&mut inner);
                return;
            }
            inner.collect_work()
        };
        let results = perform_writes(&work);
        let mut inner = self.lock();
        let status = inner.apply_write_results(&self.config, &work, results);
        if let Some(status) = status {
            let fragments = fragments_from_status(status);
            self.publish(&mut inner, fragments, None);
        }
        self.ack_flush(&mut inner);
    }

    /// flush 命令：请求立即写盘并等待本轮完成（resolve 不代表全部写成功——失败可见 + 退避重试）。
    pub async fn flush(&self) -> Result<(), String> {
        let rx = {
            let mut inner = self.lock();
            let (tx, rx) = tokio::sync::oneshot::channel();
            inner.pending_flush.push(tx);
            inner.flush_requested = true;
            self.sched.notify_all();
            rx
        };
        rx.await.map_err(|_| "写盘轮已终止".to_string())
    }

    fn ack_flush(&self, inner: &mut Inner) {
        inner.flush_requested = false;
        let pending: Vec<_> = inner.pending_flush.drain(..).collect();
        for tx in pending {
            let _ = tx.send(());
        }
        self.sched.notify_all();
    }

    /// 写链线程主循环：脏标记防抖（500ms）到期或 flush 请求时执行写盘轮；失败按退避序列重试。
    pub fn spawn_writer(self: &Arc<Self>) {
        let state = Arc::clone(self);
        std::thread::spawn(move || loop {
            {
                let mut inner = state.lock();
                loop {
                    let now = Instant::now();
                    let retry_due = inner
                        .next_retry_at
                        .map(|at| now >= at)
                        .unwrap_or(false);
                    let debounce_due = inner
                        .last_mark
                        .map(|mark| now.duration_since(mark) >= state.config.debounce)
                        .unwrap_or(false);
                    if inner.flush_requested
                        || (inner.has_dirty() && (debounce_due || retry_due))
                    {
                        break;
                    }
                    // 等待到最近一个到期点（无待办则无限等待）
                    let wait = if inner.has_dirty() {
                        let mark_due = inner.last_mark.map(|mark| {
                            state
                                .config
                                .debounce
                                .checked_sub(now.duration_since(mark))
                                .unwrap_or(Duration::ZERO)
                        });
                        let retry_due = inner.next_retry_at.map(|at| {
                            at.saturating_duration_since(now)
                        });
                        Some(
                            mark_due
                                .into_iter()
                                .chain(retry_due)
                                .min()
                                .unwrap_or(Duration::ZERO),
                        )
                    } else {
                        None
                    };
                    match wait {
                        None => inner = state.sched.wait(inner).unwrap_or_else(poison),
                        Some(d) => {
                            let (guard, _) = state
                                .sched
                                .wait_timeout(inner, d)
                                .unwrap_or_else(|e| e.into_inner());
                            inner = guard;
                        }
                    }
                }
            }
            state.run_write_cycle();
        });
    }
}

fn poison<T>(e: std::sync::PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    e.into_inner()
}

// ===== 视图 / 序列化（线契约对齐）=====

/// 会话出线形状（payload 运行时缓存剥离；title/agentId/compaction 缺省 = 键不出现）。
fn session_to_wire(s: &Session) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(s.id));
    obj.insert("file".into(), json!(s.file));
    if let Some(title) = &s.title {
        obj.insert("title".into(), json!(title));
    }
    if let Some(agent_id) = &s.agent_id {
        obj.insert("agentId".into(), json!(agent_id));
    }
    if let Some(compaction) = &s.compaction {
        obj.insert("compaction".into(), compaction.clone());
    }
    obj.insert("createdAt".into(), json!(s.created_at));
    obj.insert("updatedAt".into(), json!(s.updated_at));
    obj.insert(
        "messages".into(),
        Value::Array(s.messages.iter().map(strip_message).collect()),
    );
    Value::Object(obj)
}

fn snapshot_of(inner: &Inner) -> ContainerSnapshot {
    ContainerSnapshot {
        seq: inner.seq,
        session_vault_key: inner.vault_key(),
        sessions: inner.sessions.iter().map(session_to_wire).collect(),
        streaming: inner.streaming,
        compacting: inner.compacting.clone(),
        persist_error: inner.persist_error.clone(),
        model_override: inner.model_override.clone(),
        effort_override: inner.effort_override.clone(),
    }
}

/// 剥离消息的附件运行时缓存（payload 只活在各窗口本地，按 file 引用水合）。
fn strip_message(m: &Value) -> Value {
    let Some(atts) = m.get("attachments").and_then(Value::as_array) else {
        return m.clone();
    };
    let stripped: Vec<Value> = atts
        .iter()
        .map(|a| match a.as_object() {
            Some(obj) => {
                let mut next = obj.clone();
                next.remove("payload");
                Value::Object(next)
            }
            None => a.clone(),
        })
        .collect();
    let mut next = m.clone();
    next["attachments"] = Value::Array(stripped);
    next
}

fn message_id_of(m: &Value) -> Option<String> {
    m.get("id").and_then(Value::as_str).map(str::to_string)
}

/// 消息内容级相同判定（剥 payload 后 Value 相等；运行时缓存回填不产生增量噪声）。
fn same_message_content(a: &Value, b: &Value) -> bool {
    strip_message(a) == strip_message(b)
}

/// 元数据完整片段（title/agentId/compaction 恒在场：null = 清除，与前端折叠语义对齐）。
fn meta_fragment_full(s: &Session) -> Value {
    json!({
        "id": s.id,
        "file": s.file,
        "title": s.title,
        "agentId": s.agent_id,
        "compaction": s.compaction,
        "createdAt": s.created_at,
        "updatedAt": s.updated_at,
    })
}

fn meta_fragment_removed(id: &str) -> Value {
    json!({ "id": id, "removed": true })
}

fn fragments_from_status(status: Value) -> Fragments {
    Fragments {
        metas: Vec::new(),
        messages: Vec::new(),
        status: Some(status),
        message_session_ids: Vec::new(),
        meta_session_ids: Vec::new(),
        deleted_ids: Vec::new(),
    }
}

fn fragments_empty(f: &Fragments) -> bool {
    f.metas.is_empty() && f.messages.is_empty() && f.status.is_none()
}

/// 多片段合并（与前端 mergeFragments 语义一致：数组拼接、id 集合去重、status 后到字段覆盖）。
fn merge_fragment_list(list: &[Fragments]) -> Fragments {
    let mut merged = Fragments::default();
    let mut message_ids: Vec<String> = Vec::new();
    let mut meta_ids: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    for f in list {
        merged.metas.extend(f.metas.iter().cloned());
        merged.messages.extend(f.messages.iter().cloned());
        for id in &f.message_session_ids {
            if !message_ids.contains(id) {
                message_ids.push(id.clone());
            }
        }
        for id in &f.meta_session_ids {
            if !meta_ids.contains(id) {
                meta_ids.push(id.clone());
            }
        }
        for id in &f.deleted_ids {
            if !deleted.contains(id) {
                deleted.push(id.clone());
            }
        }
        if let Some(status) = &f.status {
            let mut next = merged.status.take().unwrap_or(json!({}));
            if let (Some(base), Some(patch)) = (next.as_object_mut(), status.as_object()) {
                for (k, v) in patch {
                    base.insert(k.clone(), v.clone());
                }
            }
            merged.status = Some(next);
        }
    }
    merged.message_session_ids = message_ids;
    merged.meta_session_ids = meta_ids;
    merged.deleted_ids = deleted;
    merged
}

// ===== 差分引擎 =====

/// 变更累积器：每个变更点登记（会话创建/移除/元数据补丁/消息追加/补丁/截断/状态），
/// finish 时产出与「前后全量差分」一致的片段（cargo 测试以通用差分逐例核对）。
#[derive(Default)]
struct DiffAccumulator {
    touched: Vec<String>,
    created_ids: Vec<String>,
    removed_ids: Vec<String>,
    appended: Vec<(String, Vec<Value>)>,
    patched: Vec<(String, Value)>,
    truncated: Vec<(String, usize)>,
    meta_only: Vec<(String, MetaPatchKind)>,
    status: Option<Value>,
}

#[derive(Clone, Copy, PartialEq)]
enum MetaPatchKind {
    Fields,
    UpdatedAtOnly,
}

impl DiffAccumulator {
    fn created(&mut self, s: &Session) {
        self.touched.push(s.id.clone());
        self.created_ids.push(s.id.clone());
    }

    fn removed(&mut self, id: &str) {
        self.touched.push(id.to_string());
        self.removed_ids.push(id.to_string());
    }

    fn meta_patch(&mut self, id: &str, _patch: &MetaPatch) {
        if !self.meta_only.iter().any(|(x, _)| x == id) {
            self.touched.push(id.to_string());
            self.meta_only.push((id.to_string(), MetaPatchKind::Fields));
        }
    }

    fn meta_updated_at(&mut self, _id: &str, _at: i64) {
        // updatedAt 单独变化不产出片段（「最近使用」置顶不进容器广播）；
        // 与消息/元数据变更同批时由后者携带的完整片段覆盖
    }

    fn appended(&mut self, id: &str, messages: &[Value]) {
        if !self.touched.iter().any(|x| x == id) {
            self.touched.push(id.to_string());
        }
        self.appended.push((id.to_string(), messages.to_vec()));
    }

    fn patched(&mut self, id: &str, message: &Value) {
        if !self.touched.iter().any(|x| x == id) {
            self.touched.push(id.to_string());
        }
        self.patched.push((id.to_string(), message.clone()));
    }

    fn truncated(&mut self, id: &str, keep: usize) {
        if !self.touched.iter().any(|x| x == id) {
            self.touched.push(id.to_string());
        }
        self.truncated.push((id.to_string(), keep));
    }

    fn status(&mut self, status: Value) {
        if status.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            self.status = Some(status);
        }
    }

    /// 产出片段：每个被触及会话一个完整元数据片段 + 消息增量（除非只有 updatedAt 变化）。
    fn finish(&self, inner: &Inner) -> Fragments {
        let mut metas = Vec::new();
        let mut messages = Vec::new();
        let mut message_session_ids = Vec::new();
        let mut meta_session_ids = Vec::new();
        for id in &self.touched {
            let Some(session) = inner.session(id) else { continue };
            let meta_only = self
                .meta_only
                .iter()
                .any(|(x, kind)| x == id && *kind == MetaPatchKind::UpdatedAtOnly);
            if meta_only
                && self.appended.iter().all(|(x, _)| x != id)
                && self.patched.iter().all(|(x, _)| x != id)
                && self.truncated.iter().all(|(x, _)| x != id)
            {
                continue; // updatedAt 单独变化不进广播
            }
            metas.push(meta_fragment_full(session));
            meta_session_ids.push(id.clone());
        }
        for id in &self.created_ids {
            let Some(session) = inner.session(id) else { continue };
            messages.push(MessagesDelta {
                session_id: id.clone(),
                upserts: session.messages.iter().map(strip_message).collect(),
                keep_count: None,
            });
            message_session_ids.push(id.clone());
        }
        for (id, upserts) in &self.appended {
            messages.push(MessagesDelta {
                session_id: id.clone(),
                upserts: upserts.iter().map(strip_message).collect(),
                keep_count: None,
            });
            message_session_ids.push(id.clone());
        }
        for (id, message) in &self.patched {
            messages.push(MessagesDelta {
                session_id: id.clone(),
                upserts: vec![strip_message(message)],
                keep_count: None,
            });
            message_session_ids.push(id.clone());
        }
        for (id, keep) in &self.truncated {
            messages.push(MessagesDelta {
                session_id: id.clone(),
                upserts: Vec::new(),
                keep_count: Some(*keep),
            });
            message_session_ids.push(id.clone());
        }
        for id in &self.removed_ids {
            // 删除只产 removed 元数据与 deletedIds，不进 metaSessionIds——会话已不在真源，
            // 无 meta 可重拉（与 diff_views 的删除分支语义一致）
            metas.push(meta_fragment_removed(id));
        }
        Fragments {
            metas,
            messages,
            status: self.status.clone(),
            message_session_ids,
            meta_session_ids,
            deleted_ids: self.removed_ids.clone(),
        }
    }
}

/// 状态面克隆（差分基准）。
struct ViewClone {
    sessions: Vec<Session>,
    status: StatusViewOwned,
}struct StatusViewOwned {
    streaming: bool,
    compacting: Option<String>,
    persist_error: Option<PersistError>,
    model_override: Option<Value>,
    effort_override: Option<String>,
    vault_key: String,
}

fn view_clone(inner: &Inner) -> ViewClone {
    ViewClone {
        sessions: inner.sessions.clone(),
        status: StatusViewOwned {
            streaming: inner.streaming,
            compacting: inner.compacting.clone(),
            persist_error: inner.persist_error.clone(),
            model_override: inner.model_override.clone(),
            effort_override: inner.effort_override.clone(),
            vault_key: inner.vault_key(),
        },
    }
}

/// 相邻真源状态 → 增量片段（通用差分；装载重置与测试基准用）。
/// updatedAt 单独变化不产出——每窗口「最近使用」置顶不进容器广播。
fn diff_views(
    prev: &[Session],
    prev_status: &StatusViewOwned,
    next: &[Session],
    next_status: &StatusView<'_>,
) -> Fragments {
    let mut metas = Vec::new();
    let mut messages = Vec::new();
    let mut message_session_ids = Vec::new();
    let mut meta_session_ids = Vec::new();
    let mut deleted_ids = Vec::new();
    let next_ids: HashSet<&str> = next.iter().map(|s| s.id.as_str()).collect();
    for p in prev {
        if !next_ids.contains(p.id.as_str()) {
            metas.push(meta_fragment_removed(&p.id));
            deleted_ids.push(p.id.clone());
        }
    }
    for s in next {
        let Some(p) = prev.iter().find(|x| x.id == s.id) else {
            metas.push(meta_fragment_full(s));
            meta_session_ids.push(s.id.clone());
            messages.push(MessagesDelta {
                session_id: s.id.clone(),
                upserts: s.messages.iter().map(strip_message).collect(),
                keep_count: None,
            });
            message_session_ids.push(s.id.clone());
            continue;
        };
        let meta_changed = p.title != s.title
            || p.agent_id != s.agent_id
            || p.compaction != s.compaction
            || p.created_at != s.created_at;
        let msg_delta = diff_messages(s, p);
        if meta_changed || msg_delta.is_some() {
            metas.push(meta_fragment_full(s));
            meta_session_ids.push(s.id.clone());
        }
        if let Some(delta) = msg_delta {
            messages.push(delta);
            message_session_ids.push(s.id.clone());
        }
    }
    let status = diff_status(prev_status, next_status);
    Fragments {
        metas,
        messages,
        status,
        message_session_ids,
        meta_session_ids,
        deleted_ids,
    }
}

fn diff_messages(next: &Session, prev: &Session) -> Option<MessagesDelta> {
    let mut upserts = Vec::new();
    let mut keep_count = None;
    let common = prev.messages.len().min(next.messages.len());
    for i in 0..common {
        if !same_message_content(&prev.messages[i], &next.messages[i]) {
            upserts.push(strip_message(&next.messages[i]));
        }
    }
    if next.messages.len() < prev.messages.len() {
        keep_count = Some(next.messages.len());
    } else {
        for m in &next.messages[common..] {
            upserts.push(strip_message(m));
        }
    }
    if upserts.is_empty() && keep_count.is_none() {
        return None;
    }
    Some(MessagesDelta {
        session_id: next.id.clone(),
        upserts,
        keep_count,
    })
}

fn diff_status(prev: &StatusViewOwned, next: &StatusView<'_>) -> Option<Value> {
    let mut out = serde_json::Map::new();
    if prev.streaming != next.streaming {
        out.insert("streaming".into(), json!(next.streaming));
    }
    if prev.compacting != *next.compacting {
        out.insert(
            "compacting".into(),
            next.compacting
                .as_ref()
                .map(|id| json!(id))
                .unwrap_or(Value::Null),
        );
    }
    if persist_error_eq(&prev.persist_error, next.persist_error) {
        // 相同
    } else {
        out.insert(
            "persistError".into(),
            next.persist_error
                .as_ref()
                .map(|e| json!({ "message": e.message, "at": e.at }))
                .unwrap_or(Value::Null),
        );
    }
    if prev.model_override != *next.model_override {
        out.insert(
            "modelOverride".into(),
            next.model_override.clone().unwrap_or(Value::Null),
        );
    }
    if prev.effort_override != *next.effort_override {
        out.insert(
            "effortOverride".into(),
            next.effort_override
                .clone()
                .map(|e| json!(e))
                .unwrap_or(Value::Null),
        );
    }
    if prev.vault_key != next.vault_key {
        out.insert("sessionVaultKey".into(), json!(next.vault_key));
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

fn persist_error_eq(a: &Option<PersistError>, b: &Option<PersistError>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.message == y.message && x.at == y.at,
        _ => false,
    }
}

fn empty_apply() -> ApplyOk {
    ApplyOk {
        value: Some(Value::Null),
        created_session_id: None,
        fragments: Fragments::default(),
    }
}

fn create_opts(opts: &Option<Value>) -> (Option<String>, Option<String>) {
    let Some(opts) = opts else {
        return (None, None);
    };
    let title = opts
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let agent_id = opts
        .get("agentId")
        .and_then(Value::as_str)
        .map(str::to_string);
    (title, agent_id)
}

fn chat_message_file(session_id: &str) -> String {
    format!("{CHAT_HISTORY_DIR}/{session_id}{CHAT_MESSAGE_EXT}")
}

// ===== 磁盘装载（解析与前端 parseChatMessages 同语义：损坏行跳过、steps 收敛）=====

fn scan_sessions(root: &Path) -> Result<Vec<Session>, String> {
    let rows = list_chat_sessions_file(root)?;
    let mut sessions = Vec::new();
    for row in rows {
        let messages = read_chat_messages_file(root, &row.file)
            .map(|jsonl| parse_messages_jsonl(&jsonl))
            .unwrap_or_default();
        let (title, agent_id, compaction) = match &row.meta {
            Some(meta) => (
                meta.title.clone(),
                meta.agent_id.clone(),
                meta.compaction
                    .as_ref()
                    .map(|c| serde_json::to_value(c).unwrap_or(Value::Null)),
            ),
            None => (None, None, None),
        };
        sessions.push(Session {
            id: row.id,
            file: row.file,
            title,
            agent_id,
            compaction,
            created_at: messages
                .first()
                .and_then(|m| m.get("createdAt"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            updated_at: messages
                .last()
                .and_then(|m| m.get("createdAt"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            messages,
        });
    }
    Ok(sessions)
}

/// 解析会话消息 .jsonl（逐行 JSON，损坏行跳过；id/createdAt 缺省派生；steps 收敛）。
fn parse_messages_jsonl(jsonl: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    for line in jsonl.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let (Some(role), Some(content)) = (
            raw.get("role").and_then(Value::as_str),
            raw.get("content").and_then(Value::as_str),
        ) else {
            continue;
        };
        let mut obj = serde_json::Map::new();
        let fallback_id = uuid_v4();
        let id = raw.get("id").and_then(Value::as_str).unwrap_or(&fallback_id);
        obj.insert("id".into(), json!(id));
        obj.insert("role".into(), json!(role));
        obj.insert("content".into(), json!(content));
        if let Some(dc) = raw.get("displayContent").and_then(Value::as_str) {
            obj.insert("displayContent".into(), json!(dc));
        }
        if let Some(refs) = raw.get("refs").and_then(Value::as_array) {
            obj.insert("refs".into(), Value::Array(refs.clone()));
        }
        if let Some(steps) = raw.get("steps").and_then(Value::as_array) {
            obj.insert(
                "steps".into(),
                coalesce_agent_steps(&Value::Array(steps.clone())),
            );
        }
        if let Some(atts) = raw.get("attachments").and_then(Value::as_array) {
            obj.insert("attachments".into(), Value::Array(atts.clone()));
        }
        let created_at = raw
            .get("createdAt")
            .and_then(Value::as_i64)
            .unwrap_or(messages.len() as i64);
        obj.insert("createdAt".into(), json!(created_at));
        messages.push(Value::Object(obj));
    }
    messages
}

/// 归并被拆开的同轮思考/叙述步（幂等；与前端 coalesceAgentSteps 同语义）：
/// 按尾向前合并 reasoning/text，遇工具步即停；running 态合成工具行归一为已中断。
fn coalesce_agent_steps(steps: &Value) -> Value {
    let Some(items) = steps.as_array() else {
        return steps.clone();
    };
    let mut acc: Vec<Value> = Vec::new();
    for s in items {
        match s.get("kind").and_then(Value::as_str) {
            Some("reasoning") => {
                let text = s.get("text").and_then(Value::as_str).unwrap_or("");
                append_step_text(&mut acc, "reasoning", text);
            }
            Some("text") => {
                let text = s.get("text").and_then(Value::as_str).unwrap_or("");
                append_step_text(&mut acc, "text", text);
            }
            Some("tool") => {
                let is_pending = s
                    .get("run")
                    .and_then(|r| r.get("id"))
                    .and_then(Value::as_str)
                    .map(|id| id.starts_with(PENDING_RUN_ID_PREFIX))
                    .unwrap_or(false);
                if is_pending
                    && s.get("run")
                        .and_then(|r| r.get("status"))
                        .and_then(Value::as_str)
                        == Some("running")
                {
                    let mut run = s
                        .get("run")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    run.insert("status".into(), json!("error"));
                    run.insert("resultSummary".into(), json!("（已中断）"));
                    acc.push(json!({ "kind": "tool", "run": Value::Object(run) }));
                } else {
                    acc.push(s.clone());
                }
            }
            _ => acc.push(s.clone()),
        }
    }
    Value::Array(acc)
}

/// 增量并入当前轮最后一个同类型步（尾部向前找、遇工具步即停），否则新起一步。
fn append_step_text(acc: &mut Vec<Value>, kind: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    for i in (0..acc.len()).rev() {
        let s = &acc[i];
        if s.get("kind").and_then(Value::as_str) == Some(kind) {
            let mut merged = s.clone();
            let prev = merged.get("text").and_then(Value::as_str).unwrap_or("");
            merged["text"] = json!(format!("{prev}{text}"));
            acc[i] = merged;
            return;
        }
        if s.get("kind").and_then(Value::as_str) == Some("tool") {
            break;
        }
    }
    acc.push(json!({ "kind": kind, "text": text }));
}

// ===== 消息 .jsonl 序列化（与前端 serializeChatMessages 字段与条件对齐）=====

fn serialize_message_line(m: &Value) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), m.get("id").cloned().unwrap_or(Value::Null));
    obj.insert(
        "role".into(),
        m.get("role").cloned().unwrap_or(Value::Null),
    );
    obj.insert(
        "content".into(),
        m.get("content").cloned().unwrap_or(Value::Null),
    );
    if let Some(dc) = m.get("displayContent").and_then(Value::as_str) {
        if !dc.is_empty() {
            obj.insert("displayContent".into(), json!(dc));
        }
    }
    if let Some(refs) = m.get("refs").and_then(Value::as_array) {
        if !refs.is_empty() {
            obj.insert("refs".into(), Value::Array(refs.clone()));
        }
    }
    if let Some(steps) = m.get("steps").and_then(Value::as_array) {
        if !steps.is_empty() {
            obj.insert("steps".into(), Value::Array(steps.clone()));
        }
    }
    if let Some(atts) = m.get("attachments").and_then(Value::as_array) {
        if !atts.is_empty() {
            let stripped: Vec<Value> = atts
                .iter()
                .map(|a| match a.as_object() {
                    Some(obj) => {
                        let mut next = obj.clone();
                        next.remove("payload");
                        Value::Object(next)
                    }
                    None => a.clone(),
                })
                .collect();
            obj.insert("attachments".into(), Value::Array(stripped));
        }
    }
    obj.insert(
        "createdAt".into(),
        m.get("createdAt").cloned().unwrap_or(Value::Null),
    );
    Value::Object(obj).to_string()
}

fn serialize_messages_jsonl(messages: &[Value]) -> String {
    messages
        .iter()
        .map(serialize_message_line)
        .collect::<Vec<_>>()
        .join("\n")
}

// ===== 写盘轮 =====

/// 写盘工作项快照（锁内采集、锁外执行；version/epoch 供写后复核）。
struct WorkItem {
    kind: WorkKind,
    session_id: Option<String>,
    version: u64,
}

enum WorkKind {
    /// 消息 .jsonl：追加（基线前缀逐字一致）或全量重写。
    Messages {
        file: String,
        append_from: Option<(String, String)>, // (基线 jsonl, 追加行·带尾 '\n')
        rewrite: String,
    },
    Meta {
        file: String,
        meta: ChatSessionMeta,
    },
    Overrides {
        file: ChatMetaFile,
    },
}

struct WorkSnapshot {
    epoch: u64,
    root: Option<PathBuf>,
    items: Vec<WorkItem>,
}

impl Inner {
    fn collect_work(&mut self) -> WorkSnapshot {
        let epoch = self.epoch;
        let root = self.loaded_root.clone();
        let mut items = Vec::new();
        let ids: Vec<String> = self.dirty_messages.iter().cloned().collect();
        for id in ids {
            let Some(session) = self.session(&id) else {
                self.dirty_messages.remove(&id);
                self.persisted.remove(&id);
                continue;
            };
            let version = self.change_versions.get(&id).copied().unwrap_or(0);
            let full = serialize_messages_jsonl(&session.messages);
            let append_from = self.persisted.get(&id).and_then(|base| {
                if session.messages.len() > base.len {
                    // 基线自检：持久化时前缀与当前确定性序列化一致才允许追加
                    let prefix = serialize_messages_jsonl(&session.messages[..base.len]);
                    if prefix == base.jsonl {
                        // tail 以 '\n' 结尾（append 原语约定；磁盘行分隔由它保证）
                        let tail = session.messages[base.len..]
                            .iter()
                            .map(serialize_message_line)
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Some((base.jsonl.clone(), format!("{tail}\n")));
                    }
                }
                None
            });
            items.push(WorkItem {
                kind: WorkKind::Messages {
                    file: session.file.clone(),
                    append_from,
                    rewrite: full,
                },
                session_id: Some(id),
                version,
            });
        }
        let ids: Vec<String> = self.dirty_meta.iter().cloned().collect();
        for id in ids {
            let Some(session) = self.session(&id) else {
                self.dirty_meta.remove(&id);
                continue;
            };
            let version = self.change_versions.get(&id).copied().unwrap_or(0);
            let compaction = session
                .compaction
                .as_ref()
                .and_then(|v| serde_json::from_value::<crate::vault::ChatCompaction>(v.clone()).ok());
            let meta = ChatSessionMeta {
                id: session.id.clone(),
                title: session.title.clone(),
                agent_id: session.agent_id.clone(),
                compaction,
            };
            items.push(WorkItem {
                kind: WorkKind::Meta {
                    file: session.file.clone(),
                    meta,
                },
                session_id: Some(id),
                version,
            });
        }
        if self.overrides_dirty {
            let model_override = self
                .model_override
                .as_ref()
                .and_then(|v| serde_json::from_value::<EditorChatModelOverride>(v.clone()).ok());
            items.push(WorkItem {
                kind: WorkKind::Overrides {
                    file: ChatMetaFile {
                        schema: EDITOR_CHATS_META_SCHEMA.to_string(),
                        model_override,
                        effort_override: self.effort_override.clone(),
                    },
                },
                session_id: None,
                version: self.overrides_version,
            });
        }
        WorkSnapshot { epoch, root, items }
    }

    /// 写回结果应用：基线推进 / 脏清理 / 退避调度 / persistError 置位。
    /// 返回 Some(status) = persistError 状态面变化（由 state 层收尾广播）。
    fn apply_write_results(
        &mut self,
        config: &WriteChainConfig,
        work: &WorkSnapshot,
        results: Vec<Result<(), String>>,
    ) -> Option<Value> {
        if work.epoch != self.epoch {
            return None; // 写在途期间换仓库：旧仓库写回结果与基线整体丢弃
        }
        let mut failed = false;
        for (item, result) in work.items.iter().zip(results) {
            match result {
                Ok(()) => {
                    let unchanged = match &item.session_id {
                        Some(id) => self.change_versions.get(id).copied().unwrap_or(0) == item.version,
                        None => self.overrides_version == item.version,
                    };
                    if !unchanged {
                        continue; // 写在途又有变更：保留脏标记由下轮按新状态追平
                    }
                    if let (WorkKind::Messages { append_from, rewrite, .. }, Some(id)) =
                        (&item.kind, &item.session_id)
                    {
                        // 基线 = 磁盘成功内容的确定性形（不含尾 '\n'；磁盘由写函数恒补尾）
                        let jsonl = match append_from {
                            Some((base_jsonl, tail)) => {
                                if base_jsonl.is_empty() {
                                    tail.trim_end_matches('\n').to_string()
                                } else {
                                    format!("{base_jsonl}\n{}", tail.trim_end_matches('\n'))
                                }
                            }
                            None => rewrite.clone(),
                        };
                        self.persisted.insert(
                            id.clone(),
                            PersistedBase {
                                len: jsonl.lines().count(),
                                jsonl,
                            },
                        );
                    }
                    // 脏清理按 item.kind 对应：同会话 Messages 与 Meta 是两个独立写项，任一失败
                    // 时另一个成功不能替它清标记（否则失败项永不重试，退出进程丢数据）
                    match &item.kind {
                        WorkKind::Messages { .. } => {
                            if let Some(id) = &item.session_id {
                                self.dirty_messages.remove(id);
                            }
                        }
                        WorkKind::Meta { .. } => {
                            if let Some(id) = &item.session_id {
                                self.dirty_meta.remove(id);
                            }
                        }
                        WorkKind::Overrides { .. } => self.overrides_dirty = false,
                    }
                }
                Err(e) => {
                    failed = true;
                    eprintln!("会话容器写盘失败：{e}");
                    if let Some(id) = &item.session_id {
                        if matches!(item.kind, WorkKind::Messages { .. }) {
                            // 追加/重写失败：基线清除，下次重试走全量重写（幂等重建）
                            self.persisted.remove(id);
                        }
                    }
                }
            }
        }
        if failed {
            let delay = config
                .retries
                .get(self.retry_attempt)
                .copied()
                .unwrap_or_else(|| {
                    config
                        .retries
                        .last()
                        .copied()
                        .unwrap_or(Duration::from_millis(30000))
                });
            self.retry_attempt += 1;
            self.next_retry_at = Some(Instant::now() + delay);
            // 同文案的重复失败不重复广播（at 变化无信息量）；首次失败/恢复才进状态面
            let changed = self
                .persist_error
                .as_ref()
                .map(|e| e.message != PERSIST_ERROR_MESSAGE)
                .unwrap_or(true);
            self.persist_error = Some(PersistError {
                message: PERSIST_ERROR_MESSAGE.to_string(),
                at: now_ms(),
            });
            if changed {
                Some(json!({
                    "persistError": { "message": PERSIST_ERROR_MESSAGE, "at": now_ms() }
                }))
            } else {
                None
            }
        } else {
            self.retry_attempt = 0;
            self.next_retry_at = None;
            if self.persist_error.take().is_some() {
                Some(json!({ "persistError": Value::Null }))
            } else {
                None
            }
        }
    }
}

/// 执行写盘工作项（锁外 I/O；各项独立成败）。消息追加前核对磁盘内容与内存基线一致
/// （字节级；写函数恒补尾 '\n'，故比对 `去尾 == 基线`），不一致或文件缺失回落全量重写
/// ——磁盘事实不假设只由本真源改写。
fn perform_writes(work: &WorkSnapshot) -> Vec<Result<(), String>> {
    let Some(root) = &work.root else {
        return work
            .items
            .iter()
            .map(|_| Err("仓库未装载，无法写盘".to_string()))
            .collect();
    };
    work.items
        .iter()
        .map(|item| match &item.kind {
            WorkKind::Messages {
                file,
                append_from,
                rewrite,
            } => {
                if let Some((base_jsonl, tail)) = append_from {
                    let matches_base = read_chat_messages_file(root, file)
                        .map(|existing| {
                            if existing.is_empty() {
                                base_jsonl.is_empty()
                            } else {
                                existing.strip_suffix('\n') == Some(base_jsonl.as_str())
                            }
                        })
                        .unwrap_or(false);
                    if matches_base && append_chat_messages_raw(root, file, tail).is_ok() {
                        return Ok(());
                    }
                }
                write_chat_messages_file(root, file, rewrite)
            }
            WorkKind::Meta { file, meta } => write_chat_session_meta_file(root, file, meta),
            WorkKind::Overrides { file } => write_editor_chats_meta_file(root, file),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn msg(id: &str, role: &str, content: &str) -> Value {
        json!({ "id": id, "role": role, "content": content, "createdAt": 1_000 })
    }

    fn test_config() -> WriteChainConfig {
        WriteChainConfig {
            debounce: Duration::ZERO,
            retries: vec![Duration::ZERO],
        }
    }

    fn session(id: &str, title: Option<&str>, messages: Vec<Value>) -> Session {
        Session {
            id: id.to_string(),
            file: chat_message_file(id),
            title: title.map(str::to_string),
            agent_id: None,
            compaction: None,
            created_at: 1,
            updated_at: 1,
            messages,
        }
    }

    /// 装载了假根与会话集的容器状态（差分测试基准；不发广播）。
    fn state_with(sessions: Vec<Session>) -> ChatContainerState {
        let state = ChatContainerState::new(test_config());
        {
            let mut inner = state.lock();
            inner.loaded_root = Some(PathBuf::from("fake-root"));
            inner.sessions = sessions;
        }
        state
    }

    fn owned_status(vault_key: &str) -> StatusViewOwned {
        StatusViewOwned {
            streaming: false,
            compacting: None,
            persist_error: None,
            model_override: None,
            effort_override: None,
            vault_key: vault_key.to_string(),
        }
    }

    /// 全空状态视图（差分测试的 next 侧：字段全为 None 的静态持有者）。
    #[derive(Default)]
    struct NeutralStatus {
        compacting: Option<String>,
        persist_error: Option<PersistError>,
        model_override: Option<Value>,
        effort_override: Option<String>,
    }

    impl NeutralStatus {
        fn view(&self, vault_key: &str) -> StatusView<'_> {
            StatusView {
                streaming: false,
                compacting: &self.compacting,
                persist_error: &self.persist_error,
                model_override: &self.model_override,
                effort_override: &self.effort_override,
                vault_key: vault_key.to_string(),
            }
        }
    }

    fn assert_same_fragments(got: &Fragments, want: &Fragments) {
        assert_eq!(
            serde_json::to_value(got).unwrap(),
            serde_json::to_value(want).unwrap(),
            "累积器片段与通用差分不一致"
        );
    }

    // ===== parse / serialize 往返 =====

    #[test]
    fn parse_skips_corrupt_lines_and_derives_defaults() {
        let jsonl = format!(
            "{}\nnot-json\n{}\n",
            json!({ "role": "user", "content": "hi" }),
            json!({ "id": "m2", "role": "assistant", "content": "yo", "createdAt": 42 })
        );
        let msgs = parse_messages_jsonl(&jsonl);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0]["id"].is_string()); // 缺 id 派生
        assert_eq!(msgs[0]["createdAt"], json!(0)); // 缺 createdAt 派生为序号
        assert_eq!(msgs[1]["id"], json!("m2"));
        assert_eq!(msgs[1]["createdAt"], json!(42));
    }

    #[test]
    fn serialize_round_trip_strips_payload_and_extra() {
        let m = json!({
            "id": "m1", "role": "user", "content": "hi",
            "displayContent": "dc", "refs": [{ "type": "file" }],
            "steps": [{ "kind": "reasoning", "text": "t" }],
            "attachments": [{ "file": "a.png", "payload": "data:image/png;base64,xx" }],
            "createdAt": 7, "extra": "dropped"
        });
        let parsed = parse_messages_jsonl(&serialize_message_line(&m));
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].get("extra").is_none());
        assert!(parsed[0]["attachments"][0].get("payload").is_none());
        assert_eq!(parsed[0]["displayContent"], json!("dc"));
        assert_eq!(parsed[0]["createdAt"], json!(7));
    }

    // ===== coalesce_agent_steps 移植 =====

    #[test]
    fn coalesce_merges_adjacent_text_steps_and_stops_at_tool() {
        let steps = json!([
            { "kind": "reasoning", "text": "a" },
            { "kind": "reasoning", "text": "b" },
            { "kind": "tool", "run": { "id": "t1", "status": "ok" } },
            { "kind": "text", "text": "c" }
        ]);
        let out = coalesce_agent_steps(&steps);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["text"], json!("ab"));
        assert_eq!(arr[1]["kind"], json!("tool"));
        assert_eq!(arr[2]["text"], json!("c"));
    }

    #[test]
    fn coalesce_normalizes_running_pending_tool_to_interrupted() {
        let steps = json!([
            { "kind": "tool", "run": { "id": "pending:x", "status": "running" } }
        ]);
        let out = coalesce_agent_steps(&steps);
        let run = &out.as_array().unwrap()[0]["run"];
        assert_eq!(run["status"], json!("error"));
        assert_eq!(run["resultSummary"], json!("（已中断）"));
    }

    // ===== 累积器与通用差分对齐 =====

    #[test]
    fn acc_append_matches_full_diff() {
        let before = vec![session("s1", Some("t"), vec![msg("m1", "user", "a")])];
        let after_msgs = vec![msg("m1", "user", "a"), msg("m2", "assistant", "b")];
        let after = vec![session("s1", Some("t"), after_msgs.clone())];
        let state = state_with(after.clone());
        let inner = state.lock();
        let mut acc = DiffAccumulator::default();
        acc.appended("s1", &after_msgs[1..]);
        let got = acc.finish(&inner);
        let want = diff_views(
            &before,
            &owned_status("local:x"),
            &after,
            &NeutralStatus::default().view("local:x"),
        );
        assert_same_fragments(&got, &want);
    }

    #[test]
    fn acc_patch_matches_full_diff() {
        let before = vec![session(
            "s1",
            None,
            vec![msg("m1", "user", "a"), msg("m2", "assistant", "")],
        )];
        let patched = msg("m2", "assistant", "done");
        let after = vec![session(
            "s1",
            None,
            vec![msg("m1", "user", "a"), patched.clone()],
        )];
        let state = state_with(after.clone());
        let inner = state.lock();
        let mut acc = DiffAccumulator::default();
        acc.patched("s1", &patched);
        let got = acc.finish(&inner);
        let want = diff_views(
            &before,
            &owned_status("local:x"),
            &after,
            &NeutralStatus::default().view("local:x"),
        );
        assert_same_fragments(&got, &want);
    }

    #[test]
    fn acc_truncate_and_remove_match_full_diff() {
        // 截断
        let before = vec![session(
            "s1",
            None,
            vec![msg("m1", "user", "a"), msg("m2", "assistant", "b")],
        )];
        let after = vec![session("s1", None, vec![msg("m1", "user", "a")])];
        let state = state_with(after.clone());
        let inner = state.lock();
        let mut acc = DiffAccumulator::default();
        acc.truncated("s1", 1);
        let got = acc.finish(&inner);
        let want = diff_views(
            &before,
            &owned_status("local:x"),
            &after,
            &NeutralStatus::default().view("local:x"),
        );
        assert_same_fragments(&got, &want);
        drop(inner);

        // 移除
        let before = vec![session("s1", None, vec![msg("m1", "user", "a")])];
        let state = state_with(Vec::new());
        let inner = state.lock();
        let mut acc = DiffAccumulator::default();
        acc.removed("s1");
        let got = acc.finish(&inner);
        let want = diff_views(
            &before,
            &owned_status("local:x"),
            &[],
            &NeutralStatus::default().view("local:x"),
        );
        assert_same_fragments(&got, &want);
    }

    #[test]
    fn updated_at_only_change_yields_no_fragments() {
        let before = vec![session("s1", None, vec![msg("m1", "user", "a")])];
        let after = vec![Session {
            updated_at: 999,
            ..session("s1", None, vec![msg("m1", "user", "a")])
        }];
        let state = state_with(after.clone());
        let inner = state.lock();
        let mut acc = DiffAccumulator::default();
        acc.meta_updated_at("s1", 999);
        let got = acc.finish(&inner);
        let want = diff_views(
            &before,
            &owned_status("local:x"),
            &after,
            &NeutralStatus::default().view("local:x"),
        );
        assert!(fragments_empty(&got), "acc 不该产出片段");
        assert!(fragments_empty(&want), "差分不该产出片段");
    }

    // ===== 写链 =====

    #[test]
    fn write_chain_appends_then_rewrites_and_stays_idempotent() {
        let tmp = TempDir::new("chatc-chain");
        let state = ChatContainerState::new(test_config());
        let key;
        {
            let mut inner = state.lock();
            inner.loaded_root = Some(tmp.to_path_buf());
            inner
                .sessions
                .push(session("s1", None, vec![msg("m1", "user", "a")]));
            inner.mark_session_meta("s1");
            inner.mark_session_messages("s1");
            key = inner.vault_key();
        }
        state.run_write_cycle(); // 全量落盘（磁盘尚无文件）
        let file = chat_message_file("s1");
        let disk1 = read_chat_messages_file(&tmp, &file).unwrap();
        assert_eq!(disk1.lines().count(), 1);
        {
            let mut inner = state.lock();
            inner
                .session_mut("s1")
                .unwrap()
                .messages
                .push(msg("m2", "assistant", "b"));
            inner.mark_session_messages("s1");
        }
        state.run_write_cycle(); // 基线一致 → 追加
        {
            let inner = state.lock();
            let base = inner.persisted.get("s1").unwrap();
            assert_eq!(base.len, 2);
            assert_eq!(
                base.jsonl,
                serialize_messages_jsonl(&[msg("m1", "user", "a"), msg("m2", "assistant", "b")])
            );
        }
        // 截断 → 基线失配 → 全量重写
        {
            let mut inner = state.lock();
            inner.session_mut("s1").unwrap().messages.truncate(1);
            inner.mark_session_messages("s1");
        }
        state.run_write_cycle();
        let disk3 = read_chat_messages_file(&tmp, &file).unwrap();
        assert_eq!(parse_messages_jsonl(&disk3).len(), 1);
        // 内容不变再写一轮 → 磁盘字节不变（幂等）
        {
            let mut inner = state.lock();
            inner.dirty_messages.insert("s1".into());
            inner.bump_session("s1");
        }
        state.run_write_cycle();
        let disk4 = read_chat_messages_file(&tmp, &file).unwrap();
        assert_eq!(disk3, disk4);
        let _ = key;
    }

    #[test]
    fn append_raw_requires_verified_disk_base() {
        let tmp = TempDir::new("chatc-rawappend");
        let file = chat_message_file("s1");
        write_chat_messages_file(
            &tmp,
            &file,
            &serialize_messages_jsonl(&[msg("m1", "user", "a")]),
        )
        .unwrap();
        let disk = read_chat_messages_file(&tmp, &file).unwrap();
        assert_eq!(
            disk.strip_suffix('\n'),
            Some(serialize_messages_jsonl(&[msg("m1", "user", "a")]).as_str())
        );
        append_chat_messages_raw(&tmp, &file, "xxx\n").unwrap();
        let disk2 = read_chat_messages_file(&tmp, &file).unwrap();
        assert!(disk2.ends_with("xxx\n"));
        // 文件缺失 → 报错（回落全量重写的触发条件）
        assert!(append_chat_messages_raw(&tmp, &chat_message_file("s2"), "x\n").is_err());
    }

    #[test]
    fn write_failure_sets_persist_error_and_recovers_with_broadcast() {
        let tmp = TempDir::new("chatc-fail");
        // root 指向一个文件：create_dir_all 必败 → 写链失败路径
        let blocker = tmp.join("not-a-dir");
        std::fs::write(&blocker, b"").unwrap();
        let state = ChatContainerState::new(test_config());
        let seen: Arc<Mutex<Vec<ContainerDelta>>> = Arc::default();
        let seen2 = seen.clone();
        state.install_collector(move |ev| {
            if let EmitterEvent::Delta(d) = ev {
                seen2.lock().unwrap().push(d.clone());
            }
        });
        {
            let mut inner = state.lock();
            inner.loaded_root = Some(blocker.clone());
            inner
                .sessions
                .push(session("s1", None, vec![msg("m1", "user", "a")]));
            inner.mark_session_messages("s1");
        }
        state.run_write_cycle();
        {
            let inner = state.lock();
            assert!(inner.persist_error.is_some());
            assert_eq!(inner.retry_attempt, 1);
            assert!(inner.next_retry_at.is_some());
        }
        assert!(seen.lock().unwrap().iter().any(|d| d
            .fragments
            .status
            .as_ref()
            .map(|s| s["persistError"].is_object())
            .unwrap_or(false)));
        // 恢复：root 换回真实目录，重跑写轮 → 成功清错并广播 persistError: null
        {
            let mut inner = state.lock();
            inner.loaded_root = Some(tmp.to_path_buf());
        }
        state.run_write_cycle();
        {
            let inner = state.lock();
            assert!(inner.persist_error.is_none());
            assert_eq!(inner.retry_attempt, 0);
        }
        assert!(seen.lock().unwrap().iter().any(|d| d
            .fragments
            .status
            .as_ref()
            .map(|s| s["persistError"].is_null())
            .unwrap_or(false)));
    }

    #[tokio::test]
    async fn flush_waits_for_write_cycle() {
        let tmp = TempDir::new("chatc-flush");
        let state = Arc::new(ChatContainerState::new(test_config()));
        state.spawn_writer();
        state.load(&tmp).unwrap();
        let key = state.snapshot().session_vault_key;
        state
            .commit(
                "r1",
                "main",
                &key,
                CommitBatch {
                    created: vec![SessionDef {
                        id: "s1".into(),
                        file: chat_message_file("s1"),
                        title: None,
                        agent_id: None,
                        compaction: None,
                        created_at: 1,
                        updated_at: 1,
                        messages: vec![msg("m1", "user", "a")],
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        state.flush().await.unwrap();
        assert!(tmp.join(&chat_message_file("s1")).exists());
    }

    // ===== commit：rev 门控 / content_if_empty =====

    #[test]
    fn commit_rev_gating_and_content_if_empty() {
        let tmp = TempDir::new("chatc-commit");
        let state = ChatContainerState::new(test_config());
        state.load(&tmp).unwrap();
        let key = state.snapshot().session_vault_key;
        let batch = |patch: PatchEntry| CommitBatch {
            patches: vec![patch],
            ..Default::default()
        };
        state
            .commit(
                "r0",
                "main",
                &key,
                CommitBatch {
                    created: vec![SessionDef {
                        id: "s1".into(),
                        file: chat_message_file("s1"),
                        title: None,
                        agent_id: None,
                        compaction: None,
                        created_at: 1,
                        updated_at: 1,
                        messages: vec![msg("m1", "assistant", "")],
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        state
            .commit(
                "r1",
                "main",
                &key,
                batch(PatchEntry {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    rev: 5,
                    content: Some("hello".into()),
                    steps: None,
                    content_if_empty: None,
                }),
            )
            .unwrap();
        // 迟到的 rev=3 不覆盖
        state
            .commit(
                "r2",
                "main",
                &key,
                batch(PatchEntry {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    rev: 3,
                    content: Some("stale".into()),
                    steps: None,
                    content_if_empty: None,
                }),
            )
            .unwrap();
        // content_if_empty：内容非空不回填
        state
            .commit(
                "r3",
                "main",
                &key,
                batch(PatchEntry {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    rev: 6,
                    content: None,
                    steps: None,
                    content_if_empty: Some("fallback".into()),
                }),
            )
            .unwrap();
        let content = state.snapshot().sessions
            .into_iter()
            .find(|s| s["id"] == json!("s1"))
            .map(|s| s["messages"][0]["content"].clone())
            .unwrap();
        assert_eq!(content, json!("hello"));
        // 空内容消息被回填
        state
            .commit(
                "r4",
                "main",
                &key,
                batch(PatchEntry {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    rev: 7,
                    content: Some(String::new()),
                    steps: None,
                    content_if_empty: Some("fallback".into()),
                }),
            )
            .unwrap();
        let content = state.snapshot().sessions
            .into_iter()
            .find(|s| s["id"] == json!("s1"))
            .map(|s| s["messages"][0]["content"].clone())
            .unwrap();
        assert_eq!(content, json!("fallback"));
    }

    // ===== 意图往返 =====

    #[tokio::test]
    async fn intent_round_trip_returns_result() {
        let tmp = TempDir::new("chatc-intent");
        let state = Arc::new(ChatContainerState::new(test_config()));
        state.executor_boot("main").unwrap();
        let intents: Arc<Mutex<Vec<Value>>> = Arc::default();
        let ints2 = intents.clone();
        state.install_collector(move |ev| {
            if let EmitterEvent::Intent(v) = ev {
                ints2.lock().unwrap().push(v.clone());
            }
        });
        state.load(&tmp).unwrap();
        let key = state.snapshot().session_vault_key;
        let s2 = state.clone();
        let waiter = tokio::spawn(async move {
            loop {
                let picked = intents.lock().unwrap().pop();
                if let Some(v) = picked {
                    let intent_id = v["intentId"].as_str().unwrap().to_string();
                    s2.intent_result(
                        &intent_id,
                        IntentResult::Ok {
                            value: Some(json!(7)),
                            created_session_id: None,
                        },
                        "main",
                    )
                    .unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let ok = state
            .apply("req-1".into(), key, ChatOp::Rename { session_id: "s1".into() })
            .await
            .unwrap();
        waiter.await.unwrap();
        assert_eq!(ok.value, Some(json!(7)));
    }

    #[tokio::test]
    async fn executor_boot_fails_pending_intents() {
        let tmp = TempDir::new("chatc-boot");
        let state = Arc::new(ChatContainerState::new(test_config()));
        state.executor_boot("main").unwrap();
        state.load(&tmp).unwrap();
        let key = state.snapshot().session_vault_key;
        let s2 = state.clone();
        let waiter = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            s2.executor_boot("main").unwrap();
        });
        let err = state
            .apply("req-1".into(), key, ChatOp::Stop)
            .await
            .unwrap_err();
        waiter.await.unwrap();
        assert!(err.contains("执行体重启"));
    }

    // ===== apply：纯 op 与守卫 =====

    /// 纯持久 op 的 apply 不经过任何挂起点（oneshot 未创建），tokio 单线程测试直接 await。
    /// apply 接收 &Arc<Self>，状态以 Arc 持有。
    #[tokio::test]
    async fn apply_pure_create_and_rollback_guards() {
        let tmp = TempDir::new("chatc-pure");
        let state = Arc::new(ChatContainerState::new(test_config()));
        state.load(&tmp).unwrap();
        let key = state.snapshot().session_vault_key;
        // create
        let ok = state
            .apply("r1".into(), key.clone(), ChatOp::Create { opts: None })
            .await
            .unwrap();
        let created = ok.created_session_id.unwrap();
        assert!(!created.is_empty());
        // 追加两条消息后回滚到第一条：截断为 1 条
        {
            let mut inner = state.lock();
            inner
                .session_mut(&created)
                .unwrap()
                .messages
                .extend([msg("a", "user", "x"), msg("b", "assistant", "y")]);
            inner.mark_session_messages(&created);
        }
        let ok = state
            .apply(
                "r2".into(),
                key.clone(),
                ChatOp::Rollback {
                    session_id: created.clone(),
                    message_id: "a".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(ok.fragments.messages.len(), 1);
        let n = state
            .snapshot()
            .sessions
            .into_iter()
            .find(|s| s["id"] == json!(created))
            .map(|s| s["messages"].as_array().unwrap().len())
            .unwrap();
        assert_eq!(n, 1);
        // 流式中的 rollback 静默不动
        {
            state.lock().streaming = true;
        }
        let ok = state
            .apply(
                "r3".into(),
                key,
                ChatOp::Rollback {
                    session_id: created,
                    message_id: "x-missing".into(),
                },
            )
            .await
            .unwrap();
        assert!(fragments_empty(&ok.fragments));
    }
}
