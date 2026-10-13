//! 端到端对练：测试侧扮演宿主桩，拉起真实 echo 插件进程走完整协议——
//! 握手、订阅与首次提交、节点事件回写、宿主广播 → state 读写往返 → 再提交、EOF 收场。

use std::time::Duration;

use atelyx_plugin_sdk::protocol::envelope::{Envelope, RequestId};
use atelyx_plugin_sdk::protocol::handshake::{InitializeResult, INITIALIZE};
use atelyx_plugin_sdk::protocol::method::{self, SubmitParams};
use atelyx_plugin_sdk::protocol::{event, API_VERSION};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const TIMEOUT: Duration = Duration::from_secs(5);

/// 宿主桩：逐行读插件输出，按方法名应答或转发到断言通道。
struct Stub {
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
}

impl Stub {
    async fn send(&mut self, envelope: &Envelope) {
        let mut line = envelope.to_line();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.expect("写入插件 stdin");
        self.stdin.flush().await.expect("flush 插件 stdin");
    }

    async fn next_line(&mut self) -> String {
        let mut buf = String::new();
        tokio::time::timeout(TIMEOUT, self.reader.read_line(&mut buf))
            .await
            .expect("等插件输出超时")
            .expect("读插件输出失败");
        assert!(!buf.is_empty(), "插件 stdout 意外结束");
        buf
    }

    /// 读到下一条指定方法的请求，返回 (请求 id, params)；应答必须回显该 id。
    async fn expect_request(&mut self, method: &str) -> (RequestId, Value) {
        let line = self.next_line().await;
        let envelope = Envelope::parse(line.trim_end()).expect("插件输出应为合法信封");
        match envelope {
            Envelope::Request(request) => {
                assert_eq!(request.method, method, "应等待 {method}");
                (request.id, request.params)
            }
            other => panic!("应收到请求 {method}，实际：{other:?}"),
        }
    }

    async fn reply_ok(&mut self, id: RequestId, result: Value) {
        self.send(&Envelope::success(id, result)).await;
    }
}

/// 拉起 echo 进程并完成握手（stderr 不接管，直接进测试输出便于失败诊断）。
async fn spawn_echo() -> (Child, Stub) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_echo"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("拉起 echo 插件失败");
    let stub = Stub {
        stdin: child.stdin.take().expect("echo stdin"),
        reader: BufReader::new(child.stdout.take().expect("echo stdout")),
    };
    (child, stub)
}

/// 完成 initialize 握手并断言插件自报信息。
async fn handshake(stub: &mut Stub) {
    let line = stub.next_line().await;
    let request = match Envelope::parse(line.trim_end()).expect("握手应为合法信封") {
        Envelope::Request(r) => r,
        other => panic!("应收到握手请求，实际：{other:?}"),
    };
    assert_eq!(request.method, INITIALIZE);
    assert_eq!(request.params["apiVersion"], API_VERSION);
    assert_eq!(request.params["plugin"]["id"], "com.atelyx.echo");
    let result = InitializeResult {
        api_version: API_VERSION,
        host: atelyx_plugin_sdk::protocol::handshake::HostInfo {
            name: "AtelyxStub".into(),
            version: "0.0.0".into(),
        },
    };
    stub.reply_ok(request.id, serde_json::to_value(&result).unwrap()).await;
}

/// 消费订阅与首次提交（顺序不敏感，按方法名分流），返回首棵树。
async fn consume_initial(stub: &mut Stub) -> SubmitParams {
    let mut subscribed: Vec<String> = Vec::new();
    // 订阅条数由插件自定（当前为 echo.ping 与 vault:switch 两条），只以「收到首棵界面树」为终点
    loop {
        let line = stub.next_line().await;
        let request = match Envelope::parse(line.trim_end()).expect("初始请求应为合法信封") {
            Envelope::Request(r) => r,
            other => panic!("应收到初始请求，实际：{other:?}"),
        };
        match request.method.as_str() {
            m if m == event::SUBSCRIBE => {
                subscribed.push(request.params["name"].as_str().unwrap_or_default().to_string());
                stub.reply_ok(request.id, json!({"subscribed": true})).await;
            }
            m if m == method::view::SUBMIT => {
                let params: SubmitParams = serde_json::from_value(request.params).expect("提交 params 应定型");
                params.tree.validate().expect("首棵树应通过校验");
                stub.reply_ok(request.id, json!({"applied": true})).await;
                assert!(
                    subscribed.iter().any(|name| name == "echo.ping"),
                    "应订阅 echo.ping，实际：{subscribed:?}"
                );
                return params;
            }
            other => panic!("未知初始请求：{other}"),
        }
    }
}

/// 等到下一条 view.submit，回显其请求 id，返回 params。
async fn next_submit(stub: &mut Stub) -> SubmitParams {
    let (id, params) = stub.expect_request(method::view::SUBMIT).await;
    let parsed: SubmitParams = serde_json::from_value(params).expect("提交 params 应定型");
    parsed.tree.validate().expect("树应通过校验");
    stub.reply_ok(id, json!({"applied": true})).await;
    parsed
}

fn status_text(tree: &SubmitParams) -> String {
    let node = tree
        .tree
        .children
        .iter()
        .find(|n| n.id == "status")
        .expect("树里应有 status 文本节点");
    match &node.body {
        atelyx_plugin_sdk::protocol::view::NodeBody::Text { content, .. } => content.clone(),
        other => panic!("status 应为 text 节点，实际：{other:?}"),
    }
}

