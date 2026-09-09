//! 插件平台命令：安装/卸载/启用/更新/读取入口与插件数据。
//!
//! 存储布局：
//! - app 级插件：`app_data_dir/plugins/<目录>/`（个人工具，本机）
//! - vault 级插件：`<仓库根>/.atelyx/plugins/<目录>/`（随仓库共享）
//! - 状态：`app_data_dir/plugin-state.json`（enabled 开关 + 安装来源 kind/repo/url/path/scope）
//!
//! 身份模型：插件身份 = 清单 `atelyx.json` 的 `id`（反向域名，仍校验）；**目录名 = 原名**（本地
//! 源目录名 / 仓库名），不校验合法性、不要求等于 id。按 id 定位一律扫描目录读清单匹配；
//! 点开头目录（`.install-*`/`.bak-*` 等临时/隐藏目录）不参与扫描。
//!
//! 安装流（三类来源，统一「取源码」）：
//! - 市场：GitHub `owner/repo`，git clone 到临时目录；本机无 git 时回退下载 GitHub 自动生成的
//!   源码包（codeload，作者零操作，非 Release 资产）。
//! - 手动 git 地址：git clone（保留 `.git` 供更新）。
//! - 本地目录：junction（Windows）/ 符号链接（Unix）实时引用，无拷贝无更新。
//! 三者统一：校验 `atelyx.json` → 以原名原子落位到 `plugins/<原名>/`（本地目录为链接）；失败不留脏。
//!
//! 安全：插件 id 视为不可信输入（仍校验）；插件目录内路径访问经 `safe_plugin_path` 限制在对应插件根
//! 目录内并拒绝符号链接段（防穿越越权）；插件代码在 WebView 隔离上下文执行、只能调前端桥。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};

use crate::vault::{atomic_write, VaultState};

/// 插件清单文件名（插件根目录；与前端 `constants/plugins.ts` 的 `PLUGIN_MANIFEST_FILE` 一致）。
const MANIFEST_FILE: &str = "atelyx.json";
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
/// 入口 JS 读取字节上限（内存/加载护栏：读入后整体注入 blob，16MB 远超正常插件逻辑大小，
/// 只拦误提交的巨型文件；Python 子进程入口不经此命令，不受限）。
const MAX_ENTRY_JS_BYTES: u64 = 16 * 1024 * 1024;

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
    /// 内置（随 App 分发）：播种进 plugin-state（首启 + 版本新增补种），实现随宿主编译（无磁盘目录）。
    /// 运行时与第三方插件无差别（同一注册表/启停/卸载/恢复）；版本随 App 走。
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

/// 安装来源记录（更新依据：市场按 repo 重新拉取，git 按 url pull，本地实时引用）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct PluginSource {
    /// 市场来源的 GitHub `owner/repo`（更新定位）。
    #[serde(default)]
    repo: String,
    /// 手动 git 来源的仓库地址。
    #[serde(default)]
    url: String,
    /// 本地来源的源目录绝对路径（junction/符号链接目标）。
    #[serde(default)]
    path: String,
    /// 落位目录名（原名）；清单损坏时卸载仍可据此按路径定位删除。
    #[serde(default)]
    dir_name: String,
    /// 来源类型（缺省市场）。
    #[serde(default)]
    kind: PluginSourceKind,
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
    /// 已播种过的内置插件 id（增量播种依据：新增内置条目随 App 版本补播，已播种条目
    /// 保持现状——卸载保持卸载、停用保持停用；恢复由用户显式触发）。
    #[serde(default)]
    builtin_seeded_ids: Vec<String>,
}

/// 跨作用域 id 全局唯一：同 id 已存在于另一作用域时拒绝安装（store/enabled/运行时均按裸 id
/// 寻址，双作用域并存会互相踩踏；随仓库同步的重复由扫描兜底展示，安装路径从源头禁止）。
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
    }
    Ok(())
}

// ===== 内置插件（随 App 分发，sourceKind=builtin） =====
// 内置插件 = 分发属性：播种进 plugin-state（首启 + 版本新增补种），运行时与第三方插件同一注册表/启停/卸载/恢复，
// 无任何特权。实现随宿主编译（无磁盘目录、无桥运行时），前端按 id 对应宿主组件载荷
// （`components/plugins/builtinViews.tsx`）；版本随 App 走。

/// 内置插件定义（id/展示信息；新增内置插件 = 在此加条目 + 前端补组件载荷）。
struct BuiltinPluginDef {
    id: &'static str,
    name: &'static str,
    tagline: &'static str,
}

