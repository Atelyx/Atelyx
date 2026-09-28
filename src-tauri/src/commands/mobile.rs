//! 移动端专属命令：安卓系统能力（设备名 / 存储授权 / 目录浏览）与外部 URL 打开。
//!
//! 桌面端不提供这些能力（一律返回可读错误，不静默失败）：桌面选目录走系统原生弹窗、
//! 仓库根由用户直接指定，无需系统级存储授权，也没有「打开系统设置页」这类交互。
//!
//! Kotlin 能力桥 `com.atelyx.desktop.PlatformOps` 随安卓工程分发（见 gen/android）。

use serde::Serialize;
use tauri::AppHandle;

/// 非移动端的统一拒绝（能力缺失必须显式可见）。
#[cfg(not(target_os = "android"))]
const MOBILE_ONLY: &str = "当前平台不支持此功能";

/// Kotlin 能力桥类（随安卓工程分发，见 gen/android 的 PlatformOps.kt）。
#[cfg(target_os = "android")]
const BRIDGE_CLASS: &str = "com.atelyx.desktop.PlatformOps";
/// 错误前缀：投递/超时用前者，调用失败用后者。
#[cfg(target_os = "android")]
const BRIDGE_LABEL: &str = "系统能力桥";
#[cfg(target_os = "android")]
const CALL_FAILED_LABEL: &str = "系统能力桥调用失败";

/// 目录浏览条目（只列子目录）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsoluteDirEntry {
    pub name: String,
    /// 子目录绝对路径（规范化后）
    pub path: String,
    /// 能否进入（读失败 = 权限不足，UI 置灰不可点）
    pub readable: bool,
}

/// `list_absolute_dir` 的返回。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsoluteDirListing {
    /// 规范化后的当前目录绝对路径
    pub path: String,
    /// 上一级目录（已是文件系统根时为 null）
    pub parent: Option<String>,
    pub entries: Vec<AbsoluteDirEntry>,
}

/// 查询「所有文件访问权限」（MANAGE_EXTERNAL_STORAGE）是否已授予。
#[tauri::command]
pub async fn android_has_all_files_access(app: AppHandle) -> Result<bool, String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        bridge_call(app, |env, activity, _webview| {
            use jni::objects::JValue;
            let class = load_bridge(env, activity)?;
            env.call_static_method(
                class,
                "hasAllFilesAccess",
                "(Landroid/content/Context;)Z",
                &[JValue::Object(activity)],
            )
            .and_then(|v| v.z())
            .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))
        })
        .await
    }
}

/// 拉起系统的「所有文件访问权限」设置页。**拉起即返回**，不等待用户操作完成。
#[tauri::command]
pub async fn android_request_all_files_access(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        bridge_call(app, |env, activity, _webview| {
            use jni::objects::JValue;
            let class = load_bridge(env, activity)?;
            env.call_static_method(
                class,
                "requestAllFilesAccess",
                "(Landroid/content/Context;)V",
                &[JValue::Object(activity)],
            )
            .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))?;
            Ok(())
        })
        .await
    }
}

/// 用系统默认程序打开外部 URL（webview 不导航）。
///
/// 协议在 Rust 侧收口（与桌面 `plugins.shell.open` 的放行范围一致）：这条路径是 Intent 直达，
/// 前端各入口的 `isOpenableUrl` 只是第一道，插件/页面直连命令时必须由本层兜住。
#[tauri::command]
pub async fn android_open_url(app: AppHandle, url: String) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, url);
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        if !external_url_allowed(&url) {
            return Err(format!("不支持的链接协议：{url}"));
        }
        bridge_call(app, move |env, activity, _webview| {
            use jni::objects::JValue;
            let class = load_bridge(env, activity)?;
            let target = env
                .new_string(&url)
                .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))?;
            env.call_static_method(
                class,
                "openUrl",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(activity), JValue::Object(&target)],
            )
            .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))?;
            Ok(())
        })
        .await
    }
}

/// 允许交给系统打开的链接协议（http/https/mailto/tel/xmpp；与桌面 shell.open 的 scheme 一致，
/// 不含 file：安卓上 file URI 会触发 FileUriExposedException，本地路径不经此命令）。
#[cfg(target_os = "android")]
fn external_url_allowed(url: &str) -> bool {
    let scheme = url.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(scheme.as_str(), "http" | "https" | "mailto" | "tel" | "xmpp")
}

/// 经系统能力桥取设备型号（协作身份默认值）。失败原样上抛，由调用方决定回落口径。
#[cfg(target_os = "android")]
pub(crate) async fn bridge_device_name(app: AppHandle) -> Result<String, String> {
    bridge_call(app, |env, activity, _webview| {
        use jni::objects::JValue;
        let class = load_bridge(env, activity)?;
        env.call_static_method(
            class,
            "deviceName",
            "(Landroid/content/Context;)Ljava/lang/String;",
            &[JValue::Object(activity)],
        )
        .and_then(|v| v.l())
        .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))
        .and_then(|s| {
            let jstr = jni::objects::JString::from(s);
            env.get_string(&jstr)
                .map(|j| j.to_string_lossy().into_owned())
                .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))
        })
    })
    .await
}

