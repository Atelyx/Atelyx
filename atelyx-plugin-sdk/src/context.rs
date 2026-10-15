//! 能力面上下文：插件侧全部宿主调用的代理。按调用方插件自动绑定插件 id 是宿主侧的事，
//! 上下文不暴露也不接受任何 id 参数。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::connection::PendingMap;
use crate::protocol::envelope::{Envelope, RequestId, RpcError};
use crate::protocol::event::{SubscribeParams, SubscribeResult};
use crate::protocol::method::{self, *};

/// 常规能力调用的默认等待上限（读文件 / 起进程 / 通知等宿主侧可预期的操作）。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 可克隆的调用句柄：读任务与插件业务共享同一 pending 表与 id 计数器。
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

struct Inner {
    out_tx: mpsc::Sender<Envelope>,
    pending: PendingMap,
    next_id: AtomicU64,
}

impl Context {
    pub(crate) fn new(out_tx: mpsc::Sender<Envelope>) -> Self {
        Self {
            inner: Arc::new(Inner {
                out_tx,
                pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// 读任务收场在途调用时取走整表（drain）。
    pub(crate) fn drain_pending(&self) -> std::collections::HashMap<u64, oneshot::Sender<Result<Value, RpcError>>> {
        std::mem::take(&mut *self.inner.pending.lock().unwrap())
    }

    /// 按请求 id 摘除一个在途调用（响应配对用）。
    pub(crate) fn remove_pending(&self, id: u64) -> Option<oneshot::Sender<Result<Value, RpcError>>> {
        self.inner.pending.lock().unwrap().remove(&id)
    }

    /// 发请求、等配对响应，受 [`CALL_TIMEOUT`] 约束。
    ///
    /// 超时 = 本次调用放弃（返回可读错误），宿主侧业务不被取消；其响应迟到时由读端丢弃并留痕
    /// （见连接泵），不会影响连接与其余调用。
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_with_timeout(method, params, CALL_TIMEOUT).await
    }

    /// 同 [`Context::call_raw`]，但等待上限由调用方给定（握手用更短的上限）。
    pub async fn call_with_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let rx = self.begin_call(method, params).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::failed(format!("调用 {method} 失败：连接已结束"))),
            Err(_) => Err(RpcError::failed(format!("调用 {method} 超时（{}s）", timeout.as_secs()))),
        }
    }