const BUILTIN_PLUGINS: &[BuiltinPluginDef] = &[
    BuiltinPluginDef { id: "builtin.search", name: "搜索", tagline: "全文搜索仓库文件" },
    BuiltinPluginDef { id: "builtin.recent", name: "最近打开", tagline: "最近打开的文件列表" },
    BuiltinPluginDef { id: "builtin.calendar", name: "日历", tagline: "活动密度与手动日程" },
    BuiltinPluginDef { id: "builtin.aichat", name: "AI 对话", tagline: "AI 对话会话面板" },
    BuiltinPluginDef { id: "builtin.canvas", name: "画布", tagline: "有向图对话画布" },
    BuiltinPluginDef { id: "builtin.note", name: "笔记", tagline: "Markdown 笔记编辑器" },
    BuiltinPluginDef { id: "builtin.table", name: "表格", tagline: "多维表格编辑器" },
    BuiltinPluginDef { id: "builtin.files", name: "文件", tagline: "仓库文件树面板" },
    BuiltinPluginDef { id: "builtin.inspector", name: "属性", tagline: "节点/笔记属性面板" },
    BuiltinPluginDef { id: "builtin.collabroom", name: "协作房间", tagline: "协作在线用户面板" },
    BuiltinPluginDef { id: "builtin.repohistory", name: "仓库历史", tagline: "仓库版本历史面板" },
];

fn is_builtin_plugin_id(id: &str) -> bool {
    BUILTIN_PLUGINS.iter().any(|d| d.id == id)
}

/// 内置插件合成清单（前端消费 id/name/type/tagline；main 为校验占位——实现随宿主编译，
/// 前端按 sourceKind=builtin 跳过入口读取，只做宿主视图贡献注册）。
fn builtin_manifest(def: &BuiltinPluginDef) -> Value {
    serde_json::json!({
        "schemaVersion": 2,
        "id": def.id,
        "name": def.name,
        "version": env!("CARGO_PKG_VERSION"),
        "type": "panel",
        "scope": "app",
        "runtime": "js",
        "main": "builtin",
        "tagline": def.tagline,
        "author": "Atelyx",
        "license": "MIT",
    })
}

fn plugin_info_from_builtin(def: &BuiltinPluginDef, scope: &str, enabled: bool) -> PluginInfo {
    PluginInfo {
        id: def.id.to_string(),
        name: def.name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: "panel".to_string(),
        scope: scope.to_string(),
        install_dir: String::new(),
        enabled,
        manifest: builtin_manifest(def),
        source_kind: PluginSourceKind::Builtin,
    }
}

/// 把缺失的内置插件条目写入状态（enabled=true、来源 Builtin）；已存在条目保持现状。
/// 供「恢复内置插件」与首启/增量播种共用（卸载保持卸载、停用保持停用）。
fn seed_missing_builtins(pstate: &mut PluginState) {
    for def in BUILTIN_PLUGINS {
        if pstate.sources.contains_key(def.id) {
            continue;
        }
        pstate.sources.insert(
            def.id.to_string(),
            PluginSource { kind: PluginSourceKind::Builtin, scope: "app".to_string(), ..Default::default() },
        );
        pstate.enabled.insert(def.id.to_string(), true);
    }
}

/// 默认装配（官方默认插件集，组合配置的默认值层来源）：返回内置插件合成清单数组。
/// 含已卸载成员——`plugin_list` 不含已卸载条目，默认集必须来自编译期常量（单一权威，
/// 前端据此推导「已卸载的默认成员」灰行）；字段与 `builtin_manifest` 同源。
#[tauri::command]
pub fn plugin_default_plugins() -> Vec<Value> {
    BUILTIN_PLUGINS.iter().map(builtin_manifest).collect()
}

/// 内置插件播种（纯状态变换；真实入口 `ensure_builtin_seeded` 读写状态文件）：
/// 把未纳入 `builtin_seeded_ids` 的内置条目补播种（enabled=true、来源 Builtin）。
/// 增量语义 = 版本升级新增内置条目自动出现；已播种过的条目保持现状（卸载保持卸载、
/// 停用保持停用）；恢复由用户显式触发（`plugin_seed_builtin`）。
fn seed_new_builtins(pstate: &mut PluginState) {
    let mut seeded: HashSet<String> = pstate.builtin_seeded_ids.iter().cloned().collect();
    // 无 id 记录（首启或存量状态文件）时按现有内置来源推导已播种集，只增量补新增条目。
    // 已卸载条目与新增条目不可区分（既定边界：升级补回一次，恢复入口可再卸载），
    // 见 builtin_seeding_legacy_state_derives_seeded_from_sources 测试。
    if seeded.is_empty() {
        for id in pstate.sources.keys() {
            if is_builtin_plugin_id(id) {
                seeded.insert(id.clone());
            }
        }
    }
    for def in BUILTIN_PLUGINS {
        if seeded.contains(def.id) {
            continue;
        }
        if !pstate.sources.contains_key(def.id) {
            pstate.sources.insert(
                def.id.to_string(),
                PluginSource { kind: PluginSourceKind::Builtin, scope: "app".to_string(), ..Default::default() },
            );
            pstate.enabled.insert(def.id.to_string(), true);
        }
        seeded.insert(def.id.to_string());
    }
    // 回写完整 seeded 记录（含 legacy 从 sources 推导的条目），按 BUILTIN_PLUGINS 顺序稳定。
    pstate.builtin_seeded_ids = BUILTIN_PLUGINS
        .iter()
        .map(|d| d.id.to_string())
        .filter(|id| seeded.contains(id))
        .collect();
}

