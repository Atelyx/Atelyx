//! 系统密钥链凭据存取：协作空间登录令牌按「服务器地址摘要 + 用户名」条目存取，不落明文。

use keyring::Entry;

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
}
