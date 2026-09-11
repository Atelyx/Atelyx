//! 插件平台命令：安装/卸载/启用/更新/读取入口与插件数据。
//!
//! 存储布局：
//! - app 级插件：`app_data_dir/plugins/<目录>/`（个人工具，本机）
//! - vault 级插件：`<仓库根>/.atelyx/plugins/<目录>/`（随仓库共享）
//! - 状态：`app_data_dir/plugin-state.json`（每 id：enabled 开关 + 行来源 kind/scope/落位目录名 +
//!   随应用分发行的清单 + 已播种 id 记录）
//!
//! 身份模型：插件身份 = 清单 `package.json` 的 `name`（反向域名，仍校验）；**目录名 = 原名**（本地
//! 源目录名 / 仓库名），不校验合法性、不要求等于 name。按 name 定位一律扫描目录读清单匹配；
//! 点开头目录（`.install-*`/`.bak-*` 等临时/隐藏目录）不参与扫描。
//!
//! 行的两种实现解析：磁盘包（扫目录读清单）与随应用分发的包（实现随宿主编译、无磁盘目录，清单由
//! 前端随 `plugin_list` 的 `defaults` 交给本层播种并保存）。二者同一张行表、同一启停/卸载路径；
//! 同名 id 的磁盘包覆盖随应用分发的实现（磁盘行优先列出）。
//!
//! 安装流（三类安装来源，统一「取源码」；随应用分发行不是安装来源，只由播种产生）：
//! - 市场：GitHub `owner/repo`，git clone 到临时目录；本机无 git 时回退下载 GitHub 自动生成的
//!   源码包（codeload，作者零操作，非 Release 资产）。
//! - 手动 git 地址：git clone（保留 `.git` 供更新）。
//! - 本地目录：junction（Windows）/ 符号链接（Unix）实时引用，无拷贝无更新。
//! 三者统一：校验 `package.json` → 以原名原子落位到 `plugins/<原名>/`（本地目录为链接）；失败不留脏。
//!
//! 安全：插件 name 视为不可信输入（仍校验）；插件目录内路径访问经 `safe_plugin_path` 限制在对应插件根
//! 目录内并拒绝符号链接段（防穿越越权）；插件代码在 WebView 主上下文内执行（安装即授权，能力经 ctx 面）。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};

use crate::vault::{atomic_write, VaultState};

/// 插件包清单文件名（插件根目录）。
const MANIFEST_FILE: &str = "package.json";
/// 状态文件名（app_data_dir 下）。
const STATE_FILE: &str = "plugin-state.json";

/// 下载与解压体积上限（zip 炸弹防御）。
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
/// 单条目解压体积上限。
const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
/// 解压总字节上限（单条目上限之外的总量预算，防 10k 条 × 8MB 的理论堆积）。
const MAX_ARCHIVE_TOTAL: u64 = 512 * 1024 * 1024;
/// zip 条目数上限。
const MAX_ENTRY_COUNT: usize = 10_000;
/// 允许的插件入口扩展名（宿主只转译/求值 JS/TS；与 docs/plugins/manifest.md 的入口契约一致）。
const ENTRY_EXTENSIONS: [&str; 3] = [".js", ".ts", ".tsx"];

/// 插件 id 合法性（与前端 `pluginIdValid` 一致：反向域名式至少两段，无路径分隔符）。
fn plugin_id_valid(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 || id.contains(['/', '\\']) {
        return false;
    }
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return false;
    }
    segments.iter().all(|seg| {
        !seg.is_empty()
            && !seg.starts_with('-')
            && !seg.ends_with('-')
            && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// 插件安装来源类型（与前端 `PluginSourceKind` 一致）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum PluginSourceKind {
    /// 市场：GitHub 仓库（安装 = git clone，无 git 回退源码包）。
    #[default]
    Market,
    /// 手动 git 地址（git clone，保留 `.git` 供更新）。
    Git,
    /// 本地目录（junction/符号链接实时引用，无拷贝无更新）。
    Local,
    /// 随应用分发（实现随宿主编译，无磁盘目录；清单随 `plugin_list` 的 `defaults` 播种保存）。
    /// 仅作来源信息：更新无独立渠道、行以「有无落位目录」判定。
    Builtin,
}

/// 插件运行信息（返回前端）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub scope: String,
    pub install_dir: String,
    pub enabled: bool,
    pub manifest: Value,
    /// 安装来源类型（管理 UI 展示徽标/更新可用性）。
    pub source_kind: PluginSourceKind,
}

/// 行来源记录（更新定位与卸载定位依据：市场按 repo 重新拉取，git 即安装目录内 `git pull`，
/// 本地为实时引用无更新）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct PluginSource {
    /// 市场来源的 GitHub `owner/repo`（更新定位）。
    #[serde(default)]
    repo: String,
    /// 落位目录名（原名）；清单损坏/链接悬空时卸载仍可据此按路径定位删除。
    /// 为空 = 实现随应用编译（无磁盘目录）。
    #[serde(default)]
    dir_name: String,
    /// 来源类型（缺省市场）。
    #[serde(default)]
    kind: PluginSourceKind,
    /// 随应用分发行的清单（磁盘行的清单以磁盘为准，此处为 None）。
    #[serde(default)]
    manifest: Option<Value>,
    scope: String,
}

/// 插件平台状态（app_data_dir/plugin-state.json）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct PluginState {
    #[serde(default)]
    enabled: HashMap<String, bool>,
    #[serde(default)]
    sources: HashMap<String, PluginSource>,
    /// 已播种过的默认组合行 id（增量播种依据：新增条目随 App 版本补播，已播种条目保持现状
    /// ——卸载保持卸载、停用保持停用；恢复由用户显式触发）。
    #[serde(default)]
    seeded_ids: Vec<String>,
}

/// 跨作用域 id 全局唯一：同 id 已存在于另一作用域时拒绝安装（store/enabled/运行时均按裸 id
/// 寻址，双作用域并存会互相踩踏；随仓库同步的重复由扫描兜底展示，安装路径从源头禁止）。
/// 「实现随应用编译的行」同占 app 作用域：来源记录按裸 id 单条保存，被 vault 级包覆盖后该行
/// 在别的仓库无处重建（既不列出也恢复不回），故同 id 的 vault 级安装一并拒绝——想替换随应用
/// 分发的实现请选「本机」作用域（磁盘包覆盖该实现）。
fn ensure_global_id_unique(app: &AppHandle, state: &VaultState, scope: &str, id: &str) -> Result<(), String> {
    for other in ["app", "vault"] {
        if other == scope {
            continue;
        }
        if let Ok(b) = plugin_base_dir(app, state, other) {
            if find_plugin_dir(&b, id).is_ok() {
                return Err("已安装相同 id 的插件（另一作用域），请先卸载".into());
            }
        }
        if other == "app" && has_compiled_row(app, state, id)? {
            return Err("该 id 由随应用分发的插件占用（实现随应用编译）：请选「本机」作用域安装以替代该行".into());
        }
    }
    Ok(())
}

/// 是否存在「以随应用编译实现呈现」的行：无落位目录，或落位目录当前不可见但宿主清单已存
/// （`plugin_list` 对这两类行同样以 `install_dir` 空 = 编译实现列出，判定口径必须与展示一致）。
fn has_compiled_row(app: &AppHandle, state: &VaultState, id: &str) -> Result<bool, String> {
    let Some(src) = read_plugin_state(app)?.sources.get(id).cloned() else {
        return Ok(false);
    };
    if src.dir_name.is_empty() {
        return Ok(true);
    }
    let dir_visible = plugin_base_dir(app, state, &src.scope)
        .ok()
        .is_some_and(|base| find_plugin_dir(&base, id).is_ok());
    Ok(!dir_visible && src.manifest.is_some())
}

// ===== 默认组合播种（随应用分发的行） =====
// 默认组合清单（行定义 + 各行的清单）由前端随 `plugin_list(defaults)` 交给本层，本层不做类别区分：
// 只按 id 增量补建「无磁盘目录的实现随应用编译」的行，并保存清单（供列行、主题守恒判定消费）。

/// 逐条播种默认组合行（纯状态变换；返回是否改了状态——未变则不落盘，避免每次列表都写文件）：
/// - 磁盘已有同 id 包 → 记已播种、不建行（同名磁盘包覆盖随应用分发的实现）；
/// - 已有行（含旧状态里无清单的行）→ 刷新清单（版本/声明随 App 更新），保留启停状态。同名磁盘包
///   的目录当前不可见时（外部删除/在别的仓库）清单也在此刷新：该行随即以「实现随应用编译」继续
///   可用（默认功能自愈），卸载按「有清单无目录 = 只清记录」收尾——不会留下卸不掉的行；
/// - 已播种且无对应行（被卸载）→ 保持卸载；`restore` 时补建回默认（用户显式触发的恢复）；
/// - 其余 → 建行（来源 Builtin、无落位目录、默认启用）；
/// - 本次清单里已不存在的随应用分发行 → 连同播种标记清理（退役行不留残渣）。
/// 清单来自宿主自身，只校验 id（行的键与定位依据）；缺失/非法 id 的条目跳过。
fn seed_default_rows(pstate: &mut PluginState, defaults: &[Value], disk_ids: &HashSet<String>, restore: bool) -> bool {
    // 已播种 id（顺序 = 既有记录 + 本次清单新增）。
    let mut seeded: HashSet<String> = pstate.seeded_ids.iter().cloned().collect();
    let mut seeded_ids: Vec<String> = pstate.seeded_ids.clone();
    let mut changed = false;
    for entry in defaults {
        let Some(id) = entry.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !plugin_id_valid(id) {
            continue;
        }
        let id = id.to_string();
        if !seeded_ids.contains(&id) {
            seeded_ids.push(id.clone());
            changed = true;
        }
        if disk_ids.contains(&id) {
            continue;
        }
        match pstate.sources.get_mut(&id) {
            Some(row) => {
                if row.manifest.as_ref() != Some(entry) {
                    row.manifest = Some(entry.clone());
                    changed = true;
                }
            }
            None => {
                if seeded.contains(&id) && !restore {
                    continue;
                }
                pstate.sources.insert(
                    id.clone(),
                    PluginSource {
                        kind: PluginSourceKind::Builtin,
                        scope: "app".to_string(),
                        manifest: Some(entry.clone()),
                        ..Default::default()
                    },
                );
                pstate.enabled.insert(id.clone(), true);
                seeded.insert(id);
                changed = true;
            }
        }
    }
    changed |= pstate.seeded_ids != seeded_ids;
    pstate.seeded_ids = seeded_ids;
    // 默认组合里已不存在的「随应用分发行」记录连同其播种标记一起清理：留着它只会每次启动多出一行
    // 查不到实现的空白行（前端判为加载失败），并让状态文件单调增长。
    // 仅在本次清单非空时清理——空清单（调用方异常）不得当成「默认组合已清空」把行全删。
    if !defaults.is_empty() {
        let live: HashSet<String> = defaults
            .iter()
            .filter_map(|e| e.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        let retired: Vec<String> = pstate
            .sources
            .iter()
            .filter(|(id, s)| s.dir_name.is_empty() && !live.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in retired {
            pstate.sources.remove(&id);
            pstate.enabled.remove(&id);
            pstate.seeded_ids.retain(|x| x != &id);
            changed = true;
        }
    }
    changed
}

fn plugin_base_dir(app: &AppHandle, state: &VaultState, scope: &str) -> Result<PathBuf, String> {
    match scope {
        "app" => {
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            Ok(dir.join("plugins"))
        }
        "vault" => {
            let root = state.root()?;
            Ok(root.join(".atelyx/plugins"))
        }
        other => Err(format!("未知插件作用域：{other}")),
    }
}

/// 磁盘包 id 集（app + vault 两个作用域；未开仓库时只算 app）。
fn disk_plugin_ids(app: &AppHandle, state: &VaultState) -> HashSet<String> {
    let mut ids = HashSet::new();
    let mut collect = |base: &Path| {
        let Ok(rd) = fs::read_dir(base) else {
            return;
        };
        for dir in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && !is_hidden_dir(p)) {
            if let Ok(manifest) = read_manifest(&dir) {
                if let Some(id) = manifest["name"].as_str() {
                    ids.insert(id.to_string());
                }
            }
        }
    };
    if let Ok(base) = plugin_base_dir(app, state, "app") {
        collect(&base);
    }
    if let Ok(root) = state.root() {
        collect(&root.join(".atelyx/plugins"));
    }
    ids
}

/// 在作用域插件目录下按清单 id 定位插件目录（目录名任意，身份以清单 id 为准）。
/// 跳过点开头目录（临时/隐藏）与清单损坏目录；同 id 多目录时按目录名排序命中「第一个」。
fn find_plugin_dir(base: &Path, id: &str) -> Result<PathBuf, String> {
    let rd = fs::read_dir(base).map_err(|_| "插件不存在".to_string())?;
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden_dir(p))
        .collect();
    dirs.sort(); // 稳定顺序：与 plugin_list 展示/去重保持一致
    for dir in dirs {
        let Ok(manifest) = read_manifest(&dir) else {
            continue;
        };
        if manifest["name"].as_str() == Some(id) {
            return Ok(dir);
        }
    }
    Err("插件不存在".to_string())
}

