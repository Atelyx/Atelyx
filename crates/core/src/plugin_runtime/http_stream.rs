//! `http.stream`：流式出网连接的生命周期与分片下发。
//!
//! 宿主建连、按块把原始字节交给发起插件，由插件自己解析协议（SSE 等）。地址校验沿用
//! 供应商地址口径（[`policy_for_configured_url`]）：公网目标拒内网，本机 / 局域网服务放行。
//! 流与插件会话同寿命：插件停用 / 失败时全部取消，迟到的分片不再下发。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use atelyx_plugin_protocol::method::{self, HttpStreamOpenParams, HttpStreamOpenResult};
use base64::Engine;

use super::instance::Instance;
use super::PluginRuntime;
use crate::net_guard::{client_for, ensure_http_url, policy_for_configured_url, ClientTimeout};

/// 未声明超时时的空闲读上限（秒）：模型流式通常每几百毫秒有数据。
const DEFAULT_IDLE_SECS: u64 = 60;

pub(super) struct StreamEntry {
    pub plugin_id: String,
    /// 仍可下发（取消 / 收尾先清零，保证 end 只发一次）。
    pub alive: Arc<AtomicBool>,
    pub task: tokio::task::JoinHandle<()>,
}

/// 打开一条流：校验地址后立即返回 streamId，建连与分片在后台任务里完成。
/// 先登记再放行任务，避免建连失败时收尾早于登记留下僵尸条目。
pub(super) fn open(
    runtime: &Arc<PluginRuntime>,
    instance: &Arc<Instance>,
    params: HttpStreamOpenParams,
) -> Result<HttpStreamOpenResult, String> {
    let policy = policy_for_configured_url(&params.url);
    let url = ensure_http_url(&params.url, policy)?;
    let request_method = reqwest::Method::from_bytes(params.method.as_bytes())
        .map_err(|e| format!("请求方法无效：{e}"))?;
    let idle = params
        .timeout_ms
        .map(|ms| ms.div_ceil(1000).max(1))
        .unwrap_or(DEFAULT_IDLE_SECS);
    let client = client_for(policy, ClientTimeout::IdleRead(idle))?;
    let stream_id = match params.stream_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => {
            if runtime.streams.lock().unwrap().contains_key(&id) {
                return Err(format!("流 id 已被占用：{id}"));
            }
            id
        }
        None => format!(
            "{}-{}",
            instance.id,
            runtime.next_stream_id.fetch_add(1, Ordering::SeqCst)
        ),
    };
    let alive = Arc::new(AtomicBool::new(true));
    let entry_alive = Arc::clone(&alive);
    let task_instance = Arc::clone(instance);
    let task_runtime = Arc::clone(runtime);
    let task_stream_id = stream_id.clone();
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let task = runtime.service_handle().spawn(async move {
        if start_rx.await.is_err() {
            return;
        }
        let mut request = client.request(request_method, url);
        for (name, value) in &params.headers {
            request = request.header(name, value);
        }
        if let Some(body) = params.body {
            request = request.body(body);
        }
        let mut response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                finish(
                    &task_runtime,
                    &task_instance,
                    &task_stream_id,
                    &entry_alive,
                    Some(format!("请求失败：{e}")),
                    None,
                );
                return;
            }
        };
        let status = response.status();
        while entry_alive.load(Ordering::SeqCst) {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if !entry_alive.load(Ordering::SeqCst) {
                        break;
                    }
                    let params = method::HttpStreamChunkParams {
                        stream_id: task_stream_id.clone(),
                        base64: base64::engine::general_purpose::STANDARD.encode(chunk),
                    };
                    task_instance.notify_stream_chunk(&params);
                }
                Ok(None) => {
                    let error = (!status.is_success()).then(|| format!("HTTP {status}"));
                    finish(
                        &task_runtime,
                        &task_instance,
                        &task_stream_id,
                        &entry_alive,
                        error,
                        Some(status.as_u16()),
                    );
                    return;
                }
                Err(e) => {
                    finish(
                        &task_runtime,
                        &task_instance,
                        &task_stream_id,
                        &entry_alive,
                        Some(if status.is_success() {
                            format!("读取响应失败：{e}")
                        } else {
                            format!("HTTP {status}")
                        }),
                        Some(status.as_u16()),
                    );
                    return;
                }
            }
        }
    });
    runtime.streams.lock().unwrap().insert(
        stream_id.clone(),
        StreamEntry { plugin_id: instance.id.clone(), alive, task },
    );
    let _ = start_tx.send(());
    Ok(HttpStreamOpenResult { stream_id })
}

/// 取消一条流（幂等）。归属不符按可读错误收场，不碰别人的流。
pub(super) fn cancel(
    runtime: &Arc<PluginRuntime>,
    instance: &Arc<Instance>,
    stream_id: &str,
) -> Result<(), String> {
    let entry = {
        let mut streams = runtime.streams.lock().unwrap();
        match streams.get(stream_id) {
            Some(entry) if entry.plugin_id == instance.id => streams.remove(stream_id),
            Some(_) => return Err(format!("流不属于本插件：{stream_id}")),
            None => return Ok(()),
        }
    };
    if let Some(entry) = entry {
        if entry.alive.swap(false, Ordering::SeqCst) {
            instance.notify_stream_end(&method::HttpStreamEndParams {
                stream_id: stream_id.to_string(),
                error: None,
                cancelled: true,
                status: None,
            });
        }
        entry.task.abort();
    }
    Ok(())
}

/// 插件停用 / 失败 / 卸载：取消它名下全部流。迟到的分片由取消位拦下。
pub(super) fn drop_plugin(runtime: &Arc<PluginRuntime>, plugin_id: &str) {
    let entries: Vec<(String, StreamEntry)> = {
        let mut streams = runtime.streams.lock().unwrap();
        let ids: Vec<String> = streams
            .iter()
            .filter(|(_, entry)| entry.plugin_id == plugin_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter().filter_map(|id| streams.remove(&id).map(|entry| (id, entry))).collect()
    };
    for (stream_id, entry) in entries {
        if entry.alive.swap(false, Ordering::SeqCst) {
            // 停用 / 崩溃前让等待中的调用方看到「已取消」，而不是静默断流
            if let Some(instance) = runtime.instances.lock().unwrap().get(plugin_id).cloned() {
                instance.notify_stream_end(&method::HttpStreamEndParams {
                    stream_id,
                    error: None,
                    cancelled: true,
                    status: None,
                });
            }
        }
        entry.task.abort();
    }
}

/// 正常 / 失败收尾：只在仍存活时发一次 end，并摘除登记。
fn finish(
    runtime: &Arc<PluginRuntime>,
    instance: &Arc<Instance>,
    stream_id: &str,
    alive: &Arc<AtomicBool>,
    error: Option<String>,
    status: Option<u16>,
) {
    if !alive.swap(false, Ordering::SeqCst) {
        return;
    }
    runtime.streams.lock().unwrap().remove(stream_id);
    instance.notify_stream_end(&method::HttpStreamEndParams {
        stream_id: stream_id.to_string(),
        error,
        cancelled: false,
        status,
    });
}
