//! 出网 http/https 地址校验（`commands/web.rs` 与 `commands/search.rs` 共用）。
//!
//! 两套策略，同一次校验只能择一（见 [`HostPolicy`]）：网页抓取与插件通用 HTTP 走公网策略，
//! 用户自建的 SearXNG 等本机/局域网服务走 [`HostPolicy::LocalService`]。
//! 重定向每跳都按同一策略复检：默认重定向策略会默默跟随 302 到被拒地址，绕过入口校验。
//!
//! 限制：域名 + DNS rebinding（解析后将内网 IP 返回）场景未做全量地址验证，两套策略都
//! 只封堵 IP 字面量通路。

use reqwest::Url;

/// 地址用途策略。
#[derive(Clone, Copy)]
pub(crate) enum HostPolicy {
    /// 公网目标：回环/私网/链路本地/未指定/广播/ULA 一律拒绝（网页抓取与插件通用 HTTP 的边界）。
    PublicOnly,
    /// 本机或局域网服务：放行回环与私网/ULA，仅拒链路本地（含云元数据 169.254.169.254）、
    /// 未指定、广播——自建实例常跑在本机 Docker 或局域网主机上。
    LocalService,
}

/// 最多跟随的重定向跳数。
const MAX_REDIRECTS: usize = 10;

/// 按策略校验并解析 URL：`raw` 非 http/https、无法解析或 host 被策略拒绝即 Err。
pub(crate) fn ensure_http_url(raw: &str, policy: HostPolicy) -> Result<Url, String> {
    if !raw.starts_with("https://") && !raw.starts_with("http://") {
        return Err("仅支持 http/https 网址".to_string());
    }
    let parsed = Url::parse(raw).map_err(|e| format!("URL 解析失败：{e}"))?;
    let host = parsed.host_str().ok_or_else(|| "无效 URL host".to_string())?;
    // IPv6 字面量在 host 里带方括号（如 `[::1]`）：剥掉后才能按 IpAddr 解析、进入地址判定。
    // host 中出现 `%`（zone id，如 `[fe80::1%25eth0]`）时一并剥掉再判定：url 目前对这种字面量
    // 直接解析失败，此处只作兜底，防新形式出现时漏判。
    let zone = host.find('%').unwrap_or(host.len());
    let bare = host[..zone].trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") {
        return match policy {
            HostPolicy::PublicOnly => Err("拒绝访问 localhost（SSRF 防护）".to_string()),
            HostPolicy::LocalService => Ok(parsed),
        };
    }
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        if blocked_addr(ip, policy) {
            return Err(reject_message(policy));
        }
    }
    Ok(parsed)
}

/// 公网策略的地址校验（网页抓取 `fetch_web` / 插件 `http_request`）。
pub(crate) fn ensure_public_http_url(raw: &str) -> Result<Url, String> {
    ensure_http_url(raw, HostPolicy::PublicOnly)
}

/// 本机/局域网策略的地址校验（用户自建的本机服务）。
pub(crate) fn ensure_local_service_http_url(raw: &str) -> Result<Url, String> {
    ensure_http_url(raw, HostPolicy::LocalService)
}

/// 该 IP 是否被策略拒绝。
fn blocked_addr(ip: std::net::IpAddr, policy: HostPolicy) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => match policy {
            HostPolicy::PublicOnly => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            HostPolicy::LocalService => {
                v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
            }
        },
        std::net::IpAddr::V6(v6) => match policy {
            // 封堵回环/未指定/唯一本地(ULA fc00::/7)/link-local(fe80::/10)/IPv4-mapped(可伪装内网 v4)
            HostPolicy::PublicOnly => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    || v6.to_ipv4_mapped().is_some()
            }
            // IPv4-mapped 可伪装任意 v4（含云元数据 169.254.169.254）：按映射后的 v4 用同一策略判定
            HostPolicy::LocalService => match v6.to_ipv4_mapped() {
                Some(v4) => blocked_addr(std::net::IpAddr::V4(v4), policy),
                None => v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80,
            },
        },
    }
}

/// 被拒时的错误文案（按策略区分：公网策略拒的是内网/回环，本机策略拒的是链路本地等）。
fn reject_message(policy: HostPolicy) -> String {
    match policy {
        HostPolicy::PublicOnly => "拒绝访问内网/回环/链路本地地址（SSRF 防护）".to_string(),
        HostPolicy::LocalService => "拒绝访问链路本地/未指定/广播地址".to_string(),
    }
}

