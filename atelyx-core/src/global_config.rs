//! 应用级全局配置（应用数据目录 `global.json`）：跨仓库共享的最近仓库列表。
//!
//! 读写口径与仓库配置一致：读路径解析失败先备份原文再降级为空，写路径原子覆盖整文件。

use crate::vault::{atomic_write, backup_corrupt_config};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 最近打开仓库列表上限。
pub const MAX_RECENT_VAULTS: usize = 10;

/// 最近打开空间列表上限。
pub const MAX_RECENT_SPACES: usize = 10;

/// 登录过的服务器清单上限（与最近仓库/空间同量级口径）。
pub const MAX_SPACE_SERVERS: usize = 10;

/// 界面基准字号缺省（px）。
const DEFAULT_FONT_SIZE: u32 = 16;
/// 正文行宽缺省（px；0 = 不限）。
const DEFAULT_CONTENT_WIDTH: u32 = 780;

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

/// 登录过的协作服务器（仅地址；令牌在系统密钥链）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpaceServer {
    /// 服务器地址（含协议与端口）。
    pub url: String,
}

/// 最近打开的协作空间（与本地仓库在文件面板平级列出）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentSpace {
    /// 所属服务器地址。
    pub server_url: String,
    pub space_id: String,
    pub name: String,
    /// 最近打开时间（unix 秒）。
    pub last_opened_at: u64,
}

/// 最后一次激活的身份（启动自动进入的依据；令牌不在本结构）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LastIdentity {
    pub server_url: String,
    pub username: String,
    pub display_name: String,
}

/// 身份指向：启动自动进入取「最近仓库 vs 最近空间」中较近者。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum LastActive {
    Vault { root: String },
    Space { server_url: String, space_id: String },
}

