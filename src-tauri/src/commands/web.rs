//! 网页抓取代理命令（`fetch_web`）+ 通用 HTTP 请求命令（`http_request`，插件 `ctx.http` 面）。
//!
//! 抓取由 Rust 侧执行，理由与搜索代理一致：浏览器/WebView 前端直 fetch 受 CORS 限制，
//! 且便于统一超时与大小上限。`fetch_web` 返回 `title` + 正文纯文本，供 AI `fetch_url` 工具
//! 回填上下文做回答依据；`http_request` 返回状态码 + 响应头 + 原样文本体，供插件访问 HTTP API。
//!
//! 边界捕获：非 http/https 拒绝、内网/回环地址拒绝（SSRF 防护）、方法白名单外拒绝、
//! 网络/HTTP 错误返回 Err，前端降级为错误文本。

use std::collections::HashMap;

use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::Url;
use serde::{Deserialize, Serialize};

/// 抓取响应上限（字节）。防超大页面/二进制拖死请求，超出即截断。
const MAX_RESPONSE_BYTES: usize = 1_000_000;
/// 回填文本的字符上限（防一条 tool 消息撑爆上下文）。
const MAX_TEXT_CHARS: usize = 20_000;
/// 允许的 HTTP 方法（白名单：其余一律拒绝，防 CONNECT/TRACE 等非预期语义）。
const ALLOWED_METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];
/// 最多跟随的重定向跳数（每跳都重新过公网校验，见 redirect_policy）。
const MAX_REDIRECTS: usize = 10;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedWebPage {
    pub url: String,
    pub title: Option<String>,
    pub content: String,
    /// 正文是否命中 MAX_TEXT_CHARS 被截断（模型据此刻画内容规模/决定是否抓子页）。
    pub truncated: bool,
}

/// 通用 HTTP 请求输入。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    pub url: String,
    /// 缺省 GET（白名单见 ALLOWED_METHODS）。
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    /// 请求体（GET/HEAD 也可带，由调用方自行决定）。
    pub body: Option<String>,
}

/// 通用 HTTP 响应（正文为文本；非 UTF-8 用替换字符容错）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    /// 正文是否命中 MAX_RESPONSE_BYTES 被截断。
    pub truncated: bool,
}

/// 抓取网页正文（`https://`/`http://`）。
#[tauri::command]
pub async fn fetch_web(url: String) -> Result<FetchedWebPage, String> {
    let parsed = ensure_public_http_url(&url)?;
    // 重定向每跳复检（默认策略会默默跟随 302 到内网地址，绕过入口校验）
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(redirect_policy())
        .build()
        .map_err(|e| format!("客户端初始化失败：{}", e))?;
    let resp = client
        .get(parsed)
        .header(USER_AGENT, "Mozilla/5.0 (compatible; AtelyxWebFetch/1.0)")
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|e| format!("抓取失败：{}", e))?;
    if !resp.status().is_success() {
        return Err(format!("抓取失败：HTTP {}", resp.status()));
    }
    let (bytes, _) = read_capped(resp, MAX_RESPONSE_BYTES).await?;
    // 非 UTF-8（如部分 GBK 页）用替换字符容错，不阻塞
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let title = extract_title(&html);
    let (content, truncated) = truncate_chars(html_to_text(&html).trim(), MAX_TEXT_CHARS);
    Ok(FetchedWebPage {
        url,
        title,
        content,
        truncated,
    })
}

/// 通用 HTTP 请求（插件 `ctx.http`）：方法白名单 + 统一超时/响应上限，响应头与正文原样返回。
#[tauri::command]
pub async fn http_request(req: HttpRequest) -> Result<HttpResponse, String> {
    let url = ensure_public_http_url(&req.url)?;
    let method = normalize_method(req.method.as_deref())?;
    // 重定向每跳复检（默认策略会默默跟随 302 到内网地址，绕过入口校验）
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(redirect_policy())
        .build()
        .map_err(|e| format!("客户端初始化失败：{}", e))?;
    let mut builder = client.request(method, url);
    for (name, value) in req.headers.unwrap_or_default() {
        builder = builder.header(name, value);
    }
    if let Some(body) = req.body {
        builder = builder.body(body);
    }
    let resp = builder.send().await.map_err(|e| format!("请求失败：{}", e))?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect();
    let (bytes, truncated) = read_capped(resp, MAX_RESPONSE_BYTES).await?;
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok(HttpResponse {
        status,
        headers,
        body,
        truncated,
    })
}