/// 目录是否隐藏/临时（点开头）：`.install-*`/`.bak-*` 等残留不参与扫描。
fn is_hidden_dir(dir: &Path) -> bool {
    dir.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with('.'))
}

/// 从仓库引用/URL 提取目录名（原名 = 仓库名）：
/// `owner/repo`、`https://…/repo.git`、`git@github.com:owner/repo.git` → `repo`。
fn repo_folder_name(repo_or_url: &str) -> String {
    let trimmed = repo_or_url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// 落位目录名：优先原名（仓库名/源目录名），非法（含分隔符/点开头/空/`.`/`..`）回退清单 id。
fn target_folder_name(folder_name: &str, id: &str) -> String {
    if folder_name.is_empty()
        || folder_name.contains(['/', '\\'])
        || folder_name.starts_with('.')
        || folder_name == "."
        || folder_name == ".."
    {
        id.to_string()
    } else {
        folder_name.to_string()
    }
}

/// 把插件目录内相对路径安全地拼接到插件根（防穿越越权 + 拒绝 `.git` 段）。
fn safe_join_plugin(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel = Path::new(relative);
    for c in rel.components() {
        match c {
            Component::Normal(seg) => {
                // `.git` 不可访问：git clone 会把安装 URL（可能内嵌凭据）原样写进 `.git/config`，
                // 而插件目录位于仓库内、会随 Git/云盘同步流出。
                // Win32 归一化会剥掉每段结尾的点与空格，故先 trim 再比（`.git.`/`.git ` 同样命中）
                if seg
                    .to_string_lossy()
                    .trim_end_matches(['.', ' '])
                    .eq_ignore_ascii_case(".git")
                {
                    return Err(format!("非法插件内路径（.git 不可访问）：{relative}"));
                }
            }
            _ => return Err(format!("非法插件内路径：{relative}")),
        }
    }
    Ok(base.join(rel))
}

/// 解析插件内路径并拒绝符号链接段：git clone / 本地来源的插件目录内可能带指向插件根之外的链接，
/// 若只做语法校验，读/写会跟随链接越权。从插件根到目标逐段 `symlink_metadata`，任一段是链接
/// （Windows junction 同为 reparse point，一并拒绝）即报错；不存在的段 = 待创建的写入路径，放行。
pub(crate) fn safe_plugin_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let target = safe_join_plugin(root, relative)?;
    let rel_parts = target
        .strip_prefix(root)
        .map_err(|_| format!("非法插件内路径：{relative}"))?;
    let mut cur = root.to_path_buf();
    for comp in rel_parts.components() {
        cur.push(comp);
        match fs::symlink_metadata(&cur) {
            // 显式 reparse 位判定（is_dir_link），不依赖 std 对符号链接/junction 分类的版本语义。
            Ok(meta) if is_dir_link(&meta) => {
                return Err("插件内路径含符号链接，已拒绝访问".to_string());
            }
            Ok(_) => {}
            // 段不存在 = 待创建的写入路径：其后段不会先于它存在，直接放行。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("检查插件内路径失败：{e}")),
        }
    }
    Ok(target)
}

// ===== 状态文件 =====

fn plugin_state_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(STATE_FILE))
}

/// 读插件平台状态。
/// - `Ok(state)`：文件不存在 = 首次运行（空状态）；解析失败 = 把损坏文件改名备份后按空状态继续
///   （播种随即按默认组合重建随应用分发的行；备份保留原始文件供排查）。
/// - `Err(reason)`：文件存在但读不进来（句柄被占用/权限/非 UTF-8）。这种情况**不能**按空状态往下写——
///   会把启用开关与安装来源整表抹掉，所以写路径必须报错；只读展示路径用 `read_plugin_state_lenient`。
fn read_plugin_state(app: &AppHandle) -> Result<PluginState, String> {
    read_plugin_state_at(&plugin_state_path(app)?)
}

/// 按路径读（与 `read_plugin_state` 同一语义，供锁内读改写与单测复用）。
fn read_plugin_state_at(path: &Path) -> Result<PluginState, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PluginState::default()),
        Err(e) => return Err(format!("读取插件状态失败（可能被其它程序占用，请重试）：{e}")),
    };
    match serde_json::from_str(&raw) {
        Ok(state) => Ok(state),
        Err(e) => {
            let note = backup_corrupt_state(path)
                .map_or_else(|| "备份失败".to_string(), |p| format!("已备份为 {}", p.display()));
            eprintln!("[plugin] 插件状态文件损坏，{note}（按空状态继续）：{e}");
            Ok(PluginState::default())
        }
    }
}

/// 只读展示路径的宽松读：读不进来时按空状态展示（前端仍有默认组合行可用），并且调用方不得落盘
/// 这份「读失败得出的状态」——否则等于把用户状态清空。
fn read_plugin_state_lenient(app: &AppHandle) -> (PluginState, bool) {
    match read_plugin_state(app) {
        Ok(state) => (state, true),
        Err(e) => {
            eprintln!("[plugin] {e}（本次只作展示用，不落盘）");
            (PluginState::default(), false)
        }
    }
}

/// 把损坏的状态文件改名备份；返回备份路径（失败 = None）。
/// 备份名带随机后缀：同名目标在 Windows 上会被 rename 静默替换，固定名会让新的损坏冲掉旧备份。
fn backup_corrupt_state(path: &Path) -> Option<PathBuf> {
    let backup = path.with_file_name(format!("{STATE_FILE}.corrupt-{}", nanoid::nanoid!()));
    fs::rename(path, &backup).ok().map(|_| backup)
}

/// 按路径写状态（锁内读改写与单测复用）。
fn write_plugin_state_at(path: &Path, state: &PluginState) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    // 复用 vault::atomic_write：唯一临时名 + fsync + 失败清理（全项目同一 durability 语义）。
    atomic_write(path, &raw)
}

/// plugin-state.json 的进程级串行锁。同步命令内联在 IPC 回调、异步命令在 tokio 多线程运行时，
/// 两侧可真正并发；读-改-写不加锁会丢更新（丢 `enabled` 开关或 `sources` 记录，
/// 后者会让更新/按名卸载持续失效）。
static PLUGIN_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 在进程级锁内完成 plugin-state 的「读 → 改 → 写」，返回闭包产物。
///
/// `f` 的 `bool` = 本次是否落盘（状态未变时跳过写，避免列表路径的空写与无谓 fsync）。
/// 锁只覆盖这段临界区：`f` 内不得做耗时 I/O、不得跨 `await`（长操作请在调用前完成）；
/// 需要基于最新状态做判定时，用闭包拿到的 `&mut PluginState`，不要用锁外的陈旧读。
fn mutate_plugin_state_at<T>(
    path: &Path,
    f: impl FnOnce(&mut PluginState) -> Result<(T, bool), String>,
) -> Result<T, String> {
    // lock() 的唯一失败模式是 poison（曾持锁 panic），重试不会恢复
    let _guard = PLUGIN_STATE_LOCK
        .lock()
        .map_err(|_| "插件状态锁已损坏，请重启应用".to_string())?;
    let mut state = read_plugin_state_at(path)?;
    let (out, changed) = f(&mut state)?;
    if changed {
        write_plugin_state_at(path, &state)?;
    }
    Ok(out)
}

fn update_plugin_state<T>(
    app: &AppHandle,
    f: impl FnOnce(&mut PluginState) -> Result<(T, bool), String>,
) -> Result<T, String> {
    mutate_plugin_state_at(&plugin_state_path(app)?, f)
}

// ===== 清单校验 =====

/// 插件包清单校验（结构错误拒绝；字段枚举与前端 `validatePluginManifest` 对齐）。
/// 原始输入 = 插件根目录的 `package.json`（npm 标准字段 + 嵌套 `atelyx` 块）。
fn manifest_valid_or_error(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("清单必须是对象")?;
    let req = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("清单缺少字段：{k}"))
    };
    let name = req("name")?;
    if !plugin_id_valid(&name) {
        return Err("name 必须是合法的反向域名标识".to_string());
    }
    req("version")?;
    // main（入口，相对插件根目录）：.js/.ts/.tsx；纯 theme 插件可省略。其余扩展名一律拒绝
    // （宿主只求值 JS/TS；拒绝理由与 docs/plugins/manifest.md 的入口契约一致）。
    let main = obj.get("main");
    if let Some(m) = main {
        let s = m.as_str().filter(|s| !s.trim().is_empty()).ok_or("main 必须是非空字符串")?;
        if !ENTRY_EXTENSIONS.iter().any(|ext| s.to_ascii_lowercase().ends_with(ext)) {
            return Err("插件入口须为 .js/.ts/.tsx 文件".to_string());
        }
    }
    // atelyx 块：插件元数据（显示名/类型/作用域/披露/主题等）。
    let ax = v
        .get("atelyx")
        .and_then(|a| a.as_object())
        .ok_or("清单缺少 atelyx 块")?;
    let kind = ax
        .get("type")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or("atelyx.type 缺少")?;
    // 主分类未知即拒绝（与前端 validatePluginManifest 一致；未知附加分类安全跳过）。
    if !is_known_plugin_type(kind) {
        return Err(format!("未知插件类型：{kind}"));
    }
    // types（附加分类）非数组即拒绝：字符串等畸形形态会让前端组件 .map 崩溃。
    if let Some(t) = ax.get("types") {
        if !t.is_array() {
            return Err("atelyx.types 必须是数组".to_string());
        }
    }
    // declares（披露的服务）非数组即拒绝。
    if let Some(d) = ax.get("declares") {
        if !d.is_array() {
            return Err("atelyx.declares 必须是数组".to_string());
        }
    }
    // themes（主题条目）结构校验（与前端 validatePluginManifest 对齐）：畸形形态会让前端
    // deriveThemeProviders/normalizeThemeVarKeys 抛错击穿整窗，安装/读取时从源头拒绝。
    if let Some(t) = ax.get("themes") {
        validate_themes(t)?;
    }
    // main 仅在纯 theme 插件（无任何代码承载类型）时可省略——theme 是声明式皮肤，无入口。
    // 判定与前端一致：只按「已知类型」归一化（未知附加分类安全跳过，前向兼容）——
    // 混入 tool 等已知代码类型才必填 main；types 非数组按畸形拒绝（与前端校验对齐）。
    let theme_only = kind == "theme"
        && ax
            .get("types")
            .and_then(|t| t.as_array())
            .map_or(true, |arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .filter(|t| is_known_plugin_type(t))
                    .all(|t| t == "theme")
            });
    if !theme_only && main.is_none() {
        return Err("清单缺少字段：main".to_string());
    }
    Ok(())
}

