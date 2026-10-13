//! 系统密钥链凭据存取：协作空间登录令牌按「服务器地址摘要 + 用户名」条目存取，不落明文；
//! AI 供应商 / 搜索源 API key 按「仓库（空间）摘要 + 条目尾」存取。

use keyring::Entry;
use sha2::{Digest, Sha256};

/// 密钥链服务名（条目命名空间）。
const SERVICE: &str = "atelyx-space";

/// 服务器地址 → 条目摘要（FNV-1a 64，稳定跨启动；仅需区分条目，非安全用途）。
fn entry_digest(server_url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in server_url.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn entry_for(server_url: &str, username: &str) -> Result<Entry, String> {
    // Windows 凭据目标名对分隔字符敏感：摘要与用户名用「.」连接
    Entry::new(SERVICE, &format!("{}.{}", entry_digest(server_url), username))
        .map_err(|e| format!("密钥链不可用：{e}"))
}

/// 存令牌（覆盖同条目旧值）。
pub fn store_token(server_url: &str, username: &str, token: &str) -> Result<(), String> {
    entry_for(server_url, username)?
        .set_password(token)
        .map_err(|e| format!("写入密钥链失败：{e}"))
}

/// 取令牌；条目不存在（未在本机登录过）返回 `None`。
pub fn load_token(server_url: &str, username: &str) -> Result<Option<String>, String> {
    match entry_for(server_url, username)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取密钥链失败：{e}")),
    }
}

/// 删除令牌（登出 / 会话吊销后调用）；条目不存在视为已删除。
pub fn delete_token(server_url: &str, username: &str) -> Result<(), String> {
    match entry_for(server_url, username)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("清除密钥链失败：{e}")),
    }
}

// ===== API key 条目（AI 供应商 / 搜索源） =====
// 条目名 `provider-<sha256(scope)>.<tail>`：scope = 仓库根路径（本地仓库）或「服务器地址/空间 id」
// （协作空间），tail = 供应商 id 或 `search-tavily`。与空间令牌分服务名，互不干扰。

/// API key 密钥链服务名（条目命名空间）。
const KEY_SERVICE: &str = "atelyx-keys";

fn scope_digest(scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hex::encode(hasher.finalize())
}

fn api_key_entry(scope: &str, tail: &str) -> Result<Entry, String> {
    // Windows 凭据目标名对分隔字符敏感：摘要 + tail 均为 [A-Za-z0-9_-]，用「.」连接
    Entry::new(KEY_SERVICE, &format!("provider-{}.{}", scope_digest(scope), tail))
        .map_err(|e| format!("密钥链不可用：{e}"))
}

/// 存 API key（覆盖同条目旧值）。
pub fn store_provider_key(scope: &str, tail: &str, key: &str) -> Result<(), String> {
    api_key_entry(scope, tail)?
        .set_password(key)
        .map_err(|e| format!("写入密钥链失败：{e}"))
}

/// 取 API key；条目不存在（未在本机存过）返回 `None`。
pub fn load_provider_key(scope: &str, tail: &str) -> Result<Option<String>, String> {
    match api_key_entry(scope, tail)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取密钥链失败：{e}")),
    }
}

/// 删除 API key（供应商删除后清理；条目不存在视为已删除）。
pub fn delete_provider_key(scope: &str, tail: &str) -> Result<(), String> {
    match api_key_entry(scope, tail)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("清除密钥链失败：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 令牌存取删除往返() {
        // 平台无密钥链后端（如 CI 无 Secret Service）时跳过，不算失败
        let url = "http://test.example:11224";
        let store = match Entry::new(SERVICE, &format!("{}.{}", entry_digest(url), "tester")) {
            Ok(e) => e,
            Err(_) => return,
        };
        if store.set_password("t0ken").is_err() {
            return;
        }
        // 沙箱/虚拟化环境下写入可能被吞（返回 Ok 但读回 NoEntry）：视为环境限制跳过，
        // 其余错误仍按失败处理
        let loaded = entry_for(url, "tester").unwrap().get_password();
        if matches!(loaded, Err(keyring::Error::NoEntry)) {
            return;
        }
        let inspect = format!("{loaded:?}");
        assert_eq!(loaded.ok().as_deref(), Some("t0ken"), "get_password 实际返回：{inspect}");
        delete_token(url, "tester").unwrap();
        assert_eq!(load_token(url, "tester").unwrap(), None);
        // 重复删除幂等
        delete_token(url, "tester").unwrap();
    }

    #[test]
    fn 地址摘要稳定且互异() {
        assert_eq!(entry_digest("http://a:1"), entry_digest("http://a:1"));
        assert_ne!(entry_digest("http://a:1"), entry_digest("http://a:2"));
    }

    #[test]
    fn api_key存取删除往返() {
        // 平台无密钥链后端时跳过（与令牌往返同口径）
        let scope = "e:\\test-vault";
        let entry = match api_key_entry(scope, "prov-1") {
            Ok(e) => e,
            Err(_) => return,
        };
        if entry.set_password("k3y").is_err() {
            return;
        }
        let loaded = api_key_entry(scope, "prov-1").unwrap().get_password();
        if matches!(loaded, Err(keyring::Error::NoEntry)) {
            return;
        }
        assert_eq!(loaded.ok().as_deref(), Some("k3y"));
        delete_provider_key(scope, "prov-1").unwrap();
        assert_eq!(load_provider_key(scope, "prov-1").unwrap(), None);
        // 重复删除幂等
        delete_provider_key(scope, "prov-1").unwrap();
    }

    #[test]
    fn scope摘要区分条目且稳定() {
        assert_eq!(scope_digest("e:\\a"), scope_digest("e:\\a"));
        assert_ne!(scope_digest("e:\\a"), scope_digest("e:\\b"));
    }
}
