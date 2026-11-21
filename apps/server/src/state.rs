//! 服务端持久化状态：数据目录布局（见 `Persisted`）+ 内存态 + 原子写。
//! 全部结构化元数据是小规模数据（目标 ≤30 人），读入内存、每次变更整文件原子写，备份 = 拷目录。
//! 启动加载遇文件损坏直接拒绝启动——账号与权限数据静默重置等于放开权限。

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;

use crate::index::SpaceIndexCache;
use crate::ws::Hub;
use crate::ApiError;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 空间创建者角色：改名 / 邀请 / 移除成员 / 转让。
pub const ROLE_OWNER: &str = "owner";
/// 空间读写角色：内容读写。
pub const ROLE_EDITOR: &str = "editor";
/// 空间只读角色：内容/索引/团队元数据只读；自身 user 元数据可写；可读成员名册、可入实时频道。
pub const ROLE_VIEWER: &str = "viewer";

#[derive(Serialize, Deserialize, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub token_hash: String,
    pub device_name: String,
    /// 客户端稳定设备 id（None = 旧版客户端的会话；新客户端登录时按残留清理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Member {
    pub user_id: String,
    pub role: String,
    pub joined_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub owner_user_id: String,
    pub created_at: i64,
    pub members: Vec<Member>,
    /// 收编的既有目录（绝对路径）。None = 默认布局（数据目录下 spaces/<id>/）。
    /// 内容文件树与个人仓库同构（自由文件夹的 .md/.atlx/.atb）；隐藏目录 `.atelyx/`
    /// 只承载随内容走的历史侧文件（不进树/索引/检索，见 history 模块）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Invite {
    pub code: String,
    pub space_id: String,
    pub role: String,
    pub created_by: String,
    pub created_at: i64,
    /// 过期时刻（unix 秒；None = 长期有效）。
    pub expires_at: Option<i64>,
    /// 次数上限（None = 不限）。
    pub max_uses: Option<u32>,
    pub used_count: u32,
}

/// 数据目录（`DATA_DIR`，默认 `./data`）中的结构化元数据，对应四个 JSON 名册文件：
/// `accounts.json` 用户（argon2 密码哈希）、`sessions.json` 设备会话（令牌只存 SHA-256 摘要，
/// 文件泄露不等于令牌泄露）、`spaces.json` 空间 + 成员名册（含角色）、`invites.json`
/// 邀请码（角色 + 过期 + 次数上限）；空间内容文件树在 `spaces/<id>/`（真源）。
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Persisted {
    pub(crate) users: Vec<User>,
    pub(crate) sessions: Vec<Session>,
    pub(crate) spaces: Vec<Space>,
    pub(crate) invites: Vec<Invite>,
}

/// axum 状态要求 Clone：内部全部经 Arc 共享（每个连接克隆的是句柄不是数据）。
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<ServerStateInner>,
}

