//! 外链图片字节获取：按出网策略经 `net_guard` 全链路守卫 + 系统临时目录磁盘缓存。
//!
//! 缓存键 = 策略域 + URL 的 FNV-1a 哈希（跨进程稳定）；缓存读写尽力而为，失败只影响命中率
//! 不阻塞取图。策略域参与键：同一地址在两种策略下可取的字节相同，但若共享缓存，先按本机策略
//! 取到的内网图会被公网策略的调用命中，等于绕过公网策略。

use std::path::PathBuf;

use crate::net_guard::{self, ClientTimeout, HostPolicy};

/// 单张外链图片字节上限（超限即拒，响应体不整体进内存）。
const MAX_BYTES: usize = 20 * 1024 * 1024;

/// 取公网外链图片字节：磁盘缓存命中直接返回（无失效语义——展示稳定优先，内容更新需
/// 改名绕过），未命中走公网策略下载后落缓存。
pub async fn fetch_external_image(url: &str) -> Result<Vec<u8>, String> {
    fetch_image(url, HostPolicy::PublicOnly).await
}

/// 取本机 / 局域网服务（含回环）的图片字节。供插件显示它自己连接的本机服务图。
pub async fn fetch_local_image(url: &str) -> Result<Vec<u8>, String> {
    fetch_image(url, HostPolicy::LocalService).await
}

async fn fetch_image(url: &str, policy: HostPolicy) -> Result<Vec<u8>, String> {
    let cache_path = cache_path(url, policy);
    if let Ok(bytes) = std::fs::read(&cache_path) {
        return Ok(bytes);
    }
    let checked = net_guard::ensure_http_url(url, policy)?;
    let client = net_guard::client_for(policy, ClientTimeout::Total(30))?;
    let mut response = client
        .get(checked)
        .send()
        .await
        .map_err(|e| format!("图片下载失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("图片下载失败：{e}"))?;
    if let Some(len) = response.content_length() {
        if len > MAX_BYTES as u64 {
            return Err("图片超过 20MB 上限".to_string());
        }
    }
    // 分块累计、到上限即断：content-length 缺失或虚报时同样受控
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("图片读取失败：{e}"))? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err("图片超过 20MB 上限".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err("图片内容为空".to_string());
    }
    // 缓存写失败不影响本次取图
    if std::fs::create_dir_all(cache_dir()).is_ok() {
        let _ = std::fs::write(&cache_path, &bytes);
    }
    Ok(bytes)
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("atelyx-image-cache")
}

/// 缓存文件路径（URL 的 FNV-1a 64 位 hex——进程随机哈希不可作缓存键，须手写稳定哈希）。
/// 缓存文件路径：策略域前缀（`p` = 公网 / `l` = 本机）+ URL 的 FNV-1a 64 位 hex。
/// 策略域参与键，防止本机策略取到的内网图被公网口径命中。
fn cache_path(url: &str, policy: HostPolicy) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let scope = match policy {
        HostPolicy::PublicOnly => "p",
        HostPolicy::LocalService => "l",
    };
    cache_dir().join(format!("{scope}-{hash:016x}.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_stable_and_distinct() {
        let public = HostPolicy::PublicOnly;
        let local = HostPolicy::LocalService;
        assert_eq!(cache_path("https://a/x.png", public), cache_path("https://a/x.png", public));
        assert_ne!(cache_path("https://a/x.png", public), cache_path("https://a/y.png", public));
        assert_ne!(
            cache_path("http://127.0.0.1/x.png", public),
            cache_path("http://127.0.0.1/x.png", local),
            "策略域必须分开缓存，否则本机策略取到的图会被公网策略命中"
        );
    }
}
