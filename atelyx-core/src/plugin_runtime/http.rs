//! `http` 能力面：插件出网请求。地址一律走本机 / 局域网策略（回环与私网放行、云元数据与链路本地仍拒），
//! 方法白名单与响应上限同其他出网面；请求体与响应体走 base64，二进制不被文本编解码损坏。
//! 宿主在插件进程的 stdout 读线程上同步执行（见 `instance` 的分派），故用阻塞客户端。

use atelyx_plugin_protocol::method;

use crate::net_guard::{self, HostPolicy};

/// 响应体上限（字节）：超出即报错，不做截断——静默截断会把损坏的二进制交给插件，而结果形态里
/// 没有截断标记可让插件察觉。
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// 缺省总超时（秒）：调用方可经 `timeoutMs` 覆盖，仍受上限约束。
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 600;

pub(crate) fn request(params: &method::HttpRequestParams) -> Result<method::HttpResult, String> {
    let url = net_guard::ensure_local_service_http_url(&params.url)?;
    let request_method = crate::web::normalize_method(Some(&params.method))?;
    let client = net_guard::blocking_client_for(
        HostPolicy::LocalService,
        timeout_secs(params.timeout_ms),
    )?;
    let mut request = client.request(request_method, url);
    for (name, value) in params.headers.iter().flatten() {
        request = request.header(name, value);
    }
    if let Some(body) = &params.body_base64 {
        request = request.body(decode_base64(body)?);
    }
    let mut response = request.send().map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect();
    let body = read_capped(&mut response)?;
    use base64::Engine;
    Ok(method::HttpResult {
        status,
        headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
    })
}

/// `timeoutMs` → 秒（缺省 30，至少 1 秒，封顶 [`MAX_TIMEOUT_SECS`]）。
fn timeout_secs(timeout_ms: Option<u64>) -> u64 {
    timeout_ms
        .map(|ms| (ms / 1000).max(1))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .min(MAX_TIMEOUT_SECS)
}

/// 边读边计到上限即停；超限报错（不截断，见 [`MAX_RESPONSE_BYTES`]）。
fn read_capped(response: &mut reqwest::blocking::Response) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = response.read(&mut buf).map_err(|e| format!("读取响应失败：{e}"))?;
        if read == 0 {
            return Ok(out);
        }
        if out.len() + read > MAX_RESPONSE_BYTES {
            return Err(format!(
                "响应体超过上限（{} MiB）",
                MAX_RESPONSE_BYTES / (1024 * 1024)
            ));
        }
        out.extend_from_slice(&buf[..read]);
    }
}

fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| format!("请求体 base64 解码失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_clamped_and_bounded() {
        assert_eq!(timeout_secs(None), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some(0)), 1, "不足 1 秒按 1 秒");
        assert_eq!(timeout_secs(Some(2500)), 2);
        assert_eq!(timeout_secs(Some(u64::MAX)), MAX_TIMEOUT_SECS);
    }

    #[test]
    fn base64_body_decode_failure_is_readable() {
        let err = decode_base64("不是 base64").unwrap_err();
        assert!(err.contains("base64"), "{err}");
    }

    #[test]
    fn loopback_targets_pass_the_local_policy() {
        // 本机 / 局域网策略是插件 http 的既定口径：回环放行，云元数据仍拒
        assert!(net_guard::ensure_local_service_http_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(net_guard::ensure_local_service_http_url("http://localhost:8080/").is_ok());
        assert!(net_guard::ensure_local_service_http_url("http://169.254.169.254/").is_err());
        assert!(net_guard::ensure_local_service_http_url("ftp://example.com").is_err());
    }
}