/// themes（主题条目）结构校验（非数组/空/条目畸形/id 重复均拒绝）。
fn validate_themes(t: &Value) -> Result<(), String> {
    let arr = t.as_array().ok_or("themes 必须是数组")?;
    if arr.is_empty() {
        return Err("themes 至少需要一个主题条目".to_string());
    }
    let mut seen_ids = std::collections::HashSet::new();
    for item in arr {
        let o = item.as_object().ok_or("themes 项必须是对象")?;
        let has_nonempty = |k: &str| {
            o.get(k).and_then(|x| x.as_str()).is_some_and(|s| !s.trim().is_empty())
        };
        if !has_nonempty("id") {
            return Err("themes 项 id 必须是非空字符串".to_string());
        }
        let tid = o.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        if !seen_ids.insert(tid.clone()) {
            return Err(format!("themes 内 id 重复：{tid}"));
        }
        if !has_nonempty("name") {
            return Err("themes 项 name 必须是非空字符串".to_string());
        }
        let scheme = o.get("colorScheme").and_then(|x| x.as_str()).unwrap_or("");
        if scheme != "light" && scheme != "dark" {
            return Err("themes 项 colorScheme 仅支持 light/dark".to_string());
        }
        if !o.get("variables").is_some_and(|v| v.is_object()) {
            return Err("themes 项 variables 必须是对象".to_string());
        }
    }
    Ok(())
}

/// 已知插件类型（与前端 PluginType 联合一致）：主分类未知即拒绝；未知附加分类安全跳过。
fn is_known_plugin_type(t: &str) -> bool {
    matches!(
        t,
        "tool" | "setting" | "panel" | "app" | "node" | "theme" | "command" | "background" | "tableview"
    )
}

/// 读取插件根目录的清单。
pub(crate) fn read_manifest(plugin_root: &Path) -> Result<Value, String> {
    let path = safe_plugin_path(plugin_root, MANIFEST_FILE)?;
    let raw = fs::read_to_string(&path).map_err(|e| format!("读取清单失败：{e}"))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("清单不是合法 JSON：{e}"))?;
    manifest_valid_or_error(&v)?;
    Ok(v)
}

// ===== git 与源码包获取 =====

/// 是否为 GitHub `owner/repo` 引用（无 scheme、无 `@`、恰含一个非空 `/` 分隔段）。
fn is_github_repo_ref(s: &str) -> bool {
    if s.contains("://") || s.contains('@') || s.starts_with('/') || s.ends_with('/') || s.contains("..") {
        return false;
    }
    let mut parts = s.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    parts.next().is_none() && !owner.is_empty() && !repo.is_empty()
}

/// git 命令构造：关掉交互式凭据提示（否则私有仓库会让 git 在 stdin 上等死），
/// 并让子进程随 future 一起被丢弃（超时/取消时不留下孤儿 git 进程）。
fn git_command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0").kill_on_drop(true);
    cmd
}

/// git 单次操作的挂死宽限期（**不是体积/时长上限**：git 的下载体积不限）。
/// 正常插件源码仓库远小于该值，只有网络黑洞/半开连接才会触发；触发即报错而非无限等待。
const GIT_HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(1800);

/// 执行 git 并把超时收敛为可读错误（`kill_on_drop` 保证超时后进程被回收）。
async fn run_git(mut cmd: tokio::process::Command) -> Result<std::process::Output, String> {
    match tokio::time::timeout(GIT_HANG_GUARD, cmd.output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(if e.kind() == std::io::ErrorKind::NotFound {
            "未检测到 git 命令，请安装 Git 或改用本地文件夹安装".to_string()
        } else {
            format!("执行 git 失败：{e}")
        }),
        Err(_) => Err("git 操作超时（网络不可达或仓库过大），已中止".to_string()),
    }
}

/// git 是否可用（探测 `git --version`；同样给短超时，防探测本身挂住）。
async fn git_available() -> bool {
    match tokio::time::timeout(std::time::Duration::from_secs(10), git_command().arg("--version").output()).await {
        Ok(Ok(out)) => out.status.success(),
        _ => false,
    }
}

/// 传给 git 的路径参数：Windows UNC 一律转 POSIX 形式 `//server/share/...`——
/// Git for Windows（MSYS2）对 `\\?\UNC\` verbatim 网络路径创建目录报 Invalid argument
/// （NAS/中文路径场景），前斜杠 UNC 可绕开（MSYS 不再内部转 verbatim 形式）。
/// 覆盖普通 UNC（`\\server\share\...`）与 verbatim UNC（`\\?\UNC\server\share\...`，
/// 仓库根可能以 verbatim 形式存储）；本地盘路径与 verbatim 本地盘（`\\?\C:\`）原样透传。
fn git_path_arg(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return format!("//{}", rest.replace('\\', "/"));
        }
        if s.starts_with(r"\\") && !s.starts_with(r"\\?\") {
            return s.replace('\\', "/");
        }
    }
    s.into_owned()
}

/// 手动 git 地址白名单（显式 scheme 的绝对地址；`git@host:owner/repo` 形式以 `git@` 开头）。
/// 拒绝以 `-` 开头的输入：git 会把位置参数当选项解析（如 `--upload-pack=<cmd>`、`ext::<cmd>`
/// 均可触发本机命令执行）。市场来源（owner/repo）由 `is_github_repo_ref` 单独校验，不走此函数。
/// 另拒绝内嵌凭据：git clone 会把 URL 原样写进 `.git/config`，
/// 该文件位于仓库内、会随 Git/云盘同步。
fn validate_git_url(url: &str) -> Result<(), String> {
    if url.starts_with('-') {
        return Err("git 地址不能以 - 开头".into());
    }
    const SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "git@"];
    if !SCHEMES.iter().any(|s| url.starts_with(s)) {
        return Err("git 地址须以 https:// / http:// / ssh:// / git:// / git@ 开头".into());
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        // authority = `://` 到首个路径/查询/锚点之前
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            // `ssh://git@host` 是常规写法（无密码、另走公钥认证），放行；
            // 其余 userinfo 都算凭据：`user:pass@` 含密码，而 http(s)/git 的 `user@`
            // 常被当作「用户名即令牌」（如 GitHub PAT 的 `https://<token>@github.com/...`）
            let ssh_key_only = scheme.eq_ignore_ascii_case("ssh") && !userinfo.contains(':');
            if !ssh_key_only {
                return Err(
                    "git 地址不能内嵌凭据（会明文写入插件目录的 .git/config）；请改用 SSH 地址或凭据管理器"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

/// 克隆 git 仓库到插件基础目录下的临时目录（保留 `.git` 供更新）；失败清理并返回错误。
/// 参数经 `--` 终止选项解析（URL 只可能来自白名单校验或 owner/repo 拼装，双保险防选项注入）；
/// 体积不设上限，仅有挂死宽限（见 GIT_HANG_GUARD）。
async fn git_clone_to(base: &Path, url: &str) -> Result<PathBuf, String> {
    let target = base.join(format!(".install-{}", nanoid::nanoid!()));
    let out = run_git({
        let mut cmd = git_command();
        cmd.args(["clone", "--", url]).arg(git_path_arg(&target));
        cmd
    })
    .await?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&target);
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() { "git clone 失败".to_string() } else { msg });
    }
    Ok(target)
}

/// HTTP 客户端（仅用于本层自己的请求：GitHub 分支解析与源码包下载）：
/// 逐块空闲超时（60s 无数据即失败）——不用总超时，慢链路上的大源码包不会被中途 abort；
/// 体积上限在 download_zip 的累计校验里（64MB），不会无限挂。
fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("创建网络客户端失败：{e}"))
}

/// 解析仓库默认分支（无 git 回退源码包时需要分支名定位归档）。
async fn resolve_default_branch(repo: &str) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{repo}");
    let resp = http_client()?
        .get(&url)
        .header("User-Agent", "atelyx")
        .send()
        .await
        .map_err(|e| format!("解析仓库信息失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("解析仓库信息失败（{repo}）：HTTP {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("解析仓库信息响应失败：{e}"))?;
    body.get("default_branch")
        .and_then(|b| b.as_str())
        .map(|s| s.to_string())
        .ok_or("仓库无默认分支".into())
}

/// GitHub 自动生成的源码包地址（作者零操作，非 Release 资产）。
fn codeload_url(repo: &str, branch: &str) -> String {
    format!("https://codeload.github.com/{repo}/zip/refs/heads/{branch}")
}

/// 无 git 时回退：下载 GitHub 源码包并解压到插件基础目录下的临时目录；
/// 返回 (插件根, 解压临时目录)——调用方负责无条件清理临时目录。
async fn codeload_extract_to(
    app: &AppHandle,
    state: &VaultState,
    scope: &str,
    repo: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let base = plugin_base_dir(app, state, scope)?;
    let branch = resolve_default_branch(repo).await?;
    let client = http_client()?;
    let zip_temp = base.join(format!(".download-{}.zip", nanoid::nanoid!()));
    let download = download_zip(&client, &codeload_url(repo, &branch), &zip_temp).await;
    if let Err(e) = download {
        let _ = fs::remove_file(&zip_temp);
        return Err(e);
    }
    let extract_temp = base.join(format!(".install-{}", nanoid::nanoid!()));
    if let Err(e) = fs::create_dir_all(&extract_temp) {
        let _ = fs::remove_file(&zip_temp);
        let _ = fs::remove_dir_all(&extract_temp);
        return Err(format!("创建临时目录失败：{e}"));
    }
    let extract = extract_zip_safe(&zip_temp, &extract_temp);
    let _ = fs::remove_file(&zip_temp);
    if let Err(e) = extract {
        let _ = fs::remove_dir_all(&extract_temp);
        return Err(e);
    }
    let root = match locate_plugin_root(&extract_temp) {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_dir_all(&extract_temp);
            return Err(e);
        }
    };
    Ok((root, extract_temp))
}

