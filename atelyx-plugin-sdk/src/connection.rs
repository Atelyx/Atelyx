//! 连接泵：握手、NDJSON 读写循环、响应按 id 配对、宿主通知分发。
//! 宿主→插件只有通知（无响应），插件→宿主一律请求；读端收到请求即协议违规并断链。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::context::Context;
use crate::handler::{dispatch, PluginHandler};
use crate::protocol::envelope::{Envelope, Notification, RpcError};
use crate::protocol::handshake::{self, InitializeParams, InitializeResult, PluginInfo};
use crate::protocol::API_VERSION;

/// 握手应答等待上限：插件进程启动到宿主应答之间的合理上限，超时即断链。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("握手失败：{0}")]
    Handshake(String),
    #[error("协议版本不一致：插件 {plugin}，宿主 {host}")]
    VersionMismatch { plugin: u32, host: u32 },
    #[error("帧共享内存不可用：{0}")]
    FrameRegion(String),
    #[error("连接异常结束：{0}")]
    Abnormal(String),
}

/// 连接结束的两种形态：宿主正常关闭（EOF）与异常结束（协议违规 / 读写失败，携带可读原因）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConnectionOutcome {
    PeerClosed,
    Abnormal(String),
}

pub(crate) type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// 与宿主的活跃连接。读任务在 stdin EOF（正常）或协议违规 / 读写失败（异常）时结束，
/// [`Connection::closed`] 带结果完成；异常结束使 [`run_stdio`] 返回错误，插件以非零码退出（失败可见）。
pub struct Connection {
    closed_rx: oneshot::Receiver<ConnectionOutcome>,
}

impl Connection {
    /// 以给定流建立连接并完成握手。返回连接与能力面上下文；插件随后的全部交互经 `ctx`。
    /// 握手失败或帧共享内存映射失败都直接返回错误，此时读写任务与通知分发任务都已启动——
    /// 错误被吞掉重试会泄漏持有流的任务；常规用法（[`run_stdio`]）失败即进程退出，不存在重试。
    pub async fn bind<R, W, H>(input: R, output: W, plugin: PluginInfo, handler: Arc<H>) -> Result<(Connection, Context), ConnectError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
        H: crate::handler::PluginHandler + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<Envelope>(64);
        let ctx = Context::new(out_tx.clone());
        spawn_writer(output, out_rx);

        // 读任务必须先于握手启动：握手应答本身要靠读任务配对回送，先后颠倒即互等死锁
        let (closed_tx, closed_rx) = oneshot::channel();
        let notify_tx = spawn_notify_worker(handler.clone(), ctx.clone());
        spawn_reader(input, ctx.clone(), notify_tx, closed_tx);

        let result = Self::handshake(&ctx, &plugin).await?;
        // 首帧之前映射必须就绪：接不上就拒绝启动，不让插件带着「提交必失败」跑起来
        ctx.attach_frame_region(result.frame_region)
            .map_err(ConnectError::FrameRegion)?;
        Ok((Self { closed_rx }, ctx))
    }

    /// 连接结束（宿主侧关闭 / 协议违规 / 读写失败）时完成并给出结论。
    pub async fn closed(self) -> Result<(), ConnectError> {
        match self.closed_rx.await {
            Ok(ConnectionOutcome::PeerClosed) => Ok(()),
            Ok(ConnectionOutcome::Abnormal(reason)) => Err(ConnectError::Abnormal(reason)),
            // 通道关闭只可能是读任务异常消失（panic），按异常收场而非伪装成正常关闭
            Err(_) => Err(ConnectError::Abnormal("读任务异常结束".into())),
        }
    }

    async fn handshake(ctx: &Context, plugin: &PluginInfo) -> Result<InitializeResult, ConnectError> {
        let params = InitializeParams { api_version: API_VERSION, plugin: plugin.clone() };
        let value = serde_json::to_value(&params).expect("握手 params 序列化失败");
        let raw = ctx
            .call_with_timeout(handshake::INITIALIZE, value, HANDSHAKE_TIMEOUT)
            .await
            .map_err(|e| ConnectError::Handshake(format!("{e}")))?;
        let result: InitializeResult =
            serde_json::from_value(raw).map_err(|e| ConnectError::Handshake(format!("应答不可解析：{e}")))?;
        if !handshake::compatible(API_VERSION, result.api_version) {
            return Err(ConnectError::VersionMismatch {
                plugin: API_VERSION,
                host: result.api_version,
            });
        }
        Ok(result)
    }
}

