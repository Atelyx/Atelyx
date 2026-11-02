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
use crate::protocol::method::PaddingSpec;
use crate::protocol::service;

/// 常规能力调用的默认等待上限（读文件 / 起进程 / 通知等宿主侧可预期的操作）。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 出帧路径上的调用上限（量算 / Markdown 解析）：宿主正等这一帧时会与插件互相卡住，
/// 上限必须短到「卡一下也要出帧」，按估算排一版继续走。
pub const RENDER_CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// 可克隆的调用句柄：读任务与插件业务共享同一 pending 表与 id 计数器。
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

/// 同进程宿主的帧提交：帧在这一层直接编成 ABI 指令，不落共享内存。
pub(crate) type FrameSubmit =
    Arc<dyn Fn(&str, crate::protocol::frame::SurfaceFrame) -> Result<SubmitResult, RpcError> + Send + Sync>;

/// 同进程宿主的服务回执：`call_id` 来自宿主发起的服务调用，直接走 ABI 回执。
pub(crate) type ServiceReply =
    Arc<dyn Fn(&str, bool, &str) -> Result<(), RpcError> + Send + Sync>;

/// 同进程宿主的日志落点（宿主写进插件运行日志）。
pub(crate) type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// 只持有在途调用表的句柄：同进程泵用它回填宿主应答，不拖住发送端关闭。
#[derive(Clone)]
pub(crate) struct PendingHandle {
    pending: PendingMap,
}

impl PendingHandle {
    pub(crate) fn remove(&self, id: u64) -> Option<oneshot::Sender<Result<Value, RpcError>>> {
        self.pending.lock().unwrap().remove(&id)
    }

    pub(crate) fn drain(&self) -> std::collections::HashMap<u64, oneshot::Sender<Result<Value, RpcError>>> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
}

struct Inner {
    out_tx: mpsc::Sender<Envelope>,
    pending: PendingMap,
    next_id: AtomicU64,
    /// 帧提交器：由传输层接上，未接上时提交直接报错。
    frame_submit: Option<FrameSubmit>,
    /// 同进程传输的服务回执器；`Some` 时服务结果不再走 `service.result` 请求。
    service_reply: Option<ServiceReply>,
    /// 同进程传输的日志落点；`None` 时写 stderr。
    log: Option<LogSink>,
}

impl Context {
    /// 仅测试用：构造一个没有帧 / 服务回执 / 日志落点的上下文。
    #[cfg(test)]
    pub(crate) fn new(out_tx: mpsc::Sender<Envelope>) -> Self {
        Self::build(out_tx, None, None, None)
    }

    /// 仅测试用：接上帧提交器，其余同 [`Context::new`]。
    #[cfg(test)]
    pub(crate) fn build_fixture(out_tx: mpsc::Sender<Envelope>, submit: FrameSubmit) -> Self {
        Self::build(out_tx, Some(submit), None, None)
    }

    /// 同进程传输：请求仍走信封通道，帧、服务回执与日志直接交给宿主。
    pub(crate) fn new_in_process(
        out_tx: mpsc::Sender<Envelope>,
        submit: FrameSubmit,
        service_reply: ServiceReply,
        log: LogSink,
    ) -> Self {
        Self::build(out_tx, Some(submit), Some(service_reply), Some(log))
    }