fn ensure_builtin_seeded(app: &AppHandle) {
    let mut pstate = read_plugin_state(app);
    let before = pstate.builtin_seeded_ids.len();
    seed_new_builtins(&mut pstate);
    // 播种失败不阻塞列表（下次重试）。
    if pstate.builtin_seeded_ids.len() != before {
        let _ = write_plugin_state(app, &pstate);
    }
}

/// 恢复内置插件（用户显式触发）：补播种缺失的内置条目（enabled=true）。
/// 已存在条目（启用/停用）保持现状，不覆盖用户改动。
fn seed_builtin_plugins(app: &AppHandle) -> Result<(), String> {
    let mut pstate = read_plugin_state(app);
    seed_missing_builtins(&mut pstate);
    // 恢复后把已播种 id 记入 seeded 列表（此后这些条目不再增量补播，卸载保持卸载）。
    let seeded: HashSet<&str> = pstate.builtin_seeded_ids.iter().map(String::as_str).collect();
    let missing: Vec<&str> = BUILTIN_PLUGINS.iter().map(|d| d.id).filter(|id| !seeded.contains(*id)).collect();
    pstate.builtin_seeded_ids.extend(missing.iter().map(|id| id.to_string()));
    write_plugin_state(app, &pstate)
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
        if manifest["id"].as_str() == Some(id) {
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

/// 把插件目录内相对路径安全地拼接到插件根（防穿越越权）。
fn safe_join_plugin(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel = Path::new(relative);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("非法插件内路径：{relative}"));
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

fn read_plugin_state(app: &AppHandle) -> PluginState {
    let Ok(path) = plugin_state_path(app) else {
        return PluginState::default();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_plugin_state(app: &AppHandle, state: &PluginState) -> Result<(), String> {
    let path = plugin_state_path(app)?;
    let raw = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    // 复用 vault::atomic_write：唯一临时名 + fsync + 失败清理（全项目同一 durability 语义）。
    atomic_write(&path, &raw)
}

// ===== 清单校验 =====

/// 最小清单校验（结构错误拒绝；字段枚举与前端 `validatePluginManifest` 对齐）。
fn manifest_valid_or_error(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("清单必须是对象")?;
    let req = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("清单缺少字段：{k}"))
    };
    let schema = obj
        .get("schemaVersion")
        .and_then(|x| x.as_i64())
        .filter(|n| *n > 0)
        .ok_or("schemaVersion 必须是正整数")?;
    if schema > 2 {
        return Err(format!("清单格式版本过新（{schema}），需要更新 Atelyx"));
    }
    // runtime（多语言执行平面）：未知运行时本 App 无法执行，直接拒绝。
    if let Some(rt) = obj.get("runtime") {
        if !matches!(rt.as_str(), Some("js" | "ts" | "python")) {
            return Err("runtime 仅支持 js/ts/python".to_string());
        }
    }
    // declares（披露的命名空间）非数组即拒绝：字符串等畸形形态会让前端组件 .map 崩溃。
    if let Some(d) = obj.get("declares") {
        if !d.is_array() {
            return Err("declares 必须是数组".to_string());
        }
    }
    let id = req("id")?;
    if !plugin_id_valid(&id) {
        return Err("id 必须是合法的反向域名标识".to_string());
    }
    req("name")?;
    req("version")?;
    let kind = req("type")?;
    // 主分类未知即拒绝（与前端 validatePluginManifest 一致；未知附加分类安全跳过）。
    if !is_known_plugin_type(&kind) {
        return Err(format!("未知插件类型：{kind}"));
    }
    // main 仅在纯 theme 插件（无任何代码承载类型）时可省略——theme 是声明式皮肤，无入口。
    // 判定与前端一致：只按「已知类型」归一化（未知附加分类安全跳过，前向兼容）——
    // 混入 tool 等已知代码类型才必填 main；types 非数组按畸形拒绝（与前端校验对齐）。
    let raw_types = obj.get("types");
    if raw_types.is_some() && !raw_types.and_then(|t| t.as_array()).is_some() {
        return Err("types 必须是数组".to_string());
    }
    let theme_only = kind == "theme"
        && raw_types
            .and_then(|t| t.as_array())
            .map_or(true, |arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .filter(|t| is_known_plugin_type(t))
                    .all(|t| t == "theme")
            });
    if !theme_only {
        req("main")?;
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

/// git 是否可用（探测 `git --version`）。
fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

/// 克隆 git 仓库到插件基础目录下的临时目录（保留 `.git` 供更新）；失败清理并返回错误。
async fn git_clone_to(base: &Path, url: &str) -> Result<PathBuf, String> {
    let target = base.join(format!(".install-{}", nanoid::nanoid!()));
    let out = tokio::process::Command::new("git")
        .args(["clone", url])
        .arg(git_path_arg(&target))
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "未检测到 git 命令，请安装 Git 或改用本地文件夹安装".to_string()
            } else {
                format!("执行 git 失败：{e}")
            }
        })?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&target);
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() { "git clone 失败".to_string() } else { msg });
    }
    Ok(target)
}