/// 外部存储根目录（已授予「所有文件访问权限」时的目录浏览起点）。
///
/// 文件系统根（/）对应用不可读，目录浏览必须从有权限的那一层开始。
#[tauri::command]
pub async fn android_storage_root(app: AppHandle) -> Result<String, String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        bridge_call(app, |env, activity, _webview| {
            use jni::objects::JValue;
            let class = load_bridge(env, activity)?;
            env.call_static_method(
                class,
                "storageRoot",
                "(Landroid/content/Context;)Ljava/lang/String;",
                &[JValue::Object(activity)],
            )
            .and_then(|v| v.l())
            .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))
            .and_then(|s| {
                let jstr = jni::objects::JString::from(s);
                env.get_string(&jstr)
                    .map(|j| j.to_string_lossy().into_owned())
                    .map_err(|e| {
                        crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e)
                    })
            })
        })
        .await
    }
}

/// 应用内部私有目录下的回落仓库根（未开「所有文件访问权限」时使用）。
///
/// 取 `app_data_dir/vault` 而非 `app_data_dir` 本身：后者同级放着 `plugins/`、`global.json`、
/// `ui-state.json`，直接当仓库根会让这些内部产物出现在文件树里。
#[tauri::command]
pub fn android_private_vault_path(app: AppHandle) -> Result<String, String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("vault");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建私有仓库目录失败：{e}"))?;
        Ok(dir.to_string_lossy().into_owned())
    }
}

/// 列出任意绝对路径下的子目录（自研目录浏览的数据源）。
///
/// 只读；单项读元数据失败即跳过，整目录读失败才报错（权限不足的子目录在 UI 上置灰而非整体失败）。
/// 目录可能很大（顶层目录 + 逐子目录试读），故放到阻塞线程池，不占安卓主线程。
#[tauri::command]
pub async fn list_absolute_dir(path: String) -> Result<AbsoluteDirListing, String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        Err(MOBILE_ONLY.to_string())
    }
    #[cfg(target_os = "android")]
    {
        tokio::task::spawn_blocking(move || list_absolute_dir_blocking(&path))
            .await
            .map_err(|e| format!("目录浏览任务中断：{e}"))?
    }
}

/// `list_absolute_dir` 的实际读盘实现（在阻塞线程池上执行）。
#[cfg(target_os = "android")]
fn list_absolute_dir_blocking(path: &str) -> Result<AbsoluteDirListing, String> {
    let raw = std::path::PathBuf::from(path);
    if !raw.is_absolute() {
        return Err("需要绝对路径".to_string());
    }
    let dir = dunce::canonicalize(&raw).map_err(|e| format!("目录不可达：{path}（{e}）"))?;
    if !dir.is_dir() {
        return Err(format!("不是文件夹：{path}"));
    }
    let mut entries: Vec<AbsoluteDirEntry> = Vec::new();
    let reader = std::fs::read_dir(&dir).map_err(|e| format!("目录读取失败：{path}（{e}）"))?;
    for entry in reader.flatten() {
        let child = entry.path();
        // 只列目录：选仓库根的场景下文件无用；元数据读不到（权限/已删除）即跳过该条
        let Ok(meta) = std::fs::metadata(&child) else { continue };
        if !meta.is_dir() {
            continue;
        }
        entries.push(AbsoluteDirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            readable: std::fs::read_dir(&child).is_ok(),
            path: child.to_string_lossy().into_owned(),
        });
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    Ok(AbsoluteDirListing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        entries,
    })
}

/// 投递桥调用并在阻塞线程池上等待：桥回调只在 webview 线程执行，主线程同步等待会互等死锁。
#[cfg(target_os = "android")]
async fn bridge_call<T, F>(app: AppHandle, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(
            &mut jni::JNIEnv,
            &jni::objects::JObject,
            &jni::objects::JObject,
        ) -> Result<T, String>
        + Send
        + 'static,
{
    tokio::task::spawn_blocking(move || crate::android_bridge::with_activity(&app, BRIDGE_LABEL, f))
        .await
        .map_err(|e| format!("{BRIDGE_LABEL}任务中断：{e}"))?
}

/// 解析桥类（失败即按调用失败措辞上报）。
#[cfg(target_os = "android")]
fn load_bridge<'local>(
    env: &mut jni::JNIEnv<'local>,
    activity: &jni::objects::JObject,
) -> Result<jni::objects::JClass<'local>, String> {
    crate::android_bridge::load_bridge_class(env, activity, BRIDGE_CLASS)
        .map_err(|e| crate::android_bridge::jni_error_message(env, CALL_FAILED_LABEL, e))
}
