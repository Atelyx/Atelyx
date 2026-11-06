//! `http` 能力面：插件出网请求（便利层）。宿主不施加额外限制——目标地址 / 方法 / 响应大小 / 超时全
//! 由插件自负其责（插件本是任意权限原生进程，本面不构成安全边界）；请求体与响应体走 base64，二进制
//! 不被文本编解码损坏。宿主在插件进程的 stdout 读线程上同步执行（见 `instance`），故用阻塞客户端。

use std::time::Duration;

use atelyx_plugin_protocol::method;

pub(crate) fn request(params: &method::HttpRequestParams) -> Result<method::HttpResult, String> {
    let url = parse_url(&params.url)?;
    let request_method = reqwest::Method::from_bytes(params.method.as_bytes())
        .map_err(|e| format!("请求方法无效：{e}"))?;
    let mut client = reqwest::blocking::Client::builder();
    // 超时由调用方经 `timeoutMs` 声明；未声明即不设上限（宿主不给缺省值）
    if let Some(ms) = params.timeout_ms {
        client = client.timeout(Duration::from_millis(ms));
    }
    let client = client.build().map_err(|e| format!("客户端初始化失败：{e}"))?;
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
    use std::io::Read;
    let mut body: Vec<u8> = Vec::new();
    response
        .read_to_end(&mut body)
        .map_err(|e| format!("读取响应失败：{e}"))?;
    use base64::Engine;
    Ok(method::HttpResult {
        status,
        headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
    })
}

/// 解析目标 URL：非 http/https 与非法 URL 给可读错误——reqwest 本身只支持这两种 scheme，
/// 这里只是把错误讲清楚，不是地址策略。
fn parse_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("URL 解析失败：{e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("仅支持 http/https 网址".to_string());
    }
    Ok(url)
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
    fn base64_body_decode_failure_is_readable() {
        let err = decode_base64("不是 base64").unwrap_err();
        assert!(err.contains("base64"), "{err}");
    }

    #[test]
    fn url_accepts_http_https_and_rejects_other_schemes() {
        assert!(parse_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(parse_url("https://example.com/").is_ok());
        assert!(parse_url("ftp://example.com").is_err());
        assert!(parse_url("不是网址").is_err());
    }
}
