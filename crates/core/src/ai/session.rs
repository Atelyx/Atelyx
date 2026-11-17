//! 会话仓库（磁盘真源）：commit 批次应用、防抖落盘、失败退避与换根纪元守卫。
//! 展示状态（流式 / 压缩中）由编排方自持；本模块只管「磁盘上有什么」。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::ai::session_io::{session_meta_file, SessionIo};
use crate::ai::PENDING_RUN_ID_PREFIX;
use crate::vault::{
    ChatMetaFile, ChatSessionMeta, EditorChatModelOverride, CHAT_HISTORY_DIR, CHAT_MESSAGE_EXT,
    EDITOR_CHATS_META_SCHEMA,
};

/// 写链失败的 persistError 文案（同文案重复失败不重复上报）。
const PERSIST_ERROR_MESSAGE: &str = "会话保存失败，将自动重试";

fn now_ms() -> i64 {
    crate::time::now_ms()
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ===== 写入批次（编排方 → 仓库的声明式变更）=====

/// 新会话登记（id 由调用方先行确定：发送的附件临时目录归属依赖会话 id）。
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
    /// 双层 Option：外层缺省 = 不动，内层 None = 清除（切回默认 Agent）。
    #[serde(default)]
    pub agent_id: Option<Option<String>>,
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

/// 消息内容补丁（rev 单调：流式补丁乱序到达时，迟到的旧补丁不得覆盖新内容）。
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "keepCount")]
    pub keep_count: usize,
}

/// 会话删除：`remove_files` = 连磁盘文件一并删（用户删除）；false = 仅摘除内存登记
/// （磁盘文件已在外部消失的对账路径）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default = "default_true")]
    pub remove_files: bool,
}

/// 单条消息移除（空占位收敛 / 程序化清理；不影响其余消息）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRemoval {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
}

fn default_true() -> bool {
    true
}

/// 覆盖项更新（双层 Option：外层缺省 = 不动，内层 None = 清除覆盖）。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OverridesPatch {
    #[serde(default)]
    pub model_override: Option<Option<Value>>,
    #[serde(default)]
    pub effort_override: Option<Option<String>>,
    #[serde(default)]
    pub agent_override: Option<Option<String>>,
}

/// 一次提交：登记 / 元数据 / 消息追加 / 内容补丁 / 截断 / 删除 / 覆盖项，
/// 原子校验后一次应用并标记脏；落盘由写链异步追平。
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
    pub deletes: Vec<DeleteEntry>,
    #[serde(default)]
    pub message_removals: Vec<MessageRemoval>,
    #[serde(default)]
    pub overrides: OverridesPatch,
}

// ===== 读面快照 =====

/// 落盘失败信息（message + 时间戳；同文案重复失败不更新时间）。
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PersistError {
    pub message: String,
    pub at: i64,
}

/// 仓库读面快照（进仓 / 切仓 / 面板引导拉全量）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSnapshot {
    pub session_vault_key: String,
    pub sessions: Vec<Value>,
    pub model_override: Option<Value>,
    pub effort_override: Option<String>,
    pub agent_override: Option<String>,
    pub persist_error: Option<PersistError>,
}

// ===== 内部状态 =====

/// 会话内存镜像（消息保持原样 JSON；payload 运行时缓存保留在镜像里，出线时剥离）。
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
    /// 装载时磁盘原文与归一后序列化不一致（崩溃残留 running 行等）：保留原文做首次写基线，
    /// 下一次重写把归一行落盘；装载完成后清空。
    loaded_disk: Option<String>,
}

/// 消息 .jsonl 的追加基线：上次成功落盘的内容前缀（确定性序列化，字符串比对判定可追加）。
struct PersistedBase {
    len: usize,
    /// 上次成功落盘的内容（Arc 便于写轮快照零拷贝持有）
    jsonl: Arc<str>,
}

struct Inner {
    /// 会话真源后端（None = 未装载）。本地目录与协作空间用户元数据在此统一。
    backend: Option<Arc<dyn SessionIo>>,
    /// 待删会话侧文件：删除走写盘轮，与在途写盘串行（否则在途全量重写会把已删文件写回）。
    /// 记所属后端——换真源后仍要删回原真源，不落到新真源的同名文件上。
    pending_deletes: Vec<(Arc<dyn SessionIo>, String)>,
    sessions: Vec<Session>,
    persist_error: Option<PersistError>,
    model_override: Option<Value>,
    effort_override: Option<String>,
    agent_override: Option<String>,
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
    /// flush 以请求时刻的脏代数为界：写轮追平该代数即放行，之后的新提交不阻塞
    flush_target: Option<u64>,
    /// 脏变更代数（每次标脏递增）
    dirty_gen: u64,
    retry_attempt: usize,
    next_retry_at: Option<Instant>,
    pending_flush: Vec<tokio::sync::oneshot::Sender<()>>,
    /// 消息补丁 rev（(sessionId, messageId) → 已应用的最大 rev）
    patch_revs: HashMap<(String, String), u64>,
    /// 真源换根纪元（写在途期间换仓库 → 写回结果整体丢弃，防旧仓库基线污染新真源）
    epoch: u64,
}

impl Inner {
    fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    fn has_dirty(&self) -> bool {
        !self.dirty_messages.is_empty()
            || !self.dirty_meta.is_empty()
            || self.overrides_dirty
            || !self.pending_deletes.is_empty()
    }

    /// 写链脏标记（每次内容变化都必须伴随调用，否则变更只存在于内存）。
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
        self.dirty_gen += 1;
        self.last_mark = Some(Instant::now());
    }

    fn mark_overrides(&mut self) {
        self.dirty_gen += 1;
        self.overrides_dirty = true;
        self.overrides_version += 1;
        self.last_mark = Some(Instant::now());
    }
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

/// 落盘失败状态观察者：persistError 置位 / 恢复时回调（锁外调用，可安全转投 UI 消息）。
type StatusHook = Box<dyn Fn(Option<PersistError>) + Send + Sync>;

pub struct SessionStore {
    inner: Mutex<Inner>,
    sched: Condvar,
    /// 写盘轮互斥：换根冲洗与写链线程可能各自发起写轮，串行防双重追加。
    io_lock: Mutex<()>,
    status_hook: Mutex<Option<StatusHook>>,
    config: WriteChainConfig,
    /// 装载票：异步装载（空间）先取票、读真源后才落状态；期间若有更新的装载取票，
    /// 过期票不再改写真源状态（否则慢装载会覆盖新容器的真源）。
    load_seq: std::sync::atomic::AtomicU64,
}

