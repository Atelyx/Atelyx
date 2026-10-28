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
use crate::protocol::envelope::{Envelope, RpcError};
use crate::protocol::event::{SubscribeParams, SubscribeResult};
use crate::protocol::method::{self, *};
use crate::protocol::view::{PaddingSpec, TextFamily, TextWeight};

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
    ui: crate::ui::Registry,
}

impl Context {
    pub(crate) fn new(out_tx: mpsc::Sender<Envelope>) -> Self {
        Self {
            inner: Arc::new(Inner {
                out_tx,
                pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
                next_id: AtomicU64::new(1),
                ui: crate::ui::Registry::default(),
            }),
        }
    }

    /// 界面树托管句柄：挂「状态 → 树」构建器、按面通知提交（见 [`crate::ui::Ui`]）。
    pub fn ui(&self) -> crate::ui::Ui<'_> {
        crate::ui::Ui::new(self)
    }

    pub(crate) fn ui_registry(&self) -> &crate::ui::Registry {
        &self.inner.ui
    }

    pub(crate) fn ui_slot(&self, surface_id: &str) -> Option<Arc<crate::ui::Slot>> {
        self.inner.ui.lock().unwrap().get(surface_id).cloned()
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
        let envelope = Envelope::request(id, method, params);
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

    /// 界面树的底层提交口（仅供 [`crate::ui`] 的托管循环使用）：插件侧经 `Ui::mount` / `Ui::notify` 提交。
    pub(crate) async fn submit_view(
        &self,
        surface_id: &str,
        tree: crate::protocol::view::ViewNode,
    ) -> Result<SubmitResult, RpcError> {
        tree.validate().map_err(RpcError::failed)?;
        self.call_typed(method::view::SUBMIT, &SubmitParams { surface_id: surface_id.into(), tree })
            .await
    }

    /// 提交一份画布指令清单（整表重建，空清单 = 清空画布）。目标节点须在当前界面树里
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

    /// 按 id 增量维护画布清单：只发变动的条目，`upsert` 命中已有 id 即原位替换、未命中追加到末尾，
    /// `remove` 删除。须先经 [`Context::submit_canvas`] 建立清单（整表提交会丢掉 id 槽）。
    pub async fn patch_canvas(
        &self,
        surface_id: &str,
        node_id: &str,
        upsert: Vec<crate::protocol::canvas::CanvasUpsert>,
        remove: Vec<String>,
    ) -> Result<SubmitResult, RpcError> {
        let params = crate::protocol::method::CanvasPatchParams {
            surface_id: surface_id.into(),
            node_id: node_id.into(),
            upsert,
            remove,
        };
        self.call_typed(method::canvas::PATCH, &params).await
    }

    /// 量算一段文字的占位尺寸：`size` 为绝对字号（与画布 `text` 指令同口径），`max_width`
    /// 给定即按该宽度换行。画布排版按真实字宽摆放，不要按字符数估算。
    /// 纯计算调用，不改清单也不产生帧；同一段文字反复量算时请自行缓存。
    pub async fn measure_text(
        &self,
        text: &str,
        size: f32,
        weight: Option<TextWeight>,
        family: Option<TextFamily>,
        max_width: Option<f32>,
    ) -> Result<MeasureTextResult, RpcError> {
        let params = MeasureTextParams { text: text.into(), size, weight, family, max_width };
        self.call_typed(method::canvas::MEASURE_TEXT, &params).await
    }

    /// 一次量算多段文字（排一屏文本的往返落在单批里，上限见协议 `MAX_MEASURE_ITEMS`）。
    pub async fn measure_batch(
        &self,
        items: Vec<MeasureItem>,
    ) -> Result<MeasureBatchResult, RpcError> {
        let params = MeasureBatchParams { items };
        self.call_typed(method::query::MEASURE, &params).await
    }

    /// 取宿主主题：颜色 / 字号档 / 圆角档随用户设置变化，供插件绘制对齐宿主外观。
    pub async fn query_theme(&self) -> Result<ThemeResult, RpcError> {
        self.call_typed(method::query::THEME, &Value::Null).await
    }

    /// 提交整面帧：整发整换该面的绘制指令、命中区与输入声明（空指令 = 清空绘制）。
    /// 帧与该面的界面树互斥：提交帧后宿主只渲染帧（树存档保留但不渲染，树内画布节拍停掉），
    /// 再提交一次界面树即交还控件通道并撤掉帧。
    pub async fn submit_frame(
        &self,
        surface_id: &str,
        frame: crate::protocol::frame::SurfaceFrame,
    ) -> Result<SubmitResult, RpcError> {
        let params = crate::protocol::frame::FrameSubmitParams {
            surface_id: surface_id.into(),
            frame,
        };
        self.call_typed(method::frame::SUBMIT, &params).await
    }

    /// 注册一个贡献点，使其出现在宿主对应入口（面板 / 表格视图 / 设置页）。
    /// `id` 插件内唯一且不得含 `:`，同时是该面挂载后的 surfaceId；重复或非法 id 由宿主可读拒绝。
    /// `padding` = 面外壳内边距（0 = 贴边；缺省用宿主默认），按四边等距或分轴 / 分边声明。
    pub async fn register(
        &self,
        slot: ContributionSlot,
        id: &str,
        label: &str,
        icon: Option<&str>,
        padding: Option<PaddingSpec>,
    ) -> Result<(), RpcError> {
        let params = RegisterParams {
            slot,
            id: id.into(),
            label: label.into(),
            icon: icon.map(Into::into),
            padding,
        };
        self.call_unit(method::view::REGISTER, &params).await
    }

    /// 定义一枚自备图标形状（24x24 网格描边路径），此后按名字在贡献注册与图标钮里引用。
    /// 名字须为字母 / 数字 / `_` / `-`；同名重复定义即原地更新；随插件会话收尾清除。
    pub async fn define_icon(&self, name: &str, path: &str) -> Result<(), RpcError> {
        let params = DefineIconParams { name: name.into(), path: path.into() };
        self.call_unit(method::view::DEFINE_ICON, &params).await
    }

    /// 把一个可滚动容器（`list` / `scroll`）滚到一端。目标节点须在当前树里且为可滚动容器。
    pub async fn scroll_to(&self, surface_id: &str, node_id: &str, edge: ScrollEdge) -> Result<(), RpcError> {
        let params = ScrollToParams {
            surface_id: surface_id.into(),
            node_id: node_id.into(),
            edge: Some(edge),
            offset: None,
        };
        self.call_unit(method::view::SCROLL_TO, &params).await
    }

    /// 按比例滚动：`offset` 0 = 起点、1 = 终点（越界与内容不足一屏时由宿主钳制）。
    pub async fn scroll_to_offset(&self, surface_id: &str, node_id: &str, offset: f32) -> Result<(), RpcError> {
        let params = ScrollToParams {
            surface_id: surface_id.into(),
            node_id: node_id.into(),
            edge: None,
            offset: Some(offset),
        };
        self.call_unit(method::view::SCROLL_TO, &params).await
    }

    /// 声明本插件的默认布局：宿主在首次启用时摆进当前场景，之后改名 / 删除都不再补回。
    /// 重名由宿主追加序号；规格结构非法由宿主可读拒绝。
    pub async fn declare_default_layout(
        &self,
        name: &str,
        spec: crate::protocol::layout::LayoutSpecNode,
    ) -> Result<(), RpcError> {
        let params = DeclareDefaultLayoutParams { name: name.into(), spec };
        self.call_unit(method::layout::DECLARE_DEFAULT, &params).await
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
    /// surfaceId（树经 `ctx.ui().mount(id, build)` 登记后由 `notify` 提交）。重复 id 或宿主不可开窗由宿主可读拒绝。
    pub async fn window_open(&self, params: WindowOpenParams) -> Result<(), RpcError> {
        self.call_unit(method::window::OPEN, &params).await
    }

    /// 关闭自己打开的一个窗口（未打开的 id 报可读错误）。
    pub async fn window_close(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::CLOSE, &WindowCloseParams { id: id.into() }).await
    }

    /// 运行时改一个已打开窗口的属性（尺寸 / 置顶；至少给一项；「不进任务栏」只能在开窗时声明，
    /// 窗口不存在报可读错误）。
    pub async fn window_update(&self, params: WindowUpdateParams) -> Result<(), RpcError> {
        self.call_unit(method::window::UPDATE, &params).await
    }

    /// 重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
    pub async fn window_show(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::SHOW, &WindowIdParams { id: id.into() }).await
    }

    // ---------- ai ----------

    /// 发起 / 续聊一轮 AI 对话（须在 manifest 声明 `ai` 权限）。回执 = run 已就位，
    /// 流式事件经 [`PluginHandler::on_chat_event`] 流达。模型跟随面板当前选择，Agent 缺省沿用
    /// 会话或内置「对话」；`session_id` 缺省 = 新建会话。
    /// 受 [`CALL_TIMEOUT`] 约束：超时后宿主侧 run 可能已启动（事件照常流达），
    /// 调用方应把超时当作「结果未知」而非「未发送」。
    pub async fn chat_send(&self, text: &str, session_id: Option<&str>) -> Result<ChatSendResult, RpcError> {
        self.chat_send_with(ChatSendParams {
            text: text.into(),
            session_id: session_id.map(Into::into),
            ..ChatSendParams::default()
        })
        .await
    }

    /// [`Self::chat_send`] 的完整参数入口：可另指定 Agent、模型、供应商与仓库内图片。
    /// 显式指定的 Agent / 模型须在当前仓库可用，否则宿主可读拒绝。
    pub async fn chat_send_with(&self, params: ChatSendParams) -> Result<ChatSendResult, RpcError> {
        self.call_typed(method::ai::CHAT_SEND, &params).await
    }

    /// 中断一个会话的活跃 run（幂等：无活跃 run 也成功）。
    pub async fn chat_stop(&self, session_id: &str) -> Result<(), RpcError> {
        self.call_unit(method::ai::CHAT_STOP, &ChatStopParams { session_id: session_id.into() }).await
    }

    /// 读当前仓库的会话清单（`session_id` = `None`）或单会话消息历史。
    /// 单会话模式可用 `offset`（从末尾起跳过的条数）与 `query`（关键词过滤）做分页与搜索。
    pub async fn chat_history(
        &self,
        session_id: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
        query: Option<&str>,
    ) -> Result<ChatHistoryResult, RpcError> {
        self.call_typed(
            method::ai::CHAT_HISTORY,
            &ChatHistoryParams {
                session_id: session_id.map(Into::into),
                limit,
                offset,
                query: query.map(Into::into),
            },
        )
        .await
    }

    /// 读当前仓库可用的模型清单（按供应商分组，只含 id / 名称）与面板当前的模型选择。
    pub async fn ai_models(&self) -> Result<AiModelsResult, RpcError> {
        self.call_null(method::ai::MODELS).await
    }

    /// 读当前仓库可用的 Agent 清单（内置 + 仓库自定义）与面板当前生效的 Agent。
    pub async fn ai_agents(&self) -> Result<AiAgentsResult, RpcError> {
        self.call_null(method::ai::AGENTS).await
    }

    /// 重命名一个会话（写其元数据标题；标题空白或会话不存在报可读错误）。
    pub async fn chat_rename(&self, session_id: &str, title: &str) -> Result<(), RpcError> {
        let params = ChatRenameParams { session_id: session_id.into(), title: title.into() };
        self.call_unit(method::ai::CHAT_RENAME, &params).await
    }

    /// 删除一个会话（连同其消息文件）。
    pub async fn chat_delete(&self, session_id: &str) -> Result<(), RpcError> {
        let params = ChatSessionParams { session_id: session_id.into() };
        self.call_unit(method::ai::CHAT_DELETE, &params).await
    }

    /// 压缩一个会话（把较早消息总结为压缩起点）。回执 = 已受理，结果经 [`Context::chat_history`] 复核。
    pub async fn chat_compact(&self, session_id: &str) -> Result<(), RpcError> {
        let params = ChatSessionParams { session_id: session_id.into() };
        self.call_unit(method::ai::CHAT_COMPACT, &params).await
    }

    /// 注册一个可被 Agent 调用的工具：名称全局唯一（不得与内置工具或其他插件的工具重名），
    /// `parameters` 为 JSON Schema 对象。同一插件重复注册同名工具即原地更新描述与 schema。
    pub async fn register_tool(
        &self,
        name: &str,
        description: &str,
        parameters: Value,
    ) -> Result<(), RpcError> {
        let params = RegisterToolParams {
            name: name.into(),
            description: description.into(),
            parameters,
        };
        self.call_unit(method::ai::REGISTER_TOOL, &params).await
    }

    /// 撤销自己注册的一个工具（未注册报可读错误）。
    pub async fn unregister_tool(&self, name: &str) -> Result<(), RpcError> {
        let params = UnregisterToolParams { name: name.into() };
        self.call_unit(method::ai::UNREGISTER_TOOL, &params).await
    }

    /// 回报一次工具调用结果：`call_id` 原样带回 [`PluginHandler::on_tool_call`] 收到的值，
    /// `content` 为模型可见文本。调用已超时 / 已取消或不归本插件时报可读错误，结果不会
    /// 进入模型上下文——据此区分「已送达」与「迟到被丢弃」。
    pub async fn tool_result(
        &self,
        call_id: &str,
        content: &str,
        is_error: bool,
    ) -> Result<(), RpcError> {
        let params = ToolResultParams {
            call_id: call_id.into(),
            content: content.into(),
            is_error,
        };
        self.call_unit(method::ai::TOOL_RESULT, &params).await
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

    // ---------- vault（当前激活仓库；协作空间由宿主转发服务端） ----------

    pub async fn vault_read_text(&self, path: &str) -> Result<String, RpcError> {
        let result: TextContent =
            self.call_typed(method::vault::READ_TEXT, &PathParams { path: path.into() }).await?;
        Ok(result.content)
    }

    pub async fn vault_write_text(&self, path: &str, content: &str) -> Result<(), RpcError> {
        let params = WriteTextParams { path: path.into(), content: content.into() };
        self.call_unit(method::vault::WRITE_TEXT, &params).await
    }

    /// 读仓库内二进制文件（与文本读写同一出口；单次上限由宿主设定，超限回可读错误）。
    pub async fn vault_read_binary(&self, path: &str) -> Result<Vec<u8>, RpcError> {
        let result: BinaryContent =
            self.call_typed(method::vault::READ_BINARY, &PathParams { path: path.into() }).await?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(result.base64)
            .map_err(|e| RpcError::failed(format!("vault.readBinary 应答 base64 解码失败：{e}")))
    }

    /// 写仓库内二进制文件（父目录自动创建；正在编辑器打开的文件会被宿主拒写）。
    pub async fn vault_write_binary(&self, path: &str, bytes: &[u8]) -> Result<(), RpcError> {
        use base64::Engine;
        let base64_text = base64::engine::general_purpose::STANDARD.encode(bytes);
        let params = WriteBinaryParams { path: path.into(), base64: base64_text };
        self.call_unit(method::vault::WRITE_BINARY, &params).await
    }

    /// 把插件私有目录里的一个文件收进仓库（大文件走这条：字节不进 RPC 载荷）。
    /// `source` 为 [`Self::private_dir`] 下的绝对路径，`path` 为仓库相对目标。
    /// 复制语义——源文件落库后仍在，需清理就用 `fs` 删除；不受单次载荷上限约束，
    /// 也不按默认调用超时判失败（大文件与协作空间上传耗时由宿主实际进展决定）。
    pub async fn vault_import_file(&self, path: &str, source: &str) -> Result<(), RpcError> {
        let params = ImportFileParams { path: path.into(), source: source.into() };
        self.call_typed_unbounded::<_, Value>(method::vault::IMPORT_FILE, &params)
            .await
            .map(|_| ())
    }

    pub async fn vault_list(&self, path: &str) -> Result<Vec<FsEntry>, RpcError> {
        let result: ListResult =
            self.call_typed(method::vault::LIST, &PathParams { path: path.into() }).await?;
        Ok(result.entries)
    }

    pub async fn vault_exists(&self, path: &str) -> Result<bool, RpcError> {
        let result: ExistsResult =
            self.call_typed(method::vault::EXISTS, &PathParams { path: path.into() }).await?;
        Ok(result.exists)
    }

    /// 读图片的固有像素尺寸（png / jpeg / gif / bmp / webp）；只取文件头，不送回文件内容。
    pub async fn vault_image_info(&self, path: &str) -> Result<ImageInfoResult, RpcError> {
        self.call_typed(method::vault::IMAGE_INFO, &PathParams { path: path.into() }).await
    }

    /// 删除仓库内文件或文件夹（文件夹连同内容递归删除；仓库根不可删）。
    /// 被编辑面打开或被当前画布节点引用的文件由宿主拒绝，错误可读。
    pub async fn vault_remove(&self, path: &str) -> Result<(), RpcError> {
        self.call_unit(method::vault::REMOVE, &PathParams { path: path.into() }).await
    }

    /// 改名 / 移动仓库内文件或文件夹：`to` 不可已存在，父目录自动创建。
    /// `.md` 内部链接与画布文件引用随改名同步改写；源文件被编辑面打开或被当前画布节点引用时由宿主拒绝。
    pub async fn vault_rename(&self, from: &str, to: &str) -> Result<(), RpcError> {
        let params = VaultRenameParams { from: from.into(), to: to.into() };
        self.call_unit(method::vault::RENAME, &params).await
    }

    // ---------- table（当前打开表格；写入与宿主编辑同一出口，由宿主合并落盘并广播协作补丁） ----------

    /// 读当前打开表格的只读快照（稳定 id / 路径 / 字段 / 行 / 本端与远端选区）；未打开表格 = `open: false`。
    pub async fn table_snapshot(&self) -> Result<TableSnapshot, RpcError> {
        self.call_null(method::table::SNAPSHOT).await
    }

    /// 逐格写：只改指定字段（不整行替换，不覆盖同行其它字段与并发编辑）；`value` 按字段类型
    /// 定形态，`null` = 清空该格。
    pub async fn table_update_cell(
        &self,
        row_id: &str,
        field_id: &str,
        value: Value,
    ) -> Result<(), RpcError> {
        let params = TableUpdateCellParams { row_id: row_id.into(), field_id: field_id.into(), value };
        self.call_unit(method::table::UPDATE_CELL, &params).await
    }

    /// 新增一行：返回宿主分配的新行 id（随后按它引用该行）。
    pub async fn table_add_row(&self) -> Result<String, RpcError> {
        let result: TableAddRowResult = self.call_null(method::table::ADD_ROW).await?;
        Ok(result.row_id)
    }

    pub async fn table_remove_row(&self, row_id: &str) -> Result<(), RpcError> {
        self.call_unit(method::table::REMOVE_ROW, &TableRowIdParams { row_id: row_id.into() }).await
    }

    /// 把该行设为表格选中行（与网格选中态联动）。
    pub async fn table_select_row(&self, row_id: &str) -> Result<(), RpcError> {
        self.call_unit(method::table::SELECT_ROW, &TableRowIdParams { row_id: row_id.into() }).await
    }

    /// 向图片字段单元格写入图片引用（整格替换）：`images` 为仓库相对路径，须先经
    /// [`Context::vault_write_binary`] 落盘；`display` 为显示形态（缺省保持宿主默认）。
    pub async fn table_set_cell_image(
        &self,
        row_id: &str,
        field_id: &str,
        images: Vec<String>,
        display: Option<&str>,
    ) -> Result<(), RpcError> {
        let params = TableSetCellImageParams {
            row_id: row_id.into(),
            field_id: field_id.into(),
            images,
            display: display.map(Into::into),
        };
        self.call_unit(method::table::SET_CELL_IMAGE, &params).await
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

    /// 在应用内弹一条通知（宿主已收尾、通知未投递时报可读错误）。
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

    /// 事前查询某键位是否可注册：`available` = 可注册，`owner` = 占用方插件 id（被系统 / 其他
    /// 应用占用或无占用时缺省）。
    pub async fn query_shortcut(&self, accelerator: &str) -> Result<ShortcutQueryResult, RpcError> {
        let params = ShortcutQueryParams { accelerator: accelerator.into() };
        self.call_typed(method::shortcut::QUERY, &params).await
    }

    // ---------- tray（插件条目与宿主条目共用一个托盘图标） ----------

    /// 整发整换本插件的托盘菜单（空清单 = 收起）；宿主条目恒在末尾。
    /// 平台不支持托盘时回可读错误（插件应据此降级，不重试）。
    pub async fn tray_set_items(&self, items: Vec<TrayItemSpec>) -> Result<(), RpcError> {
        self.call_unit(method::tray::SET_ITEMS, &SetTrayItemsParams { items }).await
    }

    // ---------- collab（同空间的同插件实例互通；通道按插件命名空间隔离） ----------

    /// 当前空间连接状态与在场成员（不含自己）。
    pub async fn collab_peers(&self) -> Result<CollabPeersResult, RpcError> {
        self.call_null(method::collab::PEERS).await
    }

    /// 向同空间对等插件发消息（可靠档：广播帧可补投）：`target_peer_id` 缺省 = 广播给全部对端。
    /// `queued` = 已交本机连接发出（不代表送达）；未连接空间时 `queued = false`。
    pub async fn collab_send(
        &self,
        channel: &str,
        payload: Value,
        target_peer_id: Option<&str>,
    ) -> Result<CollabSendResult, RpcError> {
        self.collab_send_with(channel, payload, target_peer_id, None).await
    }

    /// 瞬时档：不缓存、不补投、不占房间序号，适合光标 / 选中这类随时被新状态覆盖的在场数据。
    /// 对端断开期间发出的消息不会在重连后补达。
    pub async fn collab_send_ephemeral(
        &self,
        channel: &str,
        payload: Value,
        target_peer_id: Option<&str>,
    ) -> Result<CollabSendResult, RpcError> {
        self.collab_send_with(channel, payload, target_peer_id, Some(CollabSendQos::Ephemeral))
            .await
    }

    async fn collab_send_with(
        &self,
        channel: &str,
        payload: Value,
        target_peer_id: Option<&str>,
        qos: Option<CollabSendQos>,
    ) -> Result<CollabSendResult, RpcError> {
        let params = CollabSendParams {
            channel: channel.into(),
            payload,
            target_peer_id: target_peer_id.map(Into::into),
            qos,
        };
        self.call_typed(method::collab::SEND, &params).await
    }

    // ---------- clipboard / dialog / http ----------

    pub async fn clipboard_write(&self, text: &str) -> Result<(), RpcError> {
        self.call_unit(method::clipboard::WRITE_TEXT, &TextPayload { text: text.into() }).await
    }

    pub async fn clipboard_read(&self) -> Result<String, RpcError> {
        let result: TextPayload = self.call_null(method::clipboard::READ_TEXT).await?;
        Ok(result.text)
    }

    /// 读剪贴板图片（PNG 字节在 `base64` 字段，需自行解码；`width` / `height` 为像素尺寸）；
    /// 剪贴板里没有图片 = `None`。
    pub async fn clipboard_read_image(&self) -> Result<Option<ClipboardImage>, RpcError> {
        let result: ClipboardImageResult = self.call_null(method::clipboard::READ_IMAGE).await?;
        Ok(result.image)
    }

    /// 写一张 PNG 图片到剪贴板；`png` 为完整 PNG 字节。单次上限 32 MiB，
    /// 解码失败或超限由宿主可读拒绝。
    pub async fn clipboard_write_image(&self, png: &[u8]) -> Result<(), RpcError> {
        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(png);
        self.call_unit(method::clipboard::WRITE_IMAGE, &ClipboardWriteImageParams { base64 }).await
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

    /// 出网请求：等待上限由 params 的 `timeoutMs` 决定（宿主据此设请求超时），SDK 不另设等待
    /// 上限——宿主不对 http 施加地址 / 大小 / 超时限制，超时完全由插件自负其责。
    pub async fn http_request(&self, params: HttpRequestParams) -> Result<HttpResult, RpcError> {
        self.call_typed_unbounded(method::http::REQUEST, &params).await
    }

    async fn call_unit<P: Serialize>(&self, method: &str, params: &P) -> Result<(), RpcError> {
        let value = serde_json::to_value(params)
            .map_err(|e| RpcError::failed(format!("调用 {method} 失败：params 序列化错误（{e}）")))?;
        self.call_raw(method, value).await.map(|_| ())
    }
}
