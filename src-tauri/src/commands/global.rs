//! 全局配置命令（应用级数据）。
//!
//! 读写 `app_data_dir/global.json`：**应用级配置**——最近打开仓库列表 + 自动更新开关 +
//! 界面外观（主题/字号/字体）+ 自动恢复上次打开文件。
//! 个人仓库的 AI 供应商 / 搜索源在 `vault.rs` 的 `VaultConfig.providers/search`；
//! 协作空间的 AI 配置（含 API key）在服务端团队元数据，不经本文件
//! （`commands/keychain.rs` 的空间条目身份 `space:<serverUrl>#<spaceId>` 已不再使用）。
//! 字段只按当前形状读写；文件里出现未知字段时由 serde 忽略（不报错、不写回）。
//!
//! 另有 `app_data_dir/ui-state.json`（应用级 UI 使用状态：工作区布局 + 上次打开文件 +
//! 文件面板展开；本机独有、不随仓库同步，由 `crate::layout` 迷你窗口管理器单一写者
//! 承载，见 `layout.rs`）。
//!
//! 整文件读写 + 原子写（写 `.tmp` → rename）。前端写入统一走补丁命令：
//! `updateGlobalConfig` → `patch_global_config`（锁内读-合并-写）。
//! ——跨窗口（主/撕裂窗口各有独立 webview）并发整文件写会互相覆盖丢字段，合并必须在后端单点完成。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// 最近打开的仓库条目。
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RecentVault {
    pub root: String,
    pub name: String,
    pub last_opened_at: i64,
}

/// 协作空间仓库条目（应用级：最近打开的空间仓库；与 space_servers 登录清单区分）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpaceEntry {
    pub server_url: String,
    pub space_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<i64>,
}

/// 全局配置根结构（**应用级**：最近仓库列表 + 自动更新开关 + 界面外观（主题/字号/字体）+
/// 自动恢复上次打开文件；未知字段由 serde 忽略）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    #[serde(default)]
    pub recent_vaults: Vec<RecentVault>,
    /// 自动检查更新（应用级）：开启后每次启动应用静默检查新版本并自动安装。缺省 None = 关闭。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// 应用级激活的主题插件 id。缺失时前端默认取默认主题插件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// 各主题插件的设置项值字典（键 = 插件 id；预置键 colorMode/accentColor + 插件自定义键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_settings: Option<std::collections::BTreeMap<String, std::collections::BTreeMap<String, serde_json::Value>>>,
    /// 应用级界面基础字号（px，覆盖 :root font-size；缺省 = 18）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    /// 应用级界面字体（CSS font-family，缺省 = system-ui 默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// 进入仓库时自动恢复上次打开的文件。缺省 None = true（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_restore_files: Option<bool>,
    /// 协作开关（进入协作空间时是否建立实时连接；个人仓库无协作概念）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_enabled: Option<bool>,
    /// 协作显示昵称（空 = 设备名兜底）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_nickname: Option<String>,
    /// 协作身份色（hex；空 = 随机分配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_color: Option<String>,
    /// 进入仓库时自动切到「主页」布局。缺省 None = false（保持恢复上次界面）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_home_layout: Option<bool>,
    /// 宽松换行（应用级显示偏好）：开启时预览模式单个换行符渲染为换行；关闭时按 Markdown
    /// 标准视为空格。缺省 None = true（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_line_break: Option<bool>,
    /// 页面内标题（应用级显示偏好）：开启后笔记正文顶部显示文件名（不含扩展名）作为标题。
    /// 缺省 None = false（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_title: Option<bool>,
    /// 登录过的协作服务器地址清单（应用级；由前端 space 登录态维护，去重）。
    /// 缺省空 = 未登录过任何协作服务器；旧文件无此字段照常读取（serde default）。
    #[serde(default)]
    pub space_servers: Vec<String>,
    /// 最近打开的协作空间仓库列表（应用级；与 space_servers 登录清单区分）。
    /// 缺省空 = 未打开过任何协作空间仓库；旧文件无此字段照常读取（serde default）。
    #[serde(default)]
    pub spaces: Vec<SpaceEntry>,
}

/// `read_global_config` 的返回：全局配置 + 损坏备份文件名（`None` = 正常读取）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfigRead {
    pub config: GlobalConfig,
    /// 非空 = `global.json` 原文损坏、已按该文件名备份并退回空配置（前端据此提示用户）
    pub corrupt_backup: Option<String>,
}

/// 获取本机设备名（协作身份默认值：昵称留空时前端用它兜底展示）。
#[tauri::command]
pub fn get_hostname() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "Atelyx 用户".to_string())
}

fn global_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("global.json"))
}

/// 归一化仓库根路径：dunce::canonicalize 去除 Windows `\\?\` 长路径前缀 + 解析 `..`/符号链接，
/// 保证同一物理目录只存一种格式（如 `E:\测试仓库`）。路径已不存在时保留原字符串
/// （列表里要能显示/移除已删仓库，不因归一化失败丢条目）。
fn normalize_vault_root(root: &str) -> String {
    let p = Path::new(root);
    if p.is_absolute() {
        dunce::canonicalize(p)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| root.to_string())
    } else {
        root.to_string()
    }
}

