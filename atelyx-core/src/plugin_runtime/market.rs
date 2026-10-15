//! 插件市场数据源：官方索引（CDN 静态 JSON）的读取、缓存与徽标判定；安装 = 插件仓库
//! GitHub Release 上的平台 zip 走既有安装链（解压防御由安装模块承担，本模块只管取数与落位）。
//! 纯解析与判定函数与网络路径分离，前者单测覆盖。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::net_guard::{client_for, ensure_public_http_url, ClientTimeout, HostPolicy};
use crate::updater;

/// 官方插件索引（CDN 直链，沿用官方索引仓库的发布形态）。
pub const INDEX_URL: &str = "https://cdn.jsdelivr.net/gh/Atelyx/Atelyx-plugin-index@main/index.json";
/// 精选插件表（`owner/repo` → 理由的映射；缺失或拉取失败按空表处理）。
pub const ENDORSED_URL: &str = "https://cdn.jsdelivr.net/gh/Atelyx/Atelyx-plugin-index@main/endorsed.json";
/// 官方账号名单：repo owner 命中即 official 徽标。
pub const OFFICIAL_PLUGIN_ORGS: &[&str] = &["Atelyx"];
/// 索引本地缓存有效期（毫秒）。
pub const INDEX_CACHE_MS: u64 = 6 * 60 * 60 * 1000;
/// 单次索引 / 发布信息请求的总超时（秒）。
const FETCH_TIMEOUT_SECS: u64 = 15;

/// 条目徽标：official 按 repo owner 命中官方名单；endorsed 严格按 `owner/repo` 命中精选表。
/// 不按自报 id 授予（id 可被伪造，仓库归属是背书事实），同 id 不同作者仓库不继承徽标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Badge {
    Official,
    Endorsed,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Official => "官方",
            Badge::Endorsed => "精选",
        }
    }
}

/// 索引条目（官方 `index.json` 的 `items[]`）。schema 与官方索引仓库对齐；
/// 缺失字段按缺省容错（索引是聚合产物，字段增减不应炸掉整页）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MarketEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `owner/repo`，Release 下载与更新定位。
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stars: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<Badge>,
}

/// 市场快照（内存与缓存形态）。`fetched_at` 为本机拉取时刻（unix 毫秒），过期判定用。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MarketSnapshot {
    pub generated_at: String,
    pub fetched_at: u64,
    pub items: Vec<MarketEntry>,
}

/// 某插件仓库的最新 Release 取数结果。`asset_url` 缺失 = 该 Release 没挂本平台的 zip
/// （调用方按「无安装包」呈现，不报错）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginRelease {
    /// tag 剥 `v`/`V` 前缀后的版本号。
    pub version: String,
    pub asset_url: Option<String>,
}

