//! 插件市场数据源：官方索引（CDN 静态 JSON）的读取、缓存与徽标判定；安装 = 插件仓库
//! GitHub Release 上的平台 zip 走既有安装链（解压防御由安装模块承担，本模块只管取数与落位）。
//! 纯解析与判定函数与网络路径分离，前者单测覆盖。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::net_guard::{client_for, ensure_http_url, ClientTimeout, HostPolicy};
use crate::updater;

/// 官方插件索引（CDN 直链，沿用官方索引仓库的发布形态）。
pub const INDEX_URL: &str = "https://cdn.jsdelivr.net/gh/Atelyx/Atelyx-plugin-index@main/index.json";
/// Release 查询的 GitHub API 根（其后拼 `repos/<owner>/<repo>/releases/latest`）。
const RELEASE_API_BASE: &str = "https://api.github.com";
/// 官方账号名单：repo owner 命中即 official 徽标。
pub const OFFICIAL_PLUGIN_ORGS: &[&str] = &["Atelyx"];
/// 索引本地缓存有效期（毫秒）。
pub const INDEX_CACHE_MS: u64 = 6 * 60 * 60 * 1000;
/// 单次索引 / 发布信息请求的总超时（秒）。
const FETCH_TIMEOUT_SECS: u64 = 15;

