//! 联网搜索代理命令。
//!
//! 搜索统一走 Rust 侧请求，理由：
//! - **SearXNG**：自建实例无内置 CORS 支持，浏览器/WebView 前端直 fetch 必被
//!   `Access-Control-Allow-Origin` 拦截（官方架构即「放反代后面」）；Rust 代理天然绕过。
//! - **Tavily**：官方 best practice 明确 API key 不应暴露在客户端代码；key 优先从仓库
//!   `config.json` 的 `search.tavilyApiKey` 读取（`syncKeys` 开启时随仓库落盘、多设备共享），
//!   为空则回退 keychain 条目 `provider-<vaultId>-search-tavily`（默认模式，按仓库隔离）；不落 WebView。
//!
//! 边界捕获：网络/HTTP 错误返回 Err，前端 `runSearch` 降级为 `SearchResultData.error`
//! （失败降级不阻塞对话，）。
//! 地址校验：Tavily 走公网策略、SearXNG 走本机/局域网策略（自建实例常在本机或局域网），
//! 两套策略与重定向逐跳复检见 `net_guard`。

use reqwest::Url;
use serde::Serialize;
use tauri::State;

use crate::net_guard::{ensure_local_service_http_url, ensure_public_http_url, redirect_policy};
use crate::vault::{read_vault_config, VaultConfig, VaultState};

/// 单条搜索结果（camelCase 对齐前端 `SearchResultItem`）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 执行搜索（Tavily / SearXNG，按 provider 分发）。key 与 URL 均由 Rust 侧处理。
#[tauri::command]
pub async fn search_web(
    state: State<'_, VaultState>,
    provider: String,
    query: String,
    searxng_url: Option<String>,
) -> Result<Vec<SearchResultItem>, String> {
    let root = state.root()?;
    // 仓库配置读一次：Tavily key（syncKeys 开启时随仓库落盘）与 vault_id（keychain 回退用）
    let config = read_vault_config(&root)?;
    match provider.as_str() {
        "tavily" => tavily_search(&config, &query).await,
        "searxng" => searxng_search(searxng_url.unwrap_or_default().as_str(), &query).await,
        other => Err(format!("未知搜索源：{}", other)),
    }
}

/// 带超时的 HTTP 客户端（搜索请求不被挂死；15s 对搜索 API 足够）。
/// `check` = 重定向每跳复检的地址策略（Tavily 公网 / SearXNG 本机局域网）。
fn http_client(check: fn(&str) -> Result<Url, String>) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(redirect_policy(check))
        .build()
        .map_err(|e| e.to_string())
}

