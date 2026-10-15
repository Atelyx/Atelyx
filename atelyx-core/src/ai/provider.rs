//! AI 供应商（OpenAI 兼容）端点连通性测试：GET `{baseUrl}/models` 校验地址与凭据可用性。

use crate::net_guard::{self, ClientTimeout};

/// 测试供应商端点连通性（`key` 非空时带 `Authorization: Bearer` 头）。
/// 地址策略按主机形态选择（见 `net_guard::policy_for_configured_url`），
/// 入口校验、重定向每跳与 DNS 解析同策略。Err = 地址非法或网络/HTTP 失败（不含 key）。
/// 端点连通性测试（GET {baseUrl}/models）。成功返回耗时毫秒（UI 展示用）。
pub async fn test_endpoint(base_url: &str, api_key: &str) -> Result<u128, String> {
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
    Ok(started.elapsed().as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_base_url_rejected() {
        assert!(test_endpoint("   ", "").await.is_err());
    }
}