/// 解析仓库默认分支（无 git 回退源码包时需要分支名定位归档）。
async fn resolve_default_branch(repo: &str) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{repo}");
    let resp = reqwest::Client::new()
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
    let client = reqwest::Client::new();
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

/// 下载 zip 到临时文件（体积上限校验）。
async fn download_zip(client: &reqwest::Client, url: &str, temp: &Path) -> Result<(), String> {
    let resp = client
        .get(url)
        .header("User-Agent", "atelyx")
        .send()
        .await
        .map_err(|e| format!("下载插件失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载插件失败（HTTP {}）", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("下载插件失败：{e}"))?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err("插件包超过体积上限".into());
    }
    fs::write(temp, &bytes).map_err(|e| e.to_string())
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
    Err("插件包缺少 atelyx.json".into())
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
    PluginInfo {
        id: manifest["id"].as_str().unwrap_or("").to_string(),
        name: manifest["name"].as_str().unwrap_or("").to_string(),
        version: manifest["version"].as_str().unwrap_or("").to_string(),
        kind: manifest["type"].as_str().unwrap_or("").to_string(),
        scope: scope.to_string(),
        install_dir: dir.to_string_lossy().into_owned(),
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
    let id = manifest["id"].as_str().unwrap_or("").to_string();
    if !plugin_id_valid(&id) {
        return Err("插件清单 id 非法".into());
    }
    if is_builtin_plugin_id(&id) {
        return Err(format!("插件 id {id} 为内置插件保留，无法安装"));
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

    // 记录安装来源（更新依据 + 落位目录名兜底）。
    let mut source = source;
    source.dir_name = folder.clone();
    let mut pstate = read_plugin_state(app);
    pstate.sources.insert(id.clone(), source);
    if let Err(e) = write_plugin_state(app, &pstate) {
        // 状态写失败回滚落位，防「有目录无来源记录」的幽灵插件（重装/更新都定位不到）。
        let _ = fs::rename(&target, plugin_root);
        let _ = fs::remove_dir_all(plugin_root);
        return Err(e);
    }

    let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
    Ok(plugin_info_from(&target, &manifest, scope, source_kind, enabled))
}

// ===== 命令 =====

/// 列出全部已装插件（app 级恒有；vault 级仅当前仓库；未开仓库时跳过 vault 目录）。
#[tauri::command]
pub fn plugin_list(app: AppHandle, state: State<'_, VaultState>) -> Result<Vec<PluginInfo>, String> {
    ensure_builtin_seeded(&app);
    let pstate = read_plugin_state(&app);
    let mut out: Vec<PluginInfo> = Vec::new();

    // 内置插件行（无磁盘目录，来源记录为 Builtin 即视为已装；与磁盘行同表去重）。
    let mut seen: Vec<String> = Vec::new();
    for (id, src) in &pstate.sources {
        if src.kind != PluginSourceKind::Builtin {
            continue;
        }
        let Some(def) = BUILTIN_PLUGINS.iter().find(|d| d.id == id) else {
            continue;
        };
        let enabled = pstate.enabled.get(id).copied().unwrap_or(false);
        out.push(plugin_info_from_builtin(def, "app", enabled));
        seen.push(id.clone());
    }

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
            let id = manifest["id"].as_str().unwrap_or("").to_string();
            if !plugin_id_valid(&id) {
                continue;
            }
            if seen.iter().any(|x| x == &id) {
                continue; // 含内置 id（防御：内置 id 安装已被拒，仅手动拷贝目录可能撞名）
            }
            seen.push(id.clone());
            let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
            let source_kind = pstate.sources.get(&id).map(|s| s.kind).unwrap_or_default();
            out.push(plugin_info_from(&dir, &manifest, scope, source_kind, enabled));
        }
    };

    if let Ok(base) = plugin_base_dir(&app, &state, "app") {
        scan("app", &base);
    }
    if let Ok(root) = state.root() {
        let base = root.join(".atelyx/plugins");
        scan("vault", &base);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 安装插件（来源 = GitHub `owner/repo` 或完整 git 地址；安装后默认未启用，由前端确认后启用）。
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
        if git_available() {
            let clone_target = git_clone_to(&base, &format!("https://github.com/{repo}.git")).await?;
            let source = PluginSource {
                repo: repo.clone(),
                url: String::new(),
                path: String::new(),
                dir_name: String::new(), // 落位时由 install_plugin_dir 写入
                kind: PluginSourceKind::Market,
                scope: scope.clone(),
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
                    url: String::new(),
                    path: String::new(),
                    dir_name: String::new(), // 落位时由 install_plugin_dir 写入
                    kind: PluginSourceKind::Market,
                    scope: scope.clone(),
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

    // 手动 git 地址：必须有 git。
    if !git_available() {
        return Err("未检测到 git 命令，请安装 Git 或改用本地文件夹安装".into());
    }
    let clone_target = git_clone_to(&base, &repo).await?;
    let source = PluginSource {
        repo: String::new(),
        url: repo.clone(),
        path: String::new(),
        dir_name: String::new(), // 落位时由 install_plugin_dir 写入
        kind: PluginSourceKind::Git,
        scope: scope.clone(),
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
    let id = manifest["id"].as_str().unwrap_or("").to_string();
    if !plugin_id_valid(&id) {
        return Err("插件清单 id 非法".into());
    }
    if is_builtin_plugin_id(&id) {
        return Err(format!("插件 id {id} 为内置插件保留，无法安装"));
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

    // 记录安装来源（更新依据：本地来源实时引用，无需更新）。
    let mut pstate = read_plugin_state(&app);
    pstate.sources.insert(
        id.clone(),
        PluginSource {
            repo: String::new(),
            url: String::new(),
            path: src_dir.to_string_lossy().into_owned(),
            dir_name: folder.clone(),
            kind: PluginSourceKind::Local,
            scope: scope.clone(),
        },
    );
    if let Err(e) = write_plugin_state(&app, &pstate) {
        // 状态写失败回滚链接，防「有链接无来源记录」残留。
        let _ = fs::remove_dir(&target);
        return Err(e);
    }

    let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
    Ok(plugin_info_from(&target, &manifest, &scope, PluginSourceKind::Local, enabled))
}

/// 创建目录链接：Windows 用 junction（免管理员），其余平台用符号链接。
#[cfg(windows)]
fn create_plugin_link(src: &Path, link: &Path) -> Result<(), String> {
    junction::create(src, link).map_err(|e| format!("创建 junction 失败：{e}"))
}

#[cfg(not(windows))]
fn create_plugin_link(src: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink_dir(src, link).map_err(|e| format!("创建符号链接失败：{e}"))
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

/// 卸载插件：本地来源只删链接（源目录不动）；其余删除整个插件目录。清理状态记录。
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
    let mut pstate = read_plugin_state(&app);
    // 内置插件无磁盘目录：卸载 = 仅清状态记录（恢复经「恢复内置插件」入口重新播种）。
    if pstate.sources.get(&id).map(|s| s.kind) == Some(PluginSourceKind::Builtin) {
        pstate.enabled.remove(&id);
        pstate.sources.remove(&id);
        return write_plugin_state(&app, &pstate);
    }
    let base = plugin_base_dir(&app, &state, &scope)?;
    // 优先按清单 id 扫描定位；清单损坏（扫描无法匹配）时按来源记录的落位目录名定位删除
    // （名字经 target_folder_name 同款清理校验，确保仍在插件目录内）。
    let dir = match find_plugin_dir(&base, &id) {
        Ok(d) => d,
        Err(_) => {
            let safe_name = pstate
                .sources
                .get(&id)
                .map(|s| target_folder_name(&s.dir_name, &id))
                .unwrap_or_default();
            // 无来源记录（随仓库同步/残留）时拒绝删除，防 base.join("") 把整个插件目录当目标。
            if safe_name.is_empty() {
                return Err("插件不存在".into());
            }
            let by_name = base.join(&safe_name);
            if by_name.is_dir() {
                by_name
            } else {
                return Err("插件不存在".into());
            }
        }
    };
    let is_local = pstate.sources.get(&id).map(|s| s.kind == PluginSourceKind::Local).unwrap_or(false);
    if is_local {
        remove_link_only(&dir)?;
    } else {
        fs::remove_dir_all(&dir).map_err(|e| format!("卸载失败：{e}"))?;
    }
    pstate.enabled.remove(&id);
    pstate.sources.remove(&id);
    write_plugin_state(&app, &pstate)
}

/// 启用/停用插件（前端先确认权限再启用；vault 级插件卸载/禁用不清仓库内文件）。
#[tauri::command]
pub fn plugin_set_enabled(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
    let mut pstate = read_plugin_state(&app);
    if enabled {
        pstate.enabled.insert(id.clone(), true);
    } else {
        pstate.enabled.remove(&id);
    }
    write_plugin_state(&app, &pstate)
}

/// 恢复内置插件（管理 UI「恢复内置插件」入口）：补播种缺失的内置条目。
/// 已存在条目（启用/停用）保持现状；调用后前端重载插件列表。
#[tauri::command]
pub fn plugin_seed_builtin(app: AppHandle) -> Result<(), String> {
    seed_builtin_plugins(&app)
}

/// 更新插件：按来源分派——git 来源 git pull（失败目录不变）；市场且无 .git 时重新下载源码包替换；
/// 本地目录实时引用无需更新（返回当前信息）。
#[tauri::command]
pub async fn plugin_update(
    app: AppHandle,
    state: State<'_, VaultState>,
    id: String,
) -> Result<PluginInfo, String> {
    let pstate = read_plugin_state(&app);
    let source = pstate
        .sources
        .get(&id)
        .cloned()
        .ok_or("插件无安装来源，无法更新（请先卸载重装）")?;
    let scope = source.scope.clone();
    // 内置插件版本随 App 走，无独立更新；返回当前信息（无磁盘目录，提前返回）。
    if source.kind == PluginSourceKind::Builtin {
        let Some(def) = BUILTIN_PLUGINS.iter().find(|d| d.id == id) else {
            return Err("插件不存在".into());
        };
        let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
        return Ok(plugin_info_from_builtin(def, &scope, enabled));
    }
    let base = plugin_base_dir(&app, &state, &scope)?;
    let dir = find_plugin_dir(&base, &id)?;

    match source.kind {
        PluginSourceKind::Builtin => {} // 不可达（已提前返回）
        PluginSourceKind::Local => {
            // 本地目录实时引用：目录即源码，无更新概念；重读清单返回当前信息。
            let manifest = read_manifest(&dir)?;
            let enabled = pstate.enabled.get(&id).copied().unwrap_or(false);
            return Ok(plugin_info_from(&dir, &manifest, &scope, PluginSourceKind::Local, enabled));
        }
        PluginSourceKind::Market => {
            // 源码包装的插件（无 git 环境安装，无 .git）：重新下载源码包替换。
            if !dir.join(".git").exists() {
                return codeload_update(&app, &state, &scope, &source, &dir, &base).await;
            }
        }
        PluginSourceKind::Git => {}
    }

    // git 来源（含保留 .git 的市场来源）：git pull；失败目录不变，不引入备份回滚。
    let out = tokio::process::Command::new("git")
        .args(["-C"])
        .arg(git_path_arg(&dir))
        .arg("pull")
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "未检测到 git 命令，无法更新".to_string()
            } else {
                format!("执行 git 失败：{e}")
            }
        })?;
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
    let client = reqwest::Client::new();
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
    let old_id = old_manifest["id"].as_str().unwrap_or("").to_string();
    // 校验新清单 id 与旧清单 id 一致：改 id 的版本无法原地更新，防新旧记录并存。
    let new_manifest = read_manifest(&plugin_root)?;
    if new_manifest["id"].as_str() != Some(old_id.as_str()) {
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
        if let Some(src) = read_plugin_state(app).sources.get(id) {
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

/// 读取插件入口 JS（供隔离上下文加载；限制在插件根目录内）。path 缺省 = 清单 main。
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
    let data = fs::read(&entry_path).map_err(|e| format!("读取插件入口失败：{e}"))?;
    if data.len() as u64 > MAX_ENTRY_JS_BYTES {
        return Err("插件入口文件过大".into());
    }
    String::from_utf8(data).map_err(|_| "插件入口不是合法 UTF-8 文本".to_string())
}

/// 读取插件自持数据（单 JSON 对象，桥 state:persist 落盘）。
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
        let expected = Path::new("a").join("b").join("atelyx.json");
        let got = sanitize_zip_entry("a/b/atelyx.json").unwrap();
        assert_eq!(Path::new(&got), expected);
        assert_eq!(sanitize_zip_entry("./atelyx.json").unwrap(), "atelyx.json");
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
        assert!(safe_plugin_path(root, "a/b/atelyx.json").is_ok());
        assert!(safe_plugin_path(root, "atelyx.json").is_ok());
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
        let ok = json!({
            "schemaVersion": 1,
            "id": "com.example.todo",
            "name": "示例",
            "version": "1.0.0",
            "type": "tool",
            "main": "plugin.js"
        });
        assert!(manifest_valid_or_error(&ok).is_ok());
        let bad = json!({ "schemaVersion": 3, "id": "com.x", "name": "x", "version": "1", "type": "tool", "main": "a.js" });
        assert!(manifest_valid_or_error(&bad).is_err());
        // schemaVersion 2：多语言运行时字段。
        let v2 = json!({
            "schemaVersion": 2,
            "id": "com.example.py",
            "name": "Python 插件",
            "version": "1.0.0",
            "type": "tool",
            "runtime": "python",
            "main": "main.py",
            "provides": ["com.example.py.data"],
            "declares": ["vault:read"]
        });
        assert!(manifest_valid_or_error(&v2).is_ok());
        let bad_runtime = json!({ "schemaVersion": 2, "id": "com.x", "name": "x", "version": "1", "type": "tool", "main": "a.rs", "runtime": "rust" });
        assert!(manifest_valid_or_error(&bad_runtime).is_err());
        let missing = json!({ "schemaVersion": 1, "id": "com.x", "name": "x" });
        assert!(manifest_valid_or_error(&missing).is_err());
        // theme 声明式：纯 theme 可省略 main；含代码类型则必填。
        let theme = json!({ "schemaVersion": 1, "id": "com.example.dark", "name": "x", "version": "1", "type": "theme" });
        assert!(manifest_valid_or_error(&theme).is_ok());
        let tool_no_main = json!({ "schemaVersion": 1, "id": "com.x", "name": "x", "version": "1", "type": "tool" });
        assert!(manifest_valid_or_error(&tool_no_main).is_err());
    }

    #[test]
    fn builtin_plugin_defs_are_valid() {
        // id 唯一且合法；合成清单通过校验（schemaVersion/type/runtime；main 为校验占位）。
        let ids: Vec<&str> = BUILTIN_PLUGINS.iter().map(|d| d.id).collect();
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "内置插件 id 必须唯一");
        for def in BUILTIN_PLUGINS {
            assert!(plugin_id_valid(def.id), "内置插件 id 非法：{}", def.id);
            let manifest = builtin_manifest(def);
            assert!(manifest_valid_or_error(&manifest).is_ok(), "内置插件清单非法：{}", def.id);
            assert_eq!(manifest["type"].as_str(), Some("panel"));
            assert_eq!(manifest["runtime"].as_str(), Some("js"));
        }
        assert!(is_builtin_plugin_id("builtin.search"));
        assert!(is_builtin_plugin_id("builtin.aichat"));
        assert!(!is_builtin_plugin_id("com.acme.x"));
    }

    #[test]
    fn default_plugins_expose_builtin_set() {
        // 默认装配与内置常量一致（单一权威：含已卸载成员也能枚举，供装配视图推导灰行）。
        let defaults = plugin_default_plugins();
        assert_eq!(defaults.len(), BUILTIN_PLUGINS.len());
        for (def, v) in BUILTIN_PLUGINS.iter().zip(&defaults) {
            assert_eq!(v["id"], def.id);
            assert_eq!(v["name"], def.name);
            assert_eq!(v["tagline"], def.tagline);
            assert_eq!(v["type"].as_str(), Some("panel"));
        }
    }

    #[test]
    fn seed_missing_builtins_is_idempotent_and_preserves_state() {
        let mut s = PluginState::default();
        seed_missing_builtins(&mut s);
        assert_eq!(s.sources.len(), BUILTIN_PLUGINS.len());
        for def in BUILTIN_PLUGINS {
            assert_eq!(s.sources[def.id].kind, PluginSourceKind::Builtin);
            assert_eq!(s.enabled.get(def.id), Some(&true));
        }
        // 幂等：重复调用不增删。
        let before_len = s.sources.len();
        seed_missing_builtins(&mut s);
        assert_eq!(s.sources.len(), before_len);
        // 已存在条目保持现状（停用不复活）；缺失条目补回（恢复语义）。
        s.enabled.insert(BUILTIN_PLUGINS[0].id.to_string(), false);
        s.sources.remove(BUILTIN_PLUGINS[1].id);
        seed_missing_builtins(&mut s);
        assert_eq!(s.enabled.get(BUILTIN_PLUGINS[0].id), Some(&false)); // 停用保持
        assert!(s.sources.contains_key(BUILTIN_PLUGINS[1].id)); // 缺失补回（显式恢复）
    }

    #[test]
    fn builtin_seeding_is_incremental_by_id() {
        // 增量播种：只补未记录的内置条目；已记录条目（含停用）保持现状。
        let mut s = PluginState::default();
        s.builtin_seeded_ids = BUILTIN_PLUGINS[..4].iter().map(|d| d.id.to_string()).collect();
        for def in &BUILTIN_PLUGINS[..4] {
            s.sources.insert(def.id.to_string(), PluginSource { kind: PluginSourceKind::Builtin, scope: "app".to_string(), ..Default::default() });
        }
        s.enabled.insert(BUILTIN_PLUGINS[1].id.to_string(), false); // 停用保持
        seed_new_builtins(&mut s);
        assert_eq!(s.enabled.get(BUILTIN_PLUGINS[1].id), Some(&false)); // 停用不复活
        assert!(s.sources.contains_key(BUILTIN_PLUGINS[4].id)); // 未记录的新条目补播种
        assert_eq!(s.enabled.get(BUILTIN_PLUGINS[4].id), Some(&true));
        assert_eq!(s.sources.len(), BUILTIN_PLUGINS.len());
        assert_eq!(s.builtin_seeded_ids.len(), BUILTIN_PLUGINS.len());
        // 幂等：重复调用无新增、不覆盖停用状态。
        seed_new_builtins(&mut s);
        assert_eq!(s.sources.len(), BUILTIN_PLUGINS.len());
        assert_eq!(s.builtin_seeded_ids.len(), BUILTIN_PLUGINS.len());
        assert_eq!(s.enabled.get(BUILTIN_PLUGINS[1].id), Some(&false));
    }

    #[test]
    fn builtin_seeding_legacy_state_derives_seeded_from_sources() {
        // 旧状态文件无 id 记录（一次性布尔语义）：现有内置来源视为已播种，只补新条目、
        // 不复活仍在列表中的旧条目状态；已卸载的旧条目无法与「新条目」区分（不做存量迁移
        // 的既定边界：升级补回一次，恢复入口可再卸载）。
        let mut s = PluginState::default();
        for def in &BUILTIN_PLUGINS[..4] {
            s.sources.insert(def.id.to_string(), PluginSource { kind: PluginSourceKind::Builtin, scope: "app".to_string(), ..Default::default() });
        }
        s.enabled.insert(BUILTIN_PLUGINS[0].id.to_string(), false);
        seed_new_builtins(&mut s);
        assert_eq!(s.enabled.get(BUILTIN_PLUGINS[0].id), Some(&false)); // 停用保持
        assert!(s.sources.contains_key(BUILTIN_PLUGINS[3].id)); // 现有条目不重复播种
        assert!(s.sources.contains_key(BUILTIN_PLUGINS[4].id)); // 新条目补播种
        assert_eq!(s.sources.len(), BUILTIN_PLUGINS.len());
        assert_eq!(s.builtin_seeded_ids.len(), BUILTIN_PLUGINS.len());
    }

    #[test]
    fn builtin_seeding_keeps_seeded_uninstalled_removed() {
        // 核心不变量：已记入 builtin_seeded_ids 的内置条目即使已被卸载（sources 无对应项）
        // 也不再重新播种——卸载保持卸载（增量 by-id 语义的立足点）。
        let mut s = PluginState::default();
        s.builtin_seeded_ids = BUILTIN_PLUGINS[..4].iter().map(|d| d.id.to_string()).collect();
        for def in &BUILTIN_PLUGINS[..4] {
            if def.id == BUILTIN_PLUGINS[1].id {
                continue; // 已卸载：不在 sources
            }
            s.sources.insert(def.id.to_string(), PluginSource { kind: PluginSourceKind::Builtin, scope: "app".to_string(), ..Default::default() });
        }
        seed_new_builtins(&mut s);
        // 已播种但已卸载的条目不复活。
        assert!(!s.sources.contains_key(BUILTIN_PLUGINS[1].id));
        assert!(s.enabled.get(BUILTIN_PLUGINS[1].id).is_none()); // 卸载保持：不复活也不启用
        // 未记录的后续条目仍补播种。
        assert!(s.sources.contains_key(BUILTIN_PLUGINS[4].id));
        assert_eq!(s.sources.len(), BUILTIN_PLUGINS.len() - 1);
        assert_eq!(s.builtin_seeded_ids.len(), BUILTIN_PLUGINS.len());
    }
}