async fn tavily_search(config: &VaultConfig, query: &str) -> Result<Vec<SearchResultItem>, String> {
    let key = get_tavily_key(config)?;
    if key.is_empty() {
        return Err("未配置 Tavily API Key（工作区「设置」→ 联网搜索）".to_string());
    }
    let client = http_client(ensure_public_http_url)?;
    let resp = client
        .post("https://api.tavily.com/search")
        .header("Authorization", format!("Bearer {}", key))
        .json(&serde_json::json!({ "query": query, "max_results": 5 }))
        .send()
        .await
        .map_err(|e| format!("Tavily 请求失败：{}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Tavily 请求失败：HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parse_results(json.get("results")))
}

async fn searxng_search(instance_url: &str, query: &str) -> Result<Vec<SearchResultItem>, String> {
    let url = searxng_request_url(instance_url, query)?;
    let client = http_client(ensure_local_service_http_url)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("SearXNG 请求失败：{}", e))?;
    if !resp.status().is_success() {
        // 403 = 实例未启用 json 格式（settings.yml 的 search.formats 需加入 json）
        return Err(format!(
            "SearXNG 请求失败：HTTP {}（若 403，需在实例 settings.yml 的 search.formats 加入 json）",
            resp.status()
        ));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parse_results(json.get("results")))
}

/// 解析 SearXNG/Tavily 统一的 results 数组（字段：url/title/content）。
fn parse_results(results: Option<&serde_json::Value>) -> Vec<SearchResultItem> {
    results
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let url = item
                        .get("url")
                        .and_then(|u| u.as_str())
                        .unwrap_or("")
                        .to_string();
                    if url.is_empty() {
                        return None;
                    }
                    Some(SearchResultItem {
                        title: item
                            .get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        url,
                        snippet: item
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 文件内 Tavily key：仅 `syncKeys` 开启（key 随仓库落盘多设备共享）时可采用。关闭时 key 只存
/// 本机 keychain，文件内残留（跨设备同步/手工编辑遗留）不得参与取用——否则用户在设置里换 key
/// （关闭态只写 keychain）后，搜索仍会拿文件里的旧 key。
fn file_tavily_key(config: &VaultConfig) -> Option<&str> {
    if config.sync_keys != Some(true) {
        return None;
    }
    config
        .search
        .as_ref()
        .and_then(|s| s.tavily_api_key.as_deref())
        .filter(|k| !k.is_empty())
}

/// 读 Tavily key：`syncKeys` 开启时取仓库 config.json 的 `search.tavilyApiKey`；
/// 否则取 keychain 条目 `provider-<vaultId>-search-tavily`（默认模式，按仓库隔离）。
fn get_tavily_key(config: &VaultConfig) -> Result<String, String> {
    if let Some(k) = file_tavily_key(config) {
        return Ok(k.to_string());
    }
    let vault_id = config.vault_id.as_deref().unwrap_or_default();
    // keychain 读写统一走 keychain 模块（同 service/username 拼法、NoEntry → 空串语义一致）
    crate::commands::keychain::get_api_key(vault_id.to_string(), "search-tavily".to_string())
}

/// 构造 SearXNG 查询 URL（实例地址先过本机/局域网策略，再拼 `/search` 与查询串）。
fn searxng_request_url(instance_url: &str, query: &str) -> Result<Url, String> {
    let base = instance_url.trim();
    if base.is_empty() {
        return Err("未配置 SearXNG 实例 URL（工作区「设置」→ 联网搜索）".to_string());
    }
    // 地址策略（放行本机/局域网，拒链路本地等）与协议校验统一在 net_guard
    let mut url = ensure_local_service_http_url(base)?;
    // 保留实例自身子路径（`http://host/searx/` → `/searx/search`），查询串按标准表单编码
    url.set_path(&format!("{}/search", url.path().trim_end_matches('/')));
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "json");
    Ok(url)
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::vault::VaultSearchConfig;

    #[test]
    fn searxng_url_builds_path_and_query() {
        let url = searxng_request_url("http://127.0.0.1:8080", "a b&c=1").unwrap();
        assert_eq!(url.path(), "/search");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(8080));
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("q".to_string(), "a b&c=1".to_string()),
                ("format".to_string(), "json".to_string())
            ]
        );
    }

    #[test]
    fn searxng_url_keeps_instance_subpath() {
        let url = searxng_request_url("http://192.168.1.10:8888/searx/", "q").unwrap();
        assert_eq!(url.path(), "/searx/search");
    }

    #[test]
    fn searxng_url_rejects_missing_or_blocked_instance() {
        assert!(searxng_request_url("", "q").is_err());
        assert!(searxng_request_url("   ", "q").is_err());
        // 本机/局域网策略仍拒链路本地（云元数据）与非 http 协议
        assert!(searxng_request_url("http://169.254.169.254/", "q").is_err());
        assert!(searxng_request_url("ftp://192.168.1.10/", "q").is_err());
        // 未带协议：报错而非拼出无效 URL
        assert!(searxng_request_url("192.168.1.10:8080", "q").is_err());
    }

    /// 构造只带搜索 key / syncKeys 的配置（其余字段走默认）。
    fn config_with(sync_keys: Option<bool>, tavily_api_key: Option<&str>) -> VaultConfig {
        VaultConfig {
            sync_keys,
            search: Some(VaultSearchConfig {
                tavily_api_key: tavily_api_key.map(|k| k.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn file_tavily_key_requires_sync_keys() {
        // 关闭/缺省 syncKeys：文件内 key 不参与取用（key 只在 keychain）
        assert_eq!(file_tavily_key(&config_with(Some(false), Some("tvly-file"))), None);
        assert_eq!(file_tavily_key(&config_with(None, Some("tvly-file"))), None);
        // 开启：文件内 key 即真相
        assert_eq!(
            file_tavily_key(&config_with(Some(true), Some("tvly-file"))),
            Some("tvly-file")
        );
        // 空串/无 search 段视为未配置
        assert_eq!(file_tavily_key(&config_with(Some(true), Some(""))), None);
        assert_eq!(
            file_tavily_key(&VaultConfig {
                sync_keys: Some(true),
                ..Default::default()
            }),
            None
        );
    }
}
