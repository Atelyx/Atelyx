//! 全局配置命令（应用级数据）。
//!
//! 读写 `app_data_dir/global.json`：**应用级配置**——最近打开仓库列表 + 自动更新开关 +
//! 界面外观（主题/强调色/字号/字体）+ 自动恢复上次打开文件。
//! AI 供应商 / 搜索源已仓库化（`vault.rs` 的 `VaultConfig.providers/search`），
//! 不再由本文件承载；API key 永不落文件（仅存 keychain，见 `commands/keychain.rs`）——
//! 旧 `ai`/`search`/`device_id` 字段读回时被 serde 忽略（未知字段），不再写回。
//!
//! 另有 `app_data_dir/ui-state.json`（应用级 UI 使用状态：工作区布局 + 上次打开文件 +
//! 文件面板展开；本机独有、不随仓库同步，由 `crate::layout` 迷你窗口管理器单一写者
//! 承载，见 `layout.rs`）。
//!
//! 整文件读写 + 原子写（写 `.tmp` → rename）。前端用 `updateGlobalConfig`（read-modify-write）
//! 而非直接 `write_global_config`，避免覆盖其他字段。

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

/// 全局配置根结构（**应用级**：最近仓库列表 + 自动更新开关 + 界面外观（主题/强调色/字号/字体）+
/// 自动恢复上次打开文件；旧 ai/search/deviceId 字段读回时被忽略）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    #[serde(default)]
    pub recent_vaults: Vec<RecentVault>,
    /// 自动检查更新（应用级）：开启后每次启动应用静默检查新版本并自动安装。缺省 None = 关闭。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// 应用级主题："light" | "dark" | "system"。缺失时前端默认 "dark"。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// 应用级强调色（hex，如 "#d4af37"；缺省 = 默认金色）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// 应用级界面基础字号（px，覆盖 :root font-size；缺省 = 18）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    /// 应用级界面字体（CSS font-family，缺省 = system-ui 默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// 进入仓库时自动恢复上次打开的文件。缺省 None = true（前端默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_restore_files: Option<bool>,
    /// 协作中转（collab-relay）开关（应用级）。缺省 None = 关闭。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_enabled: Option<bool>,
    /// 协作中转地址（如 `ws://192.168.1.10:17701/ws`）。缺省 None = 未配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_relay_url: Option<String>,
    /// 协作显示昵称（空 = 设备名兜底）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_nickname: Option<String>,
    /// 协作身份色（hex；空 = 随机分配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collab_color: Option<String>,
    /// 进入仓库时自动切到「主页」布局。缺省 None = false（保持恢复上次界面）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_home_layout: Option<bool>,
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

/// 读全局配置（文件不存在或解析失败时返回空配置，不报错——允许首次启动/手编辑损坏）。
/// 返回前对 recentVaults 归一化去重，兼容旧版本写入的 `\\?\` 前缀脏数据。
#[tauri::command]
pub fn read_global_config(app: AppHandle) -> Result<GlobalConfig, String> {
    let path = global_config_path(&app)?;
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut config = match serde_json::from_str::<GlobalConfig>(&data) {
        Ok(c) => c,
        Err(_) => GlobalConfig::default(),
    };
    config.recent_vaults = normalize_and_dedupe_vaults(config.recent_vaults);
    Ok(config)
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