/// 重定向策略：每跳重新做策略校验（被拒目标即停止，把 3xx 响应原样交回调用方，不再向该地址发请求）。
pub(crate) fn redirect_policy(check: fn(&str) -> Result<Url, String>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if redirect_allowed(attempt.previous().len(), attempt.url().as_str(), check) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// 该跳是否放行：跳数在上限内且目标过同一策略校验。
fn redirect_allowed(hops: usize, url: &str, check: fn(&str) -> Result<Url, String>) -> bool {
    hops < MAX_REDIRECTS && check(url).is_ok()
}

#[cfg(test)]
mod net_guard_tests {
    use super::*;

    #[test]
    fn public_policy_allows_public_targets_and_blocks_non_http() {
        assert!(ensure_public_http_url("https://example.com/a").is_ok());
        assert!(ensure_public_http_url("http://example.com/a").is_ok());
        // 协议限制。
        assert!(ensure_public_http_url("ftp://example.com/a").is_err());
        assert!(ensure_public_http_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn public_policy_blocks_internal_targets() {
        // SSRF：回环/私网/链路本地/IPv6 回环与别名。
        assert!(ensure_public_http_url("http://127.0.0.1:8080/").is_err());
        assert!(ensure_public_http_url("http://localhost/").is_err());
        // 主机名判定不区分大小写
        assert!(ensure_public_http_url("http://LOCALHOST/").is_err());
        assert!(ensure_public_http_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(ensure_public_http_url("http://10.0.0.5/").is_err());
        assert!(ensure_public_http_url("http://[::1]/").is_err());
    }

    #[test]
    fn local_service_policy_allows_loopback_and_lan() {
        // 自建实例可能跑在本机 Docker、局域网主机或 IPv6 私网
        for ok in [
            "http://127.0.0.1:8080/search",
            "http://localhost:8080/",
            "http://192.168.1.10:8888/",
            "http://10.0.0.5/searx/",
            "http://[::1]:8080/",
            "http://[fc00::1]/",
            "http://[::ffff:192.168.1.9]/",
            "https://searx.example.com/",
        ] {
            assert!(ensure_local_service_http_url(ok).is_ok(), "{ok} 应放行");
        }
    }

    #[test]
    fn local_service_policy_blocks_link_local_unspecified_broadcast() {
        for bad in [
            "http://169.254.169.254/latest/meta-data",
            "http://[::ffff:169.254.169.254]/",
            "http://[fe80::1]/",
            // 带 zone id 的字面量不构成绕过：url 直接解析失败（非本策略放行的形式）
            "http://[fe80::1%25eth0]/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://255.255.255.255/",
            "ftp://example.com/",
            "file:///etc/passwd",
        ] {
            assert!(ensure_local_service_http_url(bad).is_err(), "{bad} 应拒绝");
        }
    }

    #[test]
    fn redirect_targets_are_rechecked_per_hop() {
        // 公网策略：公网目标放行（含 http→https 之类正常跳转）
        assert!(redirect_allowed(0, "https://example.com/a", ensure_public_http_url));
        assert!(redirect_allowed(3, "http://example.com/a", ensure_public_http_url));
        // 跳向内网/回环/超跳数一律不放行（302 到内网是绕过入口校验的主通路）
        assert!(!redirect_allowed(0, "http://127.0.0.1:7701/", ensure_public_http_url));
        assert!(!redirect_allowed(0, "http://[::1]/", ensure_public_http_url));
        assert!(!redirect_allowed(
            0,
            "http://169.254.169.254/latest/meta-data",
            ensure_public_http_url
        ));
        assert!(!redirect_allowed(0, "ftp://example.com/", ensure_public_http_url));
        assert!(!redirect_allowed(
            MAX_REDIRECTS,
            "https://example.com/a",
            ensure_public_http_url
        ));
        // 本机/局域网策略：私网目标放行，云元数据仍不放行
        assert!(redirect_allowed(
            0,
            "http://192.168.1.10:8888/",
            ensure_local_service_http_url
        ));
        assert!(!redirect_allowed(
            0,
            "http://169.254.169.254/latest/meta-data",
            ensure_local_service_http_url
        ));
    }
}
