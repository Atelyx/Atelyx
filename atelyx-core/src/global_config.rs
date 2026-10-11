//! 应用级全局配置（应用数据目录 `global.json`）：跨仓库共享的最近仓库列表。
//!
//! 读写口径与仓库配置一致：读路径解析失败先备份原文再降级为空，写路径原子覆盖整文件。

use crate::vault::{atomic_write, backup_corrupt_config};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 最近打开仓库列表上限。
pub const MAX_RECENT_VAULTS: usize = 10;

/// 最近打开的仓库（文件面板列表层展示）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentVault {
    /// 仓库根绝对路径。
    pub root: String,
    /// 仓库名（文件夹名）。
    pub name: String,
    /// 最近打开时间（unix 秒）。
    pub last_opened_at: u64,
}

/// 全局配置（`global.json`）：应用级、跨仓库共享；缺字段走默认值，未知字段忽略。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalConfig {
    /// 最近打开的仓库列表（最近打开倒序）。
    pub recent_vaults: Vec<RecentVault>,
}

impl GlobalConfig {
    /// 登记某仓库为最近打开：按 root 去重 → 置顶 → 更新时间 → 截断上限。
    pub fn bump_vault(&mut self, root: &str, name: &str, now_sec: u64) {
        self.recent_vaults.retain(|v| v.root != root);
        self.recent_vaults.insert(
            0,
            RecentVault {
                root: root.to_string(),
                name: name.to_string(),
                last_opened_at: now_sec,
            },
        );
        self.recent_vaults.truncate(MAX_RECENT_VAULTS);
    }

    /// 从最近列表移除某仓库（按 root）；不影响磁盘文件与激活态。
    pub fn remove_vault(&mut self, root: &str) {
        self.recent_vaults.retain(|v| v.root != root);
    }
}

/// 读全局配置：文件缺失回落空配置；解析失败先备份原文再降级为空，并回传备份文件名。
///
/// 为什么读路径必须备份：本配置走「读 → 改 → 写」整文件覆盖，读到的空配置会被写回磁盘，
/// 等于一次外部编辑/磁盘异常就静默清空最近仓库列表。备份失败即报错（与仓库配置同口径）——
/// 留不下原文还按空配置继续，调用方紧接着就会用空配置覆盖它。
pub fn read_global_config(path: &Path) -> Result<(GlobalConfig, Option<String>), String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((GlobalConfig::default(), None))
        }
        Err(e) => return Err(format!("读取全局配置失败：{e}")),
    };
    match serde_json::from_str::<GlobalConfig>(&raw) {
        Ok(config) => Ok((config, None)),
        Err(e) => {
            let Some(backup) = backup_corrupt_config(path, "global") else {
                return Err(format!(
                    "全局配置已损坏且原文备份失败（{}），已中止读取以免覆盖原文：{e}",
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

/// 写全局配置（原子覆盖整文件）。
pub fn write_global_config(path: &Path, config: &GlobalConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    atomic_write(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    #[test]
    fn bump_dedupes_promotes_and_truncates() {
        let mut cfg = GlobalConfig::default();
        for i in 0..MAX_RECENT_VAULTS + 2 {
            cfg.bump_vault(&format!("/v{i}"), &format!("v{i}"), 100 + i as u64);
        }
        // 超上限：只留最近 MAX_RECENT_VAULTS 个，且最新一个在首位
        assert_eq!(cfg.recent_vaults.len(), MAX_RECENT_VAULTS);
        assert_eq!(cfg.recent_vaults[0].root, format!("/v{}", MAX_RECENT_VAULTS + 1));
        // 重开旧仓库：去重后置顶并更新时间，总数不增
        cfg.bump_vault("/v5", "v5-renamed", 999);
        assert_eq!(cfg.recent_vaults.len(), MAX_RECENT_VAULTS);
        assert_eq!(cfg.recent_vaults[0].root, "/v5");
        assert_eq!(cfg.recent_vaults[0].name, "v5-renamed");
        assert_eq!(cfg.recent_vaults[0].last_opened_at, 999);
    }

    #[test]
    fn remove_vault_drops_only_target() {
        let mut cfg = GlobalConfig::default();
        cfg.bump_vault("/a", "a", 1);
        cfg.bump_vault("/b", "b", 2);
        cfg.remove_vault("/a");
        assert_eq!(cfg.recent_vaults.len(), 1);
        assert_eq!(cfg.recent_vaults[0].root, "/b");
        // 移除不存在的路径是空操作
        cfg.remove_vault("/missing");
        assert_eq!(cfg.recent_vaults.len(), 1);
    }

    #[test]
    fn serde_roundtrip_ignores_unknown_and_defaults_missing() {
        let json = r#"{"recentVaults":[{"root":"/a","name":"a","lastOpenedAt":7}],"theme":"dark"}"#;
        let cfg: GlobalConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.recent_vaults[0].root, "/a");
        // 未知字段被忽略，写回只含已知字段
        let out = serde_json::to_string(&cfg).unwrap();
        assert!(!out.contains("theme"));
        assert!(out.contains("lastOpenedAt"));
        // 缺 recentVaults 走默认空
        let cfg: GlobalConfig = serde_json::from_str("{}").unwrap();
        assert!(cfg.recent_vaults.is_empty());
    }

    #[test]
    fn read_missing_returns_empty() {
        let tmp = TempDir::new("global-missing");
        let (cfg, backup) = read_global_config(&tmp.join("global.json")).unwrap();
        assert!(cfg.recent_vaults.is_empty());
        assert!(backup.is_none());
    }

    #[test]
    fn read_corrupt_backs_up_original_then_empties() {
        let tmp = TempDir::new("global-corrupt");
        let path = tmp.join("global.json");
        std::fs::write(&path, "{ not json").unwrap();
        let (cfg, backup) = read_global_config(&path).unwrap();
        assert!(cfg.recent_vaults.is_empty());
        let backup = backup.expect("损坏原文应留档");
        // 原文已改名留档（原路径不再存在），备份内容与损坏原文一致
        assert!(!path.exists());
        let content = std::fs::read_to_string(tmp.join(&backup)).unwrap();
        assert_eq!(content, "{ not json");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new("global-roundtrip");
        let path = tmp.join("global.json");
        let mut cfg = GlobalConfig::default();
        cfg.bump_vault("/a", "a", 3);
        write_global_config(&path, &cfg).unwrap();
        let (loaded, backup) = read_global_config(&path).unwrap();
        assert!(backup.is_none());
        assert_eq!(loaded.recent_vaults, cfg.recent_vaults);
    }
}