/// 主回环：握手 → 初始提交 → 点击事件回写 → 宿主广播经 state 读写往返回显 → EOF 收场。
#[tokio::test]
async fn full_roundtrip_with_echo_plugin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_echo"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("拉起 echo 插件失败");
    let mut stub = Stub {
        stdin: child.stdin.take().expect("echo stdin"),
        reader: BufReader::new(child.stdout.take().expect("echo stdout")),
    };

    handshake(&mut stub).await;
    let first = consume_initial(&mut stub).await;
    assert_eq!(status_text(&first), "就绪", "首棵树状态文本");

    // 节点事件：按钮点击 → 整树重发，状态文本带计数
    stub.send(&Envelope::notification(
        method::view::HOST_EVENT,
        json!({"surfaceId": "main", "nodeId": "go", "event": {"kind": "click"}}),
    ))
    .await;
    let after_click = next_submit(&mut stub).await;
    assert_eq!(status_text(&after_click), "点击 1 次");

    // 宿主广播 → state 写读往返 → 回显到界面
    stub.send(&Envelope::notification(
        event::HOST_PUBLISH,
        json!({"name": "echo.ping", "payload": "hello"}),
    ))
    .await;
    let (id, write) = stub.expect_request(method::state::WRITE).await;
    assert_eq!(write["json"]["pong"], "hello", "state.write 应携带广播 payload");
    stub.reply_ok(id, json!({})).await;
    let (id, read) = stub.expect_request(method::state::READ).await;
    assert!(read.is_null(), "state.read 无参数");
    stub.reply_ok(id, json!({"json": {"pong": "hello"}})).await;
    let after_ping = next_submit(&mut stub).await;
    assert_eq!(status_text(&after_ping), "回声：\"hello\"", "回显应与读回值一致");

    // EOF 收场：丢弃桩（stdin 句柄随 drop 关闭，shutdown 不等于关管道）→ 插件正常退出
    drop(stub);
    let status = tokio::time::timeout(TIMEOUT, child.wait())
        .await
        .expect("等插件退出超时")
        .expect("wait 失败");
    assert!(status.success(), "插件应正常退出，实际：{status:?}");
}

/// 容器切换事件：插件按 `PublishParams` 收到并把 payload 落进自持数据（宿主端到端对拍据此判定
/// 事件确已送达，形状由协议 `VaultSwitchPayload` 定义）。
#[tokio::test]
async fn vault_switch_event_reaches_fixture_as_typed_payload() {
    let (mut child, mut stub) = spawn_echo().await;
    handshake(&mut stub).await;
    consume_initial(&mut stub).await;

    stub.send(&Envelope::notification(
        event::HOST_PUBLISH,
        json!({"name": "vault:switch", "payload": {"kind": "space", "spaceId": "s1"}}),
    ))
    .await;
    let (id, write) = stub.expect_request(method::state::WRITE).await;
    let payload: event::VaultSwitchPayload = serde_json::from_value(write["json"]["vault_switch"].clone())
        .expect("广播载荷应按 VaultSwitchPayload 定型");
    assert_eq!(payload, event::VaultSwitchPayload::Space { space_id: "s1".into() });
    stub.reply_ok(id, json!({})).await;

    drop(stub);
    let status = tokio::time::timeout(TIMEOUT, child.wait())
        .await
        .expect("等插件退出超时")
        .expect("wait 失败");
    assert!(status.success(), "插件应正常退出，实际：{status:?}");
}

/// 协议违规断链：插件收到宿主请求（协议约定只有通知）应退出而非带病运行。
#[tokio::test]
async fn host_request_breaks_connection() {
    let (mut child, mut stub) = spawn_echo().await;
    handshake(&mut stub).await;
    consume_initial(&mut stub).await;
    stub.send(&Envelope::request(RequestId::Num(99), "fs.readText", json!({"path": "x"}))).await;
    let status = tokio::time::timeout(TIMEOUT, child.wait())
        .await
        .expect("等插件退出超时")
        .expect("wait 失败");
    assert!(!status.success(), "收到宿主请求后插件应非零退出，实际：{status:?}");
}

/// 版本不一致响亮失败：宿主回更高协议版本，插件立即退出。
#[tokio::test]
async fn version_mismatch_fails_loudly() {
    let (mut child, mut stub) = spawn_echo().await;
    let line = stub.next_line().await;
    let request = match Envelope::parse(line.trim_end()).expect("握手应为合法信封") {
        Envelope::Request(r) => r,
        other => panic!("应收到握手请求，实际：{other:?}"),
    };
    let result = InitializeResult {
        api_version: API_VERSION + 1,
        host: atelyx_plugin_sdk::protocol::handshake::HostInfo {
            name: "AtelyxStub".into(),
            version: "9.9.9".into(),
        },
    };
    stub.reply_ok(request.id, serde_json::to_value(&result).unwrap()).await;
    let status = tokio::time::timeout(TIMEOUT, child.wait())
        .await
        .expect("等插件退出超时")
        .expect("wait 失败");
    assert!(!status.success(), "版本不一致应非零退出，实际：{status:?}");
}