/// 解析官方索引：返回生成时间与条目列表。缺 `items` 视为空市场；单个条目形态不符
/// （字段类型错误等）跳过该条不炸整页——索引是聚合产物，坏一条不能拖垮其余合法条目。
pub fn parse_index(value: &Value) -> Result<(String, Vec<MarketEntry>), String> {
    let generated_at = value
        .get("generatedAt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| match serde_json::from_value::<MarketEntry>(item.clone()) {
                    Ok(entry) => Some(entry),
                    Err(e) => {
                        eprintln!("市场索引条目不可解析，已跳过：{e}");
                        None
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok((generated_at, items))
}

/// 解析精选表（`owner/repo` 键集合）；形态不符按空表处理。
pub fn parse_endorsed(value: &Value) -> HashSet<String> {
    value
        .as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// 徽标判定：owner 命中官方名单 = official；命中精选表 = endorsed。
pub fn badge_for(repo: &str, endorsed: &HashSet<String>) -> Option<Badge> {
    let owner = repo.split('/').next().unwrap_or_default();
    if OFFICIAL_PLUGIN_ORGS.contains(&owner) {
        return Some(Badge::Official);
    }
    endorsed.contains(repo).then_some(Badge::Endorsed)
}

/// 当前平台的 Release 资产标签。
pub fn platform_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x64"
    } else if cfg!(target_os = "linux") {
        "linux-x64"
    } else {
        "unknown"
    }
}

/// Release 资产命名约定：`<id>-<version>-<platform>.zip`。
pub fn asset_name(plugin_id: &str, version: &str) -> String {
    format!("{plugin_id}-{version}-{}.zip", platform_tag())
}

/// 缓存是否过期（无缓存时间戳 = 过期）。
pub fn is_stale(fetched_at: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(fetched_at) > INDEX_CACHE_MS
}

/// 读取市场缓存快照（启动秒开用）；缺失或损坏返回 `None`（缓存只是降级来源，坏即弃）。
pub fn read_cache(cache_path: &std::path::Path) -> Option<MarketSnapshot> {
    let raw = std::fs::read_to_string(cache_path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 拉取市场索引与精选表并落缓存。网络失败直接报错（由调用方提示），不回退缓存——
/// 缓存只作秒开来源，是否回退到缓存展示由壳决定。
pub async fn fetch_snapshot(cache_path: &std::path::Path) -> Result<MarketSnapshot, String> {
    let (generated_at, mut items) = fetch_json(INDEX_URL)
        .await
        .and_then(|value| parse_index(&value))
        .map_err(|e| format!("拉取插件索引失败：{e}"))?;
    let endorsed = match fetch_json(ENDORSED_URL).await {
        Ok(value) => parse_endorsed(&value),
        Err(_) => HashSet::new(),
    };
    for item in &mut items {
        item.badge = badge_for(&item.repo, &endorsed);
    }
    let snapshot = MarketSnapshot {
        generated_at,
        fetched_at: now_ms(),
        items,
    };
    // 缓存只是降级来源：写盘失败不否定已成功的网络拉取（留诊断输出，快照照常返回）
    if let Err(e) = write_cache(cache_path, &snapshot) {
        eprintln!("市场缓存写盘失败：{e}");
    }
    Ok(snapshot)
}

fn write_cache(cache_path: &std::path::Path, snapshot: &MarketSnapshot) -> Result<(), String> {
    let text = serde_json::to_string_pretty(snapshot)
        .map_err(|e| format!("序列化市场缓存失败：{e}"))?;
    crate::vault::atomic_write(cache_path, &text)
}

async fn fetch_json(url: &str) -> Result<Value, String> {
    let checked = ensure_public_http_url(url)?;
    let client = client_for(HostPolicy::PublicOnly, ClientTimeout::Total(FETCH_TIMEOUT_SECS))?;
    client
        .get(checked)
        .header("User-Agent", concat!("Atelyx/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| format!("{url}：{e}"))?
        .error_for_status()
        .map_err(|e| format!("{url}：{e}"))?
        .json()
        .await
        .map_err(|e| format!("{url}：{e}"))
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: Option<String>,
    assets: Option<Vec<GitHubAsset>>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
}

/// 查插件仓库的最新 Release：版本取 tag（剥 `v`/`V`），资产按命名约定匹配本平台 zip。
/// 无 Release、无本平台资产分别以可读结果区分（`asset_url` = `None`）。
pub async fn latest_plugin_release(repo: &str, plugin_id: &str) -> Result<PluginRelease, String> {
    let repo = repo.trim().trim_matches('/');
    if repo.split('/').count() != 2 || repo.is_empty() {
        return Err(format!("仓库地址非法：{repo}"));
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let checked = ensure_public_http_url(&url)?;
    let client = client_for(HostPolicy::PublicOnly, ClientTimeout::Total(FETCH_TIMEOUT_SECS))?;
    let release: GitHubRelease = client
        .get(checked)
        .header("User-Agent", concat!("Atelyx/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("查询插件发布信息失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("查询插件发布信息失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("查询插件发布信息失败：{e}"))?;

    let tag = release.tag_name.unwrap_or_default();
    let version = tag.trim().strip_prefix(['v', 'V']).unwrap_or(tag.trim()).to_string();
    if version.is_empty() {
        return Err(format!("插件仓库 {repo} 的最新发布缺少版本号"));
    }
    let expected = asset_name(plugin_id, &version);
    let asset_url = release
        .assets
        .unwrap_or_default()
        .into_iter()
        .find(|asset| {
            asset
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&expected))
        })
        .and_then(|asset| asset.browser_download_url);
    Ok(PluginRelease { version, asset_url })
}

/// 下载某插件最新 Release 的平台 zip 到 `dest_dir`（断点续传与取消由下载器承担），
/// 返回 zip 落盘路径。安装（解压防御 + 落位 + 启用标记）由调用方走 [`crate::plugin_runtime`] 的
/// 安装入口完成。
pub async fn download_release_zip(
    repo: &str,
    plugin_id: &str,
    dest_dir: &std::path::Path,
    cancel: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf, String> {
    let release = latest_plugin_release(repo, plugin_id).await?;
    download_release_asset(&release, plugin_id, dest_dir, cancel, on_progress).await
}

/// 下载一份已定位的 Release 资产（调用方先经 [`latest_plugin_release`] 查询并自行决策
/// ——如更新比对——再落下载，避免同一动作打两次 Release API）。
pub async fn download_release_asset(
    release: &PluginRelease,
    plugin_id: &str,
    dest_dir: &std::path::Path,
    cancel: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf, String> {
    let Some(url) = &release.asset_url else {
        return Err(format!(
            "插件发布 {} 未挂本平台安装包（{}）",
            release.version,
            asset_name(plugin_id, &release.version)
        ));
    };
    updater::download(dest_dir, url, None, cancel, on_progress).await
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_index_reads_items_and_tolerates_missing() {
        let (generated_at, items) = parse_index(&json!({
            "generatedAt": "2026-10-14T17:13:19.904Z",
            "version": "v1",
            "items": [
                { "id": "com.atelyx.demo", "name": "Demo", "repo": "Atelyx/demo", "stars": 2 },
                { "id": "com.x.third", "name": "Third", "repo": "third/plugin" }
            ]
        }))
        .unwrap();
        assert_eq!(generated_at, "2026-10-14T17:13:19.904Z");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].repo, "Atelyx/demo");
        assert_eq!(items[0].stars, Some(2));
        // 缺 items / 空对象都按空市场，不报错
        assert!(parse_index(&json!({})).unwrap().1.is_empty());
    }

    #[test]
    fn badge_uses_repo_ownership_not_self_reported_id() {
        let endorsed: HashSet<String> = ["third/plugin".to_string()].into();
        assert_eq!(badge_for("Atelyx/demo", &endorsed), Some(Badge::Official));
        assert_eq!(badge_for("third/plugin", &endorsed), Some(Badge::Endorsed));
        assert_eq!(badge_for("random/dev", &endorsed), None);
        // 同 id 不同作者仓库不继承徽标（判定只认 repo）
        assert_eq!(badge_for("impersonator/demo", &endorsed), None);
    }

    #[test]
    fn asset_name_follows_platform_convention() {
        let name = asset_name("com.atelyx.demo", "1.2.0");
        if cfg!(target_os = "windows") {
            assert_eq!(name, "com.atelyx.demo-1.2.0-windows-x64.zip");
        } else {
            assert_eq!(name, "com.atelyx.demo-1.2.0-linux-x64.zip");
        }
    }

    #[test]
    fn cache_staleness_and_roundtrip() {
        let now = 1_000_000_000_000u64;
        assert!(is_stale(now - INDEX_CACHE_MS - 1, now));
        assert!(!is_stale(now - INDEX_CACHE_MS, now));
        let snapshot = MarketSnapshot {
            generated_at: "t".into(),
            fetched_at: now,
            items: vec![MarketEntry {
                id: "com.a".into(),
                name: "A".into(),
                repo: "Atelyx/a".into(),
                badge: Some(Badge::Official),
                ..MarketEntry::default()
            }],
        };
        let dir = crate::vault::test_support::TempDir::new("market-cache");
        let path = dir.join("market-cache.json");
        assert!(read_cache(&path).is_none(), "缓存缺失返回 None");
        write_cache(&path, &snapshot).unwrap();
        assert_eq!(read_cache(&path).unwrap(), snapshot);
        // 损坏缓存即弃，不报错
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_cache(&path).is_none());
    }

    #[tokio::test]
    async fn latest_plugin_release_rejects_bad_repo() {
        assert!(latest_plugin_release("", "com.a").await.is_err());
        assert!(latest_plugin_release("not-a-repo", "com.a").await.is_err());
    }
}