    /// 发请求、等配对响应，不设等待上限。用于等待用户交互的方法（对话框）——
    /// 用户停多久不可预知；连接结束或宿主报错仍是唯一的失败出口。
    pub async fn call_unbounded(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let rx = self.begin_call(method, params).await?;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(RpcError::failed(format!("调用 {method} 失败：连接已结束"))),
        }
    }

    /// 登记在途调用并发送请求；响应经返回的通道送达。
    async fn begin_call(&self, method: &str, params: Value) -> Result<oneshot::Receiver<Result<Value, RpcError>>, RpcError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let envelope = Envelope::request(RequestId::Num(id), method, params);
        if self.inner.out_tx.send(envelope).await.is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(RpcError::failed(format!("调用 {method} 失败：连接已结束")));
        }
        Ok(rx)
    }

    /// 定型调用：params 序列化 → call_raw → 应答反序列化，错误统一带方法名。
    async fn call_typed<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> Result<R, RpcError> {
        let value = serde_json::to_value(params)
            .map_err(|e| RpcError::failed(format!("调用 {method} 失败：params 序列化错误（{e}）")))?;
        let raw = self.call_raw(method, value).await?;
        serde_json::from_value(raw).map_err(|e| RpcError::failed(format!("{method} 应答不可解析：{e}")))
    }

    /// 定型调用（无等待上限），语义见 [`Context::call_unbounded`]。
    async fn call_typed_unbounded<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> Result<R, RpcError> {
        let value = serde_json::to_value(params)
            .map_err(|e| RpcError::failed(format!("调用 {method} 失败：params 序列化错误（{e}）")))?;
        let raw = self.call_unbounded(method, value).await?;
        serde_json::from_value(raw).map_err(|e| RpcError::failed(format!("{method} 应答不可解析：{e}")))
    }

    /// 无参调用（params 缺省）。
    async fn call_null<R: DeserializeOwned>(&self, method: &str) -> Result<R, RpcError> {
        let raw = self.call_raw(method, Value::Null).await?;
        serde_json::from_value(raw).map_err(|e| RpcError::failed(format!("{method} 应答不可解析：{e}")))
    }

    // ---------- view / event ----------

    pub async fn submit_view(
        &self,
        surface_id: &str,
        tree: crate::protocol::view::ViewNode,
    ) -> Result<SubmitResult, RpcError> {
        tree.validate().map_err(RpcError::failed)?;
        self.call_typed(method::view::SUBMIT, &SubmitParams { surface_id: surface_id.into(), tree })
            .await
    }

    /// 提交一份画布指令清单（整发整换，空清单 = 清空画布）。目标节点须在当前界面树里
    /// 且为 canvas 节点，否则宿主可读拒绝；指令合法性由宿主校验（结果在错误里带指令下标）。
    pub async fn submit_canvas(
        &self,
        surface_id: &str,
        node_id: &str,
        commands: Vec<crate::protocol::canvas::DrawCommand>,
    ) -> Result<SubmitResult, RpcError> {
        let params = crate::protocol::method::CanvasSubmitParams {
            surface_id: surface_id.into(),
            node_id: node_id.into(),
            commands,
        };
        self.call_typed(method::canvas::SUBMIT, &params).await
    }

    /// 注册一个贡献点，使其出现在宿主对应入口（面板 / 表格视图下拉）。
    /// `id` 插件内唯一且不得含 `:`，同时是该面挂载后的 surfaceId；重复或非法 id 由宿主可读拒绝。
    pub async fn register(
        &self,
        slot: ContributionSlot,
        id: &str,
        label: &str,
        icon: Option<&str>,
    ) -> Result<(), RpcError> {
        let params = RegisterParams {
            slot,
            id: id.into(),
            label: label.into(),
            icon: icon.map(Into::into),
        };
        self.call_unit(method::view::REGISTER, &params).await
    }

    /// 按 id 撤销自己的一个贡献点（未注册的 id 报可读错误）。
    pub async fn unregister(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::view::UNREGISTER, &UnregisterParams { id: id.into() }).await
    }

    /// 请求把本插件的一个贡献面接管到当前聚焦窗口（整页 / 全接管）。`surface_id` 须为已注册的
    /// 贡献 id；退出接管入口由宿主常驻提供，接管期间布局真源不动，退出即恢复原布局。
    pub async fn takeover(&self, scope: TakeoverScope, surface_id: &str) -> Result<(), RpcError> {
        self.call_unit(
            method::view::TAKEOVER,
            &TakeoverParams { scope, surface_id: surface_id.into() },
        )
        .await
    }

    /// 打开一个独立窗口承载自己的界面树：`id` 插件内唯一且不得含 `:`，同时是该窗口面的
    /// surfaceId（树经 [`Context::submit_view`] 提交）。重复 id 或宿主不可开窗由宿主可读拒绝。
    pub async fn window_open(&self, params: WindowOpenParams) -> Result<(), RpcError> {
        self.call_unit(method::window::OPEN, &params).await
    }

    /// 关闭自己打开的一个窗口（未打开的 id 报可读错误）。
    pub async fn window_close(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::CLOSE, &WindowCloseParams { id: id.into() }).await
    }

    /// 声明自己实现某服务契约（候选上岗；是否上岗由用户指定，声明本身不占权限）。
    pub async fn service_register(&self, contract: ServiceContract) -> Result<(), RpcError> {
        self.call_unit(method::service::REGISTER, &ServiceRegisterParams { contract }).await
    }

    /// 撤销自己的服务实现声明（未注册的契约报可读错误）。
    pub async fn service_unregister(&self, contract: ServiceContract) -> Result<(), RpcError> {
        self.call_unit(method::service::UNREGISTER, &ServiceRegisterParams { contract }).await
    }

    /// 回一次搜索查询结果（`error` 非空 = 本次查询失败，宿主按单次回落内置处理）。
    pub async fn search_respond(&self, params: SearchRespondParams) -> Result<(), RpcError> {
        self.call_unit(method::search::RESPOND, &params).await
    }

    pub async fn subscribe(&self, name: &str) -> Result<bool, RpcError> {
        let result: SubscribeResult = self.call_typed(crate::protocol::event::SUBSCRIBE, &SubscribeParams { name: name.into() }).await?;
        Ok(result.subscribed)
    }

    // ---------- fs ----------

    pub async fn private_dir(&self) -> Result<String, RpcError> {
        let result: PrivateDirResult = self.call_null(method::fs::PRIVATE_DIR).await?;
        Ok(result.path)
    }

    pub async fn read_text(&self, path: &str) -> Result<String, RpcError> {
        let result: TextContent = self.call_typed(method::fs::READ_TEXT, &PathParams { path: path.into() }).await?;
        Ok(result.content)
    }

    pub async fn write_text(&self, path: &str, content: &str) -> Result<(), RpcError> {
        self.call_unit(method::fs::WRITE_TEXT, &WriteTextParams { path: path.into(), content: content.into() }).await
    }

    pub async fn read_binary(&self, path: &str) -> Result<Vec<u8>, RpcError> {
        let result: BinaryContent = self.call_typed(method::fs::READ_BINARY, &PathParams { path: path.into() }).await?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(result.base64)
            .map_err(|e| RpcError::failed(format!("fs.readBinary 应答 base64 解码失败：{e}")))
    }

    pub async fn write_binary(&self, path: &str, bytes: &[u8]) -> Result<(), RpcError> {
        use base64::Engine;
        let base64_text = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.call_unit(method::fs::WRITE_BINARY, &WriteBinaryParams { path: path.into(), base64: base64_text }).await
    }

    pub async fn remove(&self, path: &str) -> Result<(), RpcError> {
        self.call_unit(method::fs::REMOVE, &PathParams { path: path.into() }).await
    }

    pub async fn exists(&self, path: &str) -> Result<bool, RpcError> {
        let result: ExistsResult = self.call_typed(method::fs::EXISTS, &PathParams { path: path.into() }).await?;
        Ok(result.exists)
    }

    pub async fn list(&self, path: &str) -> Result<Vec<FsEntry>, RpcError> {
        let result: ListResult = self.call_typed(method::fs::LIST, &PathParams { path: path.into() }).await?;
        Ok(result.entries)
    }

    // ---------- process ----------

    pub async fn spawn(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        env: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<u32, RpcError> {
        let params = SpawnParams {
            program: program.into(),
            args: args.to_vec(),
            cwd: cwd.map(Into::into),
            env: env.cloned(),
        };
        let result: SpawnResult = self.call_typed(method::process::SPAWN, &params).await?;
        Ok(result.pid)
    }

    pub async fn write_stdin(&self, pid: u32, data: &str) -> Result<(), RpcError> {
        self.call_unit(method::process::WRITE_STDIN, &WriteStdinParams { pid, data: data.into() }).await
    }

    pub async fn close_stdin(&self, pid: u32) -> Result<(), RpcError> {
        self.call_unit(method::process::CLOSE_STDIN, &PidParams { pid }).await
    }

    pub async fn kill(&self, pid: u32) -> Result<(), RpcError> {
        self.call_unit(method::process::KILL, &PidParams { pid }).await
    }

    // ---------- state / notification / shortcut ----------

    pub async fn state_read(&self) -> Result<Value, RpcError> {
        let result: StateReadResult = self.call_null(method::state::READ).await?;
        Ok(result.json)
    }

    pub async fn state_write(&self, json: Value) -> Result<(), RpcError> {
        self.call_unit(method::state::WRITE, &StateWriteParams { json }).await
    }

    pub async fn notify(
        &self,
        title: &str,
        body: Option<&str>,
        level: NotificationLevel,
    ) -> Result<(), RpcError> {
        let params = ShowNotificationParams { title: title.into(), body: body.map(Into::into), level };
        self.call_unit(method::notification::SHOW, &params).await
    }

    pub async fn register_shortcut(&self, accelerator: &str) -> Result<(), RpcError> {
        self.call_unit(method::shortcut::REGISTER, &RegisterShortcutParams { accelerator: accelerator.into() }).await
    }

    pub async fn unregister_shortcut(&self, accelerator: &str) -> Result<(), RpcError> {
        self.call_unit(method::shortcut::UNREGISTER, &AcceleratorParams { accelerator: accelerator.into() }).await
    }

    // ---------- clipboard / dialog / http ----------

    pub async fn clipboard_write(&self, text: &str) -> Result<(), RpcError> {
        self.call_unit(method::clipboard::WRITE_TEXT, &TextPayload { text: text.into() }).await
    }

    pub async fn clipboard_read(&self) -> Result<String, RpcError> {
        let result: TextPayload = self.call_null(method::clipboard::READ_TEXT).await?;
        Ok(result.text)
    }

    /// 选择文件：等待用户交互，不设等待上限（取消对话框会立即返回 `None`）。
    pub async fn pick_file(&self, params: PickFileParams) -> Result<Option<String>, RpcError> {
        let result: PickedPath = self.call_typed_unbounded(method::dialog::PICK_FILE, &params).await?;
        Ok(result.path)
    }

    /// 选择目录：等待用户交互，不设等待上限（取消对话框会立即返回 `None`）。
    pub async fn pick_directory(&self, title: Option<&str>) -> Result<Option<String>, RpcError> {
        let params = PickDirectoryParams { title: title.map(Into::into) };
        let result: PickedPath = self.call_typed_unbounded(method::dialog::PICK_DIRECTORY, &params).await?;
        Ok(result.path)
    }

    pub async fn http_request(&self, params: HttpRequestParams) -> Result<HttpResult, RpcError> {
        self.call_typed(method::http::REQUEST, &params).await
    }

    async fn call_unit<P: Serialize>(&self, method: &str, params: &P) -> Result<(), RpcError> {
        let value = serde_json::to_value(params)
            .map_err(|e| RpcError::failed(format!("调用 {method} 失败：params 序列化错误（{e}）")))?;
        self.call_raw(method, value).await.map(|_| ())
    }
}