/// 下载 zip 到临时文件（边收边写 + 累计体积上限：上限在读取路径上生效，防超大响应先整包进内存）。
async fn download_zip(client: &reqwest::Client, url: &str, temp: &Path) -> Result<(), String> {
    let mut resp = client
        .get(url)
        .header("User-Agent", "atelyx")
        .send()
        .await
        .map_err(|e| format!("下载插件失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载插件失败（HTTP {}）", resp.status()));
    }
    let mut file = fs::File::create(temp).map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载插件失败：{e}"))? {
        total += chunk.len() as u64;
        if total > MAX_ARCHIVE_BYTES {
            return Err("插件包超过体积上限".into());
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ===== zip 解压 =====

/// 净化 zip 条目路径：`/` 与 `\` 都按分隔符处理（防 Unix 下反斜杠文件名绕过穿越检查），
/// 拒绝 `..`、绝对路径、盘符前缀。
fn sanitize_zip_entry(name: &str) -> Result<String, String> {
    if name.starts_with('/') || name.starts_with('\\') {
        return Err("zip 条目含绝对路径".into());
    }
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err("zip 条目含 .. 路径".into()),
            other => {
                if other.contains(':') {
                    return Err("zip 条目含非法路径段".into());
                }
                out.push(other);
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err("zip 条目路径为空".into());
    }
    Ok(out.to_string_lossy().into_owned())
}

/// 解压 zip 到目标目录（防路径穿越/zip 炸弹）。
fn extract_zip_safe(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("打开插件包失败：{e}"))?;
    if archive.len() > MAX_ENTRY_COUNT {
        return Err("插件包条目过多".into());
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("读取插件包失败：{e}"))?;
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(format!("插件包条目过大：{}", entry.name()));
        }
        let clean = sanitize_zip_entry(entry.name())?;
        let target = dest.join(&clean);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut writer = fs::File::create(&target).map_err(|e| e.to_string())?;
        // 实际写出字节硬上限：zip 头声明的 size 可谎报，不能信任。
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_ENTRY_BYTES + 1), &mut writer)
            .map_err(|e| e.to_string())?;
        if copied > MAX_ENTRY_BYTES || total + copied > MAX_ARCHIVE_TOTAL {
            return Err(format!("插件包条目过大：{}", entry.name()));
        }
        total += copied;
    }
    Ok(())
}