/// 全局配置（`global.json`）：应用级、跨仓库共享；缺字段按 `Default` 补齐，未知字段忽略。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalConfig {
    /// 最近打开的仓库列表（最近打开倒序）。
    pub recent_vaults: Vec<RecentVault>,
    /// 登录过的协作服务器。
    pub space_servers: Vec<SpaceServer>,
    /// 最近打开的协作空间（最近打开倒序）。
    pub recent_spaces: Vec<RecentSpace>,
    /// 最后一次登录的账号身份（单服务器口径：重登覆盖）。
    pub last_identity: Option<LastIdentity>,
    /// 最后一次激活的仓库 / 空间（启动自动进入依据；缺省回落最近列表首位）。
    pub last_active: Option<LastActive>,
    /// 协作展示色（远端光标 / 名单；首次登录时随机指派，应用级共享）。
    pub collab_color: Option<String>,
    /// 本机稳定设备 id（首次登录时生成；服务端据此合并同账号会话）。
    pub device_id: Option<String>,
    /// 界面基准字号（px，12–20）。
    pub font_size: u32,
    /// 界面字体族名（取自系统已安装字体；None = 跟随系统 UI 字体）。
    pub font_family: Option<String>,
    /// 启动恢复上次打开的文件。
    pub auto_restore: bool,
    /// 启动静默检查更新。
    pub auto_update: bool,
    /// 宽松换行：段内单个换行渲染为换行（关闭 = 按 Markdown 标准需空行换行）。
    pub soft_wrap: bool,
    /// 页面内标题：正文顶部显示文件名标题。
    pub in_page_title: bool,
    /// 正文行宽上限（px；0 = 不限，随面板铺满）。
    pub content_width: u32,
    /// 命令键位覆盖（键 = 命令 id，值 = 键串；缺项用默认表）。
    #[serde(default)]
    pub command_shortcuts: HashMap<String, String>,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            recent_vaults: Vec::new(),
            space_servers: Vec::new(),
            recent_spaces: Vec::new(),
            last_identity: None,
            last_active: None,
            collab_color: None,
            device_id: None,
            font_size: DEFAULT_FONT_SIZE,
            font_family: None,
            auto_restore: true,
            auto_update: false,
            soft_wrap: true,
            in_page_title: false,
            content_width: DEFAULT_CONTENT_WIDTH,
            command_shortcuts: HashMap::new(),
        }
    }
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
        self.last_active = Some(LastActive::Vault { root: root.to_string() });
    }

    /// 从最近列表移除某仓库（按 root）；不影响磁盘文件与激活态。
    pub fn remove_vault(&mut self, root: &str) {
        self.recent_vaults.retain(|v| v.root != root);
    }

    /// 登记服务器（按 url 去重置顶，截断上限）。
    pub fn bump_server(&mut self, url: &str) {
        self.space_servers.retain(|s| s.url != url);
        self.space_servers.insert(0, SpaceServer { url: url.to_string() });
        self.space_servers.truncate(MAX_SPACE_SERVERS);
    }

    /// 移除服务器及其全部最近空间（登出 / 从列表移除时调用）。
    pub fn remove_server(&mut self, url: &str) {
        self.space_servers.retain(|s| s.url != url);
        self.recent_spaces.retain(|s| s.server_url != url);
        if self.last_identity.as_ref().is_some_and(|i| i.server_url == url) {
            self.last_identity = None;
        }
        if self
            .last_active
            .as_ref()
            .is_some_and(|a| matches!(a, LastActive::Space { server_url, .. } if server_url == url))
        {
            self.last_active = None;
        }
    }

    /// 登记某空间为最近打开：按 (server, spaceId) 去重 → 置顶 → 更新时间 → 截断上限。
    pub fn bump_space(&mut self, server_url: &str, space_id: &str, name: &str, now_sec: u64) {
        self.recent_spaces
            .retain(|s| !(s.server_url == server_url && s.space_id == space_id));
        self.recent_spaces.insert(
            0,
            RecentSpace {
                server_url: server_url.to_string(),
                space_id: space_id.to_string(),
                name: name.to_string(),
                last_opened_at: now_sec,
            },
        );
        self.recent_spaces.truncate(MAX_RECENT_SPACES);
        self.last_active = Some(LastActive::Space {
            server_url: server_url.to_string(),
            space_id: space_id.to_string(),
        });
    }

    /// 从最近列表移除某空间。
    pub fn remove_space(&mut self, server_url: &str, space_id: &str) {
        self.recent_spaces
            .retain(|s| !(s.server_url == server_url && s.space_id == space_id));
    }

    /// 记录登录身份。
    pub fn set_identity(&mut self, server_url: &str, username: &str, display_name: &str) {
        self.last_identity = Some(LastIdentity {
            server_url: server_url.to_string(),
            username: username.to_string(),
            display_name: display_name.to_string(),
        });
        self.bump_server(server_url);
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
        // 缺 recentVaults 走默认空；应用级偏好按 Default 补齐（字号/行宽档位、开关态）
        let cfg: GlobalConfig = serde_json::from_str("{}").unwrap();
        assert!(cfg.recent_vaults.is_empty());
        assert_eq!(cfg.font_size, 16);
        assert_eq!(cfg.font_family, None);
        assert!(cfg.auto_restore);
        assert!(!cfg.auto_update);
        assert!(cfg.soft_wrap);
        assert!(!cfg.in_page_title);
        assert_eq!(cfg.content_width, 780);
    }

    #[test]
    fn 界面字体族按驼峰键往返() {
        let cfg: GlobalConfig = serde_json::from_str(r#"{"fontFamily":"Microsoft YaHei"}"#).unwrap();
        assert_eq!(cfg.font_family.as_deref(), Some("Microsoft YaHei"));
        let out = serde_json::to_string(&cfg).unwrap();
        assert!(out.contains(r#""fontFamily":"Microsoft YaHei""#));
        let back: GlobalConfig = serde_json::from_str(&out).unwrap();
        assert_eq!(back.font_family, cfg.font_family);
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
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn 空间条目去重置顶且记录最后激活() {
        let mut cfg = GlobalConfig::default();
        cfg.bump_space("http://s1", "sp1", "空间一", 10);
        cfg.bump_space("http://s1", "sp2", "空间二", 20);
        cfg.bump_space("http://s2", "sp1", "另服同名", 30);
        assert_eq!(cfg.recent_spaces.len(), 3);
        assert_eq!(cfg.recent_spaces[0].server_url, "http://s2");
        // 重开置顶并更新
        cfg.bump_space("http://s1", "sp1", "空间一", 40);
        assert_eq!(cfg.recent_spaces.len(), 3);
        assert_eq!(cfg.recent_spaces[0].space_id, "sp1");
        assert_eq!(cfg.recent_spaces[0].last_opened_at, 40);
        assert_eq!(
            cfg.last_active,
            Some(LastActive::Space { server_url: "http://s1".into(), space_id: "sp1".into() })
        );
        cfg.remove_space("http://s1", "sp1");
        assert_eq!(cfg.recent_spaces.len(), 2);
    }

    #[test]
    fn 服务器与身份联动清理() {
        let mut cfg = GlobalConfig::default();
        cfg.set_identity("http://s1", "alice", "Alice");
        cfg.bump_space("http://s1", "sp1", "空间一", 10);
        cfg.bump_vault("/v", "v", 5);
        assert_eq!(cfg.space_servers.len(), 1);
        // 切仓库不清理服务器身份
        assert!(matches!(cfg.last_active, Some(LastActive::Vault { .. })));
        cfg.bump_space("http://s1", "sp1", "空间一", 20);
        cfg.remove_server("http://s1");
        assert!(cfg.space_servers.is_empty() && cfg.recent_spaces.is_empty());
        assert!(cfg.last_identity.is_none() && cfg.last_active.is_none());
    }

    #[test]
    fn 空间字段序列化向后兼容() {
        // 旧格式（仅 recentVaults）可读，新字段走默认
        let cfg: GlobalConfig =
            serde_json::from_str(r#"{"recentVaults":[{"root":"/a","name":"a","lastOpenedAt":7}]}"#).unwrap();
        assert!(cfg.space_servers.is_empty() && cfg.recent_spaces.is_empty());
        // 新字段写入后可往返
        let mut cfg = GlobalConfig::default();
        cfg.set_identity("http://s", "u", "d");
        cfg.bump_space("http://s", "sp", "n", 1);
        let out = serde_json::to_string(&cfg).unwrap();
        let back: GlobalConfig = serde_json::from_str(&out).unwrap();
        assert_eq!(back, cfg);
    }
}
