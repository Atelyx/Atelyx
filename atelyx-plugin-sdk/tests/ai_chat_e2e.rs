//! AI 对话契约端到端：以真实插件进程（`ai-stub` 夹具）验证 `ai.chat.send` 请求路由、
//! run 循环对接本地假 SSE 供应商、`host:ai.chat.event` 快照流达插件、真源落盘的完整链路。

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atelyx_core::ai::agent::{AgentHub, RunConfig, RunEvent};
use atelyx_core::ai::session::{AppendBatch, CommitBatch, SessionDef, SessionStore, WriteChainConfig};
use atelyx_core::ai::tools::{SearchConfig, ToolCtx};
use atelyx_core::plugin_runtime::{HostRequest, PluginRuntime, PluginStatus};
use atelyx_core::vault::test_support::TempDir;
use atelyx_plugin_sdk::protocol::manifest::MANIFEST_FILE;
use serde_json::{json, Value};

/// 当前平台的可执行入口名（与 manifest 的 entry 映射一致）。
fn entry_name() -> &'static str {
    if cfg!(windows) { "plugin.exe" } else { "plugin" }
}

/// 本地假 OpenAI SSE 供应商：接受一次对话请求，按分片回一段固定文本后 `[DONE]`。
/// 返回（监听地址、关闭用守卫——drop 即断监听）。
fn spawn_fake_sse(reply: &'static str) -> (String, std::net::TcpListener) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地端口应成功");
    let addr = listener.local_addr().unwrap().to_string();
    let accepted = listener.try_clone().expect("克隆监听器应成功");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = accepted.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let mut body = String::new();
            // 正文分片 + finish_reason 收束 + [DONE]（覆盖流式解析的最小真实形态）
            body.push_str(&format!(
                "data: {}\n\n",
                json!({ "choices": [{ "delta": { "content": reply } }] })
            ));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] })
            ));
            body.push_str("data: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}/v1"), listener)
}

