//! API key 安全存储命令（按仓库隔离）。
//!
//! 命令接口两端一致（签名与「条目不存在返回空串」语义不变），实现按平台分档：
//! - 桌面：`keyring` crate 写 OS keychain（Windows Credential Manager / Linux Secret Service /
//!   macOS Keychain）；
//! - 安卓：Kotlin 桥 `com.atelyx.desktop.SecretStore`（gen/android 工程内）——Keystore 主密钥 +
//!   加密 SharedPreferences，Rust 经 webview JNI 线程同步调用。
//!
//! 安全边界：API key 默认仅存凭据存储，不落仓库文件或 `global.json`；
//! 仅当仓库开启 `syncKeys`（「API key 随仓库保存」，多设备同步）时前端才把 key 明文写入
//! `config.json`（`vault.rs` 的 `VaultProvider.api_key` 为可选字段，默认不落盘 = 类型层守边界）。
//!
//! 条目名（桌面 = keychain username；安卓 = prefs 键）：
//! - provider 条目 = `provider-<sha256(root)>-<providerId>`（仓库身份 = root 绝对路径，条目名取其
//!   SHA-256 哈希：路径长且含分隔符，不能直接作条目名；哈希隔离保证复制的仓库与原件互不共条目）；
//! - 通用应用秘密条目 = `app-secret-<sha256(name)>`（`name` 是任意调用方字串，整体哈希后与
//!   provider 条目共用同一命名空间隔离，且长度/字符集稳定）。
//! 与仓库级配置 `VaultConfig.providers` 配套；Tavily key 的 providerId 传 `search-tavily`。

use sha2::{Digest, Sha256};

/// 仓库身份（root 绝对路径）→ 条目名段：SHA-256 十六进制（64 字符，长度与字符集稳定）。
fn root_hash(vault_root: &str) -> String {
    let digest = Sha256::digest(vault_root.as_bytes());
    format!("{digest:x}")
}

/// 按仓库身份 + provider id 构造条目名（key 按仓库隔离，防跨仓库搞混）。
fn username_for(vault_root: &str, provider_id: &str) -> String {
    format!("provider-{}-{}", root_hash(vault_root), provider_id)
}

/// 通用应用秘密的条目名段：SHA-256 十六进制。
/// `name` 是任意调用方字串（可含空白/分隔符/非 ASCII），直接作条目名在部分平台会被拒或造成
/// 注入；整体哈希后与 provider 条目共用同一命名空间隔离，且长度/字符集稳定。
fn secret_hash(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    format!("{digest:x}")
}

/// 通用应用秘密的条目名（前缀 `app-secret-` 与 provider 条目区分命名空间）。
fn username_for_secret(name: &str) -> String {
    format!("app-secret-{}", secret_hash(name))
}

// ===== 桌面后端：OS keychain（keyring） =====