/// 边收边计读到上限即停：避免把超大响应整包读进内存（`bytes()` 先全量下载再截断，
/// 上限形同虚设——2GB 响应会在超时前把内存打满）。返回值第二项 = 是否被上限截断。
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<(Vec<u8>, bool), String> {
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取响应失败：{e}"))? {
        let room = cap - out.len();
        if chunk.len() > room {
            out.extend_from_slice(&chunk[..room]);
            return Ok((out, true));
        }
        out.extend_from_slice(&chunk);
    }
    Ok((out, false))
}

/// 重定向是否放行：跳数在上限内且目标通过公网校验。
fn redirect_allowed(hops: usize, url: &str) -> bool {
    hops < MAX_REDIRECTS && ensure_public_http_url(url).is_ok()
}

/// 重定向策略：每跳重新做公网校验（被拒目标即停止，把 3xx 响应原样交回调用方，不再向该地址发请求）。
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if redirect_allowed(attempt.previous().len(), attempt.url().as_str()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// 请求方法白名单校验 + 归一化（缺省 GET）。
fn normalize_method(raw: Option<&str>) -> Result<reqwest::Method, String> {
    let method = raw.unwrap_or("GET").to_uppercase();
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return Err(format!("不支持的请求方法：{method}"));
    }
    reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| format!("请求方法无效：{e}"))
}

/// http/https 校验 + SSRF 防护（拒绝内网/回环/链路本地地址），返回解析后的 URL。
///
/// 限制：域名 + DNS rebinding（解析后将内网 IP 返回）场景未做全量地址验证，仍封堵 IP 字面量主通路。
fn ensure_public_http_url(raw: &str) -> Result<Url, String> {
    if !raw.starts_with("https://") && !raw.starts_with("http://") {
        return Err("仅支持 http/https 网址".to_string());
    }
    let parsed = Url::parse(raw).map_err(|e| format!("URL 解析失败：{e}"))?;
    let host = parsed.host_str().ok_or_else(|| "无效 URL host".to_string())?;
    // IPv6 字面量在 URL 里带方括号（可带 zone id，如 `[fe80::1%25eth0]`）：剥掉后再按 IpAddr 解析，
    // 否则 `[::1]` 这类地址解析失败、绕过下面的内网判定。
    let zone = host.find('%').unwrap_or(host.len());
    let bare = host[..zone].trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") {
        return Err("拒绝访问 localhost（SSRF 防护）".to_string());
    }
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        let blocked = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                // 封堵回环/未指定/唯一本地(ULA fc00::/7)/link-local(fe80::/10)/IPv4-mapped(可伪装内网 v4)
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    || v6.to_ipv4_mapped().is_some()
            }
        };
        if blocked {
            return Err("拒绝访问内网/回环/链路本地地址（SSRF 防护）".to_string());
        }
    }
    Ok(parsed)
}

/// 提取 `<title>` 内容（去标签、去空白）。基于 ASCII 小写做字节定位（长度不变，安全）。
fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let gt = lower[start..].find('>')? + start + 1;
    let end = lower[gt..].find("</title")? + gt;
    let inner = strip_tags(&html[gt..end]);
    let t = collapse_ws(&decode_entities(&inner));
    if t.is_empty() {
        None
    } else {
        Some(limit_chars(&t, 200))
    }
}

/// 块级/换行标签：在闭标签处插入换行。
const NEWLINE_OPEN: [&str; 13] = [
    "p", "div", "br", "li", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "section", "article",
];

