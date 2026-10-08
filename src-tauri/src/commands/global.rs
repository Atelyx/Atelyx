//! 全局配置命令：`app_data_dir/global.json` 的读写与补丁（应用级，不随仓库同步）。
//! 仓库级 AI 供应商 / 搜索源在 `vault.rs` 的 `VaultConfig.providers/search`；协作空间的 AI 配置（含 API key）在服务端团队元数据，不经本文件。
//! 应用级 UI 使用状态在 `app_data_dir/ui-state.json`，由 `crate::layout` 迷你窗口管理器单一写者承载（见 `layout.rs`）。

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

/// 全局配置根结构（**应用级**：最近仓库列表 + 自动检查更新开关 + 界面外观（主题/字号/字体）+
/// 自动恢复上次打开文件；未知字段由 serde 忽略）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    #[serde(default)]
    pub recent_vaults: Vec<RecentVault>,
    /// 自动检查更新（应用级）：开启后每次启动应用静默检查新版本并提示更新。缺省 None = 关闭。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// 首次启动的存储授权引导是否已展示（移动端本地仓库用；缺省 None = 未展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android_storage_onboarded: Option<bool>,
    /// 应用级激活的主题插件 id。缺失时前端默认取默认主题插件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// 各主题插件的设置项值字典（键 = 插件 id；预置键 colorMode/accentColor + 插件自定义键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_settings: Option<std::collections::BTreeMap<String, std::collections::BTreeMap<String, serde_json::Value>>>,
    /// 应用级界面基础字号（px，覆盖 :root font-size；缺省 = 16）。
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
    /// 宽松换行（应用级显示偏好）：开启时预览模式单个换行符渲染为换行；关闭时按 Markdown
    /// 标准视为空格。缺省 None = true（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_line_break: Option<bool>,
    /// 页面内标题（应用级显示偏好）：开启后笔记正文顶部显示文件名（不含扩展名）作为标题。
    /// 缺省 None = false（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_title: Option<bool>,
    /// 笔记正文行宽上限（px，应用级显示偏好）：0 = 不限制。缺省 None = 780（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_line_width: Option<f64>,
    /// 移动端底部导航栏的视图顺序（应用级；视图 key 数组）。缺省空 = 前端按内建常用序排。
    #[serde(default)]
    pub mobile_nav_order: Vec<String>,
    /// 登录过的协作服务器地址清单（应用级；由前端 space 登录态维护，去重）。
    /// 缺省空 = 未登录过任何协作服务器；旧文件无此字段照常读取（serde default）。
    #[serde(default)]
    pub space_servers: Vec<String>,
    /// 最近打开的协作空间仓库列表（应用级；与 space_servers 登录清单区分）。
    /// 缺省空 = 未打开过任何协作空间仓库；旧文件无此字段照常读取（serde default）。
    #[serde(default)]
    pub spaces: Vec<SpaceEntry>,
    /// 命令快捷键的用户覆盖（命令 globalId → 键串；缺省 None = 全部用命令声明的默认键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_shortcuts: Option<std::collections::BTreeMap<String, String>>,
    /// 全局快捷键的用户覆盖（`插件id:声明id` → OS accelerator 串；缺省 None = 全部用声明默认键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_shortcuts: Option<std::collections::BTreeMap<String, String>>,
    /// 组合接管的用户层（组合行 id → 实现 id；`"default"` = 该行自身默认实现）。
    /// 用户层恒胜插件清单声明（即「钉住」），删除键 = 解除钉住。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_patches: Option<std::collections::BTreeMap<String, String>>,
}

/// `read_global_config` 的返回：全局配置 + 损坏备份文件名（`None` = 正常读取）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfigRead {
    pub config: GlobalConfig,
    /// 非空 = `global.json` 原文损坏、已按该文件名备份并退回空配置（前端据此提示用户）
    pub corrupt_backup: Option<String>,
    /// 应用装配版本快照（进程内单调计数器，计数器本体在 `commands::plugin`）：补丁含组合
    /// 用户层时自增，前端据此广播并做跨窗口装配快照比对。
    pub assembly_version: u64,
}

