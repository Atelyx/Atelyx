//! AI 供应商（OpenAI 兼容）端点探测：GET `{baseUrl}/models` 校验地址与凭据可用性。

use crate::net_guard::{self, ClientTimeout};

/// 拉取供应商模型列表并测速（同一端点）：成功返回排序后的模型 id 与耗时毫秒。
/// `key` 非空时带 `Authorization: Bearer` 头；地址策略按主机形态选择
/// （见 `net_guard::policy_for_configured_url`），入口校验、重定向每跳与 DNS 解析同策略。
/// Err = 地址非法或网络/HTTP 失败（不含 key）。
pub async fn list_models(base_url: &str, api_key: &str) -> Result<(Vec<String>, u128), String> {
    let started = std::time::Instant::now();
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("未配置 baseUrl".to_string());
    }
    let url = format!("{base}/models");
    let policy = net_guard::policy_for_configured_url(&url);
    let parsed = net_guard::ensure_http_url(&url, policy)?;
    let client = net_guard::client_for(policy, ClientTimeout::Total(15))?;
    let mut request = client.get(parsed);
    if !api_key.is_empty() {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }
    let resp = request.send().await.map_err(|e| format!("请求失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("响应不是有效的模型列表：{e}"))?;
    let mut ids: Vec<String> = body
        .data
        .into_iter()
        .filter_map(|m| m.id)
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    Ok((ids, started.elapsed().as_millis()))
}

/// 测试供应商端点连通性（成功返回耗时毫秒；模型列表丢弃）。
pub async fn test_endpoint(base_url: &str, api_key: &str) -> Result<u128, String> {
    list_models(base_url, api_key).await.map(|(_, ms)| ms)
}

#[derive(serde::Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelsResponseItem>,
}

#[derive(serde::Deserialize)]
struct ModelsResponseItem {
    /// 半兼容网关可能返回缺 id 的条目：按缺项跳过，不因单条脏数据毁掉整表。
    #[serde(default)]
    id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_base_url_rejected() {
        assert!(test_endpoint("   ", "").await.is_err());
    }
}