/// HTML → 纯文本：剥掉 script/style（含其内容），块级标签换行，去其余标签，解码实体并折叠空白。
fn html_to_text(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(html.len());
    let mut in_special = false;
    let mut i = 0;
    while i < n {
        if chars[i] == '<' {
            let mut j = i + 1;
            while j < n && chars[j] != '>' {
                j += 1;
            }
            if j >= n {
                break; // 未闭合标签：丢弃剩余
            }
            let seg: String = chars[i + 1..j].iter().collect();
            let segl = seg.to_ascii_lowercase();
            let closing = segl.starts_with('/');
            let name: String = segl
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if in_special {
                if closing && (name == "script" || name == "style") {
                    in_special = false;
                }
                i = j + 1;
                continue;
            }
            if (name == "script" || name == "style") && !closing {
                in_special = true;
                i = j + 1;
                continue;
            }
            // 常规标签：块级开标签给换行（防整页糊成一行）
            if !closing && NEWLINE_OPEN.contains(&name.as_str()) {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            i = j + 1;
            continue;
        }
        if !in_special {
            out.push(chars[i]);
        }
        i += 1;
    }
    collapse_ws(&decode_entities(&out))
}

/// 剥掉所有 `<...>` 标签（title 片段等场景用）。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// 解码常见 HTML 实体。
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let mut matched = false;
        for (k, v) in [
            ("&nbsp;", " "),
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#39;", "'"),
        ] {
            if rest.starts_with(k) {
                out.push_str(v);
                rest = &rest[k.len()..];
                matched = true;
                break;
            }
        }
        if !matched {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 折叠连续空白为单空格。
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space && !out.ends_with('\n') {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

/// 按字符截断（超长加省略号）；返回是否被截断（模型据此判断内容完整性）。
fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    let count = s.chars().count();
    if count <= max {
        (s.to_string(), false)
    } else {
        let head: String = s.chars().take(max).collect();
        (format!("{head}…"), true)
    }
}

/// 按字符截断（超长加省略号，title 等短片段用；不关心截断标志）。
fn limit_chars(s: &str, max: usize) -> String {
    truncate_chars(s, max).0
}

#[cfg(test)]
mod web_tests {
    use super::*;

    #[test]
    fn method_whitelist_allows_common_verbs_and_defaults_to_get() {
        assert!(normalize_method(None).is_ok());
        assert_eq!(normalize_method(Some("post")).unwrap(), reqwest::Method::POST);
        assert!(normalize_method(Some("get")).is_ok());
        // 白名单外拒绝（含 CONNECT/TRACE 等非预期语义）。
        assert!(normalize_method(Some("TRACE")).is_err());
        assert!(normalize_method(Some("CONNECT")).is_err());
        assert!(normalize_method(Some("")).is_err());
    }

    #[test]
    fn url_guard_blocks_non_http_and_internal_targets() {
        assert!(ensure_public_http_url("https://example.com/a").is_ok());
        assert!(ensure_public_http_url("http://example.com/a").is_ok());
        // 协议限制。
        assert!(ensure_public_http_url("ftp://example.com/a").is_err());
        assert!(ensure_public_http_url("file:///etc/passwd").is_err());
        // SSRF：回环/私网/链路本地/IPv6 回环与别名。
        assert!(ensure_public_http_url("http://127.0.0.1:8080/").is_err());
        assert!(ensure_public_http_url("http://localhost/").is_err());
        assert!(ensure_public_http_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(ensure_public_http_url("http://10.0.0.5/").is_err());
        assert!(ensure_public_http_url("http://[::1]/").is_err());
    }

    #[test]
    fn redirect_targets_are_rechecked_per_hop() {
        // 公网目标放行（含 http→https 之类正常跳转）。
        assert!(redirect_allowed(0, "https://example.com/a"));
        assert!(redirect_allowed(3, "http://example.com/a"));
        // 跳向内网/回环/超跳数一律不放行（302 到内网是绕过入口校验的主通路）。
        assert!(!redirect_allowed(0, "http://127.0.0.1:7701/"));
        assert!(!redirect_allowed(0, "http://169.254.169.254/latest/meta-data"));
        assert!(!redirect_allowed(0, "http://[::1]/"));
        assert!(!redirect_allowed(0, "ftp://example.com/"));
        assert!(!redirect_allowed(MAX_REDIRECTS, "https://example.com/a"));
    }

    #[test]
    fn truncate_chars_marks_overflow() {
        let (s, t) = truncate_chars("abcdef", 5);
        assert!(t);
        assert_eq!(s, "abcde…");
        let (s2, t2) = truncate_chars("abcdef", 10);
        assert!(!t2);
        assert_eq!(s2, "abcdef");
        let (s3, t3) = truncate_chars("", 10);
        assert!(!t3);
        assert_eq!(s3, "");
        // 恰好等于上限：不截断
        let (s4, t4) = truncate_chars("abcde", 5);
        assert!(!t4);
        assert_eq!(s4, "abcde");
    }

    #[test]
    fn truncate_chars_keeps_multibyte_chars() {
        // 中文/emoji 按码点截断，不产生半个字符
        let (s, t) = truncate_chars("中文标题很长", 3);
        assert!(t);
        assert_eq!(s, "中文标…");
        let (s2, t2) = truncate_chars("a😀b", 2);
        assert!(t2);
        assert_eq!(s2, "a😀…");
        let (s3, t3) = truncate_chars("😀😀", 2);
        assert!(!t3);
        assert_eq!(s3, "😀😀");
    }
}