impl SessionStore {
    pub fn new(config: WriteChainConfig) -> Self {
        Self {
            io_lock: Mutex::new(()),
            inner: Mutex::new(Inner {
                backend: None,
                pending_deletes: Vec::new(),
                sessions: Vec::new(),
                persist_error: None,
                model_override: None,
                effort_override: None,
                agent_override: None,
                dirty_messages: HashSet::new(),
                dirty_meta: HashSet::new(),
                overrides_dirty: false,
                persisted: HashMap::new(),
                change_versions: HashMap::new(),
                overrides_version: 0,
                last_mark: None,
                flush_target: None,
                dirty_gen: 0,
                retry_attempt: 0,
                next_retry_at: None,
                pending_flush: Vec::new(),
                patch_revs: HashMap::new(),
                epoch: 0,
            }),
            sched: Condvar::new(),
            status_hook: Mutex::new(None),
            config,
            load_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 取装载票：换容器时先取票，装载（可含网络）完成后凭票落状态；过期票的结果整体丢弃。
    pub fn begin_load(&self) -> u64 {
        self.load_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    /// 安装落盘失败观察者（persistError 置位 / 恢复各回调一次；重复安装覆盖前者）。
    pub fn install_status_hook(&self, f: impl Fn(Option<PersistError>) + Send + Sync + 'static) {
        *self.status_hook.lock().unwrap_or_else(poison) = Some(Box::new(f));
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(poison)
    }

    fn emit_status(&self, status: Option<PersistError>) {
        let hook = self.status_hook.lock().unwrap_or_else(poison);
        if let Some(f) = hook.as_ref() {
            f(status);
        }
    }

    // ===== 装载 / 读面 =====

    pub fn snapshot(&self) -> StoreSnapshot {
        let inner = self.lock();
        snapshot_of(&inner)
    }

    /// 当前仓库内某会话的消息（缺失 = None）。供编排方按需拉取历史（如插件读历史走壳侧）。
    pub fn session_messages(&self, session_id: &str) -> Option<Vec<Value>> {
        let inner = self.lock();
        inner
            .session(session_id)
            .map(|s| s.messages.iter().map(strip_message).collect())
    }

    /// 单会话元数据出线（不含消息；编排方按需补消息，避免全仓库快照克隆）。
    pub fn session_meta(&self, session_id: &str) -> Option<Value> {
        let inner = self.lock();
        inner.session(session_id).map(|s| {
            let mut wire = session_to_wire(s);
            wire.as_object_mut().map(|o| o.remove("messages"));
            wire
        })
    }

    /// 装载真源（进仓 / 切仓 / 进协作空间调用）：同源幂等；换源先落旧真源在途写。
    /// 读真源（可含网络）在锁外完成，只有落状态这一步与写轮串行；返回 None = 本次装载
    /// 已被更新的装载取代（调用方不得再应用其结果）。旧真源仍有未落盘内容（含读真源
    /// 期间到达的提交）时返回 Err 且不切换，脏状态与错误保留供重试。
    ///
    /// 空间后端是阻塞 HTTP，调用方必须在非 tokio 线程上执行（见 `session_io` 模块头）。
    pub fn load(
        &self,
        ticket: u64,
        io: Arc<dyn SessionIo>,
    ) -> Result<Option<StoreSnapshot>, String> {
        let key = io.vault_key();
        {
            let inner = self.lock();
            if backend_key(&inner).as_deref() == Some(key.as_str()) {
                return Ok(Some(snapshot_of(&inner)));
            }
        }
        // 换根前先落旧真源在途写：写不成功即中止切换，保留旧真源与脏状态继续退避重试。
        // 不在这里清 persist_error / dirty——那会让已提交但未落盘的内容无提示丢失。
        if self.lock().has_dirty() {
            self.run_write_cycle();
        }
        {
            let inner = self.lock();
            if inner.has_dirty() {
                let reason = inner
                    .persist_error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "写盘未完成".to_string());
                return Err(format!("旧会话仍有未落盘内容，已保留重试；本次会话装载中止：{reason}"));
            }
        }
        // 读真源：锁外完成（空间后端在此走网络，不能占着 io_lock 让别的装载排队）
        let sessions = scan_sessions(io.as_ref())?;
        let editor_meta = io.read_overrides()?;
        let _io = self.io_lock.lock().unwrap_or_else(poison);
        if self.load_seq.load(std::sync::atomic::Ordering::SeqCst) != ticket {
            return Ok(None); // 期间已有更新的装载：真源状态不归本次改写
        }
        let mut inner = self.lock();
        if backend_key(&inner).as_deref() == Some(key.as_str()) {
            return Ok(Some(snapshot_of(&inner))); // 并发 load 已完成
        }
        // 读真源期间（空间为网络往返）到达的提交会标脏：换状态前必须在同一把内锁下复核，
        // 否则这些内容会随下面的清脏一起丢弃
        if inner.has_dirty() {
            let reason = inner
                .persist_error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "写盘未完成".to_string());
            return Err(format!("旧会话仍有未落盘内容，已保留重试；本次会话装载中止：{reason}"));
        }
        inner.epoch += 1;
        inner.backend = Some(io);
        inner.sessions = sessions;
        inner.persist_error = None;
        inner.dirty_messages.clear();
        inner.dirty_meta.clear();
        inner.overrides_dirty = false;
        inner.persisted = inner
            .sessions
            .iter()
            .map(|s| {
                // 归一过的会话以磁盘原文为基线：首次重写即可落盘归一行
                let jsonl = s
                    .loaded_disk
                    .clone()
                    .unwrap_or_else(|| serialize_messages_jsonl(&s.messages));
                (
                    s.id.clone(),
                    PersistedBase {
                        len: jsonl.lines().count(),
                        jsonl: jsonl.into(),
                    },
                )
            })
            .collect();
        for session in &mut inner.sessions {
            session.loaded_disk = None;
        }
        inner.change_versions.clear();
        inner.patch_revs.clear();
        inner.model_override = editor_meta
            .model_override
            .and_then(|o| serde_json::to_value(o).ok());
        inner.effort_override = editor_meta.effort_override;
        inner.agent_override = editor_meta.agent_override;
        inner.last_mark = None;
        inner.next_retry_at = None;
        inner.retry_attempt = 0;
        Ok(Some(snapshot_of(&inner)))
    }

    // ===== 提交 =====

    /// 编排方变更统一入口：批次校验（未装载 / 会话 id 冲突即整体拒绝）后一次应用并标脏。
    pub fn commit(&self, batch: CommitBatch) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.backend.is_none() {
            return Err("会话仓库未装载".to_string());
        }
        // created 先整体校验（批内重复 / 与既有冲突都拒绝，避免半应用）
        for def in &batch.created {
            if inner.session(&def.id).is_some() {
                return Err(format!("会话已存在：{}", def.id));
            }
        }
        let created_ids: HashSet<&str> = batch.created.iter().map(|d| d.id.as_str()).collect();
        if batch.created.len() != created_ids.len() {
            return Err("批次内会话 id 重复".to_string());
        }
        // appends 先整体校验：目标会话不存在即拒绝整批（占位消息写入失败必须可见，
        // 不能像补丁那样静默丢弃后仍让 run 空跑）
        for append in &batch.appends {
            if !created_ids.contains(append.session_id.as_str())
                && inner.session(&append.session_id).is_none()
            {
                return Err(format!("会话不存在：{}", append.session_id));
            }
        }
        for def in &batch.created {
            let session = Session {
                id: def.id.clone(),
                file: def.file.clone(),
                title: def.title.clone(),
                agent_id: def.agent_id.clone(),
                compaction: def.compaction.clone(),
                created_at: def.created_at,
                updated_at: def.updated_at,
                messages: def.messages.clone(),
                loaded_disk: None,
            };
            inner.mark_session_meta(&def.id);
            if !session.messages.is_empty() {
                inner.mark_session_messages(&def.id);
            }
            inner.sessions.push(session);
        }
        drop(created_ids);
        for patch in &batch.metas {
            if let Some(session) = inner.session_mut(&patch.id) {
                if patch.title.is_some() {
                    session.title = patch.title.clone();
                }
                if let Some(agent_id) = &patch.agent_id {
                    session.agent_id = agent_id.clone();
                }
                if patch.compaction.is_some() {
                    session.compaction = patch.compaction.clone();
                }
                if let Some(at) = patch.updated_at {
                    session.updated_at = at;
                }
                inner.mark_session_meta(&patch.id);
            }
        }
        for append in &batch.appends {
            let Some(session) = inner.session_mut(&append.session_id) else {
                continue; // 会话已删：丢弃
            };
            session.messages.extend(append.messages.iter().cloned());
            session.updated_at = now_ms();
            inner.mark_session_messages(&append.session_id);
        }
        for patch in &batch.patches {
            let key = (patch.session_id.clone(), patch.message_id.clone());
            let applied_rev = inner.patch_revs.get(&key).copied().unwrap_or(0);
            if patch.rev < applied_rev {
                continue; // 迟到的旧补丁不得覆盖新内容
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
            if changed {
                inner.patch_revs.insert(key, patch.rev);
                inner.mark_session_messages(&patch.session_id);
            }
        }
        for trunc in &batch.truncations {
            let Some(session) = inner.session_mut(&trunc.session_id) else {
                continue;
            };
            if trunc.keep_count >= session.messages.len() {
                continue;
            }
            session.messages.truncate(trunc.keep_count);
            session.updated_at = now_ms();
            inner.mark_session_messages(&trunc.session_id);
        }
        for removal in &batch.message_removals {
            if let Some(pos) = inner
                .sessions
                .iter()
                .position(|s| s.id == removal.session_id)
            {
                let had = inner.sessions[pos].messages.len();
                inner.sessions[pos]
                    .messages
                    .retain(|m| message_id_of(m).as_deref() != Some(removal.message_id.as_str()));
                if inner.sessions[pos].messages.len() != had {
                    inner.mark_session_messages(&removal.session_id);
                    inner
                        .patch_revs
                        .remove(&(removal.session_id.clone(), removal.message_id.clone()));
                }
            }
        }
        if batch.overrides.model_override.is_some()
            || batch.overrides.effort_override.is_some()
            || batch.overrides.agent_override.is_some()
        {
            if let Some(ov) = &batch.overrides.model_override {
                inner.model_override = ov.clone();
            }
            if let Some(effort) = &batch.overrides.effort_override {
                inner.effort_override = effort.clone();
            }
            if let Some(agent) = &batch.overrides.agent_override {
                inner.agent_override = agent.clone();
            }
            inner.mark_overrides();
        }
        // 删除：内存立即摘除，落盘排进写盘轮（与在途写盘串行，防在途全量重写把文件写回）
        let to_delete: Vec<(String, String, bool)> = batch
            .deletes
            .iter()
            .filter_map(|d| {
                inner
                    .session(&d.session_id)
                    .map(|s| (s.id.clone(), s.file.clone(), d.remove_files))
            })
            .collect();
        for (id, _, _) in &to_delete {
            if let Some(pos) = inner.sessions.iter().position(|s| &s.id == id) {
                inner.sessions.remove(pos);
            }
            inner.dirty_messages.remove(id);
            inner.dirty_meta.remove(id);
            inner.persisted.remove(id);
            inner.change_versions.remove(id);
            inner
                .patch_revs
                .retain(|(sid, _), _| sid != id);
        }
        if let Some(io) = inner.backend.clone() {
            let mut queued = false;
            for (_, file, remove_files) in &to_delete {
                if !remove_files {
                    continue;
                }
                inner.pending_deletes.push((Arc::clone(&io), file.clone()));
                inner.dirty_gen += 1;
                queued = true;
            }
            if queued {
                // 起防抖计时：删除同样以尾沿 500ms 落，避免与在途写盘抢同一文件
                inner.last_mark = Some(Instant::now());
            }
        }
        drop(inner);
        // 脏标记就绪即唤醒写链线程（写链在无脏时无限期等待，无人唤醒则永不落盘）
        self.sched.notify_all();
        Ok(())
    }

    // ===== 写盘链 =====

    /// 立即执行一轮写盘（flush 与换根装载调用；写失败置 persistError 并安排退避重试）。
    /// io_lock 串行化写轮：collect 与写回之间的锁间隙内到达的删除 / 新提交，
    /// 由 apply 阶段的会话存在性复核兜底。
    pub fn run_write_cycle(&self) {
        let _io = self.io_lock.lock().unwrap_or_else(poison);
        // 本轮快照对应的脏代数：flush 只需追平自己请求时刻的内容
        let (work, gen) = {
            let mut inner = self.lock();
            if !inner.has_dirty() {
                self.ack_flush(&mut inner);
                return;
            }
            let gen = inner.dirty_gen;
            (inner.collect_work(), gen)
        };
        let results = perform_writes(&work);
        let status_change = {
            let mut inner = self.lock();
            inner.apply_write_results(&self.config, &work, results)
        };
        if let Some(status) = status_change {
            self.emit_status(status);
        }
        let mut inner = self.lock();
        let caught_up = inner.flush_target.is_some_and(|target| gen >= target);
        if inner.has_dirty() && inner.persist_error.is_none() && !caught_up {
            // 快照之后又有新提交：继续下一轮，不在未落盘时放行
            self.sched.notify_all();
        } else {
            // 追平请求时刻或写失败时放行：错误已可见，退出 / 关窗不得被持续失败永久挂住
            self.ack_flush(&mut inner);
        }
    }

    /// flush：请求立即写盘，等写轮追平请求时刻的内容即放行（此后新提交不阻塞本次等待）。
    /// resolve 不代表全部写成功——失败已置 persistError 并走退避重试。
    pub async fn flush(&self) -> Result<(), String> {
        let rx = {
            let mut inner = self.lock();
            let (tx, rx) = tokio::sync::oneshot::channel();
            inner.pending_flush.push(tx);
            inner.flush_target = Some(inner.dirty_gen);
            self.sched.notify_all();
            rx
        };
        rx.await.map_err(|_| "写盘轮已终止".to_string())
    }

    fn ack_flush(&self, inner: &mut Inner) {
        inner.flush_target = None;
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
                    // 退避优先：失败后只按 next_retry_at 起轮。若仍看防抖到期，失败轮会立刻
                    // 满足 debounce_due 而成紧循环重试（退避序列形同失效）。
                    let retrying = inner.next_retry_at.is_some();
                    let due = if retrying {
                        inner.next_retry_at.map(|at| now >= at).unwrap_or(false)
                    } else {
                        inner
                            .last_mark
                            .map(|mark| now.duration_since(mark) >= state.config.debounce)
                            .unwrap_or(true)
                    };
                    if inner.flush_target.is_some() || (inner.has_dirty() && due) {
                        break;
                    }
                    // 等待到下一个到期点（无待办则无限等待）
                    let wait = if inner.has_dirty() {
                        if retrying {
                            inner.next_retry_at.map(|at| at.saturating_duration_since(now))
                        } else {
                            let delay = inner
                                .last_mark
                                .map(|mark| state.config.debounce.saturating_sub(now.duration_since(mark)))
                                .unwrap_or(Duration::ZERO);
                            Some(delay)
                        }
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

// ===== 视图 / 序列化 =====

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

fn snapshot_of(inner: &Inner) -> StoreSnapshot {
    StoreSnapshot {
        session_vault_key: inner
            .backend
            .as_ref()
            .map(|io| io.vault_key())
            .unwrap_or_default(),
        sessions: inner.sessions.iter().map(session_to_wire).collect(),
        model_override: inner.model_override.clone(),
        effort_override: inner.effort_override.clone(),
        agent_override: inner.agent_override.clone(),
        persist_error: inner.persist_error.clone(),
    }
}

/// 剥离消息的附件运行时缓存（payload 只活在编排方本地，按 file 引用水合）。
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

/// 会话消息文件名（仓库相对；进仓后的新建会话与测试共用）。
pub fn chat_message_file(session_id: &str) -> String {
    format!("{CHAT_HISTORY_DIR}/{session_id}{CHAT_MESSAGE_EXT}")
}

// ===== 磁盘装载（损坏行跳过、steps 收敛）=====

fn backend_key(inner: &Inner) -> Option<String> {
    inner.backend.as_ref().map(|io| io.vault_key())
}

/// 装载磁盘真源：损坏行跳过、steps 收敛；单文件读取失败即跳过该会话并留日志——
/// 读取失败的会话不进内存，写链就不会用空内容覆盖它（宁可这次看不到，不可丢内容）。
fn scan_sessions(io: &dyn SessionIo) -> Result<Vec<Session>, String> {
    let rows = io.list_sessions()?;
    let mut sessions = Vec::new();
    for row in rows {
        let raw = match io.read_messages(&row.file) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("会话读取失败，本次跳过（不覆盖磁盘内容）：{} — {e}", row.file);
                continue;
            }
        };
        let messages = parse_messages_jsonl_loaded(&raw);
        // 归一后的序列化与磁盘原文不一致时，保留原文做基线：首次写走重写，把归一行落盘
        let disk = raw.trim_end_matches('\n');
        let loaded_disk = (serialize_messages_jsonl(&messages) != disk).then(|| disk.to_string());
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
            loaded_disk,
        });
    }
    Ok(sessions)
}