/// 名册内存态 + 落盘监管：内存变更（`mutate`）与落盘解耦。
/// 内存锁只保护内存态本身（鉴权与成员校验只短暂取它，磁盘阻塞不串行全站请求）；
/// 提交锁把「快照 → 变更 → 落盘 → 成败处理」整体串行化，保证任一时刻至多一批在途：
/// 失败时内存回滚到本批快照，磁盘由写者写回上一批内容，二者必一致。
#[derive(Default)]
struct Journal {
    locked: Mutex<JournalState>,
    commit: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct JournalState {
    memory: Persisted,
    /// 最后发起的提交号（单调递增，日志按它定位批次）。
    seq: u64,
    /// 在途提交的提交号；None = 无在途提交（内存与磁盘一致）。
    dirty_seq: Option<u64>,
}

/// 一个待落盘的名册批次：提交号递增即提交顺序，写者按该顺序落地。
struct PersistJob {
    seq: u64,
    files: Vec<(&'static str, serde_json::Value)>,
    /// 落盘结果回执（`mutate` 也据此判定成败并按 seq 决定是否回滚内存）。
    ack: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// 默认落盘实现：四个名册文件逐个原子写，任一失败即中止（已写成功的由写者按快照回滚）。
fn persist_files(data_dir: &Path, files: &[(&'static str, serde_json::Value)]) -> Result<(), String> {
    for (name, value) in files {
        atomic_write_json(&data_dir.join(name), value)?;
    }
    Ok(())
}

struct ServerStateInner {
    data_dir: PathBuf,
    /// 进程启动时刻（unix 秒；管理台运行状态展示用）。
    started_at: i64,
    journal: Arc<Journal>,
    /// 名册落盘串行通道（构造或首次落盘时拉起单写者任务，见 `state::persist_worker`）。
    writer: tokio::sync::OnceCell<tokio::sync::mpsc::UnboundedSender<PersistJob>>,
    /// 派生索引缓存（空间 id → 反链/标签索引；纯内存只读派生，可随时重建，不落盘）。
    index_cache: Arc<crate::index::SpaceIndexCache>,
    /// WS 房间表（`/ws/space` 入口的房间机制）。
    hub: Hub,
    /// 内容写按路径串行化的锁表（整文件写与补丁端点共用）。
    path_locks: PathLocks,
    /// 空间级结构锁表（补丁与改名/移动串行化；与 path_locks 独立成表，键不含路径）。
    structure_locks: PathLocks,
}

/// 内容写并发模型：同一路径同时只允许一个写者——读改写（读文件 → 合并 → 原子写）
/// 全程在锁内完成，两个并发写严格按到达序先后落地，后写者基于前写者的结果，无交叉损坏；
/// 跨路径不互斥。每路径一把异步锁（等待者让出任务而非阻塞线程），条目在无任何
/// 持有/等待者后从表内摘除，防长期运行无界增长。
pub(crate) struct PathLocks {
    map: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

/// 持有期间独占该路径的写权；释放时（Drop）顺带摘除无主条目。
pub(crate) struct PathLockGuard {
    map: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    key: String,
    // 手动管理释放顺序：先真正释放写锁、再查引用计数摘条目（字段先于 Drop 体析构会看不到真实计数）
    guard: std::mem::ManuallyDrop<tokio::sync::OwnedMutexGuard<()>>,
}

impl PathLocks {
    pub(crate) fn new() -> Self {
        Self { map: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// 取 `key`（空间 id + 相对路径）对应的锁并等待持有。
    pub(crate) async fn lock(&self, key: &str) -> PathLockGuard {
        let entry = {
            let mut map = self.map.lock().unwrap();
            map.entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = entry.clone().lock_owned().await;
        PathLockGuard {
            map: self.map.clone(),
            key: key.to_string(),
            guard: std::mem::ManuallyDrop::new(guard),
        }
    }
}

impl Drop for PathLockGuard {
    fn drop(&mut self) {
        // 先释放写锁，引用计数才反映真实的等待者数量
        unsafe { std::mem::ManuallyDrop::drop(&mut self.guard) };
        // 摘除判定在表锁内完成：其余取锁者要么已克隆走 Arc（计数 > 1，不摘），
        // 要么尚未取表锁（自会插入新条目），不存在「条目被摘而旧锁仍在服」的窗口
        let mut map = self.map.lock().unwrap();
        if let Some(entry) = map.get(&self.key) {
            if Arc::strong_count(entry) == 1 {
                map.remove(&self.key);
            }
        }
    }
}

/// 500 内部错误响应。
pub fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// 400 请求错误响应。
pub fn bad_request(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_string())
}

/// 404 资源不存在响应。
pub fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.to_string())
}

/// 403 无权限响应。
pub fn forbidden(message: &str) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, message.to_string())
}

/// unix 秒时刻。
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 随机十六进制 id（`bytes` 字节随机数 → 2×bytes 个 hex 字符）。
pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 令牌摘要：服务端只存 SHA-256(token)，令牌原文只在签发响应中出现一次。
pub fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 单键值字节上限默认值（配置类 value，过大拖累整目录读取与序列化）。
pub const DEFAULT_MAX_META_VALUE_BYTES: usize = 10 * 1024 * 1024;

use std::sync::atomic::{AtomicUsize, Ordering};

// 测试用覆盖：进程内全局，0 = 走默认值。集成测试经 set_size_limit_override 调小以低成本验证限额。
static FILE_LIMIT_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
static META_LIMIT_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// 测试用：临时覆盖单文件 / 单键值字节上限（传 None 恢复默认）。
pub fn set_size_limit_override(file: Option<usize>, meta: Option<usize>) {
    FILE_LIMIT_OVERRIDE.store(file.unwrap_or(0), Ordering::SeqCst);
    META_LIMIT_OVERRIDE.store(meta.unwrap_or(0), Ordering::SeqCst);
}

/// 当前生效的单文件字节上限（默认值来自 content::MAX_FILE_BYTES，可被测试覆盖）。
pub fn max_file_bytes() -> usize {
    let v = FILE_LIMIT_OVERRIDE.load(Ordering::SeqCst);
    if v == 0 { crate::content::MAX_FILE_BYTES } else { v }
}

/// 当前生效的单键值字节上限。
pub fn max_meta_value_bytes() -> usize {
    let v = META_LIMIT_OVERRIDE.load(Ordering::SeqCst);
    if v == 0 { DEFAULT_MAX_META_VALUE_BYTES } else { v }
}

/// 名册落盘写者：单任务串行消费提交（顺序 = 提交号顺序），每批四个文件按序原子写；
/// 失败时把上一批已写成功的新文件重写回该批快照（尽力而为，失败只记日志），
/// 再回报结果由 `mutate` 回滚内存。
mod persist_worker {
    use std::path::Path;
    use std::sync::Arc;

    use super::{atomic_write_json, PersistJob};

    type PersistFn = dyn Fn(&Path, &[(&'static str, serde_json::Value)]) -> Result<(), String> + Send + Sync;

    /// 拉起写者任务；当前线程没有 tokio 运行时时返回 None（由调用方在落盘时重试）。
    pub(super) fn spawn(data_dir: std::path::PathBuf, persist: Arc<PersistFn>) -> Option<tokio::sync::mpsc::UnboundedSender<PersistJob>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PersistJob>();
        // 只持数据目录、不持状态本体：`ServerState` 全部句柄释放后通道关闭，写者任务随之退出
        handle.spawn(async move {
            // 上一批成功落盘的内容：本批失败时写回它，磁盘回到「失败的操作不生效」的状态
            // （本批可能已写成功前几个文件，不回写会留下半截新态）
            let mut last_written: Option<Vec<(&'static str, serde_json::Value)>> = None;
            while let Some(job) = rx.recv().await {
                let seq = job.seq;
                let result = persist(&data_dir, &job.files);
                match &result {
                    Ok(()) => last_written = Some(job.files),
                    Err(e) => {
                        tracing::error!(seq, "名册落盘失败：{e}");
                        if let Some(baseline) = &last_written {
                            restore(&data_dir, baseline, e);
                        }
                    }
                }
                let _ = job.ack.send(result);
            }
        });
        Some(tx)
    }

    /// 提交一批并取回执（写者已停止 = 通道关闭，按失败回报，不静默丢变更）。
    pub(super) fn submit(
        sender: &tokio::sync::mpsc::UnboundedSender<PersistJob>,
        seq: u64,
        files: Vec<(&'static str, serde_json::Value)>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, super::ApiError> {
        let (ack, rx) = tokio::sync::oneshot::channel();
        sender
            .send(PersistJob { seq, files, ack })
            .map_err(|_| super::internal("名册写者已停止"))?;
        Ok(rx)
    }

    /// 失败回滚：把已落盘的新文件重写回基线内容，让磁盘回到与内存一致的状态。
    fn restore(data_dir: &Path, previous: &[(&'static str, serde_json::Value)], cause: &str) {
        for (name, value) in previous {
            if let Err(e) = atomic_write_json(&data_dir.join(name), value) {
                tracing::error!(file = name, cause = %cause, "名册回滚写盘失败：{e}");
            }
        }
    }
}

/// 快照 → 待落盘值（四个名册文件）。
fn describe(snapshot: &Persisted) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("accounts.json", serde_json::to_value(&snapshot.users).expect("名册快照序列化必成")),
        ("sessions.json", serde_json::to_value(&snapshot.sessions).expect("名册快照序列化必成")),
        ("spaces.json", serde_json::to_value(&snapshot.spaces).expect("名册快照序列化必成")),
        ("invites.json", serde_json::to_value(&snapshot.invites).expect("名册快照序列化必成")),
    ]
}

/// 原子写 JSON 文件（临时文件 + rename，防半截文件）。
pub fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    crate::fsops::atomic_write(path, text.as_bytes())
}

fn load_array<T: serde::de::DeserializeOwned>(data_dir: &Path, name: &str) -> Vec<T> {
    let path = data_dir.join(name);
    if !path.exists() {
        return Vec::new();
    }
    // 文件存在即必须可解析（含 0 字节）：截断/清空可能是损坏前兆，静默当空集等于重置账号与权限
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {name} 失败：{e}（拒绝启动，请从备份恢复）"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} 解析失败：{e}（拒绝启动，请从备份恢复）"))
}

impl ServerState {
    /// 打开（或初始化）数据目录。
    pub fn open(data_dir: &Path) -> Self {
        Self::open_with_persist(data_dir, persist_files)
    }

    /// `open` 的可注入落盘实现（测试用：替换成慢盘 / 必失败 / 可观测顺序的替身）。
    /// `persist` 在写者任务线程上被调用，一次提交一批（四个名册文件按序写）。
    pub fn open_with_persist(
        data_dir: &Path,
        persist: impl Fn(&Path, &[(&'static str, serde_json::Value)]) -> Result<(), String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        std::fs::create_dir_all(data_dir.join("spaces")).expect("创建数据目录失败");
        let memory = Persisted {
            users: load_array(data_dir, "accounts.json"),
            sessions: load_array(data_dir, "sessions.json"),
            spaces: load_array(data_dir, "spaces.json"),
            invites: load_array(data_dir, "invites.json"),
        };
        let journal = Arc::new(Journal {
            locked: Mutex::new(JournalState { memory, ..Default::default() }),
            commit: tokio::sync::Mutex::new(()),
        });
        let inner = Arc::new(ServerStateInner {
            data_dir: data_dir.to_path_buf(),
            started_at: now_secs(),
            journal,
            writer: tokio::sync::OnceCell::new(),
            index_cache: Arc::new(SpaceIndexCache::new()),
            hub: Hub::default(),
            path_locks: PathLocks::new(),
            structure_locks: PathLocks::new(),
        });
        let persist: Arc<dyn Fn(&Path, &[(&'static str, serde_json::Value)]) -> Result<(), String> + Send + Sync> =
            Arc::new(persist);
        // 运行时缺席（非 tokio 线程上构造）时留空槽：首次落盘再拉起写者
        if let Some(sender) = persist_worker::spawn(data_dir.to_path_buf(), persist) {
            let _ = inner.writer.set(sender);
        }
        Self { inner }
    }

    /// 只读访问（不落盘）。
    pub(crate) fn read<R>(&self, f: impl FnOnce(&Persisted) -> R) -> R {
        let guard = self.inner.journal.locked.lock().unwrap();
        f(&guard.memory)
    }

    /// 按令牌摘要查会话并刷新内存 last_seen（不落盘：会话集合不变的高频请求不写盘）。
    /// 顺带带出账号显示名：署名类写入只认令牌反查的身份，不采信请求体。
    /// 只取内存锁（落盘在写者任务上进行），磁盘阻塞不会串行鉴权。
    pub(crate) fn authenticate(&self, token_hash: &str) -> Option<crate::auth::AuthUser> {
        let now = now_secs();
        let mut guard = self.inner.journal.locked.lock().unwrap();
        let (user_id, session_id) = {
            let session = guard.memory.sessions.iter_mut().find(|s| s.token_hash == token_hash)?;
            session.last_seen_at = now;
            (session.user_id.clone(), session.id.clone())
        };
        let account = guard.memory.users.iter().find(|u| u.id == user_id);
        let username = account.map(|u| u.username.clone()).unwrap_or_default();
        let display_name = account.map(|u| u.display_name.clone()).unwrap_or_default();
        Some(crate::auth::AuthUser { user_id, session_id, username, display_name })
    }

    /// 刷新会话的内存 last_seen（随下次结构变更落盘）。
    pub(crate) fn touch_session(&self, session_id: &str, at: i64) {
        let mut guard = self.inner.journal.locked.lock().unwrap();
        if let Some(s) = guard.memory.sessions.iter_mut().find(|s| s.id == session_id) {
            s.last_seen_at = at;
        }
    }

    /// 变更并持久化（闭包内完成权限判定与状态修改）。落盘失败即向调用方报错——
    /// 内存与磁盘不一致比拒绝一次操作更危险。
    pub(crate) async fn mutate<R>(
        &self,
        f: impl FnOnce(&mut Persisted) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        // 提交串行化：多批在途时前批失败回滚会让后批带上已回滚的变更落盘，
        // 内存与磁盘从此分歧；串行化后至多一批在途，回滚边界唯一
        let _commit = self.inner.journal.commit.lock().await;
        let (seq, snapshot, files, out) = {
            let mut g = self.inner.journal.locked.lock().unwrap();
            // 先快照再变更：落盘失败时回滚内存，保证「失败的操作不生效」
            let snapshot = g.memory.clone();
            let out = match f(&mut g.memory) {
                Ok(out) => out,
                Err(e) => {
                    g.memory = snapshot;
                    return Err(e);
                }
            };
            let files = describe(&g.memory);
            g.seq += 1;
            g.dirty_seq = Some(g.seq);
            (g.seq, snapshot, files, out)
        };
        // 锁外落盘：等待期间不持内存锁，鉴权与其他读操作照常（写者按提交序落地）
        let committed = match self.send_persist(seq, files) {
            Ok(rx) => rx.await.unwrap_or_else(|_| Err("名册写者已停止".to_string())),
            Err(e) => Err(e.1),
        };
        let mut g = self.inner.journal.locked.lock().unwrap();
        // seq 守卫：只清理本次提交的在途标记，不回滚别人的
        if g.dirty_seq == Some(seq) {
            g.dirty_seq = None;
        }
        if let Err(message) = committed {
            // 写者已把磁盘写回上一批内容；内存回滚本批快照，二者一致
            g.memory = snapshot;
            tracing::error!(seq, "名册落盘失败：{message}");
            return Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("名册落盘失败：{message}"),
            ));
        }
        Ok(out)
    }

    /// 取写者通道（首次调用时拉起写者任务）；同步上下文中没有运行时则返回错误。
    fn send_persist(
        &self,
        seq: u64,
        files: Vec<(&'static str, serde_json::Value)>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, ApiError> {
        if let Some(sender) = self.inner.writer.get() {
            return persist_worker::submit(sender, seq, files);
        }
        let started =
            persist_worker::spawn(self.inner.data_dir.clone(), std::sync::Arc::new(persist_files));
        match started {
            Some(sender) => {
                let sent = persist_worker::submit(&sender, seq, files);
                let _ = self.inner.writer.set(sender);
                sent
            }
            None => Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "名册写者不可用（无 tokio 运行时）".to_string(),
            )),
        }
    }

    /// 进程启动时刻（unix 秒）。
    pub(crate) fn started_at(&self) -> i64 {
        self.inner.started_at
    }

    /// 派生索引缓存句柄（index 端点经此取用与刷新）。
    pub(crate) fn index_cache(&self) -> &crate::index::SpaceIndexCache {
        &self.inner.index_cache
    }

    /// WS 房间表句柄（空间频道 `/ws/space` 入房用）。
    pub(crate) fn hub(&self) -> Hub {
        self.inner.hub.clone()
    }

    /// 内容写按路径串行化的锁（同路径互斥、跨路径无碍；key = 空间 id + 相对路径，
    /// 不同空间的同形路径互不阻塞）。
    pub(crate) async fn path_lock(&self, space_id: &str, rel: &str) -> PathLockGuard {
        self.inner.path_locks.lock(&format!("{space_id}/{rel}")).await
    }

    /// 空间级结构锁：补丁端点与改名端点都先取本锁再取路径锁。补丁显式携带 title 时按标题
    /// 推导落盘路径，可能对请求路径之外的第二条路径写/删——与改名的双路径锁若各自按不同顺序两两取锁
    /// 无法静态排出全局顺序（第二把锁的 key 要读了文件才知道），统一先串行到本锁上消解死锁，
    /// 顺带把「补丁改名」与「改名」完全互斥。持有时长为单请求处理窗口（毫秒级）。
    pub(crate) async fn structure_lock(&self, space_id: &str) -> PathLockGuard {
        self.inner.structure_locks.lock(space_id).await
    }

    /// 默认布局的空间内容根（未收编目录时的落点）。
    pub fn default_space_root(&self, space_id: &str) -> PathBuf {
        self.inner.data_dir.join("spaces").join(space_id)
    }

    /// 空间内容根的磁盘路径（已收编目录用登记路径，未收编回落默认布局根）。
    pub fn space_content_root(&self, space: &Space) -> PathBuf {
        match &space.root_path {
            Some(p) => PathBuf::from(p),
            None => self.default_space_root(&space.id),
        }
    }

    /// 数据目录（收编校验用：空间内容根不得与数据目录互相嵌套，防元数据混进内容树）。
    pub(crate) fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Arc};
    use std::time::{Duration, Instant};

    use super::*;

    /// 落盘替身：记录每批写到的账号名，再按真实实现落盘（顺序可断言、磁盘内容可断言）。
    fn recorded(
        dir: &Path,
        seen: &Arc<Mutex<Vec<Vec<String>>>>,
        files: &[(&'static str, serde_json::Value)],
    ) -> Result<(), String> {
        if let Some((_, accounts)) = files.iter().find(|(name, _)| *name == "accounts.json") {
            let names = accounts
                .as_array()
                .map(|rows| rows.iter().filter_map(|u| u["username"].as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            seen.lock().unwrap().push(names);
        }
        persist_files(dir, files)
    }

    #[tokio::test]
    async fn concurrent_mutates_persist_in_commit_order() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let state = ServerState::open_with_persist(dir.path(), move |dir, files| recorded(dir, &recorder, files));
        let (a, b) = tokio::join!(
            state.mutate(|p| {
                p.users.push(User {
                    id: "a".to_string(),
                    username: "alice".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "alice".to_string(),
                    created_at: 0,
                });
                Ok(())
            }),
            state.mutate(|p| {
                p.users.push(User {
                    id: "b".to_string(),
                    username: "bob".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "bob".to_string(),
                    created_at: 0,
                });
                Ok(())
            })
        );
        a.expect("第一批应当落盘成功");
        b.expect("第二批应当落盘成功");
        let waves = seen.lock().unwrap().clone();
        assert_eq!(waves.len(), 2, "每批提交各落盘一次：{waves:?}");
        assert!(waves[0].len() < waves[1].len(), "落盘顺序必须是提交顺序（先 1 人后 2 人）：{waves:?}");
        // 磁盘最终内容 = 内存态
        let disk: Vec<User> =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("accounts.json")).unwrap()).unwrap();
        let memory = state.read(|p| p.users.iter().map(|u| u.id.clone()).collect::<Vec<_>>());
        assert_eq!(disk.iter().map(|u| u.id.clone()).collect::<Vec<_>>(), memory);
        assert_eq!(memory.len(), 2);
    }

    #[tokio::test]
    async fn persist_failure_rolls_back_memory_and_later_writes_still_land() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let state = ServerState::open_with_persist(dir.path(), move |dir, files| {
            // 第 2 批必失败：模拟一次瞬时写盘错误
            if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err("模拟磁盘错误".to_string());
            }
            persist_files(dir, files)
        });
        state
            .mutate(|p| {
                p.users.push(User {
                    id: "a".to_string(),
                    username: "alice".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "alice".to_string(),
                    created_at: 0,
                });
                Ok(())
            })
            .await
            .unwrap();
        let failed = state
            .mutate(|p| {
                p.users.push(User {
                    id: "b".to_string(),
                    username: "bob".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "bob".to_string(),
                    created_at: 0,
                });
                Ok(())
            })
            .await;
        assert!(failed.is_err(), "写盘失败必须向调用方报错");
        let memory = state.read(|p| p.users.iter().map(|u| u.username.clone()).collect::<Vec<_>>());
        assert_eq!(memory, vec!["alice"], "失败的操作不得留在内存");
        let disk: Vec<User> =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("accounts.json")).unwrap()).unwrap();
        assert_eq!(disk.iter().map(|u| u.username.clone()).collect::<Vec<_>>(), vec!["alice"], "磁盘只保留成功的变更");
        // 后续成功变更照常落盘（失败不吞掉后面的提交）
        state
            .mutate(|p| {
                p.users.push(User {
                    id: "c".to_string(),
                    username: "carol".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "carol".to_string(),
                    created_at: 0,
                });
                Ok(())
            })
            .await
            .expect("写盘恢复后应照常成功");
        let disk: Vec<User> =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("accounts.json")).unwrap()).unwrap();
        assert_eq!(
            disk.iter().map(|u| u.username.clone()).collect::<Vec<_>>(),
            vec!["alice", "carol"],
            "后续变更必须落盘，且不带已回滚的变更"
        );
    }

    #[tokio::test]
    async fn slow_disk_persist_does_not_block_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let slow = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let (flag, gate, begun) = (slow.clone(), slow.clone(), started.clone());
        let probe = begun.clone();
        let state = ServerState::open_with_persist(dir.path(), move |dir, files| {
            if gate.load(Ordering::SeqCst) {
                probe.store(true, Ordering::SeqCst);
                // 慢盘：落盘阻塞 400ms 后失败
                std::thread::sleep(Duration::from_millis(400));
                return Err("模拟慢盘".to_string());
            }
            persist_files(dir, files)
        });
        // 先建账号与会话（此时落盘正常）
        state
            .mutate(|p| {
                p.users.push(User {
                    id: "u".to_string(),
                    username: "alice".to_string(),
                    password_hash: "x".to_string(),
                    display_name: "alice".to_string(),
                    created_at: 0,
                });
                p.sessions.push(Session {
                    id: "s".to_string(),
                    user_id: "u".to_string(),
                    token_hash: "t".to_string(),
                    device_name: "d".to_string(),
                    device_id: None,
                    created_at: 0,
                    last_seen_at: 0,
                });
                Ok(())
            })
            .await
            .unwrap();
        flag.store(true, Ordering::SeqCst);
        let slow_state = state.clone();
        let blocked = tokio::spawn(async move {
            slow_state
                .mutate(|p| {
                    p.spaces.push(Space {
                        id: "sp".to_string(),
                        name: "空间".to_string(),
                        owner_user_id: "u".to_string(),
                        created_at: 0,
                        members: vec![],
                        root_path: None,
                    });
                    Ok(())
                })
                .await
        });
        // 等写者真的开始落盘（内存锁此刻早已释放）
        while !started.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let t0 = Instant::now();
        let who = state.authenticate("t");
        let elapsed = t0.elapsed();
        assert!(who.is_some(), "慢盘期间鉴权仍须可用");
        assert!(elapsed < Duration::from_millis(100), "鉴权不得等落盘：{elapsed:?}");
        assert!(blocked.await.unwrap().is_err(), "慢盘落盘失败应按错误返回");
    }
}
