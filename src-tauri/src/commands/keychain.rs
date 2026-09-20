//! API key 安全存储命令（按仓库隔离）。
//!
//! 通过 `keyring` crate 把 AI provider 的 API key 存入 OS keychain：
//! - Windows：Windows Credential Manager
//! - Linux：Secret Service（DBus，需 gnome-keyring / kwalletd）
//! - macOS：Keychain Services
//!
//! 安全边界：API key 默认仅存 keychain，不落仓库文件或 `global.json`；
//! 仅当仓库开启 `syncKeys`（「API key 随仓库保存」，多设备同步）时前端才把 key 明文写入
//! `config.json`（`vault.rs` 的 `VaultProvider.api_key` 为可选字段，默认不落盘 = 类型层守边界）。
//!
//! service = `com.atelyx.app`（keychain 条目的独立命名空间，与 tauri.conf.json 的 identifier 无关），
//! username = `provider-<sha256(root)>-<providerId>`（仓库身份 = root 绝对路径，条目名取其
//! SHA-256 哈希：路径长且含分隔符，不能直接作条目名；哈希隔离保证复制的仓库与原件互不共条目）。
//! 与仓库级配置 `VaultConfig.providers` 配套；Tavily key 的 providerId 传 `search-tavily`。

use keyring::Entry;
use sha2::{Digest, Sha256};

/// keychain 的 service 名（keychain 条目的独立命名空间，不随应用 identifier 变化）。
const SERVICE: &str = "com.atelyx.app";

/// 仓库身份（root 绝对路径）→ keychain 条目名段：SHA-256 十六进制（64 字符，长度与字符集稳定）。
fn root_hash(vault_root: &str) -> String {
    let digest = Sha256::digest(vault_root.as_bytes());
    format!("{digest:x}")
}

/// 按仓库身份 + provider id 构造 keychain username（key 按仓库隔离，防跨仓库搞混）。
fn username_for(vault_root: &str, provider_id: &str) -> String {
    format!("provider-{}-{}", root_hash(vault_root), provider_id)
}

/// 通用应用秘密的 keychain 条目名段：SHA-256 十六进制。
/// `name` 是任意调用方字串（可含空白/分隔符/非 ASCII），直接作条目名在部分平台会被拒或造成
/// 注入；整体哈希后与 provider 条目共用同一命名空间隔离，且长度/字符集稳定。
fn secret_hash(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    format!("{digest:x}")
}

/// 通用应用秘密的 keychain username（前缀 `app-secret-` 与 provider 条目区分命名空间）。
fn username_for_secret(name: &str) -> String {
    format!("app-secret-{}", secret_hash(name))
}

/// 保存 API key 到 keychain（按仓库 + provider id）。
/// 空串会覆盖旧值（前端删除 key 时传空串即可，无需调 delete）。
#[tauri::command]
pub fn set_api_key(vault_root: String, provider_id: String, key: String) -> Result<(), String> {
    let entry = Entry::new(SERVICE, &username_for(&vault_root, &provider_id)).map_err(|e| e.to_string())?;
    entry.set_password(&key).map_err(|e| e.to_string())
}

/// 读取仓库内 provider 的 API key。
/// 条目不存在（NoEntry）返回空串，与「未设置 key」语义一致；keychain 故障返回 Err。
#[tauri::command]
pub fn get_api_key(vault_root: String, provider_id: String) -> Result<String, String> {
    let entry = Entry::new(SERVICE, &username_for(&vault_root, &provider_id)).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// 删除仓库内 provider 的 keychain 条目（幂等：条目不存在视为成功）。
#[tauri::command]
pub fn delete_api_key(vault_root: String, provider_id: String) -> Result<(), String> {
    let entry = Entry::new(SERVICE, &username_for(&vault_root, &provider_id)).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 保存通用应用秘密到 keychain（按 `name` 隔离，不落文件）。
/// `name` 经哈希后作条目名（见 `username_for_secret`）。空串覆盖旧值（删除即传空串）。
/// 空间仓库复用 `set_api_key` 时把 `space:<serverUrl>#<spaceId>` 作为 `vault_root` 传入，
/// 哈希隔离天然成立——本组命令的 `vault_root`/`name` 不要求路径形态，任意非空字符串即可。
#[tauri::command]
pub fn set_app_secret(name: String, value: String) -> Result<(), String> {
    let entry = Entry::new(SERVICE, &username_for_secret(&name)).map_err(|e| e.to_string())?;
    entry.set_password(&value).map_err(|e| e.to_string())
}

/// 读取通用应用秘密。`name` 对应条目不存在（NoEntry）返回空串，与「未设置」语义一致；
/// keychain 故障返回 Err。
#[tauri::command]
pub fn get_app_secret(name: String) -> Result<String, String> {
    let entry = Entry::new(SERVICE, &username_for_secret(&name)).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// 删除通用应用秘密 keychain 条目（幂等：条目不存在视为成功）。
#[tauri::command]
pub fn delete_app_secret(name: String) -> Result<(), String> {
    let entry = Entry::new(SERVICE, &username_for_secret(&name)).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_hash_is_stable_hex_and_differs_per_root() {
        let a = root_hash("E:/repo");
        assert_eq!(a, root_hash("E:/repo"));
        assert_ne!(a, root_hash("E:/repo-copy"));
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // SHA-256("") = e3b0c4…（公开向量）：锁定算法为小写十六进制 SHA-256，防回归成其他摘要
        assert_eq!(
            root_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn secret_hash_is_stable_and_namespaced_from_provider() {
        let name = "space-token-http://192.168.1.10:11224";
        let user = username_for_secret(name);
        // 前缀隔离 provider 条目，防命名空间冲突
        assert!(user.starts_with("app-secret-"));
        assert_eq!(user, username_for_secret(name));
        // 任意 name（含空白/分隔符）映射到定长十六进制，防条目名注入
        let hashed = &user["app-secret-".len()..];
        assert_eq!(hashed.len(), 64);
        assert!(hashed.chars().all(|c| c.is_ascii_hexdigit()));
        // SHA-256 公开向量：锁定算法
        assert_eq!(
            secret_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