/// 轮询等插件达到期望状态（超时即失败）。
fn wait_status(runtime: &PluginRuntime, id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.status)
            .unwrap_or(PluginStatus::Stopped);
        if status == PluginStatus::Running {
            return;
        }
        assert!(Instant::now() < deadline, "等待插件 {id} 运行超时，最后状态：{status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 轮询等自持数据满足条件并返回该 JSON（超时即失败）。
fn wait_state(path: &std::path::Path, want: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if want(&value) {
                    return value;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "等待 {path:?} 满足条件超时，实际：{}",
            std::fs::read_to_string(path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 对话契约全链路：插件 send 请求路由到宿主 → 编排起 run（假 SSE 供应商）→
/// 回执 sessionId/messageId → 快照与终态事件流达插件 → 会话真源含模型回复。
/// 多线程运行时：run 循环与事件转发是 spawn 的任务，单线程运行时会被轮询阻塞饿死。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_contract_roundtrip_via_plugin() {
    let reply: &'static str = Box::leak("你好，回复完毕".to_string().into_boxed_str());
    let (base_url, _listener) = spawn_fake_sse(reply);

    let tmp = TempDir::new("plugin-ai-chat-e2e");
    let root = tmp.join("plugins");
    let runtime = PluginRuntime::new(root.clone());

    // 安装 ai-stub：权限 = state（自持数据）+ ai（对话契约）
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join(MANIFEST_FILE),
        r#"{"id":"com.atelyx.ai-stub","name":"AiStub","version":"1.0.0","apiVersion":1,
            "entry":{"windows":"plugin.exe","linux":"plugin"},"permissions":["state","ai"]}"#,
    )
    .unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_ai_stub"), src.join(entry_name())).expect("拷贝入口失败");
    let id = runtime.install_dir(&src).expect("安装 ai-stub 应成功");
    runtime.set_enabled(&id, true).expect("启用应成功");
    wait_status(&runtime, &id, Duration::from_secs(20));

    // 会话真源就位（仓库装载 + 一个已存在会话，供 send 路由校验与落盘断言）
    let vault = tmp.join("vault");
    let store = Arc::new(SessionStore::new(WriteChainConfig::default()));
    store.spawn_writer();
    store.load(&vault).expect("装载空仓库应成功");
    let session_id = "e2e-session";
    store
        .commit(CommitBatch {
            created: vec![SessionDef {
                id: session_id.into(),
                file: atelyx_core::ai::session::chat_message_file(session_id),
                title: None,
                agent_id: None,
                compaction: None,
                created_at: 1,
                updated_at: 1,
                messages: Vec::new(),
            }],
            ..Default::default()
        })
        .expect("预置会话应成功");

    // 取插件 send 请求（orchestrator 模拟壳侧编排：续聊指定会话）
    let request = tokio::time::timeout(Duration::from_secs(20), runtime.recv_host_request())
        .await
        .expect("等待插件对话请求超时")
        .expect("宿主请求通道不应关闭");
    let HostRequest::ChatSend { request_id, params, .. } = request else {
        panic!("应收到 ChatSend 请求");
    };
    assert_eq!(params.text, "你好");

    // 编排：user 消息落真源 → hub.start（历史 = 已有消息 + user）→ 回执
    let mut history = store.session_messages(session_id).unwrap_or_default();
    history.push(json!({
        "id": "u1",
        "role": "user",
        "content": params.text,
        "displayContent": params.text,
        "createdAt": 2,
    }));
    store
        .commit(CommitBatch {
            appends: vec![AppendBatch { session_id: session_id.into(), messages: history.clone() }],
            ..Default::default()
        })
        .expect("user 消息应成功");
    let hub = Arc::new(AgentHub::new());
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let run_session = session_id.to_string();
    let start = {
        let events = events_tx.clone();
        let run_store = Arc::clone(&store);
        let run_session = run_session.clone();
        let history = history.clone();
        async move {
            AgentHub::start(
                &hub,
                run_store,
                events,
                run_session,
                None,
                RunConfig {
                    base_url,
                    api_key: "test-key".into(),
                    model: "test-model".into(),
                    reasoning_effort: None,
                    system_prompt: None,
                    tool_ids: Vec::new(),
                    search: SearchConfig::default(),
                    exclude: Vec::new(),
                },
                history,
                ToolCtx {
                    root: vault.clone(),
                    queue: Default::default(),
                    search: SearchConfig::default(),
                    exclude: Vec::new(),
                },
            )
            .await
        }
    };
    let message_id = start.await.expect("run 启动应成功");
    runtime.resolve_chat_call(
        request_id,
        Ok(json!({ "sessionId": session_id, "messageId": message_id })),
    );

    // 事件转发（与壳侧编排同形：快照拍按事件直发、终态直发）
    let runtime2 = Arc::clone(&runtime);
    let plugin_id = id.clone();
    let fwd_store = Arc::clone(&store);
    let fwd_session = session_id.to_string();
    tokio::spawn(async move {
        let mut rev = 0u64;
        while let Some(event) = events_rx.recv().await {
            let message_id = match &event {
                RunEvent::Started { message_id, .. }
                | RunEvent::TextDelta { message_id, .. }
                | RunEvent::ReasoningDelta { message_id, .. }
                | RunEvent::ToolStart { message_id, .. }
                | RunEvent::ToolEnd { message_id, .. }
                | RunEvent::Finished { message_id, .. }
                | RunEvent::Failed { message_id, .. } => message_id.clone(),
            };
            let chat_event = match &event {
                RunEvent::Finished { reason, .. } => {
                    atelyx_core::plugin_runtime::ChatEvent::Finished { reason: reason.clone() }
                }
                RunEvent::Failed { error, .. } => {
                    atelyx_core::plugin_runtime::ChatEvent::Failed { error: error.clone() }
                }
                _ => {
                    let Some(message) = fwd_store
                        .session_messages(&fwd_session)
                        .and_then(|ms| {
                            ms.into_iter().find(|m| {
                                m.get("id").and_then(Value::as_str) == Some(message_id.as_str())
                            })
                        })
                    else {
                        continue;
                    };
                    rev += 1;
                    atelyx_core::plugin_runtime::ChatEvent::Message { rev, message }
                }
            };
            runtime2.notify_chat_event(
                &plugin_id,
                &atelyx_core::plugin_runtime::ChatEventParams {
                    session_id: fwd_session.clone(),
                    message_id: message_id.clone(),
                    event: chat_event,
                },
            );
            if matches!(event, RunEvent::Finished { .. } | RunEvent::Failed { .. }) {
                break;
            }
        }
    });

    // 插件自持数据收口：快照正文 = 模型回复，终态 = finished
    let state_path = root.join(&id).join("data").join("state.json");
    let state = wait_state(&state_path, |v| {
        v.get("content").and_then(Value::as_str).map(|c| c.contains(reply)) == Some(true)
            && v.get("finished").and_then(Value::as_str) == Some("stop")
    });
    assert_eq!(state["sessionId"], json!(session_id), "回执的会话 id 应在事件流中一致");

    // 真源落盘：会话消息含模型回复（模型可见即已记录）
    let _ = events_tx;
    store.flush().await.expect("落盘应成功");
    let messages = store.session_messages(session_id).unwrap();
    let assistant = messages
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(message_id.as_str()))
        .expect("会话里应有本轮 assistant 消息");
    assert_eq!(assistant["content"], json!(reply));

    // 停用即收尾：在途调用失败收场（此处无在途），进程结束
    runtime.set_enabled(&id, false).expect("停用应成功");
}