/// 解压后定位插件根：清单在临时目录根，或位于唯一顶层子目录（归档常见形态）。
fn locate_plugin_root(extract_dir: &Path) -> Result<PathBuf, String> {
    if extract_dir.join(MANIFEST_FILE).exists() {
        return Ok(extract_dir.to_path_buf());
    }
    let entries: Vec<PathBuf> = fs::read_dir(extract_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let dirs: Vec<PathBuf> = entries.iter().filter(|p| p.is_dir()).cloned().collect();
    if entries.len() == 1 && dirs.len() == 1 && dirs[0].join(MANIFEST_FILE).exists() {
        return Ok(dirs[0].clone());
    }
    Err("插件包缺少 package.json".into())
}

// ===== 安装 / 卸载 / 更新 =====

/// 由目录 + 清单构建插件运行信息（list/install/update 共用）。
fn plugin_info_from(
    dir: &Path,
    manifest: &Value,
    scope: &str,
    source_kind: PluginSourceKind,
    enabled: bool,
) -> PluginInfo {
    let id = manifest["name"].as_str().unwrap_or("").to_string();
    let display = manifest["atelyx"]["name"].as_str().unwrap_or(&id).to_string();
    PluginInfo {
        id,
        name: display,
        version: manifest["version"].as_str().unwrap_or("").to_string(),
        kind: manifest["atelyx"]["type"].as_str().unwrap_or("").to_string(),
        scope: scope.to_string(),
        install_dir: dir.to_string_lossy().into_owned(),
        enabled,
        manifest: manifest.clone(),
        source_kind,
    }
}

/// 由清单构建随应用分发行的运行信息（无磁盘目录：install_dir 空 = 实现随应用编译）。
fn plugin_info_from_manifest(id: &str, manifest: &Value, scope: &str, source_kind: PluginSourceKind, enabled: bool) -> PluginInfo {
    let display = manifest["atelyx"]["name"].as_str().unwrap_or(id).to_string();
    PluginInfo {
        id: id.to_string(),
        name: display,
        version: manifest["version"].as_str().unwrap_or("").to_string(),
        kind: manifest["atelyx"]["type"].as_str().unwrap_or("").to_string(),
        scope: scope.to_string(),
        install_dir: String::new(),
        enabled,
        manifest: manifest.clone(),
        source_kind,
    }
}

/// 从已就绪的插件源码根目录执行校验 + 原子落位（git clone / 源码包解压共用）。
/// 目录名 = `folder_name`（原名）；同名目录或同清单 id 目录已存在时报错，先卸载再装。
/// 调用方负责清理 `plugin_root` 所在临时目录残留。
fn install_plugin_dir(
    app: &AppHandle,
    state: &VaultState,
    scope: &str,
    source: PluginSource,
    folder_name: &str,
    plugin_root: &Path,
) -> Result<PluginInfo, String> {
    let base = plugin_base_dir(app, state, scope)?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let manifest = read_manifest(plugin_root)?;
    let id = manifest["name"].as_str().unwrap_or("").to_string();
    if !plugin_id_valid(&id) {
        return Err("插件清单 name 非法".into());
    }
    let source_kind = source.kind;

    let folder = target_folder_name(folder_name, &id);
    let target = base.join(&folder);
    if target.exists() {
        return Err("同名文件夹已存在，请先卸载".into());
    }
    // 同清单 id 冲突：作用域下已有同 id 插件目录（目录名可不同）。
    if find_plugin_dir(&base, &id).is_ok() {
        return Err("已安装相同 id 的插件，请先卸载".into());
    }
    ensure_global_id_unique(app, state, scope, &id)?;
    // 原子落位：改名为目标目录。
    let move_result = fs::rename(plugin_root, &target);
    move_result.map_err(|e| format!("安装失败：{e}"))?;

    // 记录安装来源（更新依据 + 落位目录名兜底）；启用状态按 id 保持（新装默认停用，
    // 同名替换沿用原行状态）——两者在同一次锁内读改写取回，与并发的启停命令不互相覆盖。
    let mut source = source;
    source.dir_name = folder.clone();
    let enabled = update_plugin_state(app, |pstate| {
        pstate.sources.insert(id.clone(), source);
        Ok((pstate.enabled.get(&id).copied().unwrap_or(false), true))
    })
    .map_err(|e| {
        // 状态写失败回滚落位，防「有目录无来源记录」的幽灵插件（重装/更新都定位不到）。
        let _ = fs::rename(&target, plugin_root);
        let _ = fs::remove_dir_all(plugin_root);
        e
    })?;

    Ok(plugin_info_from(&target, &manifest, scope, source_kind, enabled))
}

// ===== 命令 =====

/// 列出全部插件行：先按 `defaults`（默认组合清单）增量播种随应用分发的行，再列出磁盘包行
/// （app 级恒有；vault 级仅当前仓库；未开仓库时跳过 vault 目录）+ 无同名磁盘包的随应用分发行。
#[tauri::command]
pub fn plugin_list(
    app: AppHandle,
    state: State<'_, VaultState>,
    defaults: Vec<Value>,
) -> Result<Vec<PluginInfo>, String> {
    // 只读展示路径：读不到状态也不阻断列表（播种只在读成功时落盘，见下）。
    let (mut pstate, state_readable) = read_plugin_state_lenient(&app);
    let disk_ids = disk_plugin_ids(&app, &state);
    // 展示行就地播种（本次调用即可见）。
    let seeded = seed_default_rows(&mut pstate, &defaults, &disk_ids, false);
    // 落盘走锁内读改写：以最新状态为基础补一遍（幂等），未变不写、写失败不吞
    // （否则用户改动默默不持久化）——转 stderr 日志，列表本身照常返回。
    if state_readable && seeded {
        if let Err(e) = update_plugin_state(&app, |fresh| {
            let changed = seed_default_rows(fresh, &defaults, &disk_ids, false);
            Ok(((), changed))
        }) {
            eprintln!("[plugin] 默认组合播种状态写盘失败：{e}");
        }
    }

    let mut out: Vec<PluginInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut scan = |scope: &str, base: &Path| {
        let Ok(rd) = fs::read_dir(base) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !is_hidden_dir(p))
            .collect();
        dirs.sort(); // 稳定遍历顺序：同 id 多目录时「第一个」可复现（与 find_plugin_dir 一致）
        // 同作用域内同 id 多目录只收第一个，防身份错乱；app/vault 两作用域各自独立去重。
        for dir in dirs {
            let Ok(manifest) = read_manifest(&dir) else {
                continue; // 损坏插件跳过展示（管理 UI 仍可整体删除目录）
            };
            let id = manifest["name"].as_str().unwrap_or("").to_string();
            if !plugin_id_valid(&id) || !seen.insert(id.clone()) {
                continue;
            }
            let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
            let source_kind = pstate.sources.get(&id).map(|s| s.kind).unwrap_or_default();
            out.push(plugin_info_from(&dir, &manifest, scope, source_kind, enabled));
        }
    };

    // 磁盘行优先：同名磁盘包覆盖随应用分发的实现（随应用分发行不再列出）。
    if let Ok(base) = plugin_base_dir(&app, &state, "app") {
        scan("app", &base);
    }
    if let Ok(root) = state.root() {
        scan("vault", &root.join(".atelyx/plugins"));
    }
    // 随应用分发的行：有清单且无同名磁盘包的来源记录。
    for (id, src) in &pstate.sources {
        let Some(manifest) = &src.manifest else {
            continue;
        };
        if seen.contains(id) {
            continue;
        }
        seen.insert(id.clone());
        let enabled = pstate.enabled.get(id).copied().unwrap_or(false);
        out.push(plugin_info_from_manifest(id, manifest, &src.scope, src.kind, enabled));
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 恢复默认装配（用户显式触发）：把 `entries`（默认组合清单）里缺失的行补建回默认启用。
/// 已存在的行（启用/停用）保持现状，不覆盖用户改动。
#[tauri::command]
pub fn plugin_seed_default(app: AppHandle, state: State<'_, VaultState>, entries: Vec<Value>) -> Result<(), String> {
    let disk_ids = disk_plugin_ids(&app, &state);
    update_plugin_state(&app, |pstate| {
        let changed = seed_default_rows(pstate, &entries, &disk_ids, true);
        Ok(((), changed))
    })
}

/// 安装插件（来源 = GitHub `owner/repo` 或完整 git 地址；新装默认停用，由用户确认后启用；
/// 同名行被替换时沿用该行原有启停状态，原行已启用则安装后即生效）。
/// 市场来源优先 git clone，本机无 git 时回退 GitHub 自动生成的源码包；
/// 手动 git 地址必须有 git。id 以清单为准，repo 只做获取定位。
#[tauri::command]
pub async fn plugin_install(
    app: AppHandle,
    state: State<'_, VaultState>,
    repo: String,
    scope: String,
) -> Result<PluginInfo, String> {
    let base = plugin_base_dir(&app, &state, &scope)?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;

    if is_github_repo_ref(&repo) {
        // 市场来源：优先 git clone，无 git 回退源码包。
        if git_available().await {
            let clone_target = git_clone_to(&base, &format!("https://github.com/{repo}.git")).await?;
            let source = PluginSource {
                repo: repo.clone(),
                dir_name: String::new(), // 落位时由 install_plugin_dir 写入
                kind: PluginSourceKind::Market,
                scope: scope.clone(),
                ..Default::default()
            };
            let result = install_plugin_dir(&app, &state, &scope, source, &repo_folder_name(&repo), &clone_target);
            if result.is_err() {
                let _ = fs::remove_dir_all(&clone_target);
            }
            return result;
        }
        // 无 git：回退 GitHub 源码包（作者零操作，非 Release 资产）。
        return match codeload_extract_to(&app, &state, &scope, &repo).await {
            Ok((root, extract_temp)) => {
                let source = PluginSource {
                    repo: repo.clone(),
                    dir_name: String::new(), // 落位时由 install_plugin_dir 写入
                    kind: PluginSourceKind::Market,
                    scope: scope.clone(),
                    ..Default::default()
                };
                let result =
                    install_plugin_dir(&app, &state, &scope, source, &repo_folder_name(&repo), &root);
                // 成功/失败都无条件清理解压临时目录（成功时插件根已移走，残留仅外层包装目录）。
                let _ = fs::remove_dir_all(&extract_temp);
                result
            }
            Err(e) => Err(e),
        };
    }

    // 手动 git 地址：必须有 git，且地址须过白名单（拒选项注入/未知协议）。
    validate_git_url(&repo)?;
    if !git_available().await {
        return Err("未检测到 git 命令，请安装 Git 或改用本地文件夹安装".into());
    }
    let clone_target = git_clone_to(&base, &repo).await?;
    let source = PluginSource {
        dir_name: String::new(), // 落位时由 install_plugin_dir 写入
        kind: PluginSourceKind::Git,
        scope: scope.clone(),
        ..Default::default()
    };
    let result = install_plugin_dir(&app, &state, &scope, source, &repo_folder_name(&repo), &clone_target);
    if result.is_err() {
        let _ = fs::remove_dir_all(&clone_target);
    }
    result
}

/// 从本地目录安装插件（junction/符号链接实时引用，无拷贝；源目录改动即时生效）。
#[tauri::command]
pub fn plugin_install_local(
    app: AppHandle,
    state: State<'_, VaultState>,
    path: String,
    scope: String,
) -> Result<PluginInfo, String> {
    let src_dir = dunce::canonicalize(&path).map_err(|e| format!("路径无效：{e}"))?;
    if !src_dir.is_dir() {
        return Err("所选路径不是有效目录".into());
    }
    let manifest = read_manifest(&src_dir).map_err(|e| format!("所选目录不是有效插件：{e}"))?;
    let id = manifest["name"].as_str().unwrap_or("").to_string();
    if !plugin_id_valid(&id) {
        return Err("插件清单 name 非法".into());
    }

    let base = plugin_base_dir(&app, &state, &scope)?;
    // 源目录与插件目录都规范化后再判包含关系（大小写/长路径前缀差异会导致误判）。
    let base = dunce::canonicalize(&base).unwrap_or(base);
    if src_dir.starts_with(&base) || base.starts_with(&src_dir) {
        return Err("源目录不能位于插件目录内".into());
    }
    // 目录名 = 源目录原名（非法名回退清单 id）。
    let src_name = src_dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = target_folder_name(&src_name, &id);
    let target = base.join(&folder);
    if target.exists() {
        return Err("同名文件夹已存在，请先卸载".into());
    }
    // 同清单 id 冲突：作用域下已有同 id 插件目录（目录名可不同）。
    if find_plugin_dir(&base, &id).is_ok() {
        return Err("已安装相同 id 的插件，请先卸载".into());
    }
    ensure_global_id_unique(&app, &state, &scope, &id)?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    create_plugin_link(&src_dir, &target)?;

    // 记录行来源（本地来源为实时引用，无更新）；启用状态按 id 保持——同一次锁内读改写取回。
    let enabled = update_plugin_state(&app, |pstate| {
        pstate.sources.insert(
            id.clone(),
            PluginSource {
                dir_name: folder.clone(),
                kind: PluginSourceKind::Local,
                scope: scope.clone(),
                ..Default::default()
            },
        );
        Ok((pstate.enabled.get(&id).copied().unwrap_or(false), true))
    })
    .map_err(|e| {
        // 状态写失败回滚链接，防「有链接无来源记录」残留。
        let _ = fs::remove_dir(&target);
        e
    })?;

    Ok(plugin_info_from(&target, &manifest, &scope, PluginSourceKind::Local, enabled))
}

/// 创建目录链接：Windows 用 junction（免管理员），其余平台用符号链接。
#[cfg(windows)]
fn create_plugin_link(src: &Path, link: &Path) -> Result<(), String> {
    junction::create(src, link).map_err(|e| format!("创建 junction 失败：{e}"))
}

#[cfg(not(windows))]
fn create_plugin_link(src: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(src, link).map_err(|e| format!("创建符号链接失败：{e}"))
}

/// Windows 下目录链接（junction）判定：检查 reparse point 位（junction 与符号链接同属）。
#[cfg(windows)]
fn is_dir_link(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_dir_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// 仅删除目录链接（junction/符号链接），绝不触碰链接目标；真实目录（状态与目录不一致兜底）整删。
/// 显式先判链接再删，把「本地来源卸载不误删源目录」从依赖 std 行为变为代码保证。
fn remove_link_only(dir: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| format!("读取链接信息失败：{e}"))?;
    if is_dir_link(&meta) {
        fs::remove_dir(dir).map_err(|e| format!("卸载失败：{e}"))
    } else {
        fs::remove_dir_all(dir).map_err(|e| format!("卸载失败：{e}"))
    }
}

/// 卸载插件：本地来源只删链接（源目录不动，链接悬空也能删）；其余删除整个插件目录；
/// 无落位目录的行（实现随应用编译）只清状态记录与启用开关。
/// 被拒的情形：最后一个启用的主题插件（守恒）、未开仓库/未知作用域、落位目录不可达（网络路径离线）、磁盘删除失败。
#[tauri::command]
pub fn plugin_uninstall(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
    scope: String,
) -> Result<(), String> {
    // id 视为不可信输入：非法 id 直接拒绝（防来源记录被篡改时 target_folder_name 回退 join(id)
    // 把含分隔符的 id 拼进插件目录内任意子路径）。
    if !plugin_id_valid(&id) {
        return Err("插件不存在".to_string());
    }
    let pstate = read_plugin_state(&app)?;
    // 守恒守护：卸载「当前启用且为最后一个」的主题插件被拒（与停用同一规则）
    let target_enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
    guard_last_enabled_theme_plugin(&app, &state, &pstate, &id, target_enabled)?;
    let base = plugin_base_dir(&app, &state, &scope)?;
    let source = pstate.sources.get(&id).cloned().unwrap_or_default();
    // 定位待删目录：优先按清单 id 扫描；清单损坏/链接悬空（扫描按 is_dir 判定收不到）时按来源记录的
    // 落位目录名定位（名字经 target_folder_name 同款清理校验，确保仍在插件目录内）。
    let dir = match find_plugin_dir(&base, &id) {
        Ok(d) => Some(d),
        Err(_) if source.dir_name.is_empty() => {
            // 无落位目录 = 实现随应用编译：无目录可删，仅清状态记录（恢复经「恢复默认组合」）。
            None
        }
        Err(_) => {
            let by_name = base.join(target_folder_name(&source.dir_name, &id));
            if fs::symlink_metadata(&by_name).is_ok() {
                // 目录/链接仍在（本地源目录被移走后 junction 悬空也在此列）：按名删除。
                Some(by_name)
            } else if source.manifest.is_some() {
                // 目录已不在，但该行以随应用分发的实现呈现（清单随宿主保存）：无目录可删，清记录即可。
                None
            } else {
                // 有落位目录但不可达（如网络路径离线）：报错而不清记录，防误删安装来源。
                return Err("插件不存在".into());
            }
        }
    };
    if let Some(dir) = dir {
        if source.kind == PluginSourceKind::Local {
            remove_link_only(&dir)?;
        } else {
            fs::remove_dir_all(&dir).map_err(|e| format!("卸载失败：{e}"))?;
        }
    }
    // 状态清理由锁内读改写完成：目录删除在锁外（可能慢），写入必须以最新状态为基础，
    // 否则会覆盖并发命令刚写入的 enabled/sources
    update_plugin_state(&app, |fresh| {
        fresh.enabled.remove(&id);
        fresh.sources.remove(&id);
        Ok(((), true))
    })
}

/// 启用/停用插件（前端先确认权限再启用；vault 级插件卸载/禁用不清仓库内文件）。
#[tauri::command]
pub fn plugin_set_enabled(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    // id 校验与其它入口同口径：非法 id 不往 enabled 表写垃圾键。
    if !plugin_id_valid(&id) {
        return Err("插件不存在".to_string());
    }
    // 守卫读只作决策输入（锁外）；开关变更走锁内读改写，避免覆盖并发命令刚写入的 enabled/sources
    let pstate = read_plugin_state(&app)?;
    if !enabled {
        let target_enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
        guard_last_enabled_theme_plugin(&app, &state, &pstate, &id, target_enabled)?;
    }
    update_plugin_state(&app, |fresh| {
        if enabled {
            fresh.enabled.insert(id.clone(), true);
        } else {
            fresh.enabled.remove(&id);
        }
        Ok(((), true))
    })
}

/// 基础主题条目 id（浅/深基底；其他插件不得占用，防冒名——与前端 utils/pluginTheme.ts 的
/// BUILTIN_THEME_IDS 同规则，守恒计数须与前端派生一致）。
const BUILTIN_BASE_THEME_IDS: [&str; 2] = ["light", "dark"];

/// 基础主题提供者 id（声明浅/深基底那一个插件；主题内核的身份常量，与前端
/// utils/pluginTheme.ts 的 BUILTIN_THEME_PLUGIN_ID 同值）。
const BASE_THEME_PLUGIN_ID: &str = "builtin.theme";

/// 行清单解析：磁盘包读磁盘清单；随应用分发的行读来源记录里保存的清单。
fn row_manifest(app: &AppHandle, state: &VaultState, pstate: &PluginState, id: &str) -> Option<Value> {
    let source = pstate.sources.get(id)?;
    let base = plugin_base_dir(app, state, &source.scope).ok()?;
    if let Ok(dir) = find_plugin_dir(&base, id) {
        if let Ok(manifest) = read_manifest(&dir) {
            return Some(manifest);
        }
    }
    source.manifest.clone()
}

/// 是否主题插件（清单含「去除基础主题重名条目后仍非空」的 themes；随应用分发与磁盘包同一字段契约）。
/// 基础主题提供者自身声明基底条目，不受该过滤影响（与前端派生口径一致）。
/// 边界：清单读不到（目录缺失/清单损坏）降级为 false——损坏插件不参与守恒、可被停用/卸载清理，
/// 避免用户被损坏插件困住（前端此时回退基底主题，不崩溃）。
fn plugin_is_theme(app: &AppHandle, state: &VaultState, pstate: &PluginState, id: &str) -> bool {
    row_manifest(app, state, pstate, id).is_some_and(|m| manifest_is_theme(&m, id))
}

/// 清单主题判定（与前端 utils/pluginTheme.ts 的派生口径一致）：
/// themes 非空，且（基础主题提供者自身 或 存在基础条目之外的条目——重名条目按占用处理、不计入）。
fn manifest_is_theme(manifest: &Value, id: &str) -> bool {
    manifest
        .get("atelyx")
        .and_then(|a| a.get("themes"))
        .and_then(|v| v.as_array())
        .is_some_and(|a| {
            !a.is_empty()
                && (id == BASE_THEME_PLUGIN_ID
                    || a.iter().any(|t| {
                        t.get("id")
                            .and_then(|id| id.as_str())
                            .is_some_and(|tid| !BUILTIN_BASE_THEME_IDS.contains(&tid))
                    }))
        })
}

/// 当前启用中的主题插件数量。
fn enabled_theme_plugin_count(app: &AppHandle, state: &VaultState, pstate: &PluginState) -> usize {
    pstate
        .enabled
        .iter()
        .filter(|(id, enabled)| **enabled && plugin_is_theme(app, state, pstate, id))
        .count()
}

/// 守恒守护：主题插件必须至少保留一个启用——停用/卸载「当前启用且为最后一个」的主题插件被拒。
fn guard_last_enabled_theme_plugin(
    app: &AppHandle,
    state: &VaultState,
    pstate: &PluginState,
    id: &str,
    target_enabled: bool,
) -> Result<(), String> {
    if target_enabled && plugin_is_theme(app, state, pstate, id)
        && enabled_theme_plugin_count(app, state, pstate) <= 1
    {
        return Err("至少保留一个主题插件（可先启用/安装其他主题插件）".to_string());
    }
    Ok(())
}

/// 更新插件：按来源分派——git 来源 git pull（失败目录不变）；市场且无 .git 时重新下载源码包替换；
/// 本地目录实时引用无需更新（返回当前信息）。
#[tauri::command]
pub async fn plugin_update(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
) -> Result<PluginInfo, String> {
    let pstate = read_plugin_state(&app)?;
    let source = pstate
        .sources
        .get(&id)
        .cloned()
        .ok_or("插件无安装来源，无法更新（请先卸载重装）")?;
    let scope = source.scope.clone();
    // 无落位目录 = 实现随应用编译：版本随 App 走，无独立更新；返回当前信息。
    if source.dir_name.is_empty() {
        let Some(manifest) = source.manifest else {
            return Err("插件不存在".into());
        };
        let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
        return Ok(plugin_info_from_manifest(&id, &manifest, &scope, source.kind, enabled));
    }
    let base = plugin_base_dir(&app, &state, &scope)?;
    let dir = find_plugin_dir(&base, &id)?;

    // 本地目录实时引用：目录即源码，无更新概念；重读清单返回当前信息。
    if source.kind == PluginSourceKind::Local {
        let manifest = read_manifest(&dir)?;
        let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
        return Ok(plugin_info_from(&dir, &manifest, &scope, PluginSourceKind::Local, enabled));
    }
    // 源码包装的市场插件（无 git 环境安装，无 .git）：重新下载源码包替换。
    if source.kind == PluginSourceKind::Market && !dir.join(".git").exists() {
        return codeload_update(&app, &state, &scope, &source, &dir, &base).await;
    }

    // 其余（Git 来源与保留 .git 的市场来源）：git pull；失败目录不变，不引入备份回滚。
    // `--ff-only`：上游改写历史/本地分叉时直接失败并保持工作树干净（半合并会让清单读不出、行从列表消失）。
    let out = run_git({
        let mut cmd = git_command();
        cmd.args(["-C"]).arg(git_path_arg(&dir)).args(["pull", "--ff-only"]);
        cmd
    })
    .await?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() { "git pull 失败".to_string() } else { msg });
    }
    let manifest = read_manifest(&dir)?;
    let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
    Ok(plugin_info_from(&dir, &manifest, &scope, source.kind, enabled))
}