/// 条目徽标：official 按 repo owner 命中官方名单。不按自报 id 授予（id 可被伪造，
/// 仓库归属是背书事实），同 id 不同作者仓库不继承徽标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Badge {
    Official,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Official => "官方",
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
    /// 索引自报的插件类型（取值由索引侧定义，如 `panel` / `tableview`）；索引缺该字段即无类型。
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
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
/// （调用方按「无安装包」呈现，不报错）。`sha256` 为资产摘要（资产未提供摘要时 `None`）。
#[derive(Clone, Debug, PartialEq)]
pub struct PluginRelease {
    /// tag 剥 `v`/`V` 前缀后的版本号。
    pub version: String,
    pub asset_url: Option<String>,
    pub sha256: Option<String>,
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

/// 徽标判定：owner 命中官方名单即 official，其余无徽标。
pub fn badge_for(repo: &str) -> Option<Badge> {
    let owner = repo.split('/').next().unwrap_or_default();
    OFFICIAL_PLUGIN_ORGS.contains(&owner).then_some(Badge::Official)
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

/// 拉取市场索引并落缓存。网络失败直接报错（由调用方提示），不回退缓存——
/// 缓存只作秒开来源，是否回退到缓存展示由壳决定。
pub async fn fetch_snapshot(cache_path: &std::path::Path) -> Result<MarketSnapshot, String> {
    fetch_snapshot_from(INDEX_URL, cache_path, HostPolicy::PublicOnly).await
}

/// 从给定索引地址拉取（策略随地址用途选择：官方 CDN 走公网，本地验收源走本机/局域网）。
async fn fetch_snapshot_from(
    index_url: &str,
    cache_path: &std::path::Path,
    policy: HostPolicy,
) -> Result<MarketSnapshot, String> {
    let (generated_at, mut items) = fetch_json(index_url, policy)
        .await
        .and_then(|value| parse_index(&value))
        .map_err(|e| format!("拉取插件索引失败：{e}"))?;
    for item in &mut items {
        item.badge = badge_for(&item.repo);
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

async fn fetch_json(url: &str, policy: HostPolicy) -> Result<Value, String> {
    let checked = ensure_http_url(url, policy)?;
    let client = client_for(policy, ClientTimeout::Total(FETCH_TIMEOUT_SECS))?;
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
    /// `sha256:<hex>` 形态的资产摘要（旧资产可能没有）。
    digest: Option<String>,
}

/// 查插件仓库的最新 Release：版本取 tag（剥 `v`/`V`），资产按命名约定匹配本平台 zip。
/// 无 Release、无本平台资产分别以可读结果区分（`asset_url` = `None`）。
pub async fn latest_plugin_release(repo: &str, plugin_id: &str) -> Result<PluginRelease, String> {
    latest_plugin_release_from(RELEASE_API_BASE, repo, plugin_id, HostPolicy::PublicOnly).await
}

/// 从给定 API 根查询（策略随源地址选择，用法同 [`fetch_snapshot_from`]）。
async fn latest_plugin_release_from(
    api_base: &str,
    repo: &str,
    plugin_id: &str,
    policy: HostPolicy,
) -> Result<PluginRelease, String> {
    let repo = repo.trim().trim_matches('/');
    if repo.split('/').count() != 2 || repo.is_empty() {
        return Err(format!("仓库地址非法：{repo}"));
    }
    let url = format!("{}/repos/{repo}/releases/latest", api_base.trim_end_matches('/'));
    let checked = ensure_http_url(&url, policy)?;
    let client = client_for(policy, ClientTimeout::Total(FETCH_TIMEOUT_SECS))?;
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
    let asset = release
        .assets
        .unwrap_or_default()
        .into_iter()
        .find(|asset| {
            asset
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&expected))
        });
    let (asset_url, sha256) = match asset {
        Some(asset) => (asset.browser_download_url, digest_sha256(asset.digest.as_deref())),
        None => (None, None),
    };
    Ok(PluginRelease { version, asset_url, sha256 })
}

/// GitHub 资产摘要（`sha256:<hex>`）转成纯 hex；形态不符按「无摘要」处理（下载器跳过校验）。
fn digest_sha256(digest: Option<&str>) -> Option<String> {
    let hex = digest?.trim().strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
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
    updater::download(dest_dir, url, release.sha256.as_deref(), cancel, on_progress).await
}

/// 当前 unix 毫秒时刻（缓存新鲜度判定用）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Mutex;

    use serde_json::json;
    use sha2::Digest;

    use crate::plugin_runtime::PluginRuntime;

    #[test]
    fn parse_index_reads_items_and_tolerates_missing() {
        let (generated_at, items) = parse_index(&json!({
            "generatedAt": "2026-10-14T17:13:19.904Z",
            "version": "v1",
            "items": [
                { "id": "com.atelyx.demo", "name": "Demo", "repo": "Atelyx/demo", "stars": 2, "type": "panel" },
                { "id": "com.x.third", "name": "Third", "repo": "third/plugin" }
            ]
        }))
        .unwrap();
        assert_eq!(generated_at, "2026-10-14T17:13:19.904Z");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].repo, "Atelyx/demo");
        assert_eq!(items[0].stars, Some(2));
        assert_eq!(items[0].r#type.as_deref(), Some("panel"), "索引 type 字段映射到插件类型");
        assert_eq!(items[1].r#type, None, "缺 type 即无类型");
        // 索引的 type 字段名与序列化口径一致（回写快照不丢类型）
        let encoded = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(encoded["type"], "panel");
        // 缺 items / 空对象都按空市场，不报错
        assert!(parse_index(&json!({})).unwrap().1.is_empty());
    }

    #[test]
    fn badge_uses_repo_ownership_not_self_reported_id() {
        assert_eq!(badge_for("Atelyx/demo"), Some(Badge::Official));
        assert_eq!(badge_for("third/plugin"), None);
        // 同 id 不同作者仓库不继承徽标（判定只认 repo）
        assert_eq!(badge_for("impersonator/demo"), None);
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
    fn digest_is_normalized_and_malformed_is_ignored() {
        let hex = "A".repeat(64);
        assert_eq!(digest_sha256(Some(&format!("sha256:{hex}"))), Some("a".repeat(64)));
        // 形态不符 / 缺失一律按无摘要（下载器跳过校验，不阻断安装）
        assert_eq!(digest_sha256(Some("sha512:abcd")), None);
        assert_eq!(digest_sha256(Some("sha256:xyz")), None);
        assert_eq!(digest_sha256(None), None);
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

    // ---------- 本地真实源端到端：取索引 → 下载 → sha256 → 包内 id → 安装 → 发现新版本 → 更新 ----------

    /// 市场源替身：一个按路径返回固定内容的本机 HTTP 服务，内容可替换以模拟发新版。
    /// 只解析请求行与终止符；请求走 `HostPolicy::LocalService`（真实安装链的地址校验照常执行）。
    struct SourceServer {
        base: String,
        routes: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for SourceServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
        }
    }

    impl SourceServer {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let routes = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let thread = {
                let routes = Arc::clone(&routes);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    listener.set_nonblocking(true).unwrap();
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => serve_once(stream, &routes),
                            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Err(_) => break,
                        }
                    }
                })
            };
            Self {
                base: format!("http://{addr}"),
                routes,
                stop,
                thread: Some(thread),
            }
        }

        fn route(&self, path: &str, body: Vec<u8>) {
            self.routes.lock().unwrap().insert(path.to_string(), body);
        }

        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
    }

    fn serve_once(
        mut stream: std::net::TcpStream,
        routes: &Mutex<std::collections::HashMap<String, Vec<u8>>>,
    ) {
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let path = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_string();
        let body = routes.lock().unwrap().get(&path).cloned();
        let response = match body {
            Some(body) => format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes()
            .into_iter()
            .chain(body)
            .collect::<Vec<_>>(),
            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        };
        stream.write_all(&response).ok();
        stream.flush().ok();
    }

    const ACCEPT_ID: &str = "com.example.demo";
    const ACCEPT_REPO: &str = "local/demo";

    fn manifest_json(id: &str, version: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"Demo","version":"{version}","apiVersion":1,"entry":{{"windows":"app.exe","linux":"app"}}}}"#
        )
    }

    fn build_zip(path: &std::path::Path, id: &str, version: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let entry = if cfg!(windows) { "app.exe" } else { "app" };
        writer
            .start_file(atelyx_plugin_protocol::manifest::MANIFEST_FILE, options)
            .unwrap();
        writer.write_all(manifest_json(id, version).as_bytes()).unwrap();
        writer.start_file(entry, options).unwrap();
        writer.write_all(b"stub").unwrap();
        writer.finish().unwrap();
    }

    /// 发布一个版本的索引 / Release / 资产（资产字节留在 `tmp` 下，RAII 清理随临时根走）。
    fn publish(server: &SourceServer, tmp: &std::path::Path, version: &str) {
        let zip_path = tmp.join(format!("source-{version}.zip"));
        build_zip(&zip_path, ACCEPT_ID, version);
        let bytes = std::fs::read(&zip_path).unwrap();
        let asset = asset_name(ACCEPT_ID, version);
        let asset_path = format!("/download/{asset}");
        server.route(&asset_path, bytes.clone());
        let release = serde_json::json!({
            "tag_name": format!("v{version}"),
            "assets": [{
                "name": asset,
                "browser_download_url": server.url(&asset_path),
                "digest": format!("sha256:{}", hex::encode(sha2::Sha256::digest(&bytes))),
            }]
        });
        server.route(
            &format!("/repos/{ACCEPT_REPO}/releases/latest"),
            serde_json::to_vec(&release).unwrap(),
        );
        let index = serde_json::json!({
            "generatedAt": "2026-11-04T00:00:00.000Z",
            "items": [{ "id": ACCEPT_ID, "name": "Demo", "repo": ACCEPT_REPO }]
        });
        server.route("/index.json", serde_json::to_vec(&index).unwrap());
    }

    async fn download_asset(release: &PluginRelease, dir: &std::path::Path) -> Result<PathBuf, String> {
        let url = release
            .asset_url
            .as_deref()
            .ok_or_else(|| "发布未挂本平台资产".to_string())?;
        updater::download_via(
            reqwest::Client::new(),
            reqwest::Url::parse(url).map_err(|e| e.to_string())?,
            dir,
            release.sha256.as_deref(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(|_, _| {}),
        )
        .await
    }

    /// 完整链路：本地真实 HTTP 源 + 真 zip；索引落缓存、sha256 生效、包内 id 拒收、安装落位、
    /// 发布新版后检测为可更新并覆盖安装。
    #[tokio::test]
    async fn local_source_runs_the_whole_market_chain() {
        use std::cmp::Ordering;

        let tmp = crate::scratch::TempDir::new("market-chain");
        let server = SourceServer::start();
        publish(&server, &tmp, "1.0.0");

        // 取索引（与生产同一份 fetch + parse + 落缓存）
        let cache = tmp.join("market-cache.json");
        let snapshot = fetch_snapshot_from(&server.url("/index.json"), &cache, HostPolicy::LocalService)
            .await
            .unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].id, ACCEPT_ID);
        assert_eq!(read_cache(&cache).unwrap(), snapshot, "索引快照应已落缓存");

        let release =
            latest_plugin_release_from(&server.base, ACCEPT_REPO, ACCEPT_ID, HostPolicy::LocalService)
                .await
                .unwrap();
        assert_eq!(release.version, "1.0.0");
        assert!(release.sha256.is_some(), "资产摘要应随 Release 取到");

        let runtime = PluginRuntime::new(tmp.join("plugins"));
        let downloads = tmp.join("downloads");
        let zip = download_asset(&release, &downloads).await.unwrap();

        // 包内清单 id 必须与市场条目一致，否则拒收且不动已装内容
        let foreign = tmp.join("foreign.zip");
        build_zip(&foreign, "com.example.other", "9.9.9");
        let error = runtime.install_zip(&foreign, Some(ACCEPT_ID)).unwrap_err();
        assert!(error.contains("不一致"), "{error}");

        runtime.install_zip(&zip, Some(ACCEPT_ID)).unwrap();
        let installed = runtime
            .list()
            .into_iter()
            .find(|plugin| plugin.id == ACCEPT_ID)
            .unwrap();
        assert_eq!(installed.version, "1.0.0");

        // 发新版：取数 → 版本比对判为可更新 → 下载 → 覆盖安装
        publish(&server, &tmp, "1.1.0");
        let next =
            latest_plugin_release_from(&server.base, ACCEPT_REPO, ACCEPT_ID, HostPolicy::LocalService)
                .await
                .unwrap();
        assert_eq!(next.version, "1.1.0");
        assert_eq!(
            updater::compare_versions("1.0.0", &next.version),
            Ordering::Less,
            "应判定为可更新"
        );

        // 摘要校验确实生效：同一资产被替换而摘要不变，下载必须失败
        let tampered_path = format!("/download/{}", asset_name(ACCEPT_ID, "1.1.0"));
        server.route(&tampered_path, b"tampered".to_vec());
        let error = download_asset(&next, &downloads).await.unwrap_err();
        assert!(error.contains("摘要不匹配"), "{error}");
        publish(&server, &tmp, "1.1.0");

        let zip = download_asset(&next, &downloads).await.unwrap();
        runtime.install_zip(&zip, Some(ACCEPT_ID)).unwrap();
        let updated = runtime
            .list()
            .into_iter()
            .find(|plugin| plugin.id == ACCEPT_ID)
            .unwrap();
        assert_eq!(updated.version, "1.1.0");
    }
}
