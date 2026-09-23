//! 服务端持久化状态：数据目录布局 + 内存态 + 原子写。
//!
//! 数据目录布局（`DATA_DIR`，默认 `./data`）：
//!
//! ```text
//! data/
//!   accounts.json    用户（argon2 密码哈希）
//!   sessions.json    设备会话（令牌只存 SHA-256 摘要，文件泄露不等于令牌泄露）
//!   spaces.json      空间 + 成员名册（含角色）
//!   invites.json     邀请码（角色 + 过期 + 次数上限）
//!   spaces/<id>/     空间内容文件树（真源）
//! ```
//!
//! 全部结构化元数据是小规模数据（目标 ≤30 人），读入内存、每次变更整文件原子写；备份 = 拷目录。
//! 启动加载遇文件损坏直接拒绝启动——账号与权限数据静默重置等于放开权限，不可接受。
//! `last_seen_at` 只在内存刷新、随下次结构变更落盘：会话集合不变的高频请求不产生写盘。

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;

use crate::index::SpaceIndex;
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

struct ServerStateInner {
    data_dir: PathBuf,
    persistent: Mutex<Persisted>,
    /// 派生索引缓存（空间 id → 反链/标签索引；纯内存只读派生，可随时重建，不落盘）。
    index_cache: Mutex<HashMap<String, SpaceIndex>>,
    /// WS 房间表：`/ws` 与 `/ws/space` 两个入口共用一套房间机制。
    pub hub: Hub,
    /// 内容写按路径串行化的锁表（整文件写与补丁端点共用）。
    path_locks: PathLocks,
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

/// unix 秒时刻。
pub fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

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
        std::fs::create_dir_all(data_dir.join("spaces")).expect("创建数据目录失败");
        let inner = Persisted {
            users: load_array(data_dir, "accounts.json"),
            sessions: load_array(data_dir, "sessions.json"),
            spaces: load_array(data_dir, "spaces.json"),
            invites: load_array(data_dir, "invites.json"),
        };
        Self {
            inner: Arc::new(ServerStateInner {
                data_dir: data_dir.to_path_buf(),
                persistent: Mutex::new(inner),
                index_cache: Mutex::new(HashMap::new()),
                hub: Hub::default(),
                path_locks: PathLocks::new(),
            }),
        }
    }

    /// 只读访问（不落盘）。
    pub(crate) fn read<R>(&self, f: impl FnOnce(&Persisted) -> R) -> R {
        let guard = self.inner.persistent.lock().unwrap();
        f(&guard)
    }

    /// 按令牌摘要查会话并刷新内存 last_seen（不落盘：会话集合不变的高频请求不写盘）。
    pub(crate) fn authenticate(&self, token_hash: &str) -> Option<crate::auth::AuthUser> {
        let now = now_secs();
        let mut guard = self.inner.persistent.lock().unwrap();
        let session = guard.sessions.iter_mut().find(|s| s.token_hash == token_hash)?;
        session.last_seen_at = now;
        Some(crate::auth::AuthUser { user_id: session.user_id.clone(), session_id: session.id.clone() })
    }

    /// 刷新会话的内存 last_seen（随下次结构变更落盘）。
    pub(crate) fn touch_session(&self, session_id: &str, at: i64) {
        let mut guard = self.inner.persistent.lock().unwrap();
        if let Some(s) = guard.sessions.iter_mut().find(|s| s.id == session_id) {
            s.last_seen_at = at;
        }
    }

    /// 变更并持久化（闭包内完成权限判定与状态修改）。落盘失败即向调用方报错——
    /// 内存与磁盘不一致比拒绝一次操作更危险。
    pub(crate) fn mutate<R>(&self, f: impl FnOnce(&mut Persisted) -> Result<R, ApiError>) -> Result<R, ApiError> {
        let mut guard = self.inner.persistent.lock().unwrap();
        // 先快照再变更：落盘失败时回滚内存与已写文件，保证「失败的操作不生效」
        // （否则调用方收到错误，但变更已在内存生效直到重启——对账号/权限类操作尤其危险）
        let snapshot = guard.clone();
        let out = f(&mut guard)?;
        let files: [(&str, serde_json::Value); 4] = [
            ("accounts.json", serde_json::to_value(&guard.users).map_err(internal)?),
            ("sessions.json", serde_json::to_value(&guard.sessions).map_err(internal)?),
            ("spaces.json", serde_json::to_value(&guard.spaces).map_err(internal)?),
            ("invites.json", serde_json::to_value(&guard.invites).map_err(internal)?),
        ];
        let mut written: Vec<&str> = Vec::new();
        for (name, value) in files {
            match atomic_write_json(&self.inner.data_dir.join(name), &value) {
                Ok(()) => written.push(name),
                Err(e) => {
                    // 回滚内存；已写成功的新文件重写回快照内容（尽力而为，再失败只能记录——
                    // 此时磁盘部分回旧、内存已回旧，重启后按磁盘旧态加载，偏差仅限写回失败的文件）
                    *guard = snapshot.clone();
                    for done in written {
                        let old = match done {
                            "accounts.json" => serde_json::to_value(&snapshot.users),
                            "sessions.json" => serde_json::to_value(&snapshot.sessions),
                            "spaces.json" => serde_json::to_value(&snapshot.spaces),
                            _ => serde_json::to_value(&snapshot.invites),
                        };
                        if let Ok(old) = old {
                            if let Err(re) = atomic_write_json(&self.inner.data_dir.join(done), &old) {
                                tracing::error!(file = done, "回滚写盘失败：{re}");
                            }
                        }
                    }
                    return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e));
                }
            }
        }
        Ok(out)
    }

    /// 派生索引缓存句柄（index 模块经此刷新与查询）。
    pub(crate) fn inner_index_cache(&self) -> &Mutex<HashMap<String, SpaceIndex>> {
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

    /// 默认布局的空间内容根（未收编目录时的落点）。
    pub fn default_space_root(&self, space_id: &str) -> PathBuf {
        self.inner.data_dir.join("spaces").join(space_id)
    }

    /// 数据目录（收编校验用：空间内容根不得与数据目录互相嵌套，防元数据混进内容树）。
    pub(crate) fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }
}