/// 源码包装的市场插件更新：重新下载源码包替换（备份 → 落位 → 失败回滚）。
async fn codeload_update(
    app: &AppHandle,
    state: &VaultState,
    scope: &str,
    source: &PluginSource,
    dir: &Path,
    base: &Path,
) -> Result<PluginInfo, String> {
    let branch = resolve_default_branch(&source.repo).await?;
    let client = http_client()?;
    let zip_temp = base.join(format!(".update-{}.zip", nanoid::nanoid!()));
    if let Err(e) = download_zip(&client, &codeload_url(&source.repo, &branch), &zip_temp).await {
        let _ = fs::remove_file(&zip_temp);
        return Err(e);
    }
    let extract_temp = base.join(format!(".update-{}", nanoid::nanoid!()));
    if let Err(e) = fs::create_dir_all(&extract_temp) {
        let _ = fs::remove_file(&zip_temp);
        return Err(format!("创建临时目录失败：{e}"));
    }
    let extract = extract_zip_safe(&zip_temp, &extract_temp);
    let _ = fs::remove_file(&zip_temp);
    if let Err(e) = extract {
        let _ = fs::remove_dir_all(&extract_temp);
        return Err(e);
    }
    let plugin_root = match locate_plugin_root(&extract_temp) {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_dir_all(&extract_temp);
            return Err(e);
        }
    };
    let old_manifest = read_manifest(dir)?;
    let old_id = old_manifest["name"].as_str().unwrap_or("").to_string();
    // 校验新清单 name 与旧清单 name 一致：改 name 的版本无法原地更新，防新旧记录并存。
    let new_manifest = read_manifest(&plugin_root)?;
    if new_manifest["name"].as_str() != Some(old_id.as_str()) {
        let _ = fs::remove_dir_all(&extract_temp);
        return Err("插件 id 已变更，无法原地更新（请先卸载重装）".into());
    }
    // 保持原目录名落位（原名 = 当前目录名）。
    let folder_name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(old_id.clone());
    let target = base.join(&folder_name);

    // 备份旧版 → 落位新版 → 任一失败回滚旧版。
    let backup = base.join(format!(".bak-{}-{}", old_id, nanoid::nanoid!()));
    if let Err(e) = fs::rename(dir, &backup) {
        let _ = fs::remove_dir_all(&extract_temp);
        return Err(format!("备份旧版失败：{e}"));
    }
    let install = install_plugin_dir(app, state, scope, source.clone(), &folder_name, &plugin_root);
    let _ = fs::remove_dir_all(&extract_temp);
    match install {
        Ok(info) => {
            let _ = fs::remove_dir_all(&backup);
            Ok(info)
        }
        Err(e) => {
            // 回滚：清掉可能的部分安装，恢复备份。
            let _ = fs::remove_dir_all(&target);
            let _ = fs::rename(&backup, dir);
            Err(format!("更新失败已回滚：{e}"))
        }
    }
}

/// 定位插件目录：优先按状态里的安装来源（scope），缺失时回退扫描两作用域按 id 定位
/// （vault 级插件随仓库同步到新机器时 sources 记录在本机不存在，仍应可读可运行）。
pub(crate) fn resolve_plugin_dir(app: &AppHandle, state: &VaultState, id: &str) -> Result<(PathBuf, String), String> {
    if plugin_id_valid(id) {
        // 状态读取失败不阻断定位：下面还有按两作用域扫描的兜底（随仓库同步过来的插件本机无来源记录）。
        if let Some(src) = read_plugin_state(app).ok().and_then(|s| s.sources.get(id).cloned()) {
            if let Ok(base) = plugin_base_dir(app, state, &src.scope) {
                if let Ok(dir) = find_plugin_dir(&base, id) {
                    return Ok((dir, src.scope.clone()));
                }
            }
        }
        for scope in ["app", "vault"] {
            if let Ok(base) = plugin_base_dir(app, state, scope) {
                if let Ok(dir) = find_plugin_dir(&base, id) {
                    return Ok((dir, scope.to_string()));
                }
            }
        }
    }
    Err("插件不存在".to_string())
}

/// 读取插件入口 JS（供宿主求值加载；限制在插件根目录内）。path 缺省 = 清单 main。
#[tauri::command]
pub fn plugin_read_entry(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
    path: Option<String>,
) -> Result<String, String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let manifest = read_manifest(&dir)?;
    let main = manifest["main"].as_str().ok_or("清单缺少 main")?;
    let entry = path.as_deref().unwrap_or(main);
    let entry_path = safe_plugin_path(&dir, entry)?;
    let size = fs::metadata(&entry_path).map_err(|e| format!("读取插件入口失败：{e}"))?.len();
    if size > MAX_ENTRY_BYTES {
        return Err(format!("插件入口过大（上限 {} 字节）", MAX_ENTRY_BYTES));
    }
    let data = fs::read(&entry_path).map_err(|e| format!("读取插件入口失败：{e}"))?;
    String::from_utf8(data).map_err(|_| "插件入口不是合法 UTF-8 文本".to_string())
}

/// 读取插件自持数据（单 JSON 对象，原子写落盘）。
#[tauri::command]
pub fn plugin_read_state(app: AppHandle, state: State<'_, VaultState>, id: String) -> Result<Value, String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let path = safe_plugin_path(&dir, "data/state.json")?;
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
    serde_json::from_str(&raw).map_err(|e| format!("插件数据损坏：{e}"))
}

/// 写入插件自持数据（原子写；vault 级插件的随仓库共享）。
#[tauri::command]
pub fn plugin_write_state(app: AppHandle, state: State<'_, VaultState>, id: String, data: Value) -> Result<(), String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let data_dir = safe_plugin_path(&dir, "data")?;
    fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let path = data_dir.join("state.json");
    let raw = serde_json::to_string(&data).map_err(|e| e.to_string())?;
    atomic_write(&path, &raw)
}

/// 插件键值存储的读改写串行化：整表读改写必须互斥，否则并发写会丢键
///（同一 realm 内 `Promise.all`、多窗口各自独立 realm 同时写同一插件）。
static PLUGIN_KV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 读键值表（文件不存在 = 空表；损坏 = 改名备份后按空表继续，同 plugin-state 的降级策略）。
fn read_kv_file(dir: &Path) -> Result<serde_json::Map<String, Value>, String> {
    let path = safe_plugin_path(dir, "data/kv.json")?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::Map::new()),
        Err(e) => return Err(format!("读取插件键值数据失败：{e}")),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("插件键值数据必须是 JSON 对象".to_string()),
        Err(e) => {
            let note = backup_corrupt_state(&path)
                .map_or_else(|| "备份失败".to_string(), |p| format!("已备份为 {}", p.display()));
            eprintln!("[plugin] 插件键值数据损坏，{note}（按空表继续）：{e}");
            Ok(serde_json::Map::new())
        }
    }
}

/// 写键值表（原子写；调用方须持有 PLUGIN_KV_LOCK）。
fn write_kv_file(dir: &Path, map: &serde_json::Map<String, Value>) -> Result<(), String> {
    let data_dir = safe_plugin_path(dir, "data")?;
    fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let raw = serde_json::to_string(&Value::Object(map.clone())).map_err(|e| e.to_string())?;
    atomic_write(&data_dir.join("kv.json"), &raw)
}

/// 读取插件键值存储（单 JSON 对象，独立于 state.json；键值面见 ctx.storage）。
#[tauri::command]
pub fn plugin_kv_read(app: AppHandle, state: State<'_, VaultState>, id: String) -> Result<Value, String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let _guard = PLUGIN_KV_LOCK.lock().map_err(|_| "键值存储忙，请重试".to_string())?;
    Ok(Value::Object(read_kv_file(&dir)?))
}

/// 写一个键（Rust 侧完成读改写：调用方无需整表往返，并发写不会互相丢键）。
#[tauri::command]
pub fn plugin_kv_set(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
    key: String,
    value: Value,
) -> Result<(), String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let _guard = PLUGIN_KV_LOCK.lock().map_err(|_| "键值存储忙，请重试".to_string())?;
    let mut map = read_kv_file(&dir)?;
    map.insert(key, value);
    write_kv_file(&dir, &map)
}