/// 获取本机设备名（协作身份默认值：昵称留空时前端用它兜底展示）。
/// 安卓没有用户可设主机名：经系统能力桥取设备型号；桥不可用或型号为空回落通用兜底，
/// 该回落即此命令的既定语义（展示用途，不上报错误）。
#[tauri::command]
pub async fn get_hostname(app: AppHandle) -> String {
    #[cfg(target_os = "android")]
    {
        super::mobile::bridge_device_name(app)
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(fallback_hostname)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        desktop_hostname().unwrap_or_else(fallback_hostname)
    }
}

/// 协作身份兜底名（各平台取不到设备名时的统一回落）。
fn fallback_hostname() -> String {
    "Atelyx 用户".to_string()
}

/// 桌面主机名：环境变量优先（Windows），无则读内核 hostname（Linux）。
#[cfg(not(target_os = "android"))]
fn desktop_hostname() -> Option<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
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

/// 最近仓库列表归一化 + 去重：同一物理目录只留一条（按解析后的路径精确去重，保留首次出现顺序），
/// 并保证列表里存的是规范路径（去 Windows `\\?\` 长路径前缀、解析 `..`/符号链接），
/// `name` 为空（网络共享根等 `file_name()` 取不到的路径）按路径重新推导。
/// 读写两侧都过一道：外部改过的文件与前端提交的整表都收敛到同一形态。
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
        assembly_version: crate::commands::plugin::assembly_version(),
    })
}

/// global.json 的进程内写互斥：补丁命令是「读文件 → 合并 → 写回」三步，
/// 并发调用（主窗口/撕裂窗口各有独立 webview 前端，跨窗口无共享）若不互斥，
/// 后写者会基于旧读数覆盖先写者的补丁（丢字段）。
static PATCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 补丁事务主体（PATCH_LOCK 内）：读盘 → 顶层合并 → 原子写 → 返回写后完整配置
/// （含本次读到的损坏备份文件名）。补丁含 compositionPatches = 装配输入变更，提交时
/// 前移装配版本（`null` 删键同样是变更，contains_key 已覆盖）。
///
/// 为什么是顶层浅替换（补丁键覆盖同名字段、`null` 删除该键）而非 RFC 7386 深合并：
/// themeSettings 的删键语义依赖整字段替换（前端先在内存里删好键再整对象提交），
/// 深合并会让「删除的键」从旧值里复活；顶层字段全部由前端整对象提交，浅替换与
/// 原 read-modify-write 行为逐字段一致。拆 `_at` 是为绕开 AppHandle 对临时路径做事务级单测。
fn patch_global_config_at(path: &Path, patch: &serde_json::Value) -> Result<GlobalConfigRead, String> {
    let _guard = PATCH_LOCK
        .lock()
        .map_err(|_| "全局配置写锁不可用".to_string())?;
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| "全局配置补丁必须是 JSON 对象".to_string())?;
    let (config, corrupt_backup) = read_global_config_with_backup(path)?;
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
    crate::vault::atomic_write(path, &json)?;
    // 补丁含组合用户层 = 装配输入变更：版本前移（`null` 删键同样是变更，contains_key 已覆盖）。
    // 放在提交成功之后：写盘失败不得留下「版本已前移、磁盘未变」的空转追平信号。
    if patch_obj.contains_key("compositionPatches") {
        crate::commands::plugin::bump_assembly_version();
    }
    Ok(GlobalConfigRead {
        config,
        corrupt_backup,
        assembly_version: crate::commands::plugin::assembly_version(),
    })
}

/// 全局配置补丁写（应用级写入单点）：整条命令在互斥锁内完成，跨窗口并发补丁不再互相覆盖。
#[tauri::command]
pub fn patch_global_config(app: AppHandle, patch: serde_json::Value) -> Result<GlobalConfigRead, String> {
    patch_global_config_at(&global_config_path(&app)?, &patch)
}

/// 原生启动底色的深浅方案（底色只能取主题 `--bg-primary` 的浅/深基底这两档）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ThemeScheme {
    Dark,
    Light,
}