    fn build(
        out_tx: mpsc::Sender<Envelope>,
        frame_submit: Option<FrameSubmit>,
        service_reply: Option<ServiceReply>,
        log: Option<LogSink>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                out_tx,
                pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
                next_id: AtomicU64::new(1),
                frame_submit,
                service_reply,
                log,
            }),
        }
    }

    pub(crate) fn pending_handle(&self) -> PendingHandle {
        PendingHandle { pending: self.inner.pending.clone() }
    }

    /// 写一条插件错误日志：同进程插件进宿主运行日志，进程插件写 stderr（宿主归入诊断输出）。
    pub fn log_error(&self, message: &str) {
        match &self.inner.log {
            Some(log) => log(message),
            None => eprintln!("{message}"),
        }
    }

    /// 按请求 id 摘除一个在途调用（响应配对用）。
    #[cfg(test)]
    pub(crate) fn remove_pending(&self, id: u64) -> Option<oneshot::Sender<Result<Value, RpcError>>> {
        self.inner.pending.lock().unwrap().remove(&id)
    }

    /// 发请求、等配对响应，受 [`CALL_TIMEOUT`] 约束。
    ///
    /// 超时 = 本次调用放弃（`RpcError::TIMEOUT`，结果未知），宿主侧业务不被取消；其响应迟到时由
    /// 读端丢弃并留痕（见连接泵），不会影响连接与其余调用。
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_with_timeout(method, params, CALL_TIMEOUT).await
    }

    /// 同 [`Context::call_raw`]，但等待上限由调用方给定（握手用更短的上限）。
    pub async fn call_with_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let rx = self.begin_call(method, params).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::failed(format!("调用 {method} 失败：连接已结束"))),
            Err(_) => Err(RpcError::timeout(format!("调用 {method} 超时（{}s）", timeout.as_secs()))),
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

    /// 一次量算多段文字（排一屏文本的往返落在单批里，上限见协议 `MAX_MEASURE_ITEMS`）。
    pub async fn measure_batch(
        &self,
        items: Vec<MeasureItem>,
    ) -> Result<MeasureBatchResult, RpcError> {
        let params = MeasureBatchParams { items };
        let value = self
            .call_with_timeout(method::query::MEASURE, serde_json::to_value(&params).map_err(|e| RpcError::failed(format!("量算参数序列化失败：{e}")))?, RENDER_CALL_TIMEOUT)
            .await?;
        serde_json::from_value(value).map_err(|e| RpcError::failed(format!("量算结果解析失败：{e}")))
    }

    // ---------- doc（当前文档会话；正文真源与保存链在宿主） ----------

    /// 打开一个文档会话：宿主读盘并建立会话，回执正文与可写性（viewer 只读为 false）。
    pub async fn doc_open(&self, path: &str) -> Result<DocOpenResult, RpcError> {
        self.call_typed(method::doc::OPEN, &DocOpenParams { path: path.into() }).await
    }

    /// 提交一次编辑（完整正文 + 全文字节偏移的光标 / 选区）。内容有变化时宿主置脏并排
    /// 自动保存，同时接入协作增量。
    pub async fn doc_edit(
        &self,
        path: &str,
        text: &str,
        anchor: Option<usize>,
        head: Option<usize>,
    ) -> Result<(), RpcError> {
        let params = DocEditParams { path: path.into(), text: text.into(), anchor, head };
        self.call_unit(method::doc::EDIT, &params).await
    }

    /// 请求一次保存（`manual` = 手动保存）；回执只表示已受理，结果经 `doc:changed` 到达。
    pub async fn doc_save(&self, path: &str, manual: bool) -> Result<(), RpcError> {
        let params = DocSaveParams { path: path.into(), manual };
        self.call_unit(method::doc::SAVE, &params).await
    }

    /// 关闭当前文档会话（换文件 / 关标签 / 切仓库）。
    pub async fn doc_close(&self, path: &str) -> Result<(), RpcError> {
        self.call_unit(method::doc::CLOSE, &PathParams { path: path.into() }).await
    }

    /// 上报本端选区（全文字节偏移；`None` = 只上报身份）。
    pub async fn doc_awareness(
        &self,
        path: &str,
        anchor: Option<usize>,
        head: Option<usize>,
    ) -> Result<(), RpcError> {
        let params = DocAwarenessParams { path: path.into(), anchor, head };
        self.call_unit(method::doc::AWARENESS, &params).await
    }

    /// 把当前文档改名为新的主名（同目录、保留扩展名）：有未落盘修改时宿主拒绝，先保存再调用。
    pub async fn doc_rename(&self, path: &str, name: &str) -> Result<(), RpcError> {
        self.call_unit(
            method::doc::RENAME,
            &DocRenameParams { path: path.into(), name: name.into() },
        )
        .await
    }

    /// 当前文档的反链（wiki 链接指向它的其它文档）。
    pub async fn vault_backlinks(&self, path: &str) -> Result<Vec<BacklinkEntry>, RpcError> {
        let result: BacklinksResult = self
            .call_typed(method::vault::BACKLINKS, &PathParams { path: path.into() })
            .await?;
        Ok(result.entries)
    }

    /// 全仓库标签词表（宿主缓存快照，按出现次数降序、最多 1000 条）：属性标签值候选这类
    /// 需要「仓库里已有哪些标签」的界面用。
    pub async fn vault_tags(&self) -> Result<Vec<String>, RpcError> {
        let result: crate::protocol::method::VaultTagsResult =
            self.call_typed(method::vault::TAGS, &Value::Null).await?;
        Ok(result.tags)
    }

    /// 当前仓库的可检索文件清单（按宿主文件树的树序，带行图标与能否打开）：
    /// 按名检索类界面一次取全量，自己在本地过滤 / 排序；清单重建时宿主另发
    /// [`crate::protocol::event::name::VAULT_FILES_CHANGED`]。
    pub async fn vault_files(&self) -> Result<FilesResult, RpcError> {
        self.call_typed(method::vault::FILES, &Value::Null).await
    }

    /// 读随仓库走的界面状态（展开集 / 排序 / 目录颜色 / 提示词标记）；`dir` 给定时另带该目录
    /// 的条目（宿主已按当前排序排好）。
    pub async fn vault_ui(
        &self,
        params: &VaultUiParams,
    ) -> Result<VaultUiResult, RpcError> {
        self.call_typed(method::vault_ui::READ, params).await
    }

    /// 按键写仓库界面状态（只改给出的字段），回执 = 写后的状态快照（不含条目与容器清单）。
    pub async fn vault_update_ui(&self, patch: &VaultUiPatch) -> Result<VaultUiState, RpcError> {
        self.call_typed(method::vault_ui::WRITE, patch).await
    }

    /// 发起一次文件拖拽：ghost、落点判定与悬停自动展开都在宿主，插件只给被拖条目与承载面。
    /// 落点状态经 [`crate::protocol::event::name::DRAG_STATE`] 回推，插件据此画高亮。
    pub async fn begin_drag(&self, params: &DragStartParams) -> Result<(), RpcError> {
        self.call_unit(method::drag::START, params).await
    }

    /// 上报本面上指针所指的落点候选（`target` 缺省 = 本面不接受落入）。
    pub async fn drag_hover(&self, params: &DragHoverParams) -> Result<(), RpcError> {
        self.call_unit(method::drag::HOVER, params).await
    }

    /// 结束拖拽（`cancelled` = 不落位）。
    pub async fn end_drag(&self, params: &DragEndParams) -> Result<(), RpcError> {
        self.call_unit(method::drag::END, params).await
    }

    /// 在指针处弹出宿主菜单；关闭时宿主发 `host:menu.closed`（`selected` = 选中项 id）。
    pub async fn open_menu(&self, params: &MenuOpenParams) -> Result<(), RpcError> {
        self.call_unit(method::menu::OPEN, params).await
    }

    /// 新建一个仓库条目（笔记 / 表格 / 画布 / 文件夹 / 插件文档类型）：宿主起空内容并落盘，
    /// 同名自动加序号。
    pub async fn create_entry(
        &self,
        params: &CreateEntryParams,
    ) -> Result<CreateEntryResult, RpcError> {
        self.call_typed(method::vault_ui::CREATE, params).await
    }

    /// 切换当前容器（本地仓库或协作空间）：`id` 取 `vault.ui` 回执里 `containers` 的原值。
    pub async fn switch_container(&self, params: &SwitchContainerParams) -> Result<(), RpcError> {
        self.call_unit(method::vault_ui::SWITCH_CONTAINER, params).await
    }

    /// 把容器移出最近列表（只移出列表，不动磁盘内容与服务端数据）。激活中的容器宿主拒绝。
    pub async fn remove_container(
        &self,
        params: &RemoveContainerParams,
    ) -> Result<(), RpcError> {
        self.call_unit(method::vault_ui::REMOVE_CONTAINER, params).await
    }

    /// 让宿主重读文件树（磁盘无监听，外部改动只能靠刷新感知）。
    pub async fn refresh_tree(&self) -> Result<(), RpcError> {
        self.call_unit(method::vault_ui::REFRESH_TREE, &Value::Null).await
    }

    /// 全仓库的目录清单（仓库相对路径，不含仓库根）：「展开全部」这类入口用。
    pub async fn vault_list_dirs(&self) -> Result<Vec<String>, RpcError> {
        let result: DirListResult =
            self.call_typed(method::vault_ui::LIST_DIRS, &Value::Null).await?;
        Ok(result.dirs)
    }

    /// 让宿主选一个文件夹并作为仓库打开（选择器与进入仓库链都在壳）。
    pub async fn pick_vault(&self) -> Result<(), RpcError> {
        self.call_unit(method::shell::PICK_VAULT, &Value::Null).await
    }

    /// 打开宿主的取色器（HSV 拖动预览 / 松手提交）；回执的 `color` 取消时缺省。
    pub async fn pick_color(&self, params: &PickColorParams) -> Result<PickColorResult, RpcError> {
        self.call_typed(method::shell::PICK_COLOR, params).await
    }

    /// 在系统文件管理器中显示一个仓库内条目（空路径 = 仓库根）。协作空间没有本机路径，宿主拒绝。
    pub async fn reveal_in_file_manager(&self, path: &str, is_dir: bool) -> Result<(), RpcError> {
        let params = RevealParams { path: path.to_string(), is_dir };
        self.call_unit(method::shell::REVEAL, &params).await
    }

    /// 取一个仓库内条目的绝对路径（空路径 = 仓库根）。协作空间没有本机路径，宿主拒绝。
    pub async fn vault_absolute_path(&self, path: &str) -> Result<String, RpcError> {
        let params = PathParams { path: path.to_string() };
        let result: AbsolutePathResult =
            self.call_typed(method::shell::ABSOLUTE_PATH, &params).await?;
        Ok(result.path)
    }

    /// 在同目录创建副本（同名自动加序号；`.atlx` / `.atb` 连同内部 id 重建）。
    /// 回执 = 落定的新路径。
    pub async fn duplicate_entry(&self, params: &DuplicateParams) -> Result<String, RpcError> {
        let result: AbsolutePathResult =
            self.call_typed(method::shell::DUPLICATE, params).await?;
        Ok(result.path)
    }

    /// 写系统剪贴板（文本）。
    pub async fn write_clipboard(&self, text: &str) -> Result<(), RpcError> {
        self.call_unit(method::clipboard::WRITE_TEXT, &TextContent { content: text.to_string() })
            .await
    }

    /// 当前容器与在线成员快照（容器种类 / 连接态 / 本端身份 / 在线设备明细）。
    /// 连接态与在场变化时宿主另发 [`crate::protocol::event::name::SPACE_PEERS_CHANGED`]。
    pub async fn space_room(&self) -> Result<SpaceRoomResult, RpcError> {
        self.call_typed(method::space::ROOM, &Value::Null).await
    }

    /// 空间成员名册快照（列表 / 加载中 / 错误）。缓存为空时宿主顺带发起一次拉取，
    /// 结果到达后另发 [`crate::protocol::event::name::SPACE_MEMBERS_CHANGED`]。
    pub async fn space_members(&self) -> Result<SpaceMembersResult, RpcError> {
        self.call_typed(method::space::MEMBERS, &Value::Null).await
    }

    /// 打开宿主的「协作空间」菜单（登录与空间列表入口）。
    pub async fn open_space_menu(&self) -> Result<(), RpcError> {
        self.call_unit(method::space::OPEN_MENU, &Value::Null).await
    }

    /// 打开宿主的「协作设置」页。
    pub async fn open_space_settings(&self) -> Result<(), RpcError> {
        self.call_unit(method::space::OPEN_SETTINGS, &Value::Null).await
    }

    /// 当前打开的文件（仓库相对路径，按打开顺序）：候选列表这类「打开中的优先」的界面用。
    pub async fn open_files(&self) -> Result<Vec<String>, RpcError> {
        let value: crate::protocol::method::EditorOpenFiles = self
            .call_typed(method::editor::OPEN_FILES, &Value::Null)
            .await?;
        Ok(value.paths)
    }

    /// 让宿主打开一个仓库内文件（按扩展名分流到对应视图；回执 = 已受理）。
    /// 布局里没有对应视图时由宿主新增并激活该视图标签。
    pub async fn open_file(&self, path: &str) -> Result<(), RpcError> {
        self.open_file_params(path, true).await
    }

    /// 只打开文件、不动布局：布局里已有对应视图就显示该文件，没有就不新增标签
    /// （列表 / 跳板类入口用）。
    pub async fn open_file_quiet(&self, path: &str) -> Result<(), RpcError> {
        self.open_file_params(path, false).await
    }

    async fn open_file_params(&self, path: &str, reveal: bool) -> Result<(), RpcError> {
        let params = EditorOpenFileParams { path: path.into(), reveal };
        self.call_unit(method::editor::OPEN_FILE, &params).await
    }

    /// 读全局编辑偏好（软换行 / 页面内标题 / 正文宽度）；变更经 `editor:prefs.changed` 下发。
    pub async fn editor_prefs(&self) -> Result<EditorPrefs, RpcError> {
        self.call_typed(method::editor::PREFS, &Value::Null).await
    }

    /// 取宿主主题：颜色 / 字号档 / 圆角档随用户设置变化，供插件绘制对齐宿主外观。
    pub async fn query_theme(&self) -> Result<ThemeResult, RpcError> {
        self.call_typed(method::query::THEME, &Value::Null).await
    }

    /// 提交整面帧：整发整换该面的绘制指令、命中区与输入声明（空指令 = 清空绘制）。
    /// 帧直接交宿主复制，`applied` 恒真。
    pub async fn submit_frame(
        &self,
        surface_id: &str,
        frame: crate::protocol::frame::SurfaceFrame,
    ) -> Result<SubmitResult, RpcError> {
        let submit = self
            .inner
            .frame_submit
            .as_ref()
            .ok_or_else(|| RpcError::failed("帧通道未就绪：连接未完成握手"))?;
        submit(surface_id, frame)
    }

    /// 注册一个贡献点，使其出现在宿主对应入口（面板 / 表格视图 / 设置页）。
    /// `id` 插件内唯一且不得含 `:`，同时是该面挂载后的 surfaceId；重复或非法 id 由宿主可读拒绝。
    /// `padding` = 面外壳内边距（缺省 / 0 = 贴边），按四边等距或分轴 / 分边声明。
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

    /// 拖动承载某个面的窗口：自绘标题栏按下时调用，宿主接管拖动手势（插件不必自己算窗口坐标）。
    /// 面先按接管面 / 插件窗口解析，解析不到即拖动当前聚焦窗口。
    pub async fn begin_window_drag(&self, surface_id: &str) -> Result<(), RpcError> {
        let params = crate::protocol::method::WindowDragParams { surface_id: surface_id.into() };
        self.call_unit(method::window::BEGIN_DRAG, &params).await
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

    /// 打开一个独立窗口承载自己的整面帧：`id` 插件内唯一且不得含 `:`，同时是该窗口面的
    /// surfaceId（插件随后用 `frame.submitShared` 提交该面）。重复 id 或宿主不可开窗由宿主可读拒绝。
    pub async fn window_open(&self, params: WindowOpenParams) -> Result<(), RpcError> {
        self.call_unit(method::window::OPEN, &params).await
    }

    /// 关闭自己打开的一个窗口（未打开的 id 报可读错误）。
    pub async fn window_close(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::CLOSE, &WindowCloseParams { id: id.into() }).await
    }

    /// 运行时改一个已打开窗口的属性（尺寸 / 置顶 / 标题；至少给一项；「不进任务栏」只能在
    /// 开窗时声明，窗口不存在报可读错误）。
    pub async fn window_update(&self, params: WindowUpdateParams) -> Result<(), RpcError> {
        self.call_unit(method::window::UPDATE, &params).await
    }

    /// 最小化自己打开的一个窗口（窗口不存在或 id 为空报可读错误）。
    pub async fn window_minimize(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::MINIMIZE, &WindowIdParams { id: id.into() }).await
    }

    /// 切换自己打开窗口的最大化态（窗口不存在或 id 为空报可读错误）。
    pub async fn window_toggle_maximize(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::TOGGLE_MAXIMIZE, &WindowIdParams { id: id.into() }).await
    }

    /// 重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
    pub async fn window_show(&self, id: &str) -> Result<(), RpcError> {
        self.call_unit(method::window::SHOW, &WindowIdParams { id: id.into() }).await
    }

    // ---------- ai ----------

    /// 发起 / 续聊一轮 AI 对话（须在 manifest 声明 `ai` 权限）。回执 = run 已就位，
    /// 流式事件经 [`PluginHandler::on_chat_event`] 流达；调用前须
    /// `subscribe("host:ai.chat.event")`，否则收不到事件。模型跟随面板当前选择，Agent
    /// 缺省沿用会话或内置「对话」；`session_id` 缺省 = 新建会话。
    /// 不设 SDK 等待上限：宿主要把会话与占位消息落真源后才回执，超时提前返回会让调用方
    /// 无法判断消息是否已写入。
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
        self.call_typed_unbounded(method::ai::CHAT_SEND, &params).await
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
    /// 不设 SDK 等待上限：宿主等核心把摘要写完才回执。
    pub async fn chat_compact(&self, session_id: &str) -> Result<(), RpcError> {
        let params = ChatSessionParams { session_id: session_id.into() };
        self.call_unit_unbounded(method::ai::CHAT_COMPACT, &params).await
    }

    /// 让模型重新生成会话标题；结果写回元数据并经 `host:ai.chat.event` 发 `Titled`。
    /// 不设 SDK 等待上限：命名要等一轮模型调用完成。
    pub async fn chat_title(&self, session_id: &str) -> Result<(), RpcError> {
        let params = ChatSessionParams { session_id: session_id.into() };
        self.call_unit_unbounded(method::ai::CHAT_TITLE, &params).await
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

    // ---------- 会话真源（chat.session.*） ----------

    /// 读单个会话（元数据 + 消息数组）。
    pub async fn chat_session_read(
        &self,
        session_id: &str,
    ) -> Result<crate::protocol::chat::SessionRecord, RpcError> {
        let params = crate::protocol::chat::SessionReadParams { session_id: session_id.into() };
        self.call_typed(method::chat::SESSION_READ, &params).await
    }

    /// 提交一个会话批次（创建 / 元数据 / 追加 / 补丁 / 截断 / 删除 / 覆盖项）。
    pub async fn chat_session_write(
        &self,
        batch: &crate::protocol::chat::CommitBatch,
    ) -> Result<(), RpcError> {
        self.call_unit(method::chat::SESSION_WRITE, batch).await
    }

    /// 读全仓库会话快照（会话清单 + 覆盖层 + 落盘失败）。
    pub async fn chat_session_snapshot(
        &self,
    ) -> Result<crate::protocol::chat::ChatStoreSnapshot, RpcError> {
        self.call_null(method::chat::SESSION_SNAPSHOT).await
    }

    // ---------- 工具执行 ----------

    /// 列可用工具声明；`tool_ids` 为空 = 全部。
    pub async fn tool_list(&self, tool_ids: Vec<String>) -> Result<Vec<ToolDecl>, RpcError> {
        let params = ToolListParams { tool_ids };
        let result: ToolListResult = self.call_typed(method::ai::TOOL_LIST, &params).await?;
        Ok(result.tools)
    }

    /// 执行一次工具调用（内置或插件工具）。工具可能等满调用方的超时上限，故不设 SDK 等待上限。
    pub async fn tool_execute(&self, name: &str, arguments: Value) -> Result<ToolExecResult, RpcError> {
        let params = ToolExecParams { name: name.into(), arguments };
        self.call_typed_unbounded(method::ai::TOOL_EXECUTE, &params).await
    }

    /// 向宿主发布一条对话事件（仅 AI 核心插件可用，宿主广播给订阅方与壳侧）。
    pub async fn chat_emit(&self, params: ChatEventParams) -> Result<(), RpcError> {
        self.call_unit(method::ai::CHAT_EMIT, &params).await
    }

    // ---------- 流式出网 ----------

    /// 打开一条流式连接：返回 streamId；数据经 [`crate::PluginHandler::on_http_stream_chunk`] 流达，
    /// 结束经 [`crate::PluginHandler::on_http_stream_end`]。
    pub async fn http_stream_open(
        &self,
        params: HttpStreamOpenParams,
    ) -> Result<HttpStreamOpenResult, RpcError> {
        self.call_typed(method::http::STREAM_OPEN, &params).await
    }

    /// 取消一条流式连接（幂等）。
    pub async fn http_stream_cancel(&self, stream_id: &str) -> Result<(), RpcError> {
        let params = HttpStreamCancelParams { stream_id: stream_id.into() };
        self.call_unit(method::http::STREAM_CANCEL, &params).await
    }

    pub async fn subscribe(&self, name: &str) -> Result<bool, RpcError> {
        let result: SubscribeResult = self.call_typed(crate::protocol::event::SUBSCRIBE, &SubscribeParams { name: name.into() }).await?;
        Ok(result.subscribed)
    }

    // ---------- 能力总线（服务目录 / 跨插件提供者） ----------

    /// 枚举在册服务：服务名、活动提供者的版本与方法名、是否可替换、其余候选。
    pub async fn service_list(&self) -> Result<service::ListResult, RpcError> {
        self.call_null(service::method::LIST).await
    }

    /// 按「服务 + 方法」调用：`params` 就是目标方法自己的入参，应答就是该方法的结果。
    /// 与直接调用 `服务.方法` 等价——活动提供者是内置实现时两者落到同一实现。
    pub async fn service_call(
        &self,
        service_name: &str,
        method_name: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let call = service::CallParams {
            service: service_name.into(),
            method: method_name.into(),
            params,
        };
        let value = serde_json::to_value(&call)
            .map_err(|e| RpcError::failed(format!("service.call params 序列化失败：{e}")))?;
        self.call_raw(service::method::CALL, value).await
    }

    /// 声明本插件提供某个服务的若干方法（方法名不含服务前缀）——收到
    /// [`crate::PluginHandler::on_service_call`] 后按方法名执行。
    ///
    /// 登记不等于生效：同一服务同时只有一个活动提供者，已有内置实现的服务只成为候选，
    /// 是否顶替由用户在设置页决定；目录里没有的服务首个提供者即活动提供者。
    pub async fn register_service(&self, service_name: &str, methods: &[&str]) -> Result<(), RpcError> {
        let params = service::RegisterParams {
            service: service_name.into(),
            version: None,
            methods: methods.iter().map(|name| (*name).to_string()).collect(),
        };
        self.call_unit(service::method::REGISTER, &params).await
    }

    /// 撤销本插件对某个服务的登记（活动提供者撤销即回落内置实现）。
    pub async fn unregister_service(&self, service_name: &str) -> Result<(), RpcError> {
        let params = service::UnregisterParams { service: service_name.into() };
        self.call_unit(service::method::UNREGISTER, &params).await
    }

    /// 回报一次服务调用的结果：`call_id` 原样带回 [`crate::PluginHandler::on_service_call`] 收到的值，
    /// `result` 即目标方法的结果。进程承载下，调用已超时 / 提供者已下线或不归本插件时报可读错误；
    /// 同进程承载的回执直接交给宿主，宿主丢弃时不再回报。
    pub async fn service_result(&self, call_id: &str, result: Value) -> Result<(), RpcError> {
        if let Some(reply) = &self.inner.service_reply {
            let payload = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
            return reply(call_id, true, &payload);
        }
        let params = service::ResultParams {
            call_id: call_id.into(),
            result: Some(result),
            error: None,
        };
        self.call_unit(service::method::RESULT, &params).await
    }

    /// 回报一次服务调用失败：`error` 为可读原因，调用方收到的就是这条原因。
    pub async fn service_error(&self, call_id: &str, error: &str) -> Result<(), RpcError> {
        if let Some(reply) = &self.inner.service_reply {
            return reply(call_id, false, error);
        }
        let params = service::ResultParams {
            call_id: call_id.into(),
            result: None,
            error: Some(error.into()),
        };
        self.call_unit(service::method::RESULT, &params).await
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

    // ---------- history（当前仓库的活动密度与版本） ----------

    /// 按天聚合的仓库活动（新建 / 修改 / 删除的文件计数），按天升序。
    pub async fn history_activity(&self) -> Result<Vec<DayActivity>, RpcError> {
        let result: HistoryActivityResult = self.call_null(method::history::ACTIVITY).await?;
        Ok(result.days)
    }

    /// 某一天（`YYYY-MM-DD`）的仓库历史版本记录，按时间新→旧。按需拉取，不在订阅事件里推送。
    pub async fn history_day(&self, day: &str) -> Result<Vec<HistoryDayEntry>, RpcError> {
        let params = HistoryDayParams { day: day.into() };
        let result: HistoryDayResult = self.call_typed(method::history::DAY, &params).await?;
        Ok(result.entries)
    }

    /// 打开某文件的仓库历史弹层（宿主在当前聚焦窗口展示）；`kind` 取 `note` / `canvas` / `table`。
    /// 回执 = 已受理，弹层内的操作不再回报。
    pub async fn history_open(&self, kind: &str, file: &str) -> Result<(), RpcError> {
        let params = HistoryOpenParams { kind: kind.into(), file: file.into() };
        self.call_unit(method::history::OPEN, &params).await
    }

    /// 全仓库按文件分组的版本聚合（组按最近版本时间新→旧）。`refresh` = 丢弃宿主聚合缓存
    /// 后重扫（刷新 / 重试用）；版本明细与回滚走 [`Context::history_open`] 的宿主弹层。
    pub async fn history_groups(&self, refresh: bool) -> Result<HistoryGroupsResult, RpcError> {
        let params = HistoryGroupsParams { refresh };
        self.call_typed(method::history::GROUPS, &params).await
    }

    /// 当前容器的最近打开清单（按打开时刻新→旧；`ready` = 容器可用）。
    pub async fn recent_list(&self) -> Result<RecentListResult, RpcError> {
        self.call_typed(method::recent::LIST, &Value::Null).await
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

    /// 读当前空间的团队元数据：`key = None` 读全部键。团队同空间的所有插件共享同一份键空间，
    /// 键缺省即无数据；仅协作空间可用，本地仓库回可读错误。
    pub async fn collab_meta_get(&self, key: Option<&str>) -> Result<CollabMetaResult, RpcError> {
        let params = CollabMetaGetParams { key: key.map(Into::into) };
        self.call_typed(method::collab::META_GET, &params).await
    }

    /// 按键写当前空间的团队元数据（只改给出的键，其余键保持原值）；成员经
    /// `space:meta.changed` 得知变更。只读身份与本地仓库回可读错误。
    pub async fn collab_meta_patch(
        &self,
        entries: Vec<CollabMetaEntry>,
    ) -> Result<(), RpcError> {
        let params = CollabMetaPatchParams { entries };
        self.call_unit(method::collab::META_PATCH, &params).await
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

    /// 读剪贴板里的图片并按 PNG 字节给出（宿主侧是 base64，SDK 负责解码）；没有图片时为 `None`。
    pub async fn clipboard_read_image_png(&self) -> Result<Option<Vec<u8>>, RpcError> {
        use base64::Engine;
        let Some(image) = self.clipboard_read_image().await? else {
            return Ok(None);
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image.base64.as_bytes())
            .map_err(|e| RpcError::failed(format!("剪贴板图片解码失败：{e}")))?;
        Ok(Some(bytes))
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

    async fn call_unit_unbounded<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<(), RpcError> {
        let value = serde_json::to_value(params)
            .map_err(|e| RpcError::failed(format!("调用 {method} 失败：params 序列化错误（{e}）")))?;
        self.call_unbounded(method, value).await.map(|_| ())
    }
}
