//! 网页抓取代理命令（`fetch_web`，返回 title + 正文纯文本供 AI `fetch_url` 工具回填）。
//! 由 Rust 侧执行：统一超时与响应上限；地址走公网策略（见 `net_guard`，防提示注入诱导抓内网）。
//! 失败一律 Err（非 http/https、地址被拒、网络/HTTP 错误），由调用方降级为错误文本。

use reqwest::header::{ACCEPT, USER_AGENT};

use serde::Serialize;

use crate::net_guard::{client_for, ensure_public_http_url, ClientTimeout, HostPolicy};

/// 抓取响应上限（字节）。防超大页面/二进制拖死请求，超出即截断。
const MAX_RESPONSE_BYTES: usize = 1_000_000;
/// 回填文本的字符上限（防一条 tool 消息撑爆上下文）。
const MAX_TEXT_CHARS: usize = 20_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedWebPage {
    pub url: String,
    pub title: Option<String>,
    pub content: String,
    /// 正文是否命中 MAX_TEXT_CHARS 被截断（模型据此刻画内容规模/决定是否抓子页）。
    pub truncated: bool,
}

/// 抓取网页正文（`https://`/`http://`）。
pub async fn fetch_web(url: String) -> Result<FetchedWebPage, String> {
    let parsed = ensure_public_http_url(&url)?;
    let client = client_for(HostPolicy::PublicOnly, ClientTimeout::Total(20))?;
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
            if !closing
                && NEWLINE_OPEN.contains(&name.as_str())
                && !out.is_empty()
                && !out.ends_with('\n')
            {
                out.push('\n');
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