/// 内置主题插件 id（与前端 `utils/pluginTheme.ts` 的 `BUILTIN_THEME_PLUGIN_ID` 同值，契约测试把守）。
const BUILTIN_THEME_PLUGIN_ID: &str = "builtin.theme";
/// 主题设置项键（前端 `utils/pluginTheme.ts` 的 `COLOR_MODE_KEY` / `VARIANT_KEY`）。
const THEME_COLOR_MODE_KEY: &str = "colorMode";
const THEME_VARIANT_KEY: &str = "variant";

/// 当前主题的深浅方案（原生启动底色用；`system_dark` = 系统是否深色）。
/// 只解析内置主题插件的浅/深基底：皮肤（内置「极光」的 variables 自定义了 `--bg-primary`）、
/// 自定义主题插件（含它被停用后前端回退内置插件的情形）与读盘/解析失败一律回落深色——
/// 宁可闪一次深色，也不猜一个错色。
pub(crate) fn startup_theme_scheme(app: &AppHandle, system_dark: bool) -> ThemeScheme {
    let Ok(path) = global_config_path(app) else {
        return ThemeScheme::Dark;
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return ThemeScheme::Dark;
    };
    match serde_json::from_str::<GlobalConfig>(&data) {
        Ok(config) => theme_scheme_of(&config, system_dark),
        Err(_) => ThemeScheme::Dark,
    }
}

