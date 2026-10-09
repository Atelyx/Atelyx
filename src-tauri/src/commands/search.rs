//! 联网搜索代理命令：Tavily / SearXNG 统一走 Rust 侧请求。
//! SearXNG 自建实例无内置 CORS，WebView 直 fetch 必被拦（官方架构即「放反代后面」），Rust 代理天然绕过。
//! Tavily key 的落点与取值统一由前端配置层处理——本模块不读仓库配置、不依赖本地仓库根（协作空间无 root）。

use reqwest::Url;
use serde::Serialize;

use crate::net_guard::{client_for, ensure_local_service_http_url, ClientTimeout, HostPolicy};

/// 单条搜索结果（camelCase 对齐前端 `SearchResultItem`）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 执行搜索（Tavily / SearXNG，按 provider 分发）。key 与实例地址由前端配置层传值、
/// Rust 侧只做请求与地址校验（不读仓库配置，本地与协作空间同一路径）。
/// Err = 网络/HTTP/地址校验失败，前端 `runSearch` 降级为 `SearchResultData.error`（不阻塞对话）；
/// 地址策略（Tavily 公网 / SearXNG 本机局域网，重定向逐跳复检）见 `net_guard`。
#[tauri::command]
pub async fn search_web(
    provider: String,
    query: String,
    searxng_url: Option<String>,
    tavily_key: Option<String>,
) -> Result<Vec<SearchResultItem>, String> {
    match provider.as_str() {
        "tavily" => tavily_search(tavily_key.unwrap_or_default().as_str(), &query).await,
        "searxng" => searxng_search(searxng_url.unwrap_or_default().as_str(), &query).await,
        other => Err(format!("未知搜索源：{}", other)),
    }
}

/// 带总超时的策略守卫客户端（搜索请求不被挂死；15s 对搜索 API 足够）。
fn http_client(policy: HostPolicy) -> Result<reqwest::Client, String> {
    client_for(policy, ClientTimeout::Total(15))
}

async fn tavily_search(key: &str, query: &str) -> Result<Vec<SearchResultItem>, String> {
    if key.is_empty() {
        return Err("未配置 Tavily API Key（工作区「设置」→ 联网搜索）".to_string());
    }
    let client = http_client(HostPolicy::PublicOnly)?;
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
    let client = http_client(HostPolicy::LocalService)?;
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

    #[test]
    fn tavily_search_rejects_empty_key() {
        // key 由前端传入；空 key 明确报错而非发出无凭据请求
        let err = match tauri::async_runtime::block_on(tavily_search("", "q")) {
            Err(e) => e,
            Ok(_) => panic!("空 key 应报错"),
        };
        assert!(err.contains("未配置 Tavily API Key"), "{err}");
    }
}