/// 解析会话消息 .jsonl（逐行 JSON，损坏行跳过；id/createdAt 缺省派生；steps 收敛）。
fn parse_messages_jsonl(jsonl: &str) -> Vec<Value> {
    parse_messages(jsonl, false)
}

/// 装载磁盘真源：进程内不可能有在途 run，任何 running 工具行都归为已中断。
fn parse_messages_jsonl_loaded(jsonl: &str) -> Vec<Value> {
    parse_messages(jsonl, true)
}

fn parse_messages(jsonl: &str, interrupt_running: bool) -> Vec<Value> {
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
                coalesce_agent_steps(&Value::Array(steps.clone()), interrupt_running),
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

/// 归并被拆开的同轮思考/叙述步（幂等）：
/// 按尾向前合并 reasoning/text，遇工具步即停；running 态合成工具行归一为已中断。
fn coalesce_agent_steps(steps: &Value, interrupt_running: bool) -> Value {
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
                if (is_pending || interrupt_running)
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

// ===== 消息 .jsonl 序列化 =====

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
        append_from: Option<(Arc<str>, String)>, // (基线 jsonl, 追加行·带尾 '\n')
        /// 上次落盘基线（重写路径同样先对齐磁盘，防覆盖他端追加）
        base: Option<Arc<str>>,
        rewrite: String,
    },
    Meta {
        file: String,
        meta: ChatSessionMeta,
    },
    Overrides {
        file: ChatMetaFile,
    },
    /// 删会话侧文件（消息 + 元数据）；幂等，项自带所属后端（换真源后仍删回原真源）。
    Delete {
        io: Arc<dyn SessionIo>,
        file: String,
    },
}

