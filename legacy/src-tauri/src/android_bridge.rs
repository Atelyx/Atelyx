//! 安卓 Kotlin 桥的共享 JNI 管道：`with_webview` 取平台载荷，经 `JniHandle::exec` 投递到
//! webview 线程执行，结果经通道回传。超时、投递与异常处理口径统一在此，调用方只拼自己的静态方法调用；
//! 三处 JNI 易错点（ClassLoader 解析 / 清除挂起异常 / 主线程死锁）就近记录在各函数上。

use std::sync::mpsc;
use std::time::Duration;

use jni::objects::{JClass, JObject, JString};
use jni::JNIEnv;
use tauri::Manager;

/// 桥响应超时。跨线程投递与 Java 侧执行都在百毫秒级，10s 已含冷启动余量。
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// 在 webview 线程上执行桥调用并同步等回结果。
///
/// `label` 是错误前缀，调用方各自传入以保留各自措辞（「凭据存储桥」「系统能力桥」）。
/// 闭包拿到的 Activity 同时是 `android.content.Context`，可直接作为桥方法的 Context 实参。
/// 本函数会阻塞等待回调：安卓上不得在主线程调用（主线程等 webview 回调互等死锁），
/// 须放到阻塞线程池执行。
pub fn with_activity<T, F>(app: &tauri::AppHandle, label: &str, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut JNIEnv, &JObject, &JObject) -> Result<T, String> + Send + 'static,
{
    let window = app.get_webview_window("main").ok_or("主 WebView 未就绪")?;
    let (tx, rx) = mpsc::channel();
    window
        .with_webview(move |platform| {
            platform.jni_handle().exec(move |env, activity, webview| {
                let _ = tx.send(f(env, activity, webview));
            });
        })
        .map_err(|e| format!("{label}投递失败：{e}"))?;
    match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("{label}响应超时")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("{label}执行线程异常退出")),
    }
}

/// 经 Activity 的 ClassLoader 解析桥类：原生线程的 `FindClass` 用系统类加载器，
/// 命不中随应用分发的桥类。
/// `activity` 的生命周期独立于 `env`（JNIEnv 在其生命周期参数上不变），调用点按各自实参推导。
pub fn load_bridge_class<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject,
    class_name: &str,
) -> jni::errors::Result<JClass<'local>> {
    let loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let name = env.new_string(class_name)?;
    Ok(env
        .call_method(
            loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[(&name).into()],
        )?
        .l()?
        .into())
}

/// JNI 错误转消息：Java 异常挂起时提取其文本并清除，其余错误原样。
pub fn jni_error_message(env: &mut JNIEnv, label: &str, e: jni::errors::Error) -> String {
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
            return format!("{label}：{e}（{detail}）");
        }
    }
    format!("{label}：{e}")
}