/// 最近仓库列表归一化 + 去重（按归一化后路径精确去重，保留首次出现顺序）。
/// 兼容历史脏数据：同一物理目录可能同时存有 `\\?\E:\x` 与 `E:\x` 两条；
/// name 为空（网络共享根等历史 `file_name()` 取不到的条目）按路径重新推导。
fn normalize_and_dedupe_vaults(mut recents: Vec<RecentVault>) -> Vec<RecentVault> {
    let mut seen = std::collections::HashSet::new();
    recents.retain(|v| {
        let norm = normalize_vault_root(&v.root);
        seen.insert(norm)
    });
    for v in &mut recents {
        if v.name.trim().is_empty() {
            v.name = crate::vault::vault_display_name(Path::new(&v.root));
        }
    }
    recents
}

/// 读全局配置原文（文件不存在返回空配置；解析失败先备份原文再降级为空配置）。
/// 返回（配置, 损坏备份文件名）。备份失败返回 Err——留不下原文就继续按空配置走，
/// 调用方随后的写回会覆盖原文，既无备份也无从告知。
fn read_global_config_with_backup(path: &Path) -> Result<(GlobalConfig, Option<String>), String> {
    if !path.exists() {
        return Ok((GlobalConfig::default(), None));
    }
    let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    match serde_json::from_str::<GlobalConfig>(&data) {
        Ok(c) => Ok((c, None)),
        Err(e) => {
            // 为什么解析失败要先备份：读到的空配置会参与后续写盘——不备份就等于
            // 「一次外部编辑/磁盘异常静默清空最近仓库/主题/字号/协作地址」。备份走改名
            // （原文完整保留，可人工取回）；备份文件名一并回传，前端能据此说明原因。
            let Some(backup) = crate::vault::backup_corrupt_config(path, "global") else {
                return Err(format!(
                    "全局配置已损坏且原文备份失败，已中止读取以免覆盖原文（{}）：{e}",
                    path.display()
                ));
            };
            eprintln!(
                "[global] 全局配置损坏，已备份为 {}（按空配置继续）：{e}",
                backup.display()
            );
            let name = backup.file_name().map(|n| n.to_string_lossy().into_owned());
            Ok((GlobalConfig::default(), name))
        }
    }
}

/// 读全局配置（文件不存在返回空配置；解析失败先备份原文再降级）。
/// 返回前对 recentVaults 归一化去重，兼容旧版本写入的 `\\?\` 前缀脏数据。
#[tauri::command]
pub fn read_global_config(app: AppHandle) -> Result<GlobalConfigRead, String> {
    let path = global_config_path(&app)?;
    let (mut config, corrupt_backup) = read_global_config_with_backup(&path)?;
    config.recent_vaults = normalize_and_dedupe_vaults(config.recent_vaults);
    Ok(GlobalConfigRead {
        config,
        corrupt_backup,
    })
}

/// 写全局配置（原子写：临时文件 + fsync + rename，与 vault 侧 `atomic_write` 同一 durability 语义）。
/// 写入前对 recentVaults 归一化去重，保证落盘路径格式统一（验收标准：无 `\\?\` 前缀）。
#[tauri::command]
pub fn write_global_config(app: AppHandle, mut config: GlobalConfig) -> Result<(), String> {
    config.recent_vaults = normalize_and_dedupe_vaults(config.recent_vaults);
    let path = global_config_path(&app)?;
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::vault::atomic_write(&path, &json)
}

/// global.json 的进程内写互斥：补丁命令是「读文件 → 合并 → 写回」三步，
/// 并发调用（主窗口/撕裂窗口各有独立 webview 前端，跨窗口无共享）若不互斥，
/// 后写者会基于旧读数覆盖先写者的补丁（丢字段）。
static PATCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());



/// 全局配置补丁写（应用级写入单点）：读盘 → 顶层合并 → 原子写 → 返回写后完整配置
/// （含本次读到的损坏备份文件名）。整条命令在互斥锁内完成，跨窗口并发补丁不再互相覆盖。
///
/// 为什么是顶层浅替换（补丁键覆盖同名字段、`null` 删除该键）而非 RFC 7386 深合并：
/// themeSettings 的删键语义依赖整字段替换（前端先在内存里删好键再整对象提交），
/// 深合并会让「删除的键」从旧值里复活；顶层字段全部由前端整对象提交，浅替换与
/// 原 read-modify-write 行为逐字段一致。
#[tauri::command]
pub fn patch_global_config(app: AppHandle, patch: serde_json::Value) -> Result<GlobalConfigRead, String> {
    let _guard = PATCH_LOCK
        .lock()
        .map_err(|_| "全局配置写锁不可用".to_string())?;
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| "全局配置补丁必须是 JSON 对象".to_string())?;
    let path = global_config_path(&app)?;
    let (config, corrupt_backup) = read_global_config_with_backup(&path)?;
    // 经 serde_json::Value 应用补丁：未知补丁键在反序列化回 GlobalConfig 时被忽略（与读路径同口径），
    // 补丁值类型不匹配则报错不写盘（失败不得静默）
    let mut root = serde_json::to_value(&config).map_err(|e| e.to_string())?;
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "全局配置序列化异常".to_string())?;
    for (key, value) in patch_obj {
        if value.is_null() {
            root_obj.remove(key);
        } else {
            root_obj.insert(key.clone(), value.clone());
        }
    }
    let mut config: GlobalConfig =
        serde_json::from_value(root).map_err(|e| format!("全局配置补丁字段类型不匹配：{e}"))?;
    config.recent_vaults = normalize_and_dedupe_vaults(config.recent_vaults);
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::vault::atomic_write(&path, &json)?;
    Ok(GlobalConfigRead {
        config,
        corrupt_backup,
    })
}