/// 主题值 → 深浅方案（colorMode 与主题插件口径逐分支对齐前端 `utils/pluginTheme.ts` 的
/// `resolveActiveThemeEntry`；皮肤变体比前端保守：任意字符串变体都按变量自定义兜底深色）。
fn theme_scheme_of(config: &GlobalConfig, system_dark: bool) -> ThemeScheme {
    // 未设置主题 = 默认主题插件（前端同口径）
    if config.theme.as_deref().unwrap_or(BUILTIN_THEME_PLUGIN_ID) != BUILTIN_THEME_PLUGIN_ID {
        return ThemeScheme::Dark;
    }
    let settings = config
        .theme_settings
        .as_ref()
        .and_then(|all| all.get(BUILTIN_THEME_PLUGIN_ID));
    // 皮肤：命中条目的 variables 自定义了底色（如内置「极光」的 `--bg-primary`），Rust 不带皮肤清单，
    // 故任意字符串变体都按「变量自定义」处理 → 深色兜底（前端只认命中条目 id 的变体，
    // 这里比前端更保守：陈旧变体值下会多闪一次深色，代价可接受）。
    if settings
        .and_then(|s| s.get(THEME_VARIANT_KEY))
        .and_then(|v| v.as_str())
        .is_some()
    {
        return ThemeScheme::Dark;
    }
    match settings {
        // 整条主题设置缺失：前端会播种缺省 colorMode = dark
        None => ThemeScheme::Dark,
        Some(s) => match s.get(THEME_COLOR_MODE_KEY).and_then(|v| v.as_str()) {
            Some("dark") => ThemeScheme::Dark,
            Some("light") => ThemeScheme::Light,
            // "system" 与未知值/键缺失：前端解析为跟随系统
            _ => {
                if system_dark {
                    ThemeScheme::Dark
                } else {
                    ThemeScheme::Light
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::plugin::{assembly_version, ASSEMBLY_TEST_GATE};

    /// 原生启动底色的主题解析：内置主题插件按 colorMode 取深浅（跟随系统时看系统），
    /// 皮肤与自定义主题插件一律深色兜底（变量在 JS 侧，Rust 读不到）。
    #[test]
    fn startup_theme_scheme_resolution() {
        let of = |json: &str, system_dark: bool| {
            let config: GlobalConfig = serde_json::from_str(json).unwrap();
            theme_scheme_of(&config, system_dark)
        };
        let with_mode = |mode: &str| {
            format!(r#"{{"theme":"builtin.theme","themeSettings":{{"builtin.theme":{{"colorMode":"{mode}"}}}}}}"#)
        };
        // 整条主题设置缺失 = 前端播种缺省 colorMode = dark；主题未设置同理（默认主题插件）
        assert_eq!(of(r#"{"theme":"builtin.theme"}"#, false), ThemeScheme::Dark);
        assert_eq!(of("{}", false), ThemeScheme::Dark);
        assert_eq!(of(&with_mode("dark"), false), ThemeScheme::Dark);
        assert_eq!(of(&with_mode("dark"), true), ThemeScheme::Dark);
        // 显式浅色：与系统无关
        assert_eq!(of(&with_mode("light"), true), ThemeScheme::Light);
        // 跟随系统
        assert_eq!(of(&with_mode("system"), true), ThemeScheme::Dark);
        assert_eq!(of(&with_mode("system"), false), ThemeScheme::Light);
        // 未知取值与「条目存在但缺 colorMode」：前端按跟随系统解析
        assert_eq!(of(&with_mode("blue"), true), ThemeScheme::Dark);
        assert_eq!(of(&with_mode("blue"), false), ThemeScheme::Light);
        assert_eq!(
            of(r#"{"theme":"builtin.theme","themeSettings":{"builtin.theme":{}}}"#, false),
            ThemeScheme::Light
        );
        // 皮肤：变量自定义底色，深色兜底（含前端会忽略的陈旧变体值——比前端保守）；
        // 非字符串变体前端同样忽略 → 回落 colorMode
        assert_eq!(
            of(
                r#"{"theme":"builtin.theme","themeSettings":{"builtin.theme":{"colorMode":"light","variant":"aurora"}}}"#,
                false
            ),
            ThemeScheme::Dark
        );
        assert_eq!(
            of(
                r#"{"theme":"builtin.theme","themeSettings":{"builtin.theme":{"colorMode":"light","variant":"gone-skin"}}}"#,
                false
            ),
            ThemeScheme::Dark
        );
        assert_eq!(
            of(
                r#"{"theme":"builtin.theme","themeSettings":{"builtin.theme":{"colorMode":"light","variant":7}}}"#,
                false
            ),
            ThemeScheme::Light
        );
        // 自定义主题插件（含被停用后前端回退内置插件的情形）
        assert_eq!(
            of(
                r#"{"theme":"com.example.theme","themeSettings":{"com.example.theme":{"colorMode":"light"}}}"#,
                false
            ),
            ThemeScheme::Dark
        );
    }

    /// 组合用户层补丁（含 `null` 删键）= 装配输入变更，版本前移；其余补丁不动版本。
    /// 计数器是进程级全局静态：精确增量断言先取闸锁独占窗口（并行测试里其他 bump 源同持此锁）。
    #[test]
    fn patch_composition_patches_bumps_assembly_version() {
        let _gate = ASSEMBLY_TEST_GATE.lock().unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("atelyx-global-patch-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("global.json");

        let before = assembly_version();
        // 含 compositionPatches：版本 +1，补丁落进写后配置，读取带回新版本
        let read = patch_global_config_at(
            &path,
            &serde_json::json!({ "compositionPatches": { "builtin.search": "default" } }),
        )
        .unwrap();
        assert_eq!(read.assembly_version, before + 1, "含 compositionPatches 的补丁应前移版本");
        assert_eq!(
            read.config
                .composition_patches
                .as_ref()
                .and_then(|m| m.get("builtin.search").map(String::as_str)),
            Some("default"),
            "补丁应落进写后配置"
        );

        // `null` 删键同样是装配输入变更：版本 +1，键被移除
        let read = patch_global_config_at(&path, &serde_json::json!({ "compositionPatches": null })).unwrap();
        assert_eq!(read.assembly_version, before + 2, "null 删键也是装配变更");
        assert!(read.config.composition_patches.is_none(), "null 应删掉组合用户层");

        // 其余补丁不动版本
        let read = patch_global_config_at(&path, &serde_json::json!({ "theme": "com.other" })).unwrap();
        assert_eq!(read.assembly_version, before + 2, "不含 compositionPatches 的补丁不得前移版本");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