/// 写任务：通道逐封送行、行内 flush。通道关闭即任务结束（EOF 交给读端感知）。
fn spawn_writer<W: AsyncWrite + Unpin + Send + 'static>(mut output: W, mut rx: mpsc::Receiver<Envelope>) {
    tokio::spawn(async move {
        while let Some(envelope) = rx.recv().await {
            let mut line = envelope.to_line();
            line.push('\n');
            if output.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if output.flush().await.is_err() {
                break;
            }
        }
    });
}

/// 通知分发任务：按到达顺序逐个执行回调，插件不必自己加锁维持顺序。
///
/// 读循环只负责入队，因此单个回调卡住（比如在回调里等宿主应答）既不拖住读取，也不会与响应
/// 配对互等死锁——配对仍在读循环上。代价是队头阻塞：一个慢回调会挡住其后全部通知。
/// 连接结束后不保证队列排空——宿主已关闭，剩下的通知没有执行意义，主进程随即可退出。
fn spawn_notify_worker<H: PluginHandler + 'static>(
    handler: Arc<H>,
    ctx: Context,
) -> mpsc::UnboundedSender<Notification> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Notification>();
    tokio::spawn(async move {
        while let Some(notification) = rx.recv().await {
            dispatch(&*handler, &ctx, &notification.method, notification.params).await;
        }
    });
    tx
}

/// 读任务：EOF 或协议违规即结束并收场在途调用；通知交给分发任务按序执行。
fn spawn_reader<R>(
    input: R,
    ctx: Context,
    notify_tx: mpsc::UnboundedSender<Notification>,
    closed_tx: oneshot::Sender<ConnectionOutcome>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let outcome = pump(input, &ctx, &notify_tx).await;
        // 在途调用全部收场：连接已死，不能让调用方悬等
        for (_, sender) in ctx.drain_pending() {
            let reason = match &outcome {
                ConnectionOutcome::PeerClosed => "宿主关闭了连接".to_string(),
                ConnectionOutcome::Abnormal(reason) => reason.clone(),
            };
            let _ = sender.send(Err(RpcError::failed(format!("连接已结束（{reason}）"))));
        }
        let _ = closed_tx.send(outcome);
    });
}

async fn pump<R>(
    input: R,
    ctx: &Context,
    notify_tx: &mpsc::UnboundedSender<Notification>,
) -> ConnectionOutcome
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(input);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => return ConnectionOutcome::PeerClosed,
            Ok(_) => {}
            Err(e) => return ConnectionOutcome::Abnormal(format!("读取失败：{e}")),
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            continue;
        }
        let envelope = match Envelope::parse(line) {
            Ok(e) => e,
            Err(e) => return ConnectionOutcome::Abnormal(format!("信封解析失败：{e}")),
        };
        match envelope {
            Envelope::Response(response) => {
                // 配对不到条目的响应 = 调用方已超时放弃：迟到结果丢弃并留痕，不断链
                // （id 单调不复用，不存在误配对的歧义；断链反而是对一次慢调用的不成比例惩罚）
                match ctx.remove_pending(response.id) {
                    Some(sender) => {
                        let _ = sender.send(match (response.result, response.error) {
                            (Some(result), _) => Ok(result),
                            (None, Some(error)) => Err(error),
                            (None, None) => Err(RpcError::failed("响应缺少 result 与 error")),
                        });
                    }
                    None => eprintln!("丢弃迟到的响应（调用已超时）：id = {}", response.id),
                }
            }
            Envelope::Notification(notification) => {
                if notify_tx.send(notification).is_err() {
                    return ConnectionOutcome::Abnormal("通知分发任务已结束".to_string());
                }
            }
            // 协议约定宿主→插件只有通知；收到请求说明对端不讲这套协议，断链比带病运行可靠
            Envelope::Request(request) => {
                return ConnectionOutcome::Abnormal(format!(
                    "收到宿主请求（协议约定宿主→插件只有通知）：{}",
                    request.method
                ));
            }
        }
    }
}

/// 标准输入输出入口：插件主函数用这个跑。连接异常结束（协议违规 / 读写失败）返回错误，
/// 插件应以非零码退出；宿主正常关闭连接返回 `Ok`。
pub async fn run_stdio<H: PluginHandler>(plugin: PluginInfo, handler: H) -> Result<(), ConnectError> {
    let handler = Arc::new(handler);
    let (connection, ctx) = Connection::bind(tokio::io::stdin(), tokio::io::stdout(), plugin, handler.clone()).await?;
    handler.on_ready(&ctx).await;
    connection.closed().await
}