/// keychain 的 service 名（keychain 条目的独立命名空间，不随应用 identifier 变化）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const SERVICE: &str = "com.atelyx.app";

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod platform_backend {
    use keyring::Entry;
    use super::SERVICE;

    /// 保存条目（空串覆盖旧值；前端删除 key 时传空串即可，无需调 delete）。
    pub fn set(username: &str, value: &str) -> Result<(), String> {
        let entry = Entry::new(SERVICE, username).map_err(|e| e.to_string())?;
        entry.set_password(value).map_err(|e| e.to_string())
    }

    /// 读取条目。不存在（NoEntry）返回空串，与「未设置 key」语义一致；keychain 故障返回 Err。
    pub fn get(username: &str) -> Result<String, String> {
        let entry = Entry::new(SERVICE, username).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(s) => Ok(s),
            Err(keyring::Error::NoEntry) => Ok(String::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// 删除条目（幂等：不存在视为成功）。
    pub fn delete(username: &str) -> Result<(), String> {
        let entry = Entry::new(SERVICE, username).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

// ===== 安卓后端：Keystore + 加密 SharedPreferences（Kotlin 桥） =====

#[cfg(target_os = "android")]
mod platform_backend {
    use super::keystore_bridge::{self, BridgeOp};
    use tauri::AppHandle;

    /// 桥调用为同步等待（webview 线程回调），包进阻塞线程池避免占住异步运行时 worker。
    async fn call(op: BridgeOp, username: String, app: AppHandle) -> Result<String, String> {
        tokio::task::spawn_blocking(move || keystore_bridge::call(&app, op, &username))
            .await
            .map_err(|e| format!("凭据存储任务中断：{e}"))?
    }

    pub async fn set(app: AppHandle, username: String, value: String) -> Result<(), String> {
        call(BridgeOp::Set(value), username, app).await.map(|_| ())
    }

    pub async fn get(app: AppHandle, username: String) -> Result<String, String> {
        call(BridgeOp::Get, username, app).await
    }

    pub async fn delete(app: AppHandle, username: String) -> Result<(), String> {
        call(BridgeOp::Delete, username, app).await.map(|_| ())
    }
}

/// 安卓凭据桥：JNIEnv + Activity 只在 webview 线程可得，经 with_webview → JniHandle::exec
/// 投递执行并经通道回传；命令为异步变体 + 阻塞线程池等待（安卓上同步命令跑在主线程，
/// 同步等待桥回调会与主线程排空互等死锁）。
#[cfg(target_os = "android")]
mod keystore_bridge {
    use jni::objects::{JClass, JObject, JValue};
    use jni::objects::JString;
    use jni::JNIEnv;
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::Manager;

    /// Kotlin 桥类（随安卓工程分发，见 gen/android 的 SecretStore.kt）。
    const BRIDGE_CLASS: &str = "com.atelyx.desktop.SecretStore";
    /// 桥响应超时（Keystore 首次建钥 + 加密 prefs 初始化在百毫秒级，10s 已含冷启动余量）。
    const CALL_TIMEOUT: Duration = Duration::from_secs(10);

    /// 桥操作（与 SecretStore.kt 的静态方法一一对应）。
    pub enum BridgeOp {
        Set(String),
        Get,
        Delete,
    }

    /// 经 webview JNI 线程同步调用桥：Set/Delete 返回空串，Get 返回值（不存在 = 空串）。
    /// JNI 句柄只在平台 webview 载荷上可得：with_webview 拿到 PlatformWebview，再经
    /// JniHandle::exec 投递到 webview 线程执行并经通道回传。
    pub fn call(app: &tauri::AppHandle, op: BridgeOp, username: &str) -> Result<String, String> {
        let window = app.get_webview_window("main").ok_or("主 WebView 未就绪")?;
        let (tx, rx) = mpsc::channel();
        let username = username.to_string();
        window
            .with_webview(move |platform| {
                platform.jni_handle().exec(
                    move |env: &mut JNIEnv, activity: &JObject, _webview: &JObject| {
                        // 错误映射在此做：JNIEnv 只在本闭包内有效（Java 异常的提取与清除同此）
                        let _ = tx.send(
                            run_op(env, activity, op, &username)
                                .map_err(|e| jni_error_message(env, e)),
                        );
                    },
                );
            })
            .map_err(|e| format!("凭据存储桥投递失败：{e}"))?;
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err("凭据存储桥响应超时".to_string()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("凭据存储桥执行线程异常退出".to_string())
            }
        }
    }

    /// 单次桥调用：经 Activity 的 ClassLoader 解析桥类（原生线程 FindClass 命不中应用类加载器）。
    fn run_op(env: &mut JNIEnv, activity: &JObject, op: BridgeOp, username: &str) -> jni::errors::Result<String> {
        let loader = env
            .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let class_name = env.new_string(BRIDGE_CLASS)?;
        let class: JClass = env
            .call_method(
                loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[(&class_name).into()],
            )?
            .l()?
            .into();
        let key = env.new_string(username)?;
        match op {
            BridgeOp::Set(value) => {
                let v = env.new_string(value)?;
                // 校验写盘结果：commit 失败（磁盘满等）必须如实报错，不得当成功丢凭据
                let written = env
                    .call_static_method(
                        class,
                        "set",
                        "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Z",
                        &[JValue::Object(activity), JValue::Object(&key), JValue::Object(&v)],
                    )?
                    .z()?;
                if !written {
                    return Err(jni::errors::Error::JavaException);
                }
                Ok(String::new())
            }
            BridgeOp::Get => {
                let ret = env
                    .call_static_method(
                        class,
                        "get",
                        "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                        &[JValue::Object(activity), JValue::Object(&key)],
                    )?
                    .l()?;
                if ret.is_null() {
                    return Ok(String::new());
                }
                Ok(env.get_string(&JString::from(ret))?.to_string_lossy().into_owned())
            }
            BridgeOp::Delete => {
                let removed = env
                    .call_static_method(
                        class,
                        "delete",
                        "(Landroid/content/Context;Ljava/lang/String;)Z",
                        &[JValue::Object(activity), JValue::Object(&key)],
                    )?
                    .z()?;
                if !removed {
                    return Err(jni::errors::Error::JavaException);
                }
                Ok(String::new())
            }
        }
    }

    /// JNI 错误转消息：Java 异常挂起时提取其文本并清除（残留挂起异常会让 webview 线程
    /// 后续 JNI 调用崩溃），其余错误原样。
    fn jni_error_message(env: &mut JNIEnv, e: jni::errors::Error) -> String {
        if env.exception_check().unwrap_or(false) {
            let detail = env
                .exception_occurred()
                .ok()
                .and_then(|t| {
                    let throwable: JObject = t.into();
                    env.call_method(throwable, "toString", "()Ljava/lang/String;", &[]).ok()
                })
                .and_then(|v| v.l().ok())
                .and_then(|o| {
                    let s = JString::from(o);
                    env.get_string(&s).ok().map(|j| j.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            let _ = env.exception_clear();
            if !detail.is_empty() {
                return format!("凭据存储桥调用失败：{e}（{detail}）");
            }
        }
        format!("凭据存储桥调用失败：{e}")
    }
}

// ===== 命令面（两端同名同签名；安卓为异步变体以避开主线程互等） =====

/// 保存 API key（按仓库 + provider id）。空串会覆盖旧值（前端删除 key 时传空串即可，无需调 delete）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn set_api_key(vault_root: String, provider_id: String, key: String) -> Result<(), String> {
    platform_backend::set(&username_for(&vault_root, &provider_id), &key)
}

/// 读取仓库内 provider 的 API key。条目不存在返回空串，与「未设置 key」语义一致；存储故障返回 Err。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn get_api_key(vault_root: String, provider_id: String) -> Result<String, String> {
    platform_backend::get(&username_for(&vault_root, &provider_id))
}

/// 删除仓库内 provider 的凭据条目（幂等：条目不存在视为成功）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn delete_api_key(vault_root: String, provider_id: String) -> Result<(), String> {
    platform_backend::delete(&username_for(&vault_root, &provider_id))
}

/// 保存通用应用秘密到凭据存储（按 `name` 隔离，不落文件）。
/// 空串覆盖旧值（删除即传空串）。空间仓库复用 `set_api_key` 时把
/// `space:<serverUrl>#<spaceId>` 作为 `vault_root` 传入，哈希隔离天然成立——本组命令的
/// `vault_root`/`name` 不要求路径形态，任意非空字符串即可。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn set_app_secret(name: String, value: String) -> Result<(), String> {
    platform_backend::set(&username_for_secret(&name), &value)
}

/// 读取通用应用秘密。`name` 对应条目不存在返回空串，与「未设置」语义一致；存储故障返回 Err。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn get_app_secret(name: String) -> Result<String, String> {
    platform_backend::get(&username_for_secret(&name))
}

/// 删除通用应用秘密条目（幂等：条目不存在视为成功）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn delete_app_secret(name: String) -> Result<(), String> {
    platform_backend::delete(&username_for_secret(&name))
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn set_api_key(
    app: tauri::AppHandle,
    vault_root: String,
    provider_id: String,
    key: String,
) -> Result<(), String> {
    platform_backend::set(app, username_for(&vault_root, &provider_id), key).await
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn get_api_key(
    app: tauri::AppHandle,
    vault_root: String,
    provider_id: String,
) -> Result<String, String> {
    platform_backend::get(app, username_for(&vault_root, &provider_id)).await
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn delete_api_key(
    app: tauri::AppHandle,
    vault_root: String,
    provider_id: String,
) -> Result<(), String> {
    platform_backend::delete(app, username_for(&vault_root, &provider_id)).await
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn set_app_secret(app: tauri::AppHandle, name: String, value: String) -> Result<(), String> {
    platform_backend::set(app, username_for_secret(&name), value).await
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn get_app_secret(app: tauri::AppHandle, name: String) -> Result<String, String> {
    platform_backend::get(app, username_for_secret(&name)).await
}

/// 安卓变体：见桌面同名命令与 keystore_bridge。
#[cfg(target_os = "android")]
#[tauri::command]
pub async fn delete_app_secret(app: tauri::AppHandle, name: String) -> Result<(), String> {
    platform_backend::delete(app, username_for_secret(&name)).await
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
