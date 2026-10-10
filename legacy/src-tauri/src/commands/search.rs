//! 命令适配层：联网搜索代理（Tavily / SearXNG）实现在 atelyx-core，此处保持 Tauri 命令面。

/// 执行搜索（Tavily / SearXNG，按 provider 分发）。key 与实例地址由前端配置层传值、
/// Rust 侧只做请求与地址校验（不读仓库配置，本地与协作空间同一路径）。
#[tauri::command]
pub async fn search_web(
    provider: String,
    query: String,
    searxng_url: Option<String>,
    tavily_key: Option<String>,
) -> Result<Vec<atelyx_core::search::SearchResultItem>, String> {
    atelyx_core::search::search_web(provider, query, searxng_url, tavily_key).await
}