struct WorkSnapshot {
    epoch: u64,
    backend: Option<Arc<dyn SessionIo>>,
    items: Vec<WorkItem>,
}

impl Inner {
    fn collect_work(&mut self) -> WorkSnapshot {
        let epoch = self.epoch;
        let backend = self.backend.clone();
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
            let persisted = self.persisted.get(&id).map(|base| (base.len, Arc::clone(&base.jsonl)));
            let append_from = persisted.as_ref().and_then(|(len, jsonl)| {
                if session.messages.len() > *len {
                    // 基线自检：持久化时前缀与当前确定性序列化一致才允许追加
                    let prefix = serialize_messages_jsonl(&session.messages[..*len]);
                    if prefix == jsonl.as_ref() {
                        // tail 以 '\n' 结尾（append 原语约定；磁盘行分隔由它保证）
                        let tail = session.messages[*len..]
                            .iter()
                            .map(serialize_message_line)
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Some((Arc::clone(jsonl), format!("{tail}\n")));
                    }
                }
                None
            });
            items.push(WorkItem {
                kind: WorkKind::Messages {
                    file: session.file.clone(),
                    append_from,
                    rewrite: full,
                    base: persisted.map(|(_, jsonl)| jsonl),
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
            // 压缩注解解析不了就不落这次元数据写：宁可留待下轮，也不静默清掉磁盘上已有的注解
            let compaction = match session.compaction.as_ref() {
                None => None,
                Some(v) => match serde_json::from_value::<crate::vault::ChatCompaction>(v.clone()) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        eprintln!("压缩注解无法写出，本次跳过该会话元数据写盘：{e}");
                        continue;
                    }
                },
            };
            let meta = ChatSessionMeta {
                id: session.id.clone(),
                title: session.title.clone(),
                agent_id: session.agent_id.clone(),
                compaction,
            };
            let meta_file = match session_meta_file(&session.file) {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("会话元数据路径异常，本次跳过：{e}");
                    continue;
                }
            };
            items.push(WorkItem {
                kind: WorkKind::Meta {
                    file: meta_file,
                    meta,
                },
                session_id: Some(id),
                version,
            });
        }
        if self.overrides_dirty {
            // 面板覆盖解析不了就不写：不静默清掉磁盘上已有的模型 / 推理等级覆盖
            let parsed = match self.model_override.as_ref() {
                None => Ok(None),
                Some(v) => serde_json::from_value::<EditorChatModelOverride>(v.clone()).map(Some),
            };
            match parsed {
                Ok(model_override) => items.push(WorkItem {
                    kind: WorkKind::Overrides {
                        file: ChatMetaFile {
                            schema: EDITOR_CHATS_META_SCHEMA.to_string(),
                            model_override,
                            effort_override: self.effort_override.clone(),
                            agent_override: self.agent_override.clone(),
                        },
                    },
                    session_id: None,
                    version: self.overrides_version,
                }),
                Err(e) => eprintln!("面板模型覆盖无法写出，本次跳过该写盘：{e}"),
            }
        }
        // 待删项排在本轮最后：同一文件的本轮写入先落地，删除再收尾，终态恒为「不存在」
        for (io, file) in self.pending_deletes.clone() {
            items.push(WorkItem {
                kind: WorkKind::Delete { io, file },
                session_id: None,
                version: 0,
            });
        }
        WorkSnapshot { epoch, backend, items }
    }

    /// 写回结果应用：基线推进 / 脏清理 / 退避调度 / persistError 置位。
    /// 返回 Some(_) = persistError 状态变化（由 store 收尾回调观察者）。
    fn apply_write_results(
        &mut self,
        config: &WriteChainConfig,
        work: &WorkSnapshot,
        results: Vec<Result<WriteOutcome, String>>,
    ) -> Option<Option<PersistError>> {
        if work.epoch != self.epoch {
            return None; // 写在途期间换仓库：旧仓库写回结果与基线整体丢弃
        }
        let mut failed = false;
        for (item, result) in work.items.iter().zip(results) {
            // 删除项：成功即摘除待删记录；失败保留由下轮重试（意图不丢，文件不残留）
            if let WorkKind::Delete { io, file } = &item.kind {
                match result {
                    Ok(_) => {
                        let key = io.vault_key();
                        self.pending_deletes.retain(|(pending_io, pending_file)| {
                            !(pending_file == file && pending_io.vault_key() == key)
                        });
                    }
                    Err(e) => {
                        failed = true;
                        eprintln!("删除会话文件失败（将重试）：{file} — {e}");
                    }
                }
                continue;
            }
            // 写轮在途期间会话被删：丢弃该项（不推进基线、不清脏），删除项在本轮末尾收尾
            if let Some(id) = &item.session_id {
                if self.session(id).is_none() {
                    continue;
                }
            }
            match result {
                Ok(outcome) => {
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
                        let jsonl = match &outcome {
                            // 合并写：磁盘内容已含他端消息，内存镜像一并对齐，后续追加不再触发漂移
                            WriteOutcome::Merged(content) => {
                                let merged = parse_messages_jsonl(content);
                                if !merged.is_empty() {
                                    if let Some(session) = self.session_mut(id) {
                                        if let Some(ts) = merged
                                            .last()
                                            .and_then(|m| m.get("createdAt"))
                                            .and_then(Value::as_i64)
                                        {
                                            session.updated_at = session.updated_at.max(ts);
                                        }
                                        session.messages = merged;
                                    }
                                    self.bump_session(id);
                                }
                                content.trim_end_matches('\n').to_string()
                            }
                            WriteOutcome::Written => match append_from {
                                Some((base_jsonl, tail)) => {
                                    if base_jsonl.is_empty() {
                                        tail.trim_end_matches('\n').to_string()
                                    } else {
                                        format!("{}\n{}", base_jsonl.as_ref(), tail.trim_end_matches('\n'))
                                    }
                                }
                                None => rewrite.clone(),
                            },
                        };
                        self.persisted.insert(
                            id.clone(),
                            PersistedBase {
                                len: jsonl.lines().count(),
                                jsonl: jsonl.into(),
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
                        WorkKind::Delete { .. } => {}
                    }
                }
                Err(e) => {
                    failed = true;
                    eprintln!("会话仓库写盘失败：{e}");
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
            // 同文案的重复失败不重复上报（at 变化无信息量）；首次失败/恢复才回调
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
                Some(self.persist_error.clone())
            } else {
                None
            }
        } else if work.items.is_empty() && self.has_dirty() {
            // 本轮没有产出任何工作项但仍有脏项（注解解析失败被跳过等）：
            // 同样按退避节奏重试，否则写链线程会以全速紧循环空转
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
            None
        } else {
            self.retry_attempt = 0;
            self.next_retry_at = None;
            if self.persist_error.take().is_some() {
                Some(None)
            } else {
                None
            }
        }
    }
}

/// 写轮成功形态：Merged 带回实际落盘内容（基线按它推进，不能用被覆盖的本地快照）。
enum WriteOutcome {
    Written,
    Merged(String),
}

/// 执行写盘工作项（锁外 I/O；各项独立成败）。追加前由后端按磁盘事实复核基线，
/// 基线漂移（他端 / 他设备先写了）时读回磁盘按消息 id 合并后全量写，不整份覆盖他端内容。
fn perform_writes(work: &WorkSnapshot) -> Vec<Result<WriteOutcome, String>> {
    work.items
        .iter()
        .map(|item| match &item.kind {
            // 删除项自带后端（换真源后仍删回原真源）
            WorkKind::Delete { io, file } => {
                io.delete_session_files(file).map(|_| WriteOutcome::Written)
            }
            WorkKind::Messages {
                file,
                append_from,
                rewrite,
                base,
            } => {
                let Some(io) = &work.backend else {
                    return Err("会话仓库未装载，无法写盘".to_string());
                };
                if let Some((base, tail)) = append_from {
                    match io.append_messages(file, base.as_ref(), tail) {
                        Ok(None) => return Ok(WriteOutcome::Written),
                        // 空间并发漂移后已按合并内容整值写回：基线/内存按实际内容推进
                        Ok(Some(content)) => return Ok(WriteOutcome::Merged(content)),
                        Err(_) => {}
                    }
                    // 追加失败（多为基线漂移）：读回磁盘 / 服务端按消息 id 合并再全量写。
                    // 文件不在或读不出时保底整份写（与既有回落一致），不在错误路径上无限重试。
                    let content = match io.read_messages(file) {
                        Ok(existing) => merge_jsonl_messages(&existing, rewrite),
                        Err(_) => rewrite.clone(),
                    };
                    io.write_messages(file, &content)?;
                    return Ok(WriteOutcome::Merged(content));
                }
                // 重写路径（流式内容更新 / 截断）同样先看磁盘事实：他端在本地基线之后追加的
                // 消息不得被整份覆盖；磁盘与基线一致时照常直写，基线之外无法归并时维持整份写。
                if let Some(base) = base {
                    if let Ok(existing) = io.read_messages(file) {
                        let on_disk = existing.strip_suffix('\n').unwrap_or(&existing);
                        if let Some(tail) = on_disk
                            .strip_prefix(base.as_ref())
                            .and_then(|rest| rest.strip_prefix('\n'))
                        {
                            let content = merge_rewrite_with_foreign_tail(rewrite, tail);
                            io.write_messages(file, &content)?;
                            return Ok(WriteOutcome::Merged(content));
                        }
                    }
                }
                io.write_messages(file, rewrite)
                    .map(|_| WriteOutcome::Written)
            }
            WorkKind::Meta { file, meta } => match &work.backend {
                Some(io) => io.write_session_meta(file, meta).map(|_| WriteOutcome::Written),
                None => Err("会话仓库未装载，无法写盘".to_string()),
            },
            WorkKind::Overrides { file } => match &work.backend {
                Some(io) => io.write_overrides(file).map(|_| WriteOutcome::Written),
                None => Err("会话仓库未装载，无法写盘".to_string()),
            },
        })
        .collect()
}

/// 重写前与磁盘合并：本地全量在前，磁盘基线之后多出的他端追加行按 id 去重接尾。
fn merge_rewrite_with_foreign_tail(rewrite: &str, foreign_tail: &str) -> String {
    let ids: HashSet<String> = rewrite.lines().filter_map(jsonl_line_id).collect();
    let mut out = rewrite.trim_end_matches('\n').to_string();
    for line in foreign_tail.lines().filter(|line| !line.trim().is_empty()) {
        if jsonl_line_id(line).is_some_and(|id| ids.contains(&id)) {
            continue;
        }
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// 追加基线漂移后的合并：保留磁盘现有行序，追加本端新消息（按消息 id 去重）。
pub(crate) fn merge_jsonl_messages(existing: &str, incoming: &str) -> String {
    let mut ids: HashSet<String> = HashSet::new();
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(id) = jsonl_line_id(line) {
            ids.insert(id);
        }
        lines.push(line.to_string());
    }
    for line in incoming.lines().filter(|l| !l.trim().is_empty()) {
        match jsonl_line_id(line) {
            Some(id) => {
                if ids.insert(id) {
                    lines.push(line.to_string());
                }
            }
            // 无 id 的异常行按整行去重（解析侧同样会丢弃这类行）
            None => {
                if !lines.iter().any(|l| l == line) {
                    lines.push(line.to_string());
                }
            }
        }
    }
    format!("{}\n", lines.join("\n"))
}

fn jsonl_line_id(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("id")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::session_io::{appended_body, local_io, matches_append_base};
    use crate::vault::ChatSessionRow;
    use crate::vault::append_chat_messages_raw;
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

    fn parse_skips_fixture() -> &'static str {
        "{\"id\":\"a\",\"role\":\"user\",\"content\":\"你好\",\"createdAt\":10}\n\
         not-json-at-all\n\
         {\"role\":\"assistant\",\"content\":\"缺 id 派生\"}\n\
         {\"id\":\"b\",\"role\":\"assistant\",\"content\":\"ok\",\"createdAt\":20,\"junk\":1}"
    }

    #[test]
    fn parse_skips_corrupt_lines_and_derives_defaults() {
        let msgs = parse_messages_jsonl(parse_skips_fixture());
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["id"], json!("a"));
        assert!(msgs[1]["id"].as_str().is_some());
        assert_eq!(msgs[1]["createdAt"], json!(1));
        assert_eq!(msgs[2]["id"], json!("b"));
        assert!(msgs[2].get("junk").is_none());
    }

    #[test]
    fn serialize_round_trip_strips_payload_and_extra() {
        let messages = [json!({
            "id": "a",
            "role": "user",
            "content": "带附件",
            "createdAt": 5,
            "junk": "extra",
            "attachments": [
                { "file": "x.png", "payload": "runtime-blob" },
                { "file": "y.png" }
            ],
            "steps": [{ "kind": "reasoning", "text": "想" }]
        })];
        let line = serialize_message_line(&messages[0]);
        let parsed = parse_messages_jsonl(&line);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].get("junk").is_none());
        let atts = parsed[0]["attachments"].as_array().unwrap();
        assert!(atts.iter().all(|a| a.get("payload").is_none()));
        assert!(atts[0].get("file").is_some());
    }

    #[test]
    fn coalesce_merges_adjacent_text_steps_and_stops_at_tool() {
        let steps = json!([
            { "kind": "reasoning", "text": "想" },
            { "kind": "reasoning", "text": "继续" },
            { "kind": "text", "text": "叙述" },
            { "kind": "text", "text": "接续" },
            { "kind": "tool", "run": { "id": "r1", "name": "read_file", "status": "done" } },
            { "kind": "reasoning", "text": "新一轮" }
        ]);
        let out = coalesce_agent_steps(&steps, false);
        let items = out.as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0]["text"], json!("想继续"));
        assert_eq!(items[1]["text"], json!("叙述接续"));
        assert_eq!(items[2]["kind"], json!("tool"));
        assert_eq!(items[3]["text"], json!("新一轮"));
    }

    #[test]
    fn coalesce_normalizes_running_pending_tool_to_interrupted() {
        let steps = json!([
            { "kind": "tool", "run": { "id": "pending:1:0", "name": "web_search", "status": "running" } }
        ]);
        let out = coalesce_agent_steps(&steps, false);
        let run = &out.as_array().unwrap()[0]["run"];
        assert_eq!(run["status"], json!("error"));
        assert_eq!(run["resultSummary"], json!("（已中断）"));
    }

    #[test]
    fn loaded_running_rows_stay_interrupted_after_first_append() {
        let dir = TempDir::new("ai-session-load-normalize");
        let file = chat_message_file("s1");
        let raw = json!({
            "id": "m1",
            "role": "assistant",
            "content": "",
            "createdAt": 1,
            "steps": [{ "kind": "tool", "run": {
                "id": "real-run-1", "name": "read_file", "status": "running"
            }}]
        })
        .to_string()
            + "\n";
        crate::vault::write_chat_messages_file(&dir, &file, &raw).unwrap();
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        assert_eq!(
            store.snapshot().sessions[0]["messages"][0]["steps"][0]["run"]["status"],
            json!("error")
        );
        // 追加触发写链：归一结果必须落盘且不被合并回 running
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m2", "user", "x")],
            }]))
            .unwrap();
        store.run_write_cycle();
        let disk = crate::vault::read_chat_messages_file(&dir, &file).unwrap();
        let parsed = parse_messages_jsonl(&disk);
        assert_eq!(parsed[0]["steps"][0]["run"]["status"], json!("error"));
        assert_eq!(parsed[0]["steps"][0]["run"]["resultSummary"], json!("（已中断）"));
        let snap = store.snapshot();
        assert_eq!(snap.sessions[0]["messages"][0]["steps"][0]["run"]["status"], json!("error"));
        assert_eq!(snap.sessions[0]["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn load_normalizes_running_tool_rows_with_real_ids() {
        let raw = "{\"id\":\"m1\",\"role\":\"assistant\",\"content\":\"\",\"createdAt\":1,\"steps\":[{\"kind\":\"tool\",\"run\":{\"id\":\"real-run-1\",\"name\":\"read_file\",\"status\":\"running\"}}]}\n";
        let parsed = parse_messages_jsonl_loaded(raw);
        let run = &parsed[0]["steps"][0]["run"];
        assert_eq!(run["status"], json!("error"));
        assert_eq!(run["resultSummary"], json!("（已中断）"));
        // 运行期的合并解析不得误伤在途行（真源不是本次进程装载）
        let live = parse_messages_jsonl(raw);
        assert_eq!(live[0]["steps"][0]["run"]["status"], json!("running"));
    }

    fn commit_batch(appends: Vec<AppendBatch>) -> CommitBatch {
        CommitBatch {
            appends,
            ..Default::default()
        }
    }

    #[test]
    fn write_chain_appends_then_rewrites_and_stays_idempotent() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        let sid = "s1";
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: sid.into(),
                    file: chat_message_file(sid),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "第一")],
                }],
                ..Default::default()
            })
            .unwrap();
        // 追加阶段：同一会话追加两条
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: sid.into(),
                messages: vec![msg("m2", "assistant", "回复一"), msg("m3", "user", "第二")],
            }]))
            .unwrap();
        store.run_write_cycle();
        // 追加写在途模拟：再追加一条（脏未落）后立刻全量重写轮
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: sid.into(),
                messages: vec![msg("m4", "assistant", "回复二")],
            }]))
            .unwrap();
        store.run_write_cycle();
        store.run_write_cycle(); // 幂等：无脏不再写
        let on_disk = crate::vault::read_chat_messages_file(&dir, &chat_message_file(sid))
            .unwrap();
        let lines: Vec<&str> = on_disk.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 4);
        let reread = store.snapshot();
        let s = reread
            .sessions
            .iter()
            .find(|s| s["id"] == json!(sid))
            .unwrap();
        assert_eq!(s["messages"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn append_raw_requires_verified_disk_base() {
        let dir = TempDir::new("ai-session");
        let file = chat_message_file("s1");
        // 文件缺失：追加原语直接报错（调用方须先核对磁盘再追加）
        assert!(append_chat_messages_raw(&dir, &file, "{\"x\":1}\n").is_err());
        crate::vault::write_chat_messages_file(&dir, &file, "{\"id\":\"a\"}").unwrap();
        // 磁盘内容与基线不一致：追加前的核对会失败（此处直接验证原语本身的行为边界）
        assert!(
            append_chat_messages_raw(&dir, &file, "{\"id\":\"b\"}\n").is_ok(),
            "原语只负责原样落盘；基线核对由写盘轮的 matches_base 承担"
        );
    }

    #[test]
    fn write_failure_sets_persist_error_and_recovers() {
        let dir = TempDir::new("ai-session");
        let config = WriteChainConfig {
            debounce: Duration::ZERO,
            retries: vec![Duration::ZERO],
        };
        let store = SessionStore::new(config);
        let events = Arc::new(Mutex::new(Vec::<Option<PersistError>>::new()));
        let sink = Arc::clone(&events);
        store.install_status_hook(move |status| sink.lock().unwrap().push(status));
        // 未装载仓库时提交的脏数据写盘必失败
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        // 制造写失败：把目标文件位置换成目录（写路径 = <root>/.atelyx/对话历史/<id>.jsonl）
        let inner_root = dir.join(CHAT_HISTORY_DIR);
        std::fs::create_dir_all(&inner_root).unwrap();
        store.run_write_cycle();
        assert!(events.lock().unwrap().is_empty(), "正常写盘不应上报");
        // 制造写失败：把目标文件替换为目录
        std::fs::remove_file(inner_root.join("s1.jsonl")).ok();
        std::fs::create_dir_all(inner_root.join("s1.jsonl")).unwrap();
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m2", "assistant", "y")],
            }]))
            .unwrap();
        store.run_write_cycle();
        assert_eq!(events.lock().unwrap().len(), 1, "首次失败应上报");
        assert!(events.lock().unwrap()[0].is_some());
        // 重复失败（同文案）不再上报
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m3", "user", "z")],
            }]))
            .unwrap();
        store.run_write_cycle();
        assert_eq!(events.lock().unwrap().len(), 1);
        // 恢复：移除目录障碍后全量重写成功 → 上报恢复
        std::fs::remove_dir_all(inner_root.join("s1.jsonl")).unwrap();
        store.run_write_cycle();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2, "恢复应上报一次");
        assert!(events[1].is_none());
    }

    #[test]
    fn append_to_missing_session_is_rejected() {
        let dir = TempDir::new("ai-session-missing");
        let store = SessionStore::new(test_config());
        store
            .load(store.begin_load(), local_io(dir.to_path_buf()))
            .unwrap();
        let error = store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "nope".into(),
                messages: vec![msg("m1", "user", "x")],
            }]))
            .unwrap_err();
        assert!(error.contains("会话不存在"), "{error}");
        assert!(store.snapshot().sessions.is_empty(), "拒绝的批次不得半应用");
    }

    #[test]
    fn append_baseline_drift_merges_other_device_messages() {
        let dir = TempDir::new("ai-session-merge");
        let store = SessionStore::new(test_config());
        store
            .load(store.begin_load(), local_io(dir.to_path_buf()))
            .unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "一")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 他端先追加：本端下次追加的基线核对必然失败
        crate::vault::append_chat_messages_raw(
            &dir,
            &chat_message_file("s1"),
            "{\"id\":\"remote\",\"role\":\"user\",\"content\":\"他端\",\"createdAt\":2}\n",
        )
        .unwrap();
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m2", "assistant", "二")],
            }]))
            .unwrap();
        store.run_write_cycle();
        let ids = |raw: &str| -> Vec<String> {
            parse_messages_jsonl(raw)
                .iter()
                .filter_map(|m| m.get("id")?.as_str().map(str::to_string))
                .collect()
        };
        let disk = crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).unwrap();
        assert_eq!(ids(&disk), vec!["m1", "remote", "m2"], "他端消息不得被覆盖");
        assert!(
            store.snapshot().persist_error.is_none(),
            "合并成功不应报写失败"
        );
        // 合并后基线已对齐：后续追加不再重复漂移
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m3", "user", "三")],
            }]))
            .unwrap();
        store.run_write_cycle();
        let disk = crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).unwrap();
        assert_eq!(ids(&disk), vec!["m1", "remote", "m2", "m3"]);
    }

    #[test]
    fn rewrite_preserves_messages_appended_after_base() {
        let dir = TempDir::new("ai-session-rewrite-merge");
        let store = SessionStore::new(test_config());
        store
            .load(store.begin_load(), local_io(dir.to_path_buf()))
            .unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "一")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 他端在本端基线之后追加：本端随后更新已有消息（走重写路径），不得整份覆盖
        crate::vault::append_chat_messages_raw(
            &dir,
            &chat_message_file("s1"),
            "{\"id\":\"remote\",\"role\":\"user\",\"content\":\"他端\",\"createdAt\":2}\n",
        )
        .unwrap();
        store
            .commit(CommitBatch {
                patches: vec![PatchEntry {
                    session_id: "s1".into(),
                    message_id: "m1".into(),
                    rev: 1,
                    content: Some("更新后".into()),
                    steps: None,
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        let disk = crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).unwrap();
        let messages = parse_messages_jsonl(&disk);
        let ids: Vec<&str> = messages.iter().filter_map(|m| m["id"].as_str()).collect();
        assert_eq!(ids, vec!["m1", "remote"], "他端消息不得被重写覆盖");
        assert_eq!(messages[0]["content"], json!("更新后"));
        assert_eq!(messages[1]["content"], json!("他端"));
        assert!(store.snapshot().persist_error.is_none(), "合并成功不应报写失败");
    }

    #[test]
    fn load_aborts_switch_while_old_backend_still_dirty() {
        let dir = TempDir::new("ai-session-switch");
        let other = TempDir::new("ai-session-switch-other");
        let store = SessionStore::new(test_config());
        store
            .load(store.begin_load(), local_io(dir.to_path_buf()))
            .unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 制造写失败：目标文件位置被目录占用
        let inner_root = dir.join(CHAT_HISTORY_DIR);
        std::fs::remove_file(inner_root.join("s1.jsonl")).ok();
        std::fs::create_dir_all(inner_root.join("s1.jsonl")).unwrap();
        store
            .commit(commit_batch(vec![AppendBatch {
                session_id: "s1".into(),
                messages: vec![msg("m2", "assistant", "y")],
            }]))
            .unwrap();
        store.run_write_cycle();
        let before = store.snapshot();
        assert!(before.persist_error.is_some(), "写失败应可见");
        let result = store.load(store.begin_load(), local_io(other.to_path_buf()));
        let Err(error) = result else {
            panic!("旧真源仍有脏内容时装载应中止");
        };
        assert!(error.contains("未落盘"), "{error}");
        let after = store.snapshot();
        assert_eq!(after.session_vault_key, before.session_vault_key, "不得切到新真源");
        assert!(after.persist_error.is_some(), "错误与脏状态应保留");
        assert_eq!(after.sessions.len(), 1, "会话不应被新真源替换");
    }

    #[test]
    fn flush_waits_for_write_cycle() {
        let dir = TempDir::new("ai-session");
        let store = Arc::new(SessionStore::new(WriteChainConfig {
            debounce: Duration::from_secs(3600), // 防抖拉长：只允许 flush 通道触发写盘
            retries: vec![Duration::ZERO],
        }));
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        // 走真实路径：写链线程在后台，flush 等待本轮完成
        Arc::clone(&store).spawn_writer();
        let handle = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(store.flush())
            })
        };
        assert!(handle.join().unwrap().is_ok());
        // 落盘已发生
        assert!(
            crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).is_ok(),
            "flush 返回前消息应已落盘"
        );
    }

    #[test]
    fn flush_resolves_after_catching_up_request_time_content() {
        let gate = Arc::new(Gate::new());
        let fake = Arc::new(FakeIo::with_gate(Arc::clone(&gate)));
        let store = Arc::new(SessionStore::new(WriteChainConfig {
            debounce: Duration::from_secs(3600), // 只允许 flush 通道触发起轮
            retries: vec![Duration::ZERO],
        }));
        let io: Arc<dyn SessionIo> = fake.clone();
        store.load(store.begin_load(), io).unwrap();
        store
            .commit(CommitBatch {
                created: vec![def("s1")],
                appends: vec![AppendBatch {
                    session_id: "s1".into(),
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        let _writer = Arc::clone(&store).spawn_writer();
        // 写轮在途（阻塞在写入中途），flush 请求发生在此刻
        let writing = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.run_write_cycle())
        };
        fake.wait_write_started();
        let (tx, rx) = std::sync::mpsc::channel();
        let flushing = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                tx.send(rt.block_on(store.flush())).unwrap();
            })
        };
        // 请求时刻之后的提交：不得阻塞本次 flush 放行
        store
            .commit(CommitBatch {
                appends: vec![AppendBatch {
                    session_id: "s1".into(),
                    messages: vec![msg("m2", "assistant", "y")],
                }],
                ..Default::default()
            })
            .unwrap();
        gate.release();
        writing.join().unwrap();
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("flush 应在追平请求时刻内容后放行");
        assert!(result.is_ok());
        flushing.join().unwrap();
    }

    #[test]
    fn truncate_falls_back_to_full_rewrite() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: None,
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "一"), msg("m2", "assistant", "二"), msg("m3", "user", "三")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 截断到 1 条：基线（3 行）与内存失配 → 追加被拒 → 全量重写
        store
            .commit(CommitBatch {
                truncations: vec![TruncateEntry {
                    session_id: "s1".into(),
                    keep_count: 1,
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        let on_disk =
            crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).unwrap();
        let lines: Vec<&str> = on_disk.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 1, "截断后磁盘应只剩 1 行：{on_disk}");
        // 再追加：新基线下追加路径正常
        store
            .commit(CommitBatch {
                appends: vec![AppendBatch {
                    session_id: "s1".into(),
                    messages: vec![msg("m4", "assistant", "四")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        let on_disk =
            crate::vault::read_chat_messages_file(&dir, &chat_message_file("s1")).unwrap();
        assert_eq!(on_disk.trim_end_matches('\n').split('\n').count(), 2);
    }

    #[test]
    fn commit_rev_gating() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        store
            .commit(CommitBatch {
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
            })
            .unwrap();
        let patch = |rev: u64, content: &str| CommitBatch {
            patches: vec![PatchEntry {
                session_id: "s1".into(),
                message_id: "m1".into(),
                rev,
                content: Some(content.into()),
                steps: None,
            }],
            ..Default::default()
        };
        store.commit(patch(3, "新")).unwrap();
        store.commit(patch(2, "旧")).unwrap(); // 迟到补丁被拒
        let s = store
            .snapshot()
            .sessions
            .into_iter()
            .find(|s| s["id"] == json!("s1"))
            .unwrap();
        assert_eq!(s["messages"][0]["content"], json!("新"));
        store.commit(patch(3, "同 rev 覆盖")).unwrap(); // rev 相等仍应用（同帧重发语义）
        let s = store
            .snapshot()
            .sessions
            .into_iter()
            .find(|s| s["id"] == json!("s1"))
            .unwrap();
        assert_eq!(s["messages"][0]["content"], json!("同 rev 覆盖"));
    }

    #[test]
    fn commit_create_conflicts_rejected_atomically() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        let def = |id: &str| SessionDef {
            id: id.into(),
            file: chat_message_file(id),
            title: None,
            agent_id: None,
            compaction: None,
            created_at: 1,
            updated_at: 1,
            messages: vec![],
        };
        store
            .commit(CommitBatch {
                created: vec![def("a")],
                ..Default::default()
            })
            .unwrap();
        // 与既有冲突：整体拒绝，批内另一条不落
        let err = store
            .commit(CommitBatch {
                created: vec![def("b"), def("a")],
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.contains("会话已存在"));
        let snap = store.snapshot();
        assert!(snap.sessions.iter().all(|s| s["id"] != json!("b")));
        // 批内重复同样拒绝
        assert!(store
            .commit(CommitBatch {
                created: vec![def("c"), def("c")],
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn delete_removes_files_and_detach_keeps_them() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        let def = |id: &str| SessionDef {
            id: id.into(),
            file: chat_message_file(id),
            title: None,
            agent_id: None,
            compaction: None,
            created_at: 1,
            updated_at: 1,
            messages: vec![msg("m1", "user", "x")],
        };
        store
            .commit(CommitBatch {
                created: vec![def("keep-files"), def("gone")],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        let msg_file = |id: &str| dir.join(&chat_message_file(id));
        assert!(msg_file("keep-files").exists());
        assert!(msg_file("gone").exists());
        // remove_files = false：只摘内存登记（磁盘文件已在外部消失的对账路径）
        store
            .commit(CommitBatch {
                deletes: vec![DeleteEntry {
                    session_id: "keep-files".into(),
                    remove_files: false,
                }],
                ..Default::default()
            })
            .unwrap();
        assert!(msg_file("keep-files").exists());
        assert_eq!(
            store
                .snapshot()
                .sessions
                .iter()
                .map(|s| s["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!("gone")],
            "detach 只摘除目标会话，其余保留"
        );
        // remove_files = true：连文件删
        store
            .commit(CommitBatch {
                created: vec![def("tmp")],
                ..Default::default()
            })
            .unwrap();
        store
            .commit(CommitBatch {
                deletes: vec![DeleteEntry {
                    session_id: "tmp".into(),
                    remove_files: true,
                }],
                ..Default::default()
            })
            .unwrap();
        assert!(!msg_file("tmp").exists());
    }

    #[test]
    fn load_resets_and_rereads_disk() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        store
            .commit(CommitBatch {
                created: vec![SessionDef {
                    id: "s1".into(),
                    file: chat_message_file("s1"),
                    title: Some("标题".into()),
                    agent_id: None,
                    compaction: None,
                    created_at: 1,
                    updated_at: 1,
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 同根幂等
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        assert_eq!(store.snapshot().sessions.len(), 1);
        // 换根再回：磁盘重建（写入已在途刷掉）
        let other = TempDir::new("ai-session");
        store.load(store.begin_load(), local_io(other.to_path_buf())).unwrap();
        assert!(store.snapshot().sessions.is_empty());
        store.load(store.begin_load(), local_io(dir.to_path_buf())).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0]["title"], json!("标题"));
        assert_eq!(snap.session_vault_key, format!("local:{}", dir.to_string_lossy()));
    }

    // ===== 写链时序（注入真源后端：读失败与在途删除的不变量）=====

    /// 写盘门：让写轮停在中途，构造「写盘在途 + 删除」的真实交错。
    struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Gate {
        fn new() -> Self {
            Self { open: Mutex::new(false), cv: Condvar::new() }
        }

        fn pass(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.cv.wait(open).unwrap();
            }
        }

        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    /// 内存真源：可注入读失败、可让写入阻塞；不落真实磁盘，只验写链时序与内容不变量。
    #[derive(Default)]
    struct FakeIo {
        files: Mutex<HashMap<String, String>>,
        unreadable: Mutex<HashSet<String>>,
        gate: Option<Arc<Gate>>,
        write_log: Mutex<Vec<String>>,
        wrote: Mutex<bool>,
    }

    impl FakeIo {
        fn with_gate(gate: Arc<Gate>) -> Self {
            Self { gate: Some(gate), ..Default::default() }
        }

        fn seed(&self, file: &str, content: &str) {
            self.files.lock().unwrap().insert(file.to_string(), content.to_string());
        }

        fn deny_read(&self, file: &str) {
            self.unreadable.lock().unwrap().insert(file.to_string());
        }

        fn content(&self, file: &str) -> Option<String> {
            self.files.lock().unwrap().get(file).cloned()
        }

        fn writes(&self) -> Vec<String> {
            self.write_log.lock().unwrap().clone()
        }

        /// 等写轮进入写盘（写轮在另一线程上跑，轮询到它真的开始再制造交错）。
        fn wait_write_started(&self) {
            for _ in 0..300 {
                if *self.wrote.lock().unwrap() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("写轮未进入写盘");
        }

        fn enter_write(&self, file: &str) {
            self.write_log.lock().unwrap().push(file.to_string());
            *self.wrote.lock().unwrap() = true;
            if let Some(gate) = &self.gate {
                gate.pass();
            }
        }
    }

    impl SessionIo for FakeIo {
        fn vault_key(&self) -> String {
            "fake:io".to_string()
        }

        fn list_sessions(&self) -> Result<Vec<ChatSessionRow>, String> {
            let prefix = format!("{CHAT_HISTORY_DIR}/");
            Ok(self
                .files
                .lock()
                .unwrap()
                .keys()
                .filter_map(|file| {
                    let id = file.strip_prefix(&prefix)?.strip_suffix(CHAT_MESSAGE_EXT)?;
                    Some(ChatSessionRow { id: id.to_string(), file: file.clone(), meta: None })
                })
                .collect())
        }

        fn read_messages(&self, file: &str) -> Result<String, String> {
            if self.unreadable.lock().unwrap().contains(file) {
                return Err("读取失败".to_string());
            }
            self.files
                .lock()
                .unwrap()
                .get(file)
                .cloned()
                .ok_or_else(|| format!("会话消息不存在：{file}"))
        }

        fn write_messages(&self, file: &str, content: &str) -> Result<(), String> {
            self.enter_write(file);
            self.files.lock().unwrap().insert(file.to_string(), content.to_string());
            Ok(())
        }

        fn append_messages(&self, file: &str, base: &str, tail: &str) -> Result<Option<String>, String> {
            self.enter_write(file);
            let mut files = self.files.lock().unwrap();
            let existing = files.get(file).cloned().unwrap_or_default();
            if !matches_append_base(&existing, base) {
                return Err("内容与追加基线不一致".to_string());
            }
            files.insert(file.to_string(), appended_body(base, tail));
            Ok(None)
        }

        fn read_session_meta(&self, _file: &str) -> Result<Option<ChatSessionMeta>, String> {
            Ok(None)
        }

        fn write_session_meta(&self, _file: &str, _meta: &ChatSessionMeta) -> Result<(), String> {
            Ok(())
        }

        fn delete_session_files(&self, file: &str) -> Result<(), String> {
            self.write_log.lock().unwrap().push(format!("delete:{file}"));
            self.files.lock().unwrap().remove(file);
            Ok(())
        }

        fn read_overrides(&self) -> Result<ChatMetaFile, String> {
            Ok(ChatMetaFile::default())
        }

        fn write_overrides(&self, _file: &ChatMetaFile) -> Result<(), String> {
            Ok(())
        }
    }

    fn def(id: &str) -> SessionDef {
        SessionDef {
            id: id.to_string(),
            file: chat_message_file(id),
            title: None,
            agent_id: None,
            compaction: None,
            created_at: 1,
            updated_at: 1,
            messages: Vec::new(),
        }
    }

    #[test]
    fn delete_committed_during_in_flight_write_leaves_no_file() {
        let gate = Arc::new(Gate::new());
        let fake = Arc::new(FakeIo::with_gate(Arc::clone(&gate)));
        let store = Arc::new(SessionStore::new(test_config()));
        let io: Arc<dyn SessionIo> = fake.clone();
        store.load(store.begin_load(), io).unwrap();
        store
            .commit(CommitBatch {
                created: vec![def("s1")],
                appends: vec![AppendBatch {
                    session_id: "s1".into(),
                    messages: vec![msg("m1", "user", "x")],
                }],
                ..Default::default()
            })
            .unwrap();
        // 写轮在途（阻塞在写入中途），此时用户删掉该会话
        let writing = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.run_write_cycle())
        };
        fake.wait_write_started();
        store
            .commit(CommitBatch {
                deletes: vec![DeleteEntry { session_id: "s1".into(), remove_files: true }],
                ..Default::default()
            })
            .unwrap();
        gate.release();
        writing.join().unwrap();
        // 收尾轮执行删除：终态必须是「文件不存在」（在途全量重写不得把已删文件留下）
        store.run_write_cycle();
        assert_eq!(fake.content(&chat_message_file("s1")), None);
    }

    #[test]
    fn unreadable_session_is_skipped_and_never_overwritten() {
        let file = chat_message_file("s1");
        let raw = "{\"id\":\"m1\",\"role\":\"user\",\"content\":\"原文\",\"createdAt\":1}\n".to_string();
        let fake = Arc::new(FakeIo::default());
        fake.seed(&file, &raw);
        fake.deny_read(&file);
        let store = SessionStore::new(test_config());
        let io: Arc<dyn SessionIo> = fake.clone();
        let snap = store.load(store.begin_load(), io).unwrap().expect("首次装载应落状态");
        // 读失败 = 本次看不到（不落内存基线），而不是「空会话」
        assert!(snap.sessions.is_empty());
        // 其余会话正常写入也不得触碰它：磁盘内容原样、从未写过该文件
        store
            .commit(CommitBatch {
                created: vec![def("s2")],
                appends: vec![AppendBatch {
                    session_id: "s2".into(),
                    messages: vec![msg("m2", "user", "y")],
                }],
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        assert_eq!(fake.content(&file), Some(raw));
        assert!(!fake.writes().iter().any(|f| f == &file));
    }

    #[test]
    fn agent_override_persists_and_reloads() {
        let dir = TempDir::new("ai-session");
        let store = SessionStore::new(test_config());
        store
            .load(store.begin_load(), local_io(dir.to_path_buf()))
            .unwrap();
        store
            .commit(CommitBatch {
                overrides: OverridesPatch {
                    agent_override: Some(Some("builtin-chat".into())),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        store.run_write_cycle();
        // 磁盘文件带 agentOverride；新 store 重载后快照带回
        let meta = std::fs::read_to_string(
            dir.to_path_buf()
                .join(".atelyx")
                .join("editor-chats-meta.json"),
        )
        .unwrap();
        assert!(meta.contains("agentOverride"), "落盘应含 agentOverride：{meta}");
        let store2 = SessionStore::new(test_config());
        let snap = store2
            .load(store2.begin_load(), local_io(dir.to_path_buf()))
            .unwrap()
            .unwrap();
        assert_eq!(snap.agent_override.as_deref(), Some("builtin-chat"));
        // 清除覆盖 → 磁盘为空 → 重载为 None
        store2
            .commit(CommitBatch {
                overrides: OverridesPatch {
                    agent_override: Some(None),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        store2.run_write_cycle();
        let snap = store2
            .load(store2.begin_load(), local_io(dir.to_path_buf()))
            .unwrap()
            .unwrap();
        assert!(snap.agent_override.is_none());
    }
}