/// 删一个键（不存在 = no-op）。
#[tauri::command]
pub fn plugin_kv_delete(app: AppHandle, state: State<'_, VaultState>, id: String, key: String) -> Result<(), String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let _guard = PLUGIN_KV_LOCK.lock().map_err(|_| "键值存储忙，请重试".to_string())?;
    let mut map = read_kv_file(&dir)?;
    map.remove(&key);
    write_kv_file(&dir, &map)
}

/// 整表覆盖写（ctx.storage.clear 用）。
#[tauri::command]
pub fn plugin_kv_write(app: AppHandle, state: State<'_, VaultState>, id: String, data: Value) -> Result<(), String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let map = data.as_object().ok_or_else(|| "键值表必须是 JSON 对象".to_string())?.clone();
    let _guard = PLUGIN_KV_LOCK.lock().map_err(|_| "键值存储忙，请重试".to_string())?;
    write_kv_file(&dir, &map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn id_validity() {
        assert!(plugin_id_valid("com.example.todo"));
        assert!(!plugin_id_valid("todo"));
        assert!(!plugin_id_valid("com/example"));
        assert!(!plugin_id_valid(".."));
        assert!(!plugin_id_valid("COM.Example"));
    }

    #[test]
    fn zip_path_sanitize() {
        // 平台无关断言：`/` 与 `\` 都是分隔符。
        let expected = Path::new("a").join("b").join("package.json");
        let got = sanitize_zip_entry("a/b/package.json").unwrap();
        assert_eq!(Path::new(&got), expected);
        assert_eq!(sanitize_zip_entry("./package.json").unwrap(), "package.json");
        assert!(sanitize_zip_entry("a\\b").is_ok());
        assert!(sanitize_zip_entry("../evil").is_err());
        assert!(sanitize_zip_entry("a\\..\\b").is_err());
        assert!(sanitize_zip_entry("/abs").is_err());
        assert!(sanitize_zip_entry("C:/x").is_err());
        assert!(sanitize_zip_entry("").is_err());
    }

    #[test]
    fn github_repo_ref_detection() {
        assert!(is_github_repo_ref("com/example"));
        assert!(!is_github_repo_ref("https://github.com/com/example"));
        assert!(!is_github_repo_ref("git@github.com:com/example.git"));
        assert!(!is_github_repo_ref("com/example/deep"));
        assert!(!is_github_repo_ref("/com/example"));
        assert!(!is_github_repo_ref("com/example/"));
    }

    #[test]
    fn git_url_whitelist_blocks_option_injection() {
        // 显式 scheme 的绝对地址放行（含 SSH 简写）。
        for ok in [
            "https://github.com/com/example.git",
            "http://192.168.1.10/git/example.git",
            "ssh://git@host/example.git",
            "git://host/example.git",
            "git@github.com:com/example.git",
        ] {
            assert!(validate_git_url(ok).is_ok(), "{ok} 应放行");
        }
        // 选项注入与未知协议拒绝（git 会把 `-` 开头的位置参数当选项）。
        for bad in ["--upload-pack=sh -c id", "-c", "ext::sh -c id", "file:///tmp/x", "example.git", ""] {
            assert!(validate_git_url(bad).is_err(), "{bad} 应拒绝");
        }
    }

    #[test]
    fn git_url_rejects_embedded_credentials() {
        // 内嵌凭据拒绝：git clone 会把 URL 原样写进 .git/config（位于仓库内，随 Git/云盘流出）
        for bad in [
            "https://user:token@github.com/com/example.git",
            "http://user:pass@192.168.1.10/git/example.git",
            "ssh://user:pass@host/example.git",
            "git://user:pass@host/example.git",
            // 无冒号但同为凭据：`user@` 形式下用户名即令牌（GitHub PAT 的官方 clone 写法）
            "https://ghp_xxx@github.com/com/example.git",
            "https://user@github.com/com/example.git",
            "https://user%3Apass@github.com/com/example.git",
            "git://user@host/example.git",
        ] {
            assert!(validate_git_url(bad).is_err(), "{bad} 应拒绝");
        }
        // 无凭据（仅端口/路径含冒号，或 ssh 的免密 user@）仍放行
        for ok in [
            "https://github.com/com/example.git",
            "https://host:8443/com/example.git",
            "ssh://git@host:22/com/example.git",
            "ssh://git@host/example.git",
            "git@github.com:com/example.git",
            "https://host/a:b/example.git",
        ] {
            assert!(validate_git_url(ok).is_ok(), "{ok} 应放行");
        }
    }

    #[test]
    fn safe_plugin_path_rejects_git_dir() {
        let root = Path::new("/tmp/plugin-root");
        assert!(safe_plugin_path(root, ".git/config").is_err());
        assert!(safe_plugin_path(root, ".GIT/config").is_err());
        assert!(safe_plugin_path(root, "sub/.git/config").is_err());
        assert!(safe_plugin_path(root, "sub/.git").is_err());
        // Win32 归一化剥掉段尾的点/空格，这些在 Windows 上等价于 `.git`
        assert!(safe_plugin_path(root, ".git./config").is_err());
        assert!(safe_plugin_path(root, ".git /config").is_err());
        assert!(safe_plugin_path(root, "sub/.git./config").is_err());
        // 仅同名子串不算命中
        assert!(safe_plugin_path(root, ".gitignore").is_ok());
        assert!(safe_plugin_path(root, "sub/.github/workflows/ci.yml").is_ok());
    }

    /// 「安装中同时切换另一插件启停」的丢更新回归：并发读改写不得丢任一方的字段
    /// （丢 `sources` 会让更新/按名卸载持续失效，丢 `enabled` 会让开关静默回退）。
    #[test]
    fn concurrent_state_updates_keep_all_fields() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("atelyx-plugin-state-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin-state.json");

        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = path.clone();
                scope.spawn(move || {
                    mutate_plugin_state_at(&path, |s| {
                        s.enabled.insert(format!("com.test.e{i}"), true);
                        Ok(((), true))
                    })
                    .unwrap();
                });
            }
            for i in 0..8 {
                let path = path.clone();
                scope.spawn(move || {
                    mutate_plugin_state_at(&path, |s| {
                        s.sources.insert(format!("com.test.s{i}"), PluginSource::default());
                        Ok(((), true))
                    })
                    .unwrap();
                });
            }
        });

        let final_state = read_plugin_state_at(&path).unwrap();
        for i in 0..8 {
            assert!(final_state.enabled.contains_key(&format!("com.test.e{i}")), "丢了 enabled[{i}]");
            assert!(final_state.sources.contains_key(&format!("com.test.s{i}")), "丢了 sources[{i}]");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_and_target_folder_names() {
        // 原名 = 仓库名（owner/repo、完整 URL、SSH 地址都收敛到 repo 段）。
        assert_eq!(repo_folder_name("com/example"), "example");
        assert_eq!(repo_folder_name("https://github.com/com/example.git"), "example");
        assert_eq!(repo_folder_name("git@github.com:com/example.git"), "example");
        // 非法/点开头/空名回退清单 id。
        assert_eq!(target_folder_name("example", "com.x"), "example");
        assert_eq!(target_folder_name("", "com.x"), "com.x");
        assert_eq!(target_folder_name(".hidden", "com.x"), "com.x");
        assert_eq!(target_folder_name("..", "com.x"), "com.x");
        assert_eq!(target_folder_name("a/b", "com.x"), "com.x");
    }

    #[test]
    fn safe_plugin_path_rejects_escape_and_passes_plain() {
        let root = Path::new("/tmp/plugin-root");
        // 语法越权（绝对路径/..）拒绝；不存在的段（待创建的写入路径）放行。
        assert!(safe_plugin_path(root, "../evil").is_err());
        assert!(safe_plugin_path(root, "/abs").is_err());
        assert!(safe_plugin_path(root, "a/b/package.json").is_ok());
        assert!(safe_plugin_path(root, "package.json").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn git_path_arg_converts_unc_and_passes_others() {
        // 普通 UNC → 前斜杠 POSIX UNC（绕开 MSYS 对 verbatim 网络路径的 mkdir 失败）。
        assert_eq!(
            git_path_arg(Path::new(r"\\Zc-nas\团队文件-工作\动画创作知识库\.atelyx\plugins\.install-x")),
            "//Zc-nas/团队文件-工作/动画创作知识库/.atelyx/plugins/.install-x"
        );
        // verbatim UNC（仓库根可能以此形式存储）同样转前斜杠 POSIX UNC。
        assert_eq!(
            git_path_arg(Path::new(r"\\?\UNC\Zc-nas\CogniVault\.atelyx\plugins\.install-x")),
            "//Zc-nas/CogniVault/.atelyx/plugins/.install-x"
        );
        // 本地盘与 verbatim 本地盘原样透传（非网络路径，git 无 verbatim 问题）。
        assert_eq!(git_path_arg(Path::new(r"C:\Users\me\plugins")), r"C:\Users\me\plugins");
        assert_eq!(git_path_arg(Path::new(r"\\?\C:\Users\me")), r"\\?\C:\Users\me");
    }

    #[test]
    fn manifest_validation() {
        // 合法插件包：package.json（name=反向域名）+ atelyx 块。
        let ok = json!({
            "name": "com.example.todo",
            "version": "1.0.0",
            "main": "plugin.ts",
            "description": "示例",
            "atelyx": { "name": "示例工具", "type": "tool", "tagline": "一句话" },
        });
        assert!(manifest_valid_or_error(&ok).is_ok());
        // 缺 atelyx 块的清单拒绝（顶层 self 描述字段不算数）。
        let no_ax_block = json!({ "schemaVersion": 2, "id": "com.x", "name": "com.x", "version": "1", "type": "tool", "main": "a.js" });
        assert!(manifest_valid_or_error(&no_ax_block).is_err());
        // name 必须反向域名。
        let bad_name = json!({ "name": "todo", "version": "1", "main": "a.js", "atelyx": { "type": "tool" } });
        assert!(manifest_valid_or_error(&bad_name).is_err());
        // 入口扩展名限 .js/.ts/.tsx（其余拒绝）。
        let bad_ext = json!({ "name": "com.x.ext", "version": "1", "main": "main.other", "atelyx": { "type": "tool" } });
        assert!(manifest_valid_or_error(&bad_ext).is_err());
        let mjs = json!({ "name": "com.x.mjs", "version": "1", "main": "index.mjs", "atelyx": { "type": "tool" } });
        assert!(manifest_valid_or_error(&mjs).is_err());
        // 未知类型拒绝。
        let bad_type = json!({ "name": "com.x", "version": "1", "main": "a.js", "atelyx": { "type": "rust" } });
        assert!(manifest_valid_or_error(&bad_type).is_err());
        // 缺 atelyx 块拒绝。
        let no_ax = json!({ "name": "com.x", "version": "1", "main": "a.js" });
        assert!(manifest_valid_or_error(&no_ax).is_err());
        // theme 声明式：纯 theme 可省略 main；含代码类型则必填。
        let theme = json!({ "name": "com.example.dark", "version": "1", "atelyx": { "type": "theme" } });
        assert!(manifest_valid_or_error(&theme).is_ok());
        let tool_no_main = json!({ "name": "com.x", "version": "1", "atelyx": { "type": "tool" } });
        assert!(manifest_valid_or_error(&tool_no_main).is_err());
        // themes 结构校验（atelyx 块内；与前端 normalizeThemes 对齐）：非数组/空/条目畸形/id 重复均拒绝。
        let themes_ok = json!({
            "name": "com.example.theme", "version": "1",
            "atelyx": {
                "type": "theme",
                "themes": [
                    { "id": "nord-light", "name": "Nord 浅", "colorScheme": "light", "variables": {} },
                    { "id": "nord-dark", "name": "Nord 深", "colorScheme": "dark", "variables": { "--bg": "#000" } },
                ],
            },
        });
        assert!(manifest_valid_or_error(&themes_ok).is_ok());
        for bad_themes in [
            json!({ "themes": "not-array" }),
            json!({ "themes": [] }),
            json!({ "themes": [{ "id": "a", "name": "A", "colorScheme": "blue", "variables": {} }] }),
            json!({ "themes": [{ "id": "a", "name": "A", "colorScheme": "light", "variables": null }] }),
            json!({ "themes": [{ "id": "a", "name": "A", "colorScheme": "light" }] }),
            json!({
                "themes": [
                    { "id": "a", "name": "A", "colorScheme": "light", "variables": {} },
                    { "id": "a", "name": "B", "colorScheme": "dark", "variables": {} },
                ],
            }),
        ] {
            let mut m = json!({ "name": "com.example.theme", "version": "1", "atelyx": { "type": "theme" } });
            m["atelyx"]["themes"] = bad_themes.clone();
            assert!(manifest_valid_or_error(&m).is_err(), "畸形 themes 应拒绝：{bad_themes}");
        }
    }

    /// 默认组合清单条目（测试用：模拟前端随 `plugin_list(defaults)` 交来的原始插件包清单
    /// ——`name` = 插件 id + `atelyx` 块，与磁盘插件包同一形状）。
    fn default_entries(ids: &[&str]) -> Vec<Value> {
        ids.iter()
            .map(|id| raw_manifest(id, "panel", None))
            .collect()
    }

    /// 原始插件包清单（name = id；显示名/类型/主题声明在 atelyx 块）。
    fn raw_manifest(id: &str, kind: &str, themes: Option<Value>) -> Value {
        let mut atelyx = json!({
            "name": format!("显示名-{id}"),
            "type": kind,
            "scope": "app",
            "tagline": "一句话",
            "author": "Atelyx",
            "license": "MIT",
        });
        if let Some(themes) = themes {
            atelyx["themes"] = themes;
        }
        json!({ "name": id, "version": "0.0.0", "main": "builtin", "atelyx": atelyx })
    }

    #[test]
    fn seeding_adds_rows_and_is_idempotent() {
        // 播种建行（来源 Builtin、默认启用、清单已存）；重复播种不增删、不覆盖启停。
        let entries = default_entries(&["builtin.search", "builtin.note", "builtin.theme"]);
        let mut s = PluginState::default();
        assert!(seed_default_rows(&mut s, &entries, &HashSet::new(), false));
        assert_eq!(s.sources.len(), 3);
        for id in ["builtin.search", "builtin.note", "builtin.theme"] {
            assert_eq!(s.sources[id].kind, PluginSourceKind::Builtin);
            assert_eq!(s.sources[id].manifest.as_ref().unwrap()["name"], id);
            assert_eq!(s.enabled.get(id), Some(&true));
        }
        assert_eq!(s.seeded_ids, vec!["builtin.search", "builtin.note", "builtin.theme"]);

        s.enabled.insert("builtin.note".into(), false);
        // 幂等：状态未变 → 不落盘（返回 false），启停保持。
        assert!(!seed_default_rows(&mut s, &entries, &HashSet::new(), false));
        assert_eq!(s.sources.len(), 3);
        assert_eq!(s.enabled.get("builtin.note"), Some(&false));
    }

    #[test]
    fn seeding_reads_frontend_payload_shape() {
        // 跨语言形状契约：前端清单（name = id + atelyx 块）必须能播种成行，
        // 且随应用分发行的展示名/类型经 atelyx 块解析（列表与主题守恒都读这里）。
        let theme = raw_manifest(
            "builtin.theme",
            "theme",
            Some(json!([{ "id": "light", "name": "浅色", "colorScheme": "light", "variables": {} }])),
        );
        let entries = vec![raw_manifest("builtin.note", "panel", None), theme];
        let mut s = PluginState::default();
        assert!(seed_default_rows(&mut s, &entries, &HashSet::new(), false));
        assert_eq!(s.sources.len(), 2, "前端形状的清单必须播种成行");

        let info = plugin_info_from_manifest(
            "builtin.note",
            s.sources["builtin.note"].manifest.as_ref().unwrap(),
            "app",
            PluginSourceKind::Builtin,
            true,
        );
        assert_eq!(info.name, "显示名-builtin.note");
        assert_eq!(info.kind, "panel");
        assert_eq!(info.install_dir, "");
        // 基础主题提供者自身声明基底条目 → 计入主题守恒（与前端派生口径一致）。
        assert!(manifest_is_theme(s.sources["builtin.theme"].manifest.as_ref().unwrap(), "builtin.theme"));
    }

    #[test]
    fn seeding_refreshes_manifest_of_existing_row() {
        // 已有行（含旧状态里无清单的行）→ 刷新清单并保留启停状态（行不再重复建）。
        let mut s = PluginState::default();
        s.sources.insert(
            "builtin.search".into(),
            PluginSource { kind: PluginSourceKind::Builtin, scope: "app".into(), ..Default::default() },
        );
        s.enabled.insert("builtin.search".into(), false);
        let entries = default_entries(&["builtin.search"]);
        seed_default_rows(&mut s, &entries, &HashSet::new(), false);
        assert_eq!(s.sources["builtin.search"].manifest.as_ref().unwrap()["name"], "builtin.search");
        assert_eq!(s.enabled.get("builtin.search"), Some(&false));
    }

    #[test]
    fn seeding_prunes_retired_default_rows() {
        // 默认组合里已不存在的随应用分发行（退役）连同播种标记一起清理：不留空白行、状态不单调增长。
        let mut s = PluginState::default();
        s.sources.insert(
            "builtin.retired".into(),
            PluginSource {
                kind: PluginSourceKind::Builtin,
                scope: "app".into(),
                manifest: Some(raw_manifest("builtin.retired", "panel", None)),
                ..Default::default()
            },
        );
        s.enabled.insert("builtin.retired".into(), true);
        s.seeded_ids = vec!["builtin.retired".into()];
        let entries = default_entries(&["builtin.search"]);
        assert!(seed_default_rows(&mut s, &entries, &HashSet::new(), false));
        assert!(!s.sources.contains_key("builtin.retired"), "退役行应被清理");
        assert_eq!(s.enabled.get("builtin.retired"), None);
        assert_eq!(s.seeded_ids, vec!["builtin.search".to_string()]);
    }

    #[test]
    fn seeding_keeps_rows_when_defaults_empty() {
        // 空清单 = 调用方异常，不得当成「默认组合已清空」把已有行删掉。
        let mut s = PluginState::default();
        s.sources.insert(
            "builtin.search".into(),
            PluginSource {
                kind: PluginSourceKind::Builtin,
                scope: "app".into(),
                manifest: Some(raw_manifest("builtin.search", "panel", None)),
                ..Default::default()
            },
        );
        seed_default_rows(&mut s, &[], &HashSet::new(), false);
        assert!(s.sources.contains_key("builtin.search"));
    }

    #[test]
    fn backup_corrupt_state_uses_unique_name() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("plugin-state-test-{}.json", nanoid::nanoid!()));
        fs::write(&path, "not json").unwrap();
        let first = backup_corrupt_state(&path).expect("首次备份应成功");
        assert!(!path.exists(), "原文件应已被移走");
        // 第二次损坏不得覆盖第一次的备份（同名 rename 在 Windows 上会静默替换）。
        fs::write(&path, "not json again").unwrap();
        let second = backup_corrupt_state(&path).expect("第二次备份应成功");
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn seeding_keeps_uninstalled_removed_and_restore_readds() {
        // 已播种但被卸载（无行）→ 保持卸载；restore（用户显式恢复）→ 补建回默认启用。
        let entries = default_entries(&["builtin.search", "builtin.note"]);
        let mut s = PluginState::default();
        s.seeded_ids = vec!["builtin.search".into(), "builtin.note".into()];
        s.sources.insert(
            "builtin.search".into(),
            PluginSource { kind: PluginSourceKind::Builtin, scope: "app".into(), ..Default::default() },
        );
        seed_default_rows(&mut s, &entries, &HashSet::new(), false);
        assert!(!s.sources.contains_key("builtin.note"));
        assert_eq!(s.enabled.get("builtin.note"), None);

        seed_default_rows(&mut s, &entries, &HashSet::new(), true);
        assert!(s.sources.contains_key("builtin.note"));
        assert_eq!(s.enabled.get("builtin.note"), Some(&true));
    }

    #[test]
    fn seeding_skips_disk_backed_ids() {
        // 磁盘已有同名包 → 不建行（磁盘包覆盖随应用分发的实现），但记入已播种 id。
        let entries = default_entries(&["builtin.note"]);
        let mut s = PluginState::default();
        let disk: HashSet<String> = ["builtin.note".to_string()].into_iter().collect();
        seed_default_rows(&mut s, &entries, &disk, false);
        assert!(s.sources.is_empty());
        assert_eq!(s.seeded_ids, vec!["builtin.note"]);
        // 覆盖包卸载后不自动复活（要恢复默认需显式恢复）。
        seed_default_rows(&mut s, &entries, &HashSet::new(), false);
        assert!(s.sources.is_empty());
    }

    #[test]
    fn seeding_skips_entries_without_valid_id() {
        let mut s = PluginState::default();
        let entries = vec![json!({ "version": "1" }), json!({ "name": "todo" }), json!({ "name": "com.ok.x" })];
        seed_default_rows(&mut s, &entries, &HashSet::new(), false);
        assert_eq!(s.sources.keys().collect::<Vec<_>>(), vec!["com.ok.x"]);
        assert_eq!(s.seeded_ids, vec!["com.ok.x"]);
    }

    #[test]
    fn theme_manifest_detection() {
        // 主题声明口径（与前端派生一致）：只含基础条目（light/dark）= 非主题插件（重名条目按占用处理）；
        // 含自定义条目 = 主题插件；基础主题提供者自身声明基底条目 = 主题插件。
        let base_only = json!({ "atelyx": { "themes": [
            { "id": "light", "name": "浅色", "colorScheme": "light", "variables": {} },
            { "id": "dark", "name": "深色", "colorScheme": "dark", "variables": {} },
        ] } });
        assert!(manifest_is_theme(&base_only, BASE_THEME_PLUGIN_ID));
        assert!(!manifest_is_theme(&base_only, "com.acme.theme"));
        let custom = json!({ "atelyx": { "themes": [
            { "id": "nord", "name": "Nord", "colorScheme": "dark", "variables": {} },
        ] } });
        assert!(manifest_is_theme(&custom, "com.acme.theme"));
        assert!(!manifest_is_theme(&json!({ "atelyx": { "themes": [] } }), "com.acme.theme"));
        assert!(!manifest_is_theme(&json!({ "atelyx": { "type": "panel" } }), "com.acme.panel"));
        assert!(!manifest_is_theme(&json!({}), "com.acme.theme"));
    }
}
