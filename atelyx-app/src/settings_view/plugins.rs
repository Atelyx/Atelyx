//! 设置-插件：已安装列表 / 启停 / 卸载 / 本地与 zip 安装 / 详情（版本、声明权限、失败原因、
//! 用到但未声明的调用）/ 市场（官方索引浏览 / 安装 / 更新）。
//! 列表与状态来自 `atelyx_core::plugin_runtime`（运行时为进程内单例，壳持有 `Arc`）；
//! 「内置插件」区块（内置插件停用与服务提供者切换）读壳的唯一注册表。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use iced::widget::text::Wrapping;
use iced::widget::{button, container, row, scrollable, space, Column};
use iced::{border, widget::container::Style, Element, Length, Padding, Task};

use atelyx_core::plugin_runtime::market::{self, MarketSnapshot};
use atelyx_core::plugin_runtime::{
    AiAgent, AiAgentsResult, AiModel, AiModelPick, AiModelProvider, AiModelsResult, AuditRecord,
    ClipboardImage, CollabMetaGetParams, CollabMetaPatchParams, CollabMetaResult, CollabPeer,
    CollabPeersResult, CollabSendParams, CollabSendQos, CollabSendResult, DayActivity,
    DialogKind, DialogRequest, FileChangeKind, HistoryActivityResult, HistoryDayParams,
    HistoryGroupsParams, HistoryOpenParams, HistoryRecordParams, HostRequest, ImageInfoResult,
    InstallOutcome, PanelFlushDoneParams, PanelHeaderParams, SpaceFilePatchParams,
    MeasureBatchParams,
    MeasureBatchResult, NotificationLevel, Permission, PluginEvent, PluginRuntime, PluginStatus,
    DocAwarenessParams, DocEditParams, DocOpenParams, DocPropsWriteParams, DocRenameParams,
    DocSaveParams,
    FilesResult, PluginSummary, SetTrayItemsParams, SpaceContainerKind, SpaceMember,
    SpaceMemberRole, SpaceMembersResult, SpacePeer, SpacePeerFile, SpaceRoomResult,
    ContainerEntry, SpaceSelfIdentity, UiDirEntry, VaultFileEntry, VaultTagsResult, VaultUiParams,
    VaultUiPatch, VaultUiResult, VaultUiState,
    MAX_VAULT_BINARY_BYTES, PLATFORM_PLUGIN,
};
use atelyx_core::space_client::SpaceError;

use crate::registry::{BuiltinPluginInfo, Entry, EntryKind};
use crate::settings_view::{card, group_head};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::{bold, mono, text};
use crate::{layout_view, vault, App, Message};

/// 数据落盘时刻格式化用的毫秒口径（审计记录为 unix 秒）。
const MS_PER_SEC: u64 = 1000;

/// 插件详情模态卡宽（日志页要放下等宽日志行）。
const DETAIL_CARD_WIDTH: f32 = 620.0;
/// 内置插件详情模态卡宽（只有视图 / 服务列表，窄一档）。
const BUILTIN_CARD_WIDTH: f32 = 520.0;
/// 能力调用记录区固定滚动高度（避免长列表把模态撑出窗口）。
const DETAIL_AUDIT_MAX_H: f32 = 180.0;
/// 日志面板固定高度。
const LOG_PANEL_H: f32 = 360.0;
/// 日志路径行的横向占位（模态左右内衬 + 复制图标 / 间距 / 按钮内衬），算可用字宽时扣除。
const LOG_PATH_CHROME_W: f32 = 24.0 + 26.0;
/// 市场安装确认模态卡宽。
const CONFIRM_CARD_WIDTH: f32 = 380.0;
/// 协作空间单文件上限（服务端强制；大文件导入在本地先挡一道，免把超限内容读进内存）。
const SPACE_MAX_FILE_BYTES: usize = 50 * 1024 * 1024;

/// 插件页模式：已安装列表 / 市场浏览。
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsMode {
    /// 已装插件列表（安装入口 / 启停 / 卸载 / 详情）。
    #[default]
    Installed,
    /// 官方索引浏览（搜索 / 类型筛选 / 安装 / 更新）。
    Market,
}

/// 插件详情分页：概览与运行日志，同一模态内切换。
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Overview,
    Log,
}

/// 待确认的市场安装目标。
#[derive(Clone, Debug, PartialEq)]
pub struct MarketInstallAsk {
    pub id: String,
    pub repo: String,
    pub name: String,
    /// 官方仓库归属：官方插件直接安装，只有非官方插件进确认弹窗。
    pub official: bool,
}

/// 市场安装 / 更新回执：内容已落位（重启失败单独给出）+ 版本 + 是否更新。
#[derive(Clone, Debug)]
pub struct MarketOutcome {
    pub result: InstallOutcome,
    pub version: String,
    pub update: bool,
}

/// 市场操作结果：版本比对判为无需更新，或内容已落位。
#[derive(Clone, Debug)]
pub enum MarketResult {
    UpToDate(String),
    Installed(MarketOutcome),
}

/// 插件发起的仓库结构变更：成功后广播 `vault:file.changed` 并刷新宿主文件树。
#[derive(Clone, Debug)]
pub enum VaultChange {
    Removed { rel: String },
    Renamed { from: String, to: String },
}

/// 插件页状态（列表缓存 + 详情 + 卸载二段确认 + 安装在途 + 市场快照与筛选）。
#[derive(Default)]
pub struct PluginsState {
    /// 已装插件摘要（打开插件页 / 收到事件 / 动作后刷新）。
    pub list: Vec<PluginSummary>,
    /// 详情打开的插件 id。
    pub detail: Option<String>,
    /// 内置插件（进程内）详情打开的插件 id：贡献与服务在这里列出。
    pub builtin_detail: Option<String>,
    /// 详情页审计记录（打开详情时拉取）。
    pub detail_audit: Vec<AuditRecord>,
    /// 卸载二段确认目标。
    pub confirm_uninstall: Option<String>,
    /// 安装（目录 / zip）在途：按钮禁用，防并发安装互踩。
    pub installing: bool,
    /// 市场快照（缓存秒开 + 后台刷新；None = 尚未拉取）。
    pub market: Option<MarketSnapshot>,
    /// 市场索引刷新在途。
    pub market_loading: bool,
    /// 市场操作在途的插件 id（该卡片按钮禁用 + 进度文案）。
    pub market_busy: Option<String>,
    /// 页面模式（已安装 / 市场）。
    pub mode: PluginsMode,
    /// 市场搜索词（名称 / id / 描述 / 仓库）。
    pub market_query: String,
    /// 市场类型筛选（`None` = 全部；取值来自索引实际出现的 type）。
    pub market_type: Option<String>,
    /// 非官方插件的安装确认目标。
    pub confirm_install: Option<MarketInstallAsk>,
    /// 详情页当前分页。
    pub detail_tab: DetailTab,
    /// 日志读取纪元：回执只认发起时的那一代，清空与重新读取都会推进它。
    pub log_seq: u64,
    /// 日志页读到的尾部（`None` = 读取中）。
    pub log_text: Option<String>,
    /// 日志读取失败的原因（`Some` = 显示失败态而非「暂无日志」）。
    pub log_error: Option<String>,
    /// 清空日志的二段确认。
    pub log_clear_ask: bool,
    /// 等历史聚合落地的查询：聚合落地或容器切换时统一结算。
    pub pending_history: Vec<PendingHistory>,
}

/// 等历史聚合落地的查询（同一个聚合缓存驱动，结算与回绝都按它逐条回填）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingHistory {
    /// 全库版本分组（`history.groups`）。
    Groups(u64),
    /// 某一天的版本记录（`history.day`）：请求 id + 自纪元天数。
    Day(u64, i64),
    /// 按天活动统计（`history.activity`，协作空间口径）。
    Activity(u64),
}

impl PendingHistory {
    pub fn request_id(&self) -> u64 {
        match self {
            PendingHistory::Groups(id) | PendingHistory::Day(id, _) | PendingHistory::Activity(id) => *id,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PluginsMsg {
    /// 打开插件页 / 动作后刷新列表。
    Refresh,
    /// 启用 / 停用。
    Toggle(String, bool),
    /// 内置插件：启用 / 停用（停用后其视图不再进选择器、服务离开能力总线；已开的标签显示停用占位）。
    SetBuiltinPluginEnabled(String, bool),
    /// 服务目录：切换活动提供者（`None` = 回落内置实现）。
    SetServiceProvider(String, Option<String>),
    /// 熔断后手动重试。
    Retry(String),
    /// 卸载二段确认（进入 / 确认 / 取消）。
    UninstallAsk(String),
    UninstallConfirm(String),
    UninstallCancel,
    /// 从本地目录 / zip 安装。
    InstallDir,
    InstallZip,
    /// 安装回执（Ok(None) = 用户取消）。
    Installed(Result<Option<InstallOutcome>, String>),
    /// 从记录下来的安装来源重新安装（开发回路：构建产物更新后一键重装）。
    ReinstallSource(String),
    Reinstalled(Result<InstallOutcome, String>),
    /// 卸载回执（携带目标 id 供提示文案）。
    Uninstalled(String, Result<(), String>),
    /// 打开 / 关闭详情。
    Detail(String),
    DetailClose,
    /// 打开 / 关闭内置插件（进程内）详情。
    BuiltinDetail(String),
    BuiltinDetailClose,
    /// 详情分页切换（切到日志页即读尾部）。
    DetailTab(DetailTab),
    /// 日志页：重读尾部 / 清空二段确认（进入 / 取消 / 执行）/ 清空回执。
    LogRefresh,
    LogClearAsk,
    LogClearCancel,
    LogClear(String),
    LogCleared(String, Result<(), String>),
    /// 复制日志文件路径（点击路径行）。
    CopyLogPath(String),
    /// 日志读取回执（带目标 id 与纪元：先发的读取不能覆盖已切换插件或已清空的显示）。
    LogLoaded { id: String, seq: u64, result: Result<String, String> },
    /// 壳侧服务请求：替插件弹系统对话框（选定的路径经 `resolve_dialog` 回填给等待中的调用）。
    DialogAsk { request_id: u64, dialog: DialogRequest },
    DialogDone { request_id: u64, paths: Option<Vec<String>> },
    /// 市场索引读取：缓存先行（未过期即不联网），过期或缺失才拉网络。
    MarketRefresh,
    /// 市场索引强制刷新（刷新钮：无视缓存有效期）。
    MarketReload,
    MarketLoaded(Result<MarketSnapshot, String>),
    /// 页面模式切换 / 市场筛选（搜索词、类型）。
    SetMode(PluginsMode),
    MarketQuery(String),
    MarketType(Option<String>),
    /// 市场安装入口：官方插件直接安装，非官方先进确认弹窗。
    MarketInstallAsk(MarketInstallAsk),
    MarketInstallConfirm,
    MarketInstallCancel,
    /// 市场更新（Release 版本比对后走既有压缩安装链）。
    MarketUpdate { id: String, repo: String },
    MarketDone(Result<MarketResult, String>),
    /// 剪贴板读写（文本与图片都走壳的剪贴板线程，见 `crate::clipboard`）。
    ClipboardWrite { request_id: u64, text: String },
    ClipboardWritten { request_id: u64, result: Result<(), String> },
    ClipboardRead { request_id: u64 },
    ClipboardReadDone { request_id: u64, result: Result<Option<String>, String> },
    /// 剪贴板图片（像素读取走壳的剪贴板线程，PNG 编码在阻塞线程池执行后 base64 回填）。
    ClipboardReadImage { request_id: u64 },
    ClipboardImageDone { request_id: u64, result: Result<Option<ClipboardImage>, String> },
    /// 写剪贴板图片（运行时已解码并校验单次上限；壳再解成 RGBA 交给剪贴板线程）。
    ClipboardWriteImage { request_id: u64, bytes: Vec<u8> },
    ClipboardImageWritten { request_id: u64, result: Result<(), String> },
    /// 仓库读写（本地仓库直接落盘；协作空间转发服务端）。`VaultCallDone` 统一回填。
    VaultRead { request_id: u64, path: String },
    VaultWrite { plugin_id: String, request_id: u64, path: String, content: String },
    VaultReadBinary { request_id: u64, path: String },
    VaultWriteBinary { request_id: u64, path: String, bytes: Vec<u8> },
    /// 大文件导入：`source` 为插件私有目录内的绝对路径（运行时已校验并规范化）。
    VaultImportFile { request_id: u64, path: String, source: String },
    /// 写入完成：成功后广播 `vault:file.changed`（Created / Updated）再回填。
    VaultWriteDone {
        request_id: u64,
        rel: String,
        created: bool,
        result: Result<serde_json::Value, String>,
    },
    VaultList { request_id: u64, path: String },
    /// 插件请求全仓库可检索文件清单（`vault.files`）。
    VaultFiles { request_id: u64 },
    /// 插件请求全仓库目录清单（`vault.listDirs`）。
    VaultListDirs { request_id: u64 },
    /// 插件请求宿主重读文件树（`vault.refreshTree`）。
    VaultRefreshTree { request_id: u64 },
    /// 目录清单落地（容器世代守卫）。
    VaultListDirsDone { request_id: u64, gen: u64, result: Result<Vec<String>, String> },
    VaultExists { request_id: u64, path: String },
    /// 删除（文件夹递归；仓库根拒绝）与重命名 / 移动（目标不可已存在；引用跟随）。
    VaultRemove { request_id: u64, path: String },
    VaultRename { request_id: u64, from: String, to: String },
    /// 结构变更回执：成功先广播 `vault:file.changed` 并刷新宿主文件树再回填。
    VaultChangeDone {
        request_id: u64,
        change: VaultChange,
        rewritten: Vec<String>,
        result: Result<serde_json::Value, String>,
    },
    /// 图片固有像素尺寸（只读文件头，不送回内容）。
    VaultImageInfo { request_id: u64, path: String },
    /// 批量文字量算（`query.measure`）：顺序与请求条目一一对应。
    MeasureBatch { request_id: u64, params: MeasureBatchParams },
    /// 主题查询（`query.theme`）：颜色 / 字号档 / 圆角档。
    QueryTheme { request_id: u64 },
    /// 已安装字体族清单（`query.fonts`）。
    QueryFonts { request_id: u64 },
    VaultCallDone { request_id: u64, result: Result<serde_json::Value, String> },
    /// AI 模型 / Agent 清单（读仓库配置与面板当前选择）。
    AiModels { request_id: u64 },
    AiAgents { request_id: u64 },
    /// 系统托盘条目整发整换（空清单 = 收起该插件条目）。
    TraySetItems { request_id: u64, plugin_id: String, params: SetTrayItemsParams },
    /// 协作空间在场成员（未连接 = `connected: false`）。
    CollabPeers { request_id: u64 },
    /// 向同空间对等插件发消息（通道加插件前缀，未连接时回 `queued: false`）。
    CollabSend { request_id: u64, plugin_id: String, params: CollabSendParams },
    /// 当前笔记选区（编辑态才有内容；未打开笔记或阅读态回空选区）。
    EditorSelection { request_id: u64 },
    /// 当前打开的文件清单（仓库相对路径，按打开顺序）。
    EditorOpenFiles { request_id: u64 },
    /// 用给定文本替换当前选区（空串 = 删除选区）。
    EditorReplaceSelection { request_id: u64, text: String },
    /// 让宿主打开一个仓库内文件（按扩展名分流视图；`reveal` = 是否让对应视图显示）。
    EditorOpenFile { request_id: u64, path: String, reveal: bool },
    /// 全局编辑偏好（软换行 / 页面内标题 / 正文宽度）。
    EditorPrefs { request_id: u64 },
    /// 打开文档：宿主读盘并建立会话，回执 = 正文与可写性。
    DocOpen { request_id: u64, plugin_id: String, params: DocOpenParams },
    /// 把当前文档改名为新的主名（同目录、保留扩展名）。
    DocRename { request_id: u64, params: DocRenameParams },
    /// 读当前文档的 frontmatter 块（属性面板）。
    DocPropsRead { request_id: u64 },
    /// 用新 frontmatter 块替换当前文档的属性区。
    DocPropsWrite { request_id: u64, params: DocPropsWriteParams },
    DocOpenDone(crate::doc_session::OpenReadResult),
    /// 提交一次文档编辑（完整正文 + 光标 / 选区）。
    DocEdit { request_id: u64, params: DocEditParams },
    /// 请求一次文档保存（回执 = 已受理，结果经 `doc:changed`）。
    DocSave { request_id: u64, params: DocSaveParams },
    /// 关闭当前文档会话。
    DocClose { request_id: u64, path: String },
    /// 上报本端文档选区。
    DocAwareness { request_id: u64, params: DocAwarenessParams },
    /// 当前文档的反链。
    VaultBacklinks { request_id: u64, path: String },
    VaultBacklinksDone { request_id: u64, result: Result<serde_json::Value, String> },
    /// 全仓库标签词表。
    VaultTags { request_id: u64 },
    /// 按天聚合的仓库活动（`history.activity`）。
    HistoryActivity { request_id: u64 },
    HistoryActivityDone { request_id: u64, result: Result<serde_json::Value, String> },
    /// 某一天的仓库历史版本（`history.day`）：缓存在手即答，否则并入聚合后再回填。
    HistoryDay { request_id: u64, params: HistoryDayParams },
    /// 全仓库版本分组（`history.groups`）：缓存在手即答，否则并入聚合后再回填。
    HistoryGroups { request_id: u64, params: HistoryGroupsParams },
    /// 打开某文件的仓库历史弹层（`history.open`，在聚焦的工作区窗口打开；回执 = 已受理）。
    HistoryOpen { request_id: u64, params: HistoryOpenParams },
    /// 当前容器的最近打开清单（`recent.list`）。
    RecentList { request_id: u64 },
    /// 系统下载目录的绝对路径（`fs.downloadsDir`）。
    DownloadsDir { request_id: u64 },
    /// 域插件落盘后记一版（`history.record`；回执 = 已受理，记版结果另行广播）。
    HistoryRecord { request_id: u64, params: HistoryRecordParams },
    /// 插件声明面板头部状态（`panel.header`；`header` 缺省 = 收起）。
    PanelHeader { plugin_id: String, request_id: u64, params: PanelHeaderParams },
    /// 插件回报落盘完成（`panel.flushDone`）：按令牌放行挂起的宿主动作。
    PanelFlushDone { plugin_id: String, request_id: u64, params: PanelFlushDoneParams },
    /// 空间文件补丁保存（`space.file.patch`）。
    SpaceFilePatch { request_id: u64, params: SpaceFilePatchParams },
    /// 空间补丁保存回执：服务端最终路径（补丁带标题时按标题改名）或可读失败原因。
    SpaceFilePatchDone {
        gen: u64,
        request_id: u64,
        path: String,
        result: Result<String, String>,
    },
    /// 当前容器与在线成员快照（`space.room`）。
    SpaceRoom { request_id: u64 },
    /// 空间成员名册快照（`space.members`；缓存为空时顺带发起拉取）。
    SpaceMembers { request_id: u64 },
    /// 打开宿主的「协作空间」菜单（`space.openMenu`）。
    SpaceOpenMenu { request_id: u64 },
    /// 打开宿主的「协作设置」页（`space.openSettings`）。
    SpaceOpenSettings { request_id: u64 },
    /// 读 / 写随仓库走的界面状态（`vault.ui` / `vault.updateUi`）。
    VaultUi { request_id: u64, params: atelyx_core::plugin_runtime::VaultUiParams },
    VaultUiWrite { request_id: u64, patch: atelyx_core::plugin_runtime::VaultUiPatch },
    /// `vault.ui` 要目录条目时的异步回执（条目按磁盘现读，不吃宿主的懒加载缓存）。
    VaultUiDone {
        request_id: u64,
        result: Result<Vec<atelyx_core::plugin_runtime::UiDirEntry>, String>,
    },
    /// 系统入口：在文件管理器中显示一个条目 / 取一个条目的绝对路径（`shell.reveal` /
    /// `shell.absolutePath`；协作空间没有本机路径）。
    ShellReveal { request_id: u64, params: atelyx_core::plugin_runtime::RevealParams },
    ShellAbsolutePath { request_id: u64, path: String },
    /// 在同目录创建副本（`shell.duplicate`），回执 = 落定的新路径。
    ShellDuplicate { request_id: u64, params: atelyx_core::plugin_runtime::DuplicateParams },
    /// 副本创建完成（容器世代守卫：在途任务不跨仓库回填）。
    ShellDuplicateDone {
        request_id: u64,
        gen: u64,
        result: Result<String, String>,
    },
    /// 新建仓库条目（`vault.create`）与容器切换（`vault.switchContainer`）。
    VaultCreate { request_id: u64, params: atelyx_core::plugin_runtime::CreateEntryParams },
    VaultSwitchContainer {
        request_id: u64,
        params: atelyx_core::plugin_runtime::SwitchContainerParams,
    },
    /// 把容器移出最近列表（`vault.removeContainer`）。
    VaultRemoveContainer {
        request_id: u64,
        params: atelyx_core::plugin_runtime::RemoveContainerParams,
    },
    /// 协作空间团队元数据（仅在协作空间可用；写入只改给出的键）。
    CollabMetaGet { request_id: u64, params: CollabMetaGetParams },
    CollabMetaPatch { request_id: u64, params: CollabMetaPatchParams },
    CollabMetaDone { request_id: u64, result: Result<serde_json::Value, String> },
}

/// 订阅标识：运行时 `Arc` 指针哈希（同一运行时不重建订阅流）。
pub struct RuntimeSpec(pub Arc<PluginRuntime>);

impl std::hash::Hash for RuntimeSpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// 插件事件订阅流：把运行时的单一广播通道接进 Elm update。
pub fn runtime_stream(spec: &RuntimeSpec) -> impl iced::futures::Stream<Item = Message> + use<> {
    let runtime = spec.0.clone();
    iced::stream::channel(64, async move |mut output| {
        let mut events = runtime.subscribe_events();
        // 广播不保留历史：先订阅再补发已存档的默认布局声明，避免插件启动快于订阅而丢声明
        for event in runtime.declared_default_layouts() {
            let _ = iced::futures::SinkExt::send(
                &mut output,
                Message::PluginEvent(Box::new(event)),
            )
            .await;
        }
        loop {
            let event = tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => event,
                    // 广播落后只丢中间事件、连接不失效：继续收后续事件
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                chat = runtime.recv_ai_chat_event() => match chat {
                    Some(params) => PluginEvent::AiChatEvent {
                        id: atelyx_core::plugin_runtime::AI_CORE_PLUGIN.to_string(),
                        params,
                    },
                    None => break,
                },
            };
            // 对话事件走上面的无界通道：广播里的副本跳过，避免重复应用
            if matches!(event, PluginEvent::AiChatEvent { .. }) {
                continue;
            }
            let _ = iced::futures::SinkExt::send(
                &mut output,
                Message::PluginEvent(Box::new(event)),
            )
            .await;
        }
    })
}

/// 壳侧服务请求流：运行时用无界通道送请求（对话框 / 接管 / 插件窗口），这里接进 Elm update。
pub fn host_requests_stream(spec: &RuntimeSpec) -> impl iced::futures::Stream<Item = Message> + use<> {
    let runtime = spec.0.clone();
    iced::stream::channel(16, async move |mut output| {
        while let Some(request) = runtime.recv_host_request().await {
            let message = match request {
                HostRequest::Dialog { request_id, dialog, .. } => {
                    Message::Plugins(PluginsMsg::DialogAsk { request_id, dialog })
                }
                HostRequest::Takeover { plugin_id, request_id, scope, surface_id } => {
                    Message::PluginTakeoverAsk { request_id, plugin_id, scope, surface_id }
                }
                HostRequest::OpenWindow { plugin_id, request_id, params } => {
                    Message::PluginWindowOpen { request_id, plugin_id, params }
                }
                HostRequest::CloseWindow { plugin_id, request_id, window_id } => {
                    Message::PluginWindowClose { request_id, plugin_id, window_key: window_id }
                }
                HostRequest::WindowUpdate { plugin_id, request_id, params } => {
                    Message::PluginWindowUpdate { request_id, plugin_id, params }
                }
                HostRequest::WindowShow { plugin_id, request_id, window_id } => {
                    Message::PluginWindowShow { request_id, plugin_id, window_key: window_id }
                }
                HostRequest::WindowMinimize { plugin_id, request_id, window_id } => {
                    Message::PluginWindowMinimize { request_id, plugin_id, window_key: window_id }
                }
                HostRequest::WindowToggleMaximize { plugin_id, request_id, window_id } => {
                    Message::PluginWindowToggleMaximize { request_id, plugin_id, window_key: window_id }
                }
                HostRequest::WindowBeginDrag { plugin_id, request_id, surface_id } => {
                    Message::PluginWindowBeginDrag { request_id, plugin_id, surface_id }
                }
                HostRequest::ChatSend { plugin_id: _, request_id, params } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSend { request_id, params },
                ),
                HostRequest::ChatSendDirect { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSendDirect { request_id, params },
                ),
                HostRequest::ChatCompactDirect { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginCompactDirect { request_id, params },
                ),
                HostRequest::AiSettings { request_id, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginAiSettings { request_id },
                ),
                HostRequest::ChatStop { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginStop {
                        request_id,
                        session_id: params.session_id,
                    },
                ),
                HostRequest::ChatHistory { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginHistory {
                        request_id,
                        session_id: params.session_id,
                        limit: params.limit,
                        offset: params.offset,
                        query: params.query,
                    },
                ),
                HostRequest::ChatRename { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginRename {
                        request_id,
                        session_id: params.session_id,
                        title: params.title,
                    },
                ),
                HostRequest::ChatDelete { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginDelete {
                        request_id,
                        session_id: params.session_id,
                    },
                ),
                HostRequest::ChatCompact { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginCompact {
                        request_id,
                        session_id: params.session_id,
                    },
                ),
                HostRequest::ChatRetitle { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginRetitle {
                        request_id,
                        session_id: params.session_id,
                    },
                ),
                HostRequest::ChatSessionRead { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSessionRead {
                        request_id,
                        session_id: params.session_id,
                    },
                ),
                HostRequest::ChatSessionWrite { request_id, batch, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSessionWrite { request_id, batch },
                ),
                HostRequest::ChatSessionSnapshot { request_id, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSessionSnapshot { request_id },
                ),
                HostRequest::AiToolExecute { request_id, params, .. } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginToolExecute { request_id, params },
                ),
                HostRequest::ClipboardWrite { request_id, text, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardWrite { request_id, text })
                }
                HostRequest::ClipboardRead { request_id, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardRead { request_id })
                }
                HostRequest::ClipboardReadImage { request_id, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardReadImage { request_id })
                }
                HostRequest::ClipboardWriteImage { request_id, bytes, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardWriteImage { request_id, bytes })
                }
                HostRequest::AiModels { request_id, .. } => {
                    Message::Plugins(PluginsMsg::AiModels { request_id })
                }
                HostRequest::AiAgents { request_id, .. } => {
                    Message::Plugins(PluginsMsg::AiAgents { request_id })
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultRead { request_id, path })
                }
                HostRequest::VaultWriteText { plugin_id, request_id, path, content, .. } => {
                    Message::Plugins(PluginsMsg::VaultWrite { plugin_id, request_id, path, content })
                }
                HostRequest::VaultReadBinary { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultReadBinary { request_id, path })
                }
                HostRequest::VaultWriteBinary { request_id, path, bytes, .. } => {
                    Message::Plugins(PluginsMsg::VaultWriteBinary { request_id, path, bytes })
                }
                HostRequest::VaultImportFile { request_id, path, source, .. } => {
                    Message::Plugins(PluginsMsg::VaultImportFile { request_id, path, source })
                }
                HostRequest::VaultList { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultList { request_id, path })
                }
                HostRequest::VaultFiles { request_id, .. } => {
                    Message::Plugins(PluginsMsg::VaultFiles { request_id })
                }
                HostRequest::VaultExists { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultExists { request_id, path })
                }
                HostRequest::VaultRemove { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultRemove { request_id, path })
                }
                HostRequest::VaultRename { request_id, from, to, .. } => {
                    Message::Plugins(PluginsMsg::VaultRename { request_id, from, to })
                }
                HostRequest::VaultImageInfo { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultImageInfo { request_id, path })
                }
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::MeasureBatch { request_id, params })
                }
                HostRequest::QueryTheme { request_id, .. } => {
                    Message::Plugins(PluginsMsg::QueryTheme { request_id })
                }
                HostRequest::QueryFonts { request_id, .. } => {
                    Message::Plugins(PluginsMsg::QueryFonts { request_id })
                }
                HostRequest::TraySetItems { plugin_id, request_id, params } => {
                    Message::Plugins(PluginsMsg::TraySetItems { request_id, plugin_id, params })
                }
                HostRequest::CollabPeers { request_id, .. } => {
                    Message::Plugins(PluginsMsg::CollabPeers { request_id })
                }
                HostRequest::CollabSend { plugin_id, request_id, params } => {
                    Message::Plugins(PluginsMsg::CollabSend { request_id, plugin_id, params })
                }
                HostRequest::EditorSelection { request_id, .. } => {
                    Message::Plugins(PluginsMsg::EditorSelection { request_id })
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    Message::Plugins(PluginsMsg::EditorOpenFiles { request_id })
                }
                HostRequest::EditorReplaceSelection { request_id, text, .. } => {
                    Message::Plugins(PluginsMsg::EditorReplaceSelection { request_id, text })
                }
                HostRequest::EditorOpenFile { request_id, path, reveal, .. } => {
                    Message::Plugins(PluginsMsg::EditorOpenFile { request_id, path, reveal })
                }
                HostRequest::EditorPrefs { request_id, .. } => {
                    Message::Plugins(PluginsMsg::EditorPrefs { request_id })
                }
                HostRequest::DocOpen { plugin_id, request_id, params } => {
                    Message::Plugins(PluginsMsg::DocOpen { request_id, plugin_id, params })
                }
                HostRequest::DocRename { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::DocRename { request_id, params })
                }
                HostRequest::DocPropsRead { request_id, .. } => {
                    Message::Plugins(PluginsMsg::DocPropsRead { request_id })
                }
                HostRequest::DocPropsWrite { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::DocPropsWrite { request_id, params })
                }
                HostRequest::DocEdit { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::DocEdit { request_id, params })
                }
                HostRequest::DocSave { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::DocSave { request_id, params })
                }
                HostRequest::DocClose { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::DocClose { request_id, path })
                }
                HostRequest::DocAwareness { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::DocAwareness { request_id, params })
                }
                HostRequest::VaultBacklinks { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultBacklinks { request_id, path })
                }
                HostRequest::VaultTags { request_id, .. } => {
                    Message::Plugins(PluginsMsg::VaultTags { request_id })
                }
                HostRequest::CollabMetaGet { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::CollabMetaGet { request_id, params })
                }
                HostRequest::CollabMetaPatch { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::CollabMetaPatch { request_id, params })
                }
                HostRequest::HistoryDay { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::HistoryDay { request_id, params })
                }
                HostRequest::HistoryGroups { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::HistoryGroups { request_id, params })
                }
                HostRequest::RecentList { request_id, .. } => {
                    Message::Plugins(PluginsMsg::RecentList { request_id })
                }
                HostRequest::SpaceRoom { request_id, .. } => {
                    Message::Plugins(PluginsMsg::SpaceRoom { request_id })
                }
                HostRequest::SpaceMembers { request_id, .. } => {
                    Message::Plugins(PluginsMsg::SpaceMembers { request_id })
                }
                HostRequest::SpaceOpenMenu { request_id, .. } => {
                    Message::Plugins(PluginsMsg::SpaceOpenMenu { request_id })
                }
                HostRequest::SpaceOpenSettings { request_id, .. } => {
                    Message::Plugins(PluginsMsg::SpaceOpenSettings { request_id })
                }
                HostRequest::SpaceFilePatch { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::SpaceFilePatch { request_id, params })
                }
                HostRequest::HistoryRecord { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::HistoryRecord { request_id, params })
                }
                HostRequest::PanelHeader { plugin_id, request_id, params } => {
                    Message::Plugins(PluginsMsg::PanelHeader { plugin_id, request_id, params })
                }
                HostRequest::PanelFlushDone { plugin_id, request_id, params } => {
                    Message::Plugins(PluginsMsg::PanelFlushDone { plugin_id, request_id, params })
                }
                HostRequest::DownloadsDir { request_id, .. } => {
                    Message::Plugins(PluginsMsg::DownloadsDir { request_id })
                }
                HostRequest::VaultUi { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::VaultUi { request_id, params })
                }
                HostRequest::VaultUiWrite { request_id, patch, .. } => {
                    Message::Plugins(PluginsMsg::VaultUiWrite { request_id, patch })
                }
                HostRequest::VaultCreate { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::VaultCreate { request_id, params })
                }
                HostRequest::VaultSwitchContainer { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::VaultSwitchContainer { request_id, params })
                }
                HostRequest::VaultRemoveContainer { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::VaultRemoveContainer { request_id, params })
                }
                HostRequest::PickColor { request_id, params, .. } => {
                    Message::PluginPickColor { request_id, params }
                }
                HostRequest::PickVault { request_id, .. } => Message::PluginPickVault { request_id },
                HostRequest::Reveal { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::ShellReveal { request_id, params })
                }
                HostRequest::AbsolutePath { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::ShellAbsolutePath { request_id, path })
                }
                HostRequest::Duplicate { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::ShellDuplicate { request_id, params })
                }
                HostRequest::VaultListDirs { request_id, .. } => {
                    Message::Plugins(PluginsMsg::VaultListDirs { request_id })
                }
                HostRequest::VaultRefreshTree { request_id, .. } => {
                    Message::Plugins(PluginsMsg::VaultRefreshTree { request_id })
                }
                HostRequest::DragStart { plugin_id, request_id, params } => {
                    Message::PluginDragStart { request_id, plugin_id, params }
                }
                HostRequest::DragHover { request_id, params, .. } => {
                    Message::PluginDragHover { request_id, params }
                }
                HostRequest::DragEnd { request_id, params, .. } => {
                    Message::PluginDragEnd { request_id, params }
                }
                HostRequest::MenuOpen { request_id, plugin_id, params } => {
                    Message::PluginMenuOpen { request_id, plugin_id, params }
                }
                HostRequest::HistoryOpen { request_id, params, .. } => {
                    Message::Plugins(PluginsMsg::HistoryOpen { request_id, params })
                }
                HostRequest::HistoryActivity { request_id, .. } => {
                    Message::Plugins(PluginsMsg::HistoryActivity { request_id })
                }
            };
            let _ = iced::futures::SinkExt::send(&mut output, message).await;
        }
    })
}

/// 表格头部是否报告了未落盘改动（停用插件前据此拦住，避免内容随实例一起消失）。
fn table_header_dirty(app: &App) -> bool {
    app.panel_headers
        .get("table")
        .is_some_and(|entry| entry.plugin_id == crate::plugin_actions::TABLES_PLUGIN && entry.state.dirty)
}

/// 处理插件页消息；安装走异步文件选择器，其余动作同步完成。
pub fn update(app: &mut App, msg: PluginsMsg) -> Task<Message> {
    match msg {
        PluginsMsg::Refresh => {
            refresh(app);
            // 市场首开：缓存先行，未过期即不联网（见 market_auto_load）
            if app.plugins_state.market.is_none() {
                return update(app, PluginsMsg::MarketRefresh);
            }
            Task::none()
        }
        PluginsMsg::Toggle(id, on) => {
            if !on && id == crate::plugin_actions::TABLES_PLUGIN && table_header_dirty(app) {
                app.notify(NoticeLevel::Warning, "表格有未保存改动，先保存再停用".to_string());
                return Task::none();
            }
            if let Err(e) = app.plugins.set_enabled(&id, on) {
                app.notify(NoticeLevel::Error, format!("插件操作失败：{e}"));
            }
            refresh(app);
            // 视图 / 服务的启用态是注册表投影：插件启停后要重建，否则贡献仍留在选择器里
            crate::plugin_actions::rebuild_registry(app);
            Task::none()
        }
        PluginsMsg::SetBuiltinPluginEnabled(id, on) => {
            if !on && id == crate::plugin_actions::TABLES_PLUGIN && table_header_dirty(app) {
                app.notify(NoticeLevel::Warning, "表格有未保存改动，先保存再停用".to_string());
                return Task::none();
            }
            let disabled = &mut app.ui.disabled_plugins;
            if on {
                disabled.retain(|plugin| plugin != &id);
            } else if !disabled.iter().any(|plugin| plugin == &id) {
                disabled.push(id.clone());
            }
            app.schedule_persist();
            // 服务目录按归属插件启停：停用即让它实现的那些服务离开总线（有外部候选则由候选接手）
            app.plugins.set_builtin_owner_enabled(&id, on);
            // 停用态在注册表里是 enabled 投影，改完即重建（选择器与标签都读它）
            crate::plugin_actions::rebuild_registry(app);
            Task::none()
        }
        PluginsMsg::SetServiceProvider(service, provider) => {
            match app.plugins.set_service_provider(&service, provider.as_deref()) {
                Ok(()) => crate::plugin_actions::rebuild_registry(app),
                Err(e) => app.notify(NoticeLevel::Error, format!("切换提供者失败：{e}")),
            }
            Task::none()
        }
        PluginsMsg::Retry(id) => {
            if let Err(e) = app.plugins.retry(&id) {
                app.notify(NoticeLevel::Error, format!("插件重试失败：{e}"));
            }
            refresh(app);
            Task::none()
        }
        PluginsMsg::UninstallAsk(id) => {
            app.plugins_state.confirm_uninstall = Some(id);
            Task::none()
        }
        PluginsMsg::UninstallCancel => {
            app.plugins_state.confirm_uninstall = None;
            Task::none()
        }
        PluginsMsg::UninstallConfirm(id) => {
            app.plugins_state.confirm_uninstall = None;
            app.plugins_state.installing = true;
            let runtime = app.plugins.clone();
            let target = id.clone();
            Task::perform(
                async move { run_blocking(move || runtime.uninstall(&target)).await },
                move |result| Message::Plugins(PluginsMsg::Uninstalled(id.clone(), result)),
            )
        }
        PluginsMsg::InstallDir => start_install(app, true),
        PluginsMsg::InstallZip => start_install(app, false),
        PluginsMsg::Installed(result) => {
            app.plugins_state.installing = false;
            refresh(app);
            match result {
                Ok(Some(outcome)) => notify_installed(app, &outcome, "已安装"),
                Ok(None) => {}
                Err(e) => app.notify(NoticeLevel::Error, format!("安装失败：{e}")),
            }
            Task::none()
        }
        PluginsMsg::ReinstallSource(id) => {
            if app.plugins_state.installing || app.plugins_state.market_busy.is_some() {
                return Task::none();
            }
            app.plugins_state.installing = true;
            let runtime = app.plugins.clone();
            Task::perform(
                async move {
                    let target = id.clone();
                    run_blocking(move || runtime.install_from_source(&target)).await
                },
                |result| Message::Plugins(PluginsMsg::Reinstalled(result)),
            )
        }
        PluginsMsg::Reinstalled(result) => {
            app.plugins_state.installing = false;
            refresh(app);
            match result {
                Ok(outcome) => notify_installed(app, &outcome, "已从来源更新"),
                Err(e) => app.notify(NoticeLevel::Error, format!("从来源更新失败：{e}")),
            }
            Task::none()
        }
        PluginsMsg::Uninstalled(id, result) => {
            app.plugins_state.installing = false;
            app.tray.remove_plugin(&id);
            match result {
                Ok(()) => {
                    app.plugins_state.detail = None;
                    app.plugins_state.log_text = None;
                    app.plugins_state.log_error = None;
                    app.plugins_state.log_clear_ask = false;
                    app.notify(NoticeLevel::Success, format!("已卸载 {id}"));
                }
                Err(e) => app.notify(NoticeLevel::Error, format!("卸载失败：{e}")),
            }
            refresh(app);
            Task::none()
        }
        PluginsMsg::Detail(id) => {
            app.plugins_state.detail_audit = app.plugins.audit(&id);
            app.plugins_state.detail = Some(id);
            app.plugins_state.builtin_detail = None;
            app.plugins_state.detail_tab = DetailTab::Overview;
            app.plugins_state.log_text = None;
            app.plugins_state.log_error = None;
            app.plugins_state.log_clear_ask = false;
            Task::none()
        }
        PluginsMsg::DetailClose => {
            app.plugins_state.detail = None;
            app.plugins_state.log_text = None;
            app.plugins_state.log_error = None;
            app.plugins_state.log_clear_ask = false;
            Task::none()
        }
        PluginsMsg::BuiltinDetail(id) => {
            app.plugins_state.builtin_detail = Some(id);
            app.plugins_state.detail = None;
            Task::none()
        }
        PluginsMsg::BuiltinDetailClose => {
            app.plugins_state.builtin_detail = None;
            Task::none()
        }
        PluginsMsg::DetailTab(tab) => {
            app.plugins_state.detail_tab = tab;
            app.plugins_state.log_clear_ask = false;
            match tab {
                DetailTab::Overview => Task::none(),
                DetailTab::Log => load_log(app),
            }
        }
        PluginsMsg::LogRefresh => load_log(app),
        PluginsMsg::LogClearAsk => {
            app.plugins_state.log_clear_ask = true;
            Task::none()
        }
        PluginsMsg::LogClearCancel => {
            app.plugins_state.log_clear_ask = false;
            Task::none()
        }
        PluginsMsg::LogClear(id) => {
            app.plugins_state.log_clear_ask = false;
            let runtime = app.plugins.clone();
            let for_task = id.clone();
            Task::perform(
                async move { run_blocking(move || runtime.clear_log(&for_task)).await },
                move |result| Message::Plugins(PluginsMsg::LogCleared(id.clone(), result)),
            )
        }
        PluginsMsg::LogCleared(id, result) => {
            match result {
                Ok(()) => {
                    // 清空即推进纪元：清空前发起的读取回执一律作废
                    app.plugins_state.log_seq = app.plugins_state.log_seq.wrapping_add(1);
                    app.plugins_state.log_error = None;
                    if app.plugins_state.detail.as_deref() == Some(id.as_str())
                        && app.plugins_state.detail_tab == DetailTab::Log
                    {
                        // 插件可能已写入新行：按磁盘再读一次才是当前真相
                        load_log(app)
                    } else {
                        app.plugins_state.log_text = None;
                        Task::none()
                    }
                }
                Err(e) => {
                    app.notify(NoticeLevel::Error, format!("清空日志失败：{e}"));
                    Task::none()
                }
            }
        }
        PluginsMsg::CopyLogPath(path) => {
            app.notify(NoticeLevel::Success, "已复制日志路径");
            crate::clipboard::write_text_task(path).map(|()| Message::Noop)
        }
        PluginsMsg::LogLoaded { id, seq, result } => {
            // 详情已换插件、换分页，或读取已被清空 / 后来的读取取代：这次回执不作数
            if app.plugins_state.log_seq != seq
                || app.plugins_state.detail.as_deref() != Some(id.as_str())
                || app.plugins_state.detail_tab != DetailTab::Log
            {
                return Task::none();
            }
            match result {
                Ok(text) => {
                    app.plugins_state.log_text = Some(text);
                    app.plugins_state.log_error = None;
                }
                Err(e) => {
                    app.plugins_state.log_text = None;
                    app.plugins_state.log_error = Some(e);
                }
            }
            Task::none()
        }
        PluginsMsg::DialogAsk { request_id, dialog } => Task::perform(pick(dialog), move |paths| {
            Message::Plugins(PluginsMsg::DialogDone { request_id, paths })
        }),
        PluginsMsg::DialogDone { request_id, paths } => {
            app.plugins.resolve_dialog(request_id, paths);
            Task::none()
        }
        PluginsMsg::MarketRefresh => market_auto_load(app),
        PluginsMsg::MarketReload => match market_cache_path(app) {
            Some(cache) => start_market_load(app, cache),
            None => {
                market_cache_unavailable(app);
                Task::none()
            }
        },
        PluginsMsg::SetMode(mode) => {
            app.plugins_state.mode = mode;
            Task::none()
        }
        PluginsMsg::MarketQuery(query) => {
            app.plugins_state.market_query = query;
            Task::none()
        }
        PluginsMsg::MarketType(kind) => {
            app.plugins_state.market_type = kind;
            Task::none()
        }
        PluginsMsg::MarketInstallAsk(ask) => {
            // 官方插件直接安装；非官方先进确认弹窗（提示可读写文件、运行程序、联网）
            if ask.official {
                start_market_fetch(app, ask.id, ask.repo, false)
            } else {
                app.plugins_state.confirm_install = Some(ask);
                Task::none()
            }
        }
        PluginsMsg::MarketInstallConfirm => match app.plugins_state.confirm_install.take() {
            Some(ask) => start_market_fetch(app, ask.id, ask.repo, false),
            None => Task::none(),
        },
        PluginsMsg::MarketInstallCancel => {
            app.plugins_state.confirm_install = None;
            Task::none()
        }
        PluginsMsg::MarketLoaded(result) => {
            app.plugins_state.market_loading = false;
            match result {
                Ok(snapshot) => {
                    app.plugins_state.market = Some(snapshot);
                    // 刷新后原选中类型可能已不在索引里：清掉筛选，避免只剩「没有匹配的插件」
                    let gone = app
                        .plugins_state
                        .market_type
                        .clone()
                        .is_some_and(|kind| !market_types(app).iter().any(|t| t == &kind));
                    if gone {
                        app.plugins_state.market_type = None;
                    }
                }
                Err(e) => {
                    let have_cache = app.plugins_state.market.is_some();
                    app.notify(
                        NoticeLevel::Warning,
                        if have_cache {
                            format!("市场索引刷新失败，当前展示缓存内容：{e}")
                        } else {
                            format!("市场索引拉取失败：{e}")
                        },
                    );
                }
            }
            Task::none()
        }
        PluginsMsg::MarketUpdate { id, repo } => start_market_fetch(app, id, repo, true),
        PluginsMsg::MarketDone(result) => {
            app.plugins_state.market_busy = None;
            refresh(app);
            match result {
                Ok(MarketResult::UpToDate(text)) => app.notify(NoticeLevel::Info, text),
                Ok(MarketResult::Installed(done)) => notify_market(app, &done),
                Err(e) => app.notify(NoticeLevel::Error, format!("插件市场操作失败：{e}")),
            }
            Task::none()
        }
        PluginsMsg::ClipboardWrite { request_id, text } => Task::perform(
            async move { run_blocking(move || crate::clipboard::write_text(text)).await },
            move |result| Message::Plugins(PluginsMsg::ClipboardWritten { request_id, result }),
        ),
        PluginsMsg::ClipboardWritten { request_id, result } => {
            app.plugins
                .resolve_shell_call(request_id, result.map(|_| serde_json::json!({})));
            Task::none()
        }
        PluginsMsg::ClipboardRead { request_id } => Task::perform(
            async move { run_blocking(crate::clipboard::read_text).await },
            move |result| Message::Plugins(PluginsMsg::ClipboardReadDone { request_id, result }),
        ),
        PluginsMsg::ClipboardReadDone { request_id, result } => {
            let result =
                result.map(|text| serde_json::json!({ "text": text.unwrap_or_default() }));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::ClipboardReadImage { request_id } => Task::perform(
            async move { run_blocking(read_clipboard_image).await },
            move |result| Message::Plugins(PluginsMsg::ClipboardImageDone { request_id, result }),
        ),
        PluginsMsg::ClipboardImageDone { request_id, result } => {
            let result = result.map(atelyx_core::plugin_runtime::clipboard_image_payload);
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::ClipboardWriteImage { request_id, bytes } => Task::perform(
            async move { run_blocking(move || write_clipboard_image(&bytes)).await },
            move |result| Message::Plugins(PluginsMsg::ClipboardImageWritten { request_id, result }),
        ),
        PluginsMsg::ClipboardImageWritten { request_id, result } => {
            let result = result.map(|_| serde_json::json!({}));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::VaultRead { request_id, path } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => Task::perform(
                        async move {
                            run_blocking(move || vault::read_note(&root, &rel))
                                .await
                                .map(|content| serde_json::json!({ "content": content }))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    ),
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            client
                                .get_file_text(&space_id, &rel)
                                .await
                                .map(|content| serde_json::json!({ "content": content }))
                                .map_err(|e| e.to_string())
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultWrite { plugin_id, request_id, path, content } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            if let Some(open) = open_file_conflict(app, &rel) {
                // 持有该文件的插件可以写回自己的文件（表格插件保存自己打开的 .atb），
                // 其余插件仍按「正在编辑中」拒写，避免互踩保存链
                let owner = (app.ui.last_table_file.as_deref() == Some(rel.as_str())
                    && plugin_id == TABLES_PLUGIN_ID)
                    || (app.ui.last_canvas_file.as_deref() == Some(rel.as_str())
                        && plugin_id == CANVAS_PLUGIN_ID);
                if !owner {
                    return resolve_vault(
                        app,
                        request_id,
                        Err(format!("文件正在编辑器中打开，拒绝写入：{open}")),
                    );
                }
            }
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => {
                        let writes = app.vault.writes.clone();
                        let done_rel = rel.clone();
                        Task::perform(
                            async move {
                                let target =
                                    run_blocking(move || vault::safe_join(&root, &rel, true)).await?;
                                // 「是否新建」由写盘队列在执行序内判定，回执用于区分广播的 Created / Updated
                                writes
                                    .write_reporting_created(target, content)
                                    .await
                                    .map(|created| (serde_json::json!({}), created))
                            },
                            move |result| vault_write_done(request_id, done_rel, result),
                        )
                    }
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    let done_rel = rel.clone();
                    Task::perform(
                        async move {
                            client
                                .put_file_text(&space_id, &rel, &content)
                                .await
                                .map(|created| (serde_json::json!({}), created))
                                .map_err(|e| e.to_string())
                        },
                        move |result| vault_write_done(request_id, done_rel, result),
                    )
                }
            }
        }
        PluginsMsg::VaultReadBinary { request_id, path } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => Task::perform(
                        async move {
                            run_blocking(move || read_vault_binary(&root, &rel))
                                .await
                                .map(|bytes| atelyx_core::plugin_runtime::binary_payload(&bytes))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    ),
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            let bytes = client
                                .get_file_bytes(&space_id, &rel)
                                .await
                                .map_err(|e| e.to_string())?;
                            if bytes.len() > MAX_VAULT_BINARY_BYTES {
                                return Err(vault_binary_too_large());
                            }
                            Ok(atelyx_core::plugin_runtime::binary_payload(&bytes))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultWriteBinary { request_id, path, bytes } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            if let Some(open) = open_file_conflict(app, &rel) {
                return resolve_vault(
                    app,
                    request_id,
                    Err(format!("文件正在编辑器中打开，拒绝写入：{open}")),
                );
            }
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => {
                        let writes = app.vault.writes.clone();
                        let done_rel = rel.clone();
                        Task::perform(
                            async move {
                                let target =
                                    run_blocking(move || vault::safe_join(&root, &rel, true)).await?;
                                writes
                                    .write_bytes_reporting_created(target, bytes)
                                    .await
                                    .map(|created| (serde_json::json!({}), created))
                            },
                            move |result| vault_write_done(request_id, done_rel, result),
                        )
                    }
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    let done_rel = rel.clone();
                    Task::perform(
                        async move {
                            client
                                .put_file_bytes(&space_id, &rel, &bytes)
                                .await
                                .map(|created| (serde_json::json!({}), created))
                                .map_err(|e| e.to_string())
                        },
                        move |result| vault_write_done(request_id, done_rel, result),
                    )
                }
            }
        }
        PluginsMsg::VaultImportFile { request_id, path, source } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            if let Some(open) = open_file_conflict(app, &rel) {
                return resolve_vault(
                    app,
                    request_id,
                    Err(format!("文件正在编辑器中打开，拒绝写入：{open}")),
                );
            }
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => {
                        let writes = app.vault.writes.clone();
                        let done_rel = rel.clone();
                        Task::perform(
                            async move {
                                let target =
                                    run_blocking(move || vault::safe_join(&root, &rel, true)).await?;
                                let source = std::path::PathBuf::from(source);
                                writes
                                    .write_from_file(target, source)
                                    .await
                                    .map(|created| (serde_json::json!({}), created))
                            },
                            move |result| vault_write_done(request_id, done_rel, result),
                        )
                    }
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    let done_rel = rel.clone();
                    Task::perform(
                        async move {
                            let bytes = run_blocking(move || {
                                let source = std::path::PathBuf::from(&source);
                                let meta = std::fs::metadata(&source)
                                    .map_err(|e| format!("读取导入源失败：{e}"))?;
                                if meta.len() > SPACE_MAX_FILE_BYTES as u64 {
                                    return Err(format!(
                                        "文件超过协作空间单文件上限（{} MiB）",
                                        SPACE_MAX_FILE_BYTES / 1024 / 1024
                                    ));
                                }
                                std::fs::read(&source).map_err(|e| format!("读取导入源失败：{e}"))
                            })
                            .await?;
                            client
                                .put_file_bytes(&space_id, &rel, &bytes)
                                .await
                                .map(|created| (serde_json::json!({}), created))
                                .map_err(|e| e.to_string())
                        },
                        move |result| vault_write_done(request_id, done_rel, result),
                    )
                }
            }
        }
        PluginsMsg::VaultFiles { request_id } => {
            let result = serde_json::to_value(files_result(app))
                .map_err(|e| format!("可检索文件清单不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultRefreshTree { request_id } => {
            app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
            app.refresh_file_tree_task()
        }
        PluginsMsg::VaultListDirs { request_id } => {
            let gen = app.epoch();
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => {
                        let exclude = app.vault.exclude_folders().unwrap_or_default();
                        Task::perform(
                            async move {
                                run_blocking(move || {
                                    let tree = vault::list_vault_tree(&root, &exclude)?;
                                    Ok::<_, String>(dir_paths(&tree))
                                })
                                .await
                            },
                            move |result| {
                                Message::Plugins(PluginsMsg::VaultListDirsDone {
                                    request_id,
                                    gen,
                                    result,
                                })
                            },
                        )
                    }
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                            Ok::<_, String>(collect_space_dirs(&tree))
                        },
                        move |result| {
                            Message::Plugins(PluginsMsg::VaultListDirsDone {
                                request_id,
                                gen,
                                result,
                            })
                        },
                    )
                }
            }
        }
        PluginsMsg::VaultListDirsDone { request_id, gen, result } => {
            if gen != app.epoch() {
                return resolve_vault(app, request_id, Err("仓库已切换，请重新打开".to_string()));
            }
            let result = result.and_then(|dirs| {
                serde_json::to_value(atelyx_core::plugin_runtime::DirListResult { dirs })
                    .map_err(|e| format!("目录清单不可序列化：{e}"))
            });
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultList { request_id, path } => {
            let rel = match plugin_path((!path.is_empty()).then_some(path.as_str())) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => {
                        let exclude = app.vault.exclude_folders().unwrap_or_default();
                        Task::perform(
                            async move {
                                run_blocking(move || {
                                    let children = vault::read_dir_entries_all(&root, &rel, &exclude)?;
                                    Ok(dir_entries_json(&children))
                                })
                                .await
                            },
                            move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                        )
                    }
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            // 插件面与本地同口径：隐藏项可见（.tmp 残留由服务端过滤）
                            let dir = (!rel.is_empty()).then_some(rel.as_str());
                            let tree = client
                                .tree_including_hidden(&space_id, dir)
                                .await
                                .map_err(|e| e.to_string())?;
                            Ok(dir_entries_json(&crate::space_actions::children_of_tree(&tree, "")))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultExists { request_id, path } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => Task::perform(
                        async move {
                            let rel = rel.clone();
                            let exists =
                                run_blocking(move || vault::file_exists(&root, &rel)).await?;
                            Ok(serde_json::json!({ "exists": exists }))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    ),
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            let exists = client
                                .file_exists(&space_id, &rel)
                                .await
                                .map_err(|e| e.to_string())?;
                            Ok(serde_json::json!({ "exists": exists }))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultRemove { request_id, path } => {
            vault_change_gated(
                app,
                request_id,
                crate::PendingAfterFlush::PluginVaultRemove { request_id, path },
            )
        }
        PluginsMsg::VaultRename { request_id, from, to } => {
            vault_change_gated(
                app,
                request_id,
                crate::PendingAfterFlush::PluginVaultRename { request_id, from, to },
            )
        }
        PluginsMsg::VaultChangeDone { request_id, change, rewritten, result } => {
            if result.is_err() {
                app.plugins.resolve_shell_call(request_id, result);
                return Task::none();
            }
            match &change {
                VaultChange::Removed { rel } => {
                    app.plugins.on_file_changed(rel, FileChangeKind::Deleted, None);
                }
                VaultChange::Renamed { from, to } => {
                    app.plugins
                        .on_file_changed(to, FileChangeKind::Renamed, Some(from.as_str()));
                    // 展开集 / 打开中文件路径与文件树子目录缓存跟随（与文件树改名同一口径）
                    app.apply_follow(&crate::FollowUp::Renamed {
                        from: from.clone(),
                        to: to.clone(),
                        rewritten: Vec::new(),
                        announce: false,
                    });
                    app.follow_children_cache(from, to);
                }
            }
            // 引用同步代写了打开中的笔记：磁盘正文已变，重读对齐——结构变更经 gate
            // 已先 flush，此刻无未落盘输入可丢
            let open = app.ui.last_note_file.clone();
            let reload = open.filter(|path| rewritten.iter().any(|rel| rel == path));
            let refresh = refresh_after_vault_change(app, &change);
            app.plugins.resolve_shell_call(request_id, result);
            match reload {
                Some(path) => refresh.chain(
                    app.open_file(&path, crate::plugin_actions::OpenMode::Reveal),
                ),
                None => refresh,
            }
        }
        PluginsMsg::AiModels { request_id } => {
            let result = serde_json::to_value(ai_models(app))
                .map_err(|e| format!("模型清单序列化失败：{e}"));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::AiAgents { request_id } => {
            let result = serde_json::to_value(ai_agents(app))
                .map_err(|e| format!("Agent 清单序列化失败：{e}"));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::VaultImageInfo { request_id, path } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            match app.repo_kind.clone() {
                crate::RepoKind::Local => match app.vault.root() {
                    Ok(root) => Task::perform(
                        async move { run_blocking(move || read_image_info(&root, &rel)).await },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    ),
                    Err(e) => resolve_vault(app, request_id, Err(e)),
                },
                crate::RepoKind::Space(space) => {
                    let (client, space_id) = (space.client.clone(), space.space_id.clone());
                    Task::perform(
                        async move {
                            // 空间侧无区间读接口，取回整文件后仍只解析起始段（服务端单文件限额封顶）
                            let bytes = client
                                .get_file_bytes(&space_id, &rel)
                                .await
                                .map_err(|e| e.to_string())?;
                            let head = &bytes[..bytes.len().min(IMAGE_HEADER_BYTES as usize)];
                            image_info_json(head, &rel)
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::MeasureBatch { request_id, params } => {
            let sizes = params
                .items
                .iter()
                .map(|item| {
                    crate::plugin_theme::measure_text(
                        &item.text,
                        item.size,
                        item.weight,
                        item.family,
                        item.family_name.as_deref(),
                        item.max_width,
                        item.layout,
                        item.segments,
                    )
                })
                .collect();
            match serde_json::to_value(MeasureBatchResult { sizes }) {
                Ok(value) => resolve_vault(app, request_id, Ok(value)),
                Err(e) => resolve_vault(app, request_id, Err(format!("量算结果不可序列化：{e}"))),
            }
        }
        PluginsMsg::QueryFonts { request_id } => {
            let families = crate::ui::font::filter("")
                .into_iter()
                .map(|entry| entry.name.clone())
                .collect();
            let result = serde_json::to_value(
                atelyx_core::plugin_runtime::FontFamiliesResult { families },
            )
            .map_err(|e| format!("字体清单不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::QueryTheme { request_id } => {
            match serde_json::to_value(crate::plugin_theme::theme_result()) {
                Ok(value) => resolve_vault(app, request_id, Ok(value)),
                Err(e) => resolve_vault(app, request_id, Err(format!("主题结果不可序列化：{e}"))),
            }
        }
        PluginsMsg::EditorOpenFiles { request_id } => {
            let result = serde_json::to_value(atelyx_core::plugin_runtime::EditorOpenFiles {
                paths: crate::space_actions::open_file_paths(app),
            })
            .map_err(|e| format!("打开文件清单不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::EditorPrefs { request_id } => {
            let result = serde_json::to_value(crate::editor_prefs(app))
                .map_err(|e| format!("编辑偏好不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::EditorSelection { request_id } => {
            let result = serde_json::to_value(app.editor_selection())
                .map_err(|e| format!("选区结果不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::EditorReplaceSelection { request_id, text } => {
            let result = app.editor_replace_selection(text).map(|_| serde_json::json!({}));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::EditorOpenFile { request_id, path, reveal } => {
            let rel = match plugin_path(Some(&path)) {
                Ok(rel) => rel,
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
            // 打开走脏拦截：未落盘输入先 flush 再换文件（与文件树单击同一通道）
            let mode = if reveal {
                crate::plugin_actions::OpenMode::Reveal
            } else {
                crate::plugin_actions::OpenMode::Background
            };
            app.gate(crate::PendingAfterFlush::OpenFile { path: rel, mode })
        }
        PluginsMsg::DocOpen { request_id, plugin_id, params } => {
            crate::doc_session::open(app, request_id, &plugin_id, params)
        }
        PluginsMsg::DocRename { request_id, params } => {
            crate::doc_session::rename(app, request_id, params)
        }
        PluginsMsg::DocOpenDone(done) => crate::doc_session::open_done(app, done),
        PluginsMsg::DocEdit { request_id, params } => {
            crate::doc_session::edit(app, request_id, params)
        }
        PluginsMsg::DocPropsRead { request_id } => {
            crate::doc_session::props_read(app, request_id)
        }
        PluginsMsg::DocPropsWrite { request_id, params } => {
            crate::doc_session::props_write(app, request_id, params)
        }
        PluginsMsg::DocSave { request_id, params } => {
            crate::doc_session::save(app, request_id, params)
        }
        PluginsMsg::DocClose { request_id, path } => {
            crate::doc_session::close(app, request_id, &path)
        }
        PluginsMsg::DocAwareness { request_id, params } => {
            crate::doc_session::awareness(app, request_id, params)
        }
        PluginsMsg::VaultBacklinks { request_id, path } => {
            vault_backlinks(app, request_id, path)
        }
        PluginsMsg::VaultBacklinksDone { request_id, result } => {
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultTags { request_id } => {
            let tags: Vec<String> = app.tag_vocab.iter().map(|row| row.tag.clone()).collect();
            let result = serde_json::to_value(VaultTagsResult { tags })
                .map_err(|e| format!("标签词表不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::HistoryActivity { request_id } => {
            // 本地仓走侧文件计数（磁盘事实）；协作空间用历史聚合的版本条数，聚合未在手就排队等落地
            if matches!(app.repo_kind, crate::RepoKind::Local) {
                let state = app.vault.clone();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let (root, exclude, _) = state.session_snapshot()?;
                            let counts = atelyx_core::history::activity_by_day(&root, &exclude);
                            let mut days: Vec<DayActivity> = counts
                                .into_iter()
                                .map(|(day, count)| DayActivity { day, count })
                                .collect();
                            days.sort_by_key(|day| day.day);
                            serde_json::to_value(HistoryActivityResult { days })
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)
                    },
                    move |result| Message::Plugins(PluginsMsg::HistoryActivityDone { request_id, result }),
                );
            }
            if let Some(cached) = &app.repo_history {
                let result = match cached {
                    Ok(history) => crate::repo_history::activity_json(&history.timestamps),
                    Err(e) => Err(format!("历史聚合失败：{e}")),
                };
                return resolve_vault(app, request_id, result);
            }
            if !crate::repo_history::container_ready(app) {
                return resolve_vault(app, request_id, Err("未打开仓库".into()));
            }
            app.plugins_state.pending_history.push(PendingHistory::Activity(request_id));
            if app.repo_history_loading {
                return Task::none();
            }
            crate::repo_history::load(app)
        }
        PluginsMsg::HistoryActivityDone { request_id, result } => {
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::HistoryDay { request_id, params } => {
            let Some(day) = atelyx_core::history::parse_day(&params.day) else {
                return resolve_vault(app, request_id, Err(format!("日期非法：{}", params.day)));
            };
            // 聚合缓存在手即答；缺失则挂起并等下一次落地回填（未在途才发起）
            if let Some(cached) = &app.repo_history {
                let result = match cached {
                    Ok(history) => crate::repo_history::history_day_json(
                        crate::repo_history::day_entries(&history.groups, day),
                    ),
                    Err(e) => Err(format!("历史聚合失败：{e}")),
                };
                return resolve_vault(app, request_id, result);
            }
            if !crate::repo_history::container_ready(app) {
                return resolve_vault(app, request_id, Err("未打开仓库".into()));
            }
            app.plugins_state.pending_history.push(PendingHistory::Day(request_id, day));
            if app.repo_history_loading {
                return Task::none();
            }
            crate::repo_history::load(app)
        }
        PluginsMsg::HistoryGroups { request_id, params } => {
            crate::repo_history::request_groups(app, request_id, params)
        }
        PluginsMsg::RecentList { request_id } => {
            let list = crate::recent_files::list_for(app);
            let result =
                serde_json::to_value(list).map_err(|e| format!("最近打开清单不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::DownloadsDir { request_id } => {
            let result = match crate::vault::downloads_dir() {
                Ok(path) => serde_json::to_value(
                    atelyx_core::plugin_runtime::DownloadsDirResult {
                        path: path.to_string_lossy().into_owned(),
                    },
                )
                .map_err(|e| format!("下载目录不可序列化：{e}")),
                Err(e) => Err(e),
            };
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::HistoryRecord { request_id, params } => {
            app.plugins.resolve_ui_call(request_id, Ok(()));
            crate::repo_history::record_from_plugin(app, params)
        }
        PluginsMsg::PanelHeader { plugin_id, request_id, params } => {
            // 头部只能声明到自己的视图贡献上：target 不校验会让后声明者静默顶掉别人的头部
            let current_owner = app
                .panel_headers
                .get(&params.target)
                .map(|entry| entry.plugin_id.as_str());
            let allowed = crate::plugin_actions::panel_header_allowed(
                &app.plugins.contributions(),
                &plugin_id,
                &params.target,
                params.header.is_some(),
                current_owner,
            );
            if !allowed {
                let error = format!("面板头部 target 不属于该插件：{}", params.target);
                app.plugins.resolve_ui_call(request_id, Err(error));
                return Task::none();
            }
            app.plugins.resolve_ui_call(request_id, Ok(()));
            match params.header {
                Some(state) => {
                    app.panel_headers.insert(
                        params.target,
                        crate::PanelHeaderEntry { plugin_id, state },
                    );
                }
                None => {
                    app.panel_headers.remove(&params.target);
                }
            }
            Task::none()
        }
        PluginsMsg::PanelFlushDone { plugin_id, request_id, params } => {
            app.plugins.resolve_ui_call(request_id, Ok(()));
            app.settle_panel_flush(&plugin_id, params)
        }
        PluginsMsg::SpaceFilePatch { request_id, params } => {
            crate::space_actions::submit_file_patch(app, request_id, params)
        }
        PluginsMsg::SpaceFilePatchDone { gen, request_id, path, result } => {
            if gen != app.epoch() {
                app.plugins.resolve_shell_call(
                    request_id,
                    Err("会话已切换，保存结果作废".to_string()),
                );
                return Task::none();
            }
            match result {
                Ok(new_file) => {
                    let renamed = new_file != path;
                    // 补丁带标题时服务端已按标题改名：先把宿主打开路径同步到新路径，
                    // 再回执给插件——插件据回执改 session.file 时宿主已认新路径
                    if renamed && app.ui.last_table_file.as_deref() == Some(path.as_str()) {
                        app.ui.last_table_file = Some(new_file.clone());
                        app.plugins.on_table_changed(Some(&new_file));
                        app.plugins.on_open_files_changed();
                        app.schedule_persist();
                    }
                    app.plugins.on_file_changed(
                        &new_file,
                        if renamed {
                            atelyx_core::plugin_runtime::FileChangeKind::Renamed
                        } else {
                            atelyx_core::plugin_runtime::FileChangeKind::Updated
                        },
                        renamed.then_some(path.as_str()),
                    );
                    app.plugins.resolve_shell_call(
                        request_id,
                        Ok(serde_json::json!({ "path": new_file })),
                    );
                    app.reload_history()
                }
                Err(e) => {
                    app.plugins.resolve_shell_call(request_id, Err(e.clone()));
                    app.notify(NoticeLevel::Error, format!("保存失败：{e}"));
                    // 远端改名 / 删除会让本地树陈旧：失败后按服务端现况重取
                    crate::space_actions::refresh_space_tree(app)
                }
            }
        }
        PluginsMsg::SpaceRoom { request_id } => {
            let result = serde_json::to_value(space_room(app))
                .map_err(|e| format!("协作房间快照不可序列化：{e}"));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::SpaceMembers { request_id } => {
            let task = crate::space_actions::ensure_members(app);
            let result = serde_json::to_value(space_members(app))
                .map_err(|e| format!("成员名册不可序列化：{e}"));
            app.plugins.resolve_shell_call(request_id, result);
            task
        }
        PluginsMsg::SpaceOpenMenu { request_id } => {
            app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
            Task::done(Message::SpaceMenuOpen)
        }
        PluginsMsg::SpaceOpenSettings { request_id } => {
            app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
            Task::done(Message::Space(crate::SpaceMsg::OpenCollabSettings))
        }
        PluginsMsg::VaultUi { request_id, params } => vault_ui_read(app, request_id, params),
        PluginsMsg::VaultUiDone { request_id, result } => {
            let result = result.and_then(|entries| vault_ui_json(app, entries));
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultUiWrite { request_id, patch } => {
            vault_ui_write(app, request_id, patch)
        }
        PluginsMsg::ShellReveal { request_id, params } => {
            let resolved = (|| {
                let root = local_root(app)?;
                let path = if params.path.is_empty() {
                    root
                } else {
                    vault::safe_join(&root, &params.path, false).map_err(|e| e.to_string())?
                };
                Ok(path)
            })();
            match resolved {
                Ok(path) => {
                    crate::reveal_in_file_manager(&path, params.is_dir);
                    resolve_vault(app, request_id, Ok(serde_json::json!({})))
                }
                Err(e) => resolve_vault(app, request_id, Err(e)),
            }
        }
        PluginsMsg::ShellAbsolutePath { request_id, path } => {
            let result = (|| {
                let root = local_root(app)?;
                if path.is_empty() {
                    return Ok(root.to_string_lossy().into_owned());
                }
                vault::safe_join(&root, &path, false)
                    .map(|path| path.to_string_lossy().into_owned())
                    .map_err(|e| e.to_string())
            })();
            match result {
                Ok(path) => {
                    let result = serde_json::to_value(
                        atelyx_core::plugin_runtime::AbsolutePathResult { path },
                    )
                    .map_err(|e| format!("绝对路径不可序列化：{e}"));
                    resolve_vault(app, request_id, result)
                }
                Err(e) => resolve_vault(app, request_id, Err(e)),
            }
        }
        PluginsMsg::ShellDuplicate { request_id, params } => {
            let rel = match plugin_path(Some(&params.path)) {
                Ok(rel) if !rel.is_empty() => rel,
                Ok(_) => {
                    return resolve_vault(app, request_id, Err("仓库根不能创建副本".to_string()))
                }
                Err(e) => return resolve_vault(app, request_id, Err(e)),
            };
            app.duplicate_entry_reply(request_id, &rel, params.is_dir)
        }
        PluginsMsg::ShellDuplicateDone { request_id, gen, result } => {
            if gen != app.epoch() {
                return resolve_vault(app, request_id, Err("仓库已切换，请重新打开".to_string()));
            }
            let result = result.and_then(|path| {
                serde_json::to_value(atelyx_core::plugin_runtime::AbsolutePathResult { path })
                    .map_err(|e| format!("副本路径不可序列化：{e}"))
            });
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultCreate { request_id, params } => {
            app.plugin_create_entry(request_id, params)
        }
        PluginsMsg::VaultSwitchContainer { request_id, params } => {
            app.plugin_switch_container(request_id, params)
        }
        PluginsMsg::VaultRemoveContainer { request_id, params } => {
            app.plugin_remove_container(request_id, params)
        }
        PluginsMsg::HistoryOpen { request_id, params } => {
            let opened = (|| -> Result<_, String> {
                if !crate::repo_history::container_ready(app) {
                    return Err("未打开仓库".to_string());
                }
                let kind = crate::repo_history::parse_kind(&params.kind)
                    .ok_or_else(|| format!("未知的历史类型：{}", params.kind))?;
                let file = vault::validate_plugin_path(&params.file)?;
                // 历史弹层挂在工作区窗口上：插件窗口没有工作区 chrome
                let window = app
                    .focused_window
                    .filter(|id| app.main_id == Some(*id) || app.detached_os.contains_key(id))
                    .ok_or_else(|| "没有可打开历史的工作区窗口".to_string())?;
                Ok((window, kind, file))
            })();
            match opened {
                Ok((window, kind, file)) => {
                    app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
                    crate::history_browser::open(app, window, kind, file)
                }
                Err(e) => resolve_vault(app, request_id, Err(e)),
            }
        }
        PluginsMsg::CollabMetaGet { request_id, params } => {
            let Some(session) = crate::space_actions::session_of(app) else {
                return resolve_vault(
                    app,
                    request_id,
                    Err("协作依赖协作空间：当前不是协作空间".to_string()),
                );
            };
            let (client, space_id) = (session.client, session.space_id);
            let single = params.key.is_some();
            Task::perform(
                async move {
                    let values = match client.team_meta_get(&space_id, params.key.as_deref()).await {
                        Ok(meta) => meta.values,
                        // 404 单键 = 键不存在（按空值答）；全量读只剩空间已不存在一种解释
                        Err(SpaceError::NotFound) if single => Default::default(),
                        Err(SpaceError::NotFound) => {
                            return Err("空间不存在或已失去访问权限".to_string())
                        }
                        Err(e) => return Err(e.to_string()),
                    };
                    serde_json::to_value(CollabMetaResult { values: values.into_iter().collect() })
                        .map_err(|e| format!("团队元数据不可序列化：{e}"))
                },
                move |result| Message::Plugins(PluginsMsg::CollabMetaDone { request_id, result }),
            )
        }
        PluginsMsg::CollabMetaPatch { request_id, params } => {
            let Some(session) = crate::space_actions::session_of(app) else {
                return resolve_vault(
                    app,
                    request_id,
                    Err("协作依赖协作空间：当前不是协作空间".to_string()),
                );
            };
            if !session.role.can_write() {
                return resolve_vault(
                    app,
                    request_id,
                    Err("该空间为只读身份，无法修改团队元数据".to_string()),
                );
            }
            let (client, space_id) = (session.client, session.space_id);
            let values: Vec<(String, String)> =
                params.entries.into_iter().map(|entry| (entry.key, entry.value)).collect();
            Task::perform(
                async move {
                    client
                        .team_meta_patch(&space_id, &values)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({}))
                },
                move |result| Message::Plugins(PluginsMsg::CollabMetaDone { request_id, result }),
            )
        }
        PluginsMsg::CollabMetaDone { request_id, result } => {
            // 键变更由服务端向空间成员广播（含本端）后经 `space:meta.changed` 送达订阅插件
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultCallDone { request_id, result } => {
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::VaultWriteDone { request_id, rel, created, result } => {
            // 只推宿主发起的写入；失败不广播（文件未被改动）
            if result.is_ok() {
                let kind = match created {
                    true => FileChangeKind::Created,
                    false => FileChangeKind::Updated,
                };
                app.plugins.on_file_changed(&rel, kind, None);
            }
            resolve_vault(app, request_id, result)
        }
        PluginsMsg::TraySetItems { request_id, plugin_id, params } => {
            let result = app
                .tray
                .set_items(&plugin_id, &params.items)
                .map(|_| serde_json::json!({}));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::CollabPeers { request_id } => {
            let result = serde_json::to_value(collab_peers(app))
                .map_err(|e| format!("协作成员序列化失败：{e}"));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
        PluginsMsg::CollabSend { request_id, plugin_id, params } => {
            let result = collab_send(app, &plugin_id, params);
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
    }
}

/// 当前容器的本机仓库根；协作空间没有本机路径（调用方据此拒绝）。
fn local_root(app: &App) -> Result<std::path::PathBuf, String> {
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return Err("协作空间没有本机路径".to_string());
    }
    app.vault.root().map_err(|e| e.to_string())
}

/// 仓库相对路径校验：`None` 与空串都表示仓库根（插件列根目录、在根上新建都这样传）。
fn plugin_path(path: Option<&str>) -> Result<String, String> {
    match path {
        None | Some("") => Ok(String::new()),
        Some(path) => vault::validate_plugin_path(path),
    }
}

/// 目录条目转协议形态（`FsEntry` 只有 name + kind：name 取条目相对路径的最后一段，与 `fs.list` 同口径）。
fn dir_entries_json(children: &[vault::DirChild]) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = children
        .iter()
        .map(|child| {
            let name = child.rel.rsplit('/').next().unwrap_or(child.rel.as_str());
            serde_json::json!({
                "name": name,
                "kind": if child.is_dir { "dir" } else { "file" },
            })
        })
        .collect();
    serde_json::json!({ "entries": entries })
}

/// 回填一次仓库调用并结束消息处理。
pub(crate) fn resolve_vault(
    app: &App,
    request_id: u64,
    result: Result<serde_json::Value, String>,
) -> Task<Message> {
    app.plugins.resolve_shell_call(request_id, result);
    Task::none()
}

/// 反链扫描：本地按 wiki 索引增量刷新，空间走服务端端点；结果按协议形态回执。
fn vault_backlinks(app: &App, request_id: u64, path: String) -> Task<Message> {
    let rel = match plugin_path(Some(&path)) {
        Ok(rel) => rel,
        Err(e) => return resolve_vault(app, request_id, Err(e)),
    };
    let title = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".md").to_string();
    let entries = |rows: Vec<(String, String)>| {
        rows.into_iter()
            .map(|(path, title)| serde_json::json!({ "path": path, "title": title }))
            .collect::<Vec<_>>()
    };
    match app.repo_kind.clone() {
        crate::RepoKind::Local => {
            let state = app.vault.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let (root, exclude, _) = state.session_snapshot()?;
                        let mut guard = state.wiki.lock().map_err(|e| e.to_string())?;
                        let index = guard.get_or_insert_with(Default::default);
                        vault::refresh_wiki_index(&root, &exclude, index)?;
                        Ok(vault::query_wiki_backlinks(index, &title, &rel))
                    })
                    .await
                    .map_err(|e| e.to_string())?
                },
                move |result| {
                    Message::Plugins(PluginsMsg::VaultBacklinksDone {
                        request_id,
                        result: result.map(|rows| {
                            let rows = rows.into_iter().map(|row| (row.file, row.title)).collect();
                            serde_json::json!({ "entries": entries(rows) })
                        }),
                    })
                },
            )
        }
        crate::RepoKind::Space(_) => {
            let Some(session) = crate::space_actions::session_of(app) else {
                return resolve_vault(
                    app,
                    request_id,
                    Err("协作依赖协作空间：当前不是协作空间".into()),
                );
            };
            let (client, space_id) = (session.client, session.space_id);
            Task::perform(
                async move {
                    client
                        .backlinks(&space_id, &title, &rel)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |result| {
                    Message::Plugins(PluginsMsg::VaultBacklinksDone {
                        request_id,
                        result: result.map(|rows| {
                            let rows = rows.into_iter().map(|row| (row.file, row.title)).collect();
                            serde_json::json!({ "entries": entries(rows) })
                        }),
                    })
                },
            )
        }
    }
}

/// 回绝挂起的历史查询（容器切换后旧查询的前提已不存在，不结算会让插件调用永远挂起）。
pub(crate) fn fail_pending_history(app: &mut App, reason: &str) {
    for query in std::mem::take(&mut app.plugins_state.pending_history) {
        app.plugins
            .resolve_shell_call(query.request_id(), Err(reason.to_string()));
    }
}

/// 写入任务的完成消息：`created` 决定广播 `vault:file.changed` 的 Created / Updated。
fn vault_write_done(
    request_id: u64,
    rel: String,
    result: Result<(serde_json::Value, bool), String>,
) -> Message {
    let (result, created) = match result {
        Ok((value, created)) => (Ok(value), created),
        Err(e) => (Err(e), false),
    };
    Message::Plugins(PluginsMsg::VaultWriteDone { request_id, rel, result, created })
}

/// 重命名任务的完成消息：引用改写清单随回执带回（打开中的笔记据此重读）。
fn renamed_done(
    request_id: u64,
    change: VaultChange,
    result: Result<(serde_json::Value, Vec<String>), String>,
) -> PluginsMsg {
    let (result, rewritten) = match result {
        Ok((value, rewritten)) => (Ok(value), rewritten),
        Err(e) => (Err(e), Vec::new()),
    };
    PluginsMsg::VaultChangeDone { request_id, change, rewritten, result }
}

/// 仓库结构变更经 gate 放行：挂起输入先落盘（与文件树删除/改名同一口径，无脏零成本直通），
/// 引用同步的代写与重读才有「无未落盘输入」前提。已有 flush 动作在途时不排队：
/// 可读拒绝，插件稍后重试。
fn vault_change_gated(
    app: &mut App,
    request_id: u64,
    pending: crate::PendingAfterFlush,
) -> Task<Message> {
    if app.flush_pending.is_some() {
        return resolve_vault(app, request_id, Err("宿主保存进行中，请稍后重试".into()));
    }
    app.gate(pending)
}

/// gate 放行后的仓库删除执行体（挂起输入已落盘）。
pub(crate) fn execute_vault_remove(
    app: &mut App,
    request_id: u64,
    path: String,
) -> Task<Message> {
    let rel = match plugin_path(Some(&path)) {
        Ok(rel) => rel,
        Err(e) => return resolve_vault(app, request_id, Err(e)),
    };
    if let Some(blocked) = vault_change_conflict(app, &rel, None) {
        return resolve_vault(app, request_id, Err(blocked));
    }
    let change = VaultChange::Removed { rel: rel.clone() };
    match app.repo_kind.clone() {
        crate::RepoKind::Local => match app.vault.root() {
            Ok(root) => Task::perform(
                async move { run_blocking(move || remove_local(&root, &rel)).await },
                move |result| {
                    Message::Plugins(PluginsMsg::VaultChangeDone {
                        request_id,
                        change,
                        rewritten: Vec::new(),
                        result,
                    })
                },
            ),
            Err(e) => resolve_vault(app, request_id, Err(e)),
        },
        crate::RepoKind::Space(space) => {
            let (client, space_id) = (space.client.clone(), space.space_id.clone());
            Task::perform(
                async move { remove_space(&client, &space_id, &rel).await },
                move |result| {
                    Message::Plugins(PluginsMsg::VaultChangeDone {
                        request_id,
                        change,
                        rewritten: Vec::new(),
                        result,
                    })
                },
            )
        }
    }
}

/// gate 放行后的仓库改名/移动执行体（挂起输入已落盘，引用代写基于完整磁盘内容）。
pub(crate) fn execute_vault_rename_allow_open(
    app: &mut App,
    request_id: u64,
    from: String,
    to: String,
) -> Task<Message> {
    execute_vault_rename_inner(app, request_id, from, to, true)
}

pub(crate) fn execute_vault_rename(
    app: &mut App,
    request_id: u64,
    from: String,
    to: String,
) -> Task<Message> {
    execute_vault_rename_inner(app, request_id, from, to, false)
}

fn execute_vault_rename_inner(
    app: &mut App,
    request_id: u64,
    from: String,
    to: String,
    allow_open_note: bool,
) -> Task<Message> {
    let from = match plugin_path(Some(&from)) {
        Ok(rel) => rel,
        Err(e) => return resolve_vault(app, request_id, Err(e)),
    };
    let to = match plugin_path(Some(&to)) {
        Ok(rel) => rel,
        Err(e) => return resolve_vault(app, request_id, Err(e)),
    };
    if from == to {
        return resolve_vault(app, request_id, Ok(serde_json::json!({})));
    }
    let allowed = allow_open_note.then_some(from.as_str());
    for target in [&from, &to] {
        if let Some(blocked) = vault_change_conflict(app, target, allowed) {
            return resolve_vault(app, request_id, Err(blocked));
        }
    }
    let change = VaultChange::Renamed { from: from.clone(), to: to.clone() };
    match app.repo_kind.clone() {
        crate::RepoKind::Local => match app.vault.root() {
            Ok(root) => {
                let exclude = app.vault.exclude_folders().unwrap_or_default();
                Task::perform(
                    async move {
                        run_blocking(move || rename_local(&root, &exclude, &from, &to)).await
                    },
                    move |result| Message::Plugins(renamed_done(request_id, change, result)),
                )
            }
            Err(e) => resolve_vault(app, request_id, Err(e)),
        },
        crate::RepoKind::Space(space) => {
            let (client, space_id) = (space.client.clone(), space.space_id.clone());
            Task::perform(
                async move { rename_space(&client, &space_id, &from, &to).await },
                move |result| Message::Plugins(renamed_done(request_id, change, result)),
            )
        }
    }
}

/// 结构变更后刷新宿主文件面：本地按父目录重读子项，空间拉整树；再重建笔记清单与标签词表。
fn refresh_after_vault_change(app: &mut App, change: &VaultChange) -> Task<Message> {
    let mut task = if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        crate::space_actions::refresh_space_tree(app)
    } else {
        match change {
            VaultChange::Removed { rel } => app.load_children(&crate::parent_of(rel)),
            VaultChange::Renamed { from, to } => app
                .load_children(&crate::parent_of(from))
                .chain(app.load_children(&crate::parent_of(to))),
        }
    };
    task = task
        .chain(app.refresh_note_files())
        .chain(app.refresh_tag_vocab());
    // 改名同步了提示词标记与 Agent 的提示词引用
    if matches!(change, VaultChange::Renamed { .. }) {
        task = task.chain(app.load_vault_meta());
    }
    task
}

/// 删除 / 重命名前的会话冲突：目标或其子路径正在编辑器中打开，或被当前画布引用。
/// 这两类持有者都在内存里保留旧路径，宿主变更会与之竞争（写回幽灵文件 / 覆盖改写后的引用）。
/// `allow_path` = 唯一允许放行的打开路径（笔记改名时的当前篇，内容已 flush）。
fn vault_change_conflict(app: &App, rel: &str, allow_path: Option<&str>) -> Option<String> {
    let target = rel.replace('\\', "/");
    let prefix = format!("{target}/");
    let open = [
        app.ui.last_note_file.as_deref(),
        app.ui.last_table_file.as_deref(),
        app.ui.last_canvas_file.as_deref(),
        app.doc_open.as_ref().map(|d| d.path.as_str()),
    ];
    if let Some(path) = open.into_iter().flatten().find(|path| {
        let path = path.replace('\\', "/");
        path == target || path.starts_with(&prefix)
    }) {
        let allowed = allow_path
            .map(|allowed| allowed.replace('\\', "/") == path.replace('\\', "/"))
            .unwrap_or(false);
        if !allowed {
            return Some(format!("路径正在编辑器中打开，拒绝操作：{path}"));
        }
    }
    // 画布内容由插件持有，按磁盘上的画布判断引用：未落盘的新引用不在此列
    let canvas_file = app.ui.last_canvas_file.as_deref()?;
    let (root, _, _) = app.vault.session_snapshot().ok()?;
    let (_, canvas) = atelyx_core::vault::read_canvas_file_cached(&app.vault, &root, canvas_file)
        .ok()?;
    for node in &canvas.nodes {
        let field = match node.node_type.as_str() {
            "text" | "media" | "table" => "file",
            "conversation" => "systemPromptFile",
            _ => continue,
        };
        let Some(path) = node.data.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = path.replace('\\', "/");
        if path == target || path.starts_with(&prefix) {
            return Some(format!(
                "路径正被画布「{}」引用，拒绝操作：{path}",
                canvas.title
            ));
        }
    }
    None
}

/// 本地删除：按磁盘事实判定文件 / 文件夹；表图片附件随表私有，删除后尽力清理。
fn remove_local(root: &std::path::Path, rel: &str) -> Result<serde_json::Value, String> {
    let target = vault::safe_join(root, rel, false)
        .map_err(|e| format!("路径不可用（{rel}）：{e}"))?;
    let meta = std::fs::metadata(&target)
        .map_err(|e| format!("路径不存在或不可访问（{rel}）：{e}"))?;
    let is_dir = meta.is_dir();
    let attachments = vault::table_attachment_dirs(root, rel, is_dir);
    if is_dir {
        vault::delete_folder(root, rel, true)?;
    } else {
        vault::delete_vault_file(root, rel)?;
    }
    vault::purge_table_attachments(attachments);
    Ok(serde_json::json!({}))
}

/// 本地重命名 / 移动：目标不可已存在（case-only 改名豁免），引用由 core 事务同步。
fn rename_local(
    root: &std::path::Path,
    exclude: &[String],
    from: &str,
    to: &str,
) -> Result<(serde_json::Value, Vec<String>), String> {
    let source = vault::safe_join(root, from, false)
        .map_err(|e| format!("源路径不可用（{from}）：{e}"))?;
    let meta = std::fs::metadata(&source)
        .map_err(|e| format!("源路径不存在或不可访问（{from}）：{e}"))?;
    let is_dir = meta.is_dir();
    if is_dir && to.starts_with(&format!("{from}/")) {
        return Err("不能把文件夹移动到自身内部".to_string());
    }
    // case-only 改名不查重：Windows 的存在性判断大小写不敏感，会把源自身当占用
    if !to.eq_ignore_ascii_case(from) {
        let occupied = vault::safe_join(root, to, false).is_ok_and(|path| path.exists());
        if occupied {
            return Err(format!("目标已存在：{to}"));
        }
    }
    let sync = if is_dir {
        atelyx_core::vault_refs::rename_folder_synced(root, exclude, from, to)?
    } else {
        atelyx_core::vault_refs::rename_note_synced(root, exclude, from, to)?
    };
    Ok((serde_json::json!({}), sync.rewritten))
}

/// 空间删除：按整树（含隐藏项）判定条目种类；文件夹递归删除。
async fn remove_space(
    client: &atelyx_core::space_client::SpaceClient,
    space_id: &str,
    rel: &str,
) -> Result<serde_json::Value, String> {
    let tree = client
        .tree_including_hidden(space_id, None)
        .await
        .map_err(|e| e.to_string())?;
    let is_dir = find_tree_entry(&tree, rel)
        .map(|node| node.is_dir)
        .ok_or_else(|| format!("路径不存在：{rel}"))?;
    if is_dir {
        client
            .delete_folder(space_id, rel, true)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        client.delete_file(space_id, rel).await.map_err(|e| e.to_string())?;
    }
    Ok(serde_json::json!({}))
}

/// 空间重命名 / 移动：目标不可已存在；改名后按最新整树改写全库 .md 链接。
async fn rename_space(
    client: &atelyx_core::space_client::SpaceClient,
    space_id: &str,
    from: &str,
    to: &str,
) -> Result<(serde_json::Value, Vec<String>), String> {
    let tree = client
        .tree_including_hidden(space_id, None)
        .await
        .map_err(|e| e.to_string())?;
    let is_dir = find_tree_entry(&tree, from)
        .map(|node| node.is_dir)
        .ok_or_else(|| format!("源路径不存在：{from}"))?;
    if is_dir && to.starts_with(&format!("{from}/")) {
        return Err("不能把文件夹移动到自身内部".to_string());
    }
    if find_tree_entry(&tree, to).is_some() {
        return Err(format!("目标已存在：{to}"));
    }
    client.rename_file(space_id, from, to).await.map_err(|e| e.to_string())?;
    let tree = client.tree(space_id).await.map_err(|e| e.to_string())?;
    let rewritten =
        crate::space_actions::rewrite_links_after_rename(client, space_id, &tree, from, to).await;
    Ok((serde_json::json!({}), rewritten))
}

fn find_tree_entry<'a>(
    tree: &'a [atelyx_core::space_client::TreeNode],
    rel: &str,
) -> Option<&'a atelyx_core::space_client::TreeNode> {
    for node in tree {
        if node.path == rel {
            return Some(node);
        }
        if let Some(found) = find_tree_entry(&node.children, rel) {
            return Some(found);
        }
    }
    None
}

/// 模型清单：只出 id / 名称与面板当前选择（地址与密钥不出宿主）。
fn ai_models(app: &App) -> AiModelsResult {
    let providers = app
        .repo_config
        .providers
        .iter()
        .flatten()
        .map(|provider| AiModelProvider {
            id: provider.id.clone(),
            name: provider.name.clone(),
            models: provider
                .models
                .iter()
                .map(|model| AiModel {
                    id: model.id.clone(),
                    nickname: model.nickname.clone(),
                })
                .collect(),
        })
        .collect();
    let current = crate::chat_actions::chat_overrides().0.as_ref().and_then(|value| {
        Some(AiModelPick {
            provider_id: value.get("providerId")?.as_str()?.to_string(),
            model: value.get("model")?.as_str()?.to_string(),
        })
    });
    AiModelsResult { providers, current }
}

/// Agent 清单；面板无显式选择时按生效值回落内置「对话」。
fn ai_agents(app: &App) -> AiAgentsResult {
    let agents = app
        .agents
        .iter()
        .map(|agent| AiAgent {
            id: agent.id.clone(),
            name: agent.name.clone(),
            builtin: agent.builtin.unwrap_or(false),
        })
        .collect();
    let current = crate::chat_actions::chat_overrides()
        .2
        .or_else(|| Some(atelyx_core::vault::BUILTIN_AGENT_CHAT_ID.to_string()));
    AiAgentsResult { agents, current }
}

/// 解码后 RGBA 容量上限：压缩载荷限额挡不住解压炸弹，按像素尺寸先挡一道。
const MAX_CLIPBOARD_IMAGE_RGBA_BYTES: u64 = 256 * 1024 * 1024;

/// PNG 字节 → 系统剪贴板（解码与写剪贴板都在阻塞线程池执行）。
fn write_clipboard_image(bytes: &[u8]) -> Result<(), String> {
    let reader = image::ImageReader::with_format(
        std::io::Cursor::new(bytes),
        image::ImageFormat::Png,
    );
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| format!("解码图片失败：{e}"))?;
    if u64::from(width) * u64::from(height) * 4 > MAX_CLIPBOARD_IMAGE_RGBA_BYTES {
        return Err("图片像素过多，无法写入剪贴板".to_string());
    }
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| format!("解码图片失败：{e}"))?;
    crate::clipboard::write_image(width, height, image.to_rgba8().into_raw())
}

/// 写入目标是否正由编辑器持有：壳的会话里保留着该文件的内存副本，插件覆盖会与之竞争。
/// 比较归一化相对路径（`\` 与 `/` 同义）。
/// 表格插件 id：它持有当前打开的 `.atb`，允许写回自己的文件。
const TABLES_PLUGIN_ID: &str = crate::plugin_actions::TABLES_PLUGIN;
/// 画布插件 id：它持有当前打开的 `.atlx`，允许写回自己的文件。
const CANVAS_PLUGIN_ID: &str = crate::plugin_actions::CANVAS_PLUGIN;

fn open_file_conflict(app: &App, rel: &str) -> Option<String> {
    let target = rel.replace('\\', "/");
    let open = [
        app.ui.last_note_file.as_deref(),
        app.ui.last_table_file.as_deref(),
        app.ui.last_canvas_file.as_deref(),
        app.doc_open.as_ref().map(|d| d.path.as_str()),
    ];
    open.into_iter()
        .flatten()
        .find(|path| path.replace('\\', "/") == target)
        .map(str::to_string)
}

/// 读仓库内一个二进制文件（与文本读同一路径校验）。按上限 +1 限量读取：既挡住超限文件，
/// 也免掉「先查大小再读」之间文件被并发改大的窗口。
fn read_vault_binary(root: &std::path::Path, rel: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let target = vault::safe_join(root, rel, false)?;
    let file = std::fs::File::open(&target)
        .map_err(|e| format!("读取文件失败（{rel}）：{e}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_VAULT_BINARY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取文件失败（{rel}）：{e}"))?;
    if bytes.len() > MAX_VAULT_BINARY_BYTES {
        return Err(vault_binary_too_large());
    }
    Ok(bytes)
}

fn vault_binary_too_large() -> String {
    format!(
        "文件超过仓库二进制上限（{} MiB），请改用 fs",
        MAX_VAULT_BINARY_BYTES / (1024 * 1024)
    )
}

/// 读图片尺寸只需文件头，按此上限取前缀（各支持格式的尺寸字段都在起始段内）。
const IMAGE_HEADER_BYTES: u64 = 4 * 1024 * 1024;

/// 本地读图片固有尺寸：超限的大图也能取（只读文件头前缀，不整读文件）。
fn read_image_info(root: &std::path::Path, rel: &str) -> Result<serde_json::Value, String> {
    let head =
        vault::read_file_head(root, rel, IMAGE_HEADER_BYTES).map_err(|e| format!("读取图片失败（{rel}）：{e}"))?;
    image_info_json(&head, rel)
}

/// 图片头 → 协议尺寸；认不出格式可读失败（插件据此提示而不是拿 0 继续画）。
fn image_info_json(bytes: &[u8], rel: &str) -> Result<serde_json::Value, String> {
    let (width, height) = crate::image_dimensions(bytes);
    if width == 0 || height == 0 {
        return Err(format!("不是可识别的图片（png / jpeg / gif / bmp / webp）：{rel}"));
    }
    serde_json::to_value(ImageInfoResult { width, height })
        .map_err(|e| format!("图片尺寸序列化失败：{e}"))
}

/// 可切换的容器清单：最近仓库按最近打开序在前，协作空间在后
/// （服务端空间列表在前、最近空间兜底补齐，登录即见全部账号空间）。
fn container_entries(app: &App) -> Vec<ContainerEntry> {
    let mut out: Vec<ContainerEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let active_space = match &app.repo_kind {
        crate::RepoKind::Space(session) => {
            Some((session.server_url.clone(), session.space_id.clone()))
        }
        crate::RepoKind::Local => None,
    };
    let active_root = app.vault_root.as_ref().map(|r| r.to_string_lossy().into_owned());
    for vault_root in &app.global.recent_vaults {
        let id = format!("local:{}", vault_root.root);
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(ContainerEntry {
            id,
            label: vault_root.name.clone(),
            root: Some(vault_root.root.clone()),
            server_url: None,
            space_id: None,
            active: active_root.as_deref() == Some(vault_root.root.as_str()),
        });
    }
    if let Some(identity) = &app.global.last_identity {
        for space in &app.space_list {
            let id = format!("space:{}#{}", identity.server_url, space.space_id);
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(ContainerEntry {
                id,
                label: space.name.clone(),
                root: None,
                server_url: Some(identity.server_url.clone()),
                space_id: Some(space.space_id.clone()),
                active: active_space
                    .as_ref()
                    .is_some_and(|(server, space_id)| {
                        server == &identity.server_url && space_id == &space.space_id
                    }),
            });
        }
    }
    for space in &app.global.recent_spaces {
        let id = format!("space:{}#{}", space.server_url, space.space_id);
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(ContainerEntry {
            id,
            label: space.name.clone(),
            root: None,
            server_url: Some(space.server_url.clone()),
            space_id: Some(space.space_id.clone()),
            active: active_space
                .as_ref()
                .is_some_and(|(server, space_id)| server == &space.server_url && space_id == &space.space_id),
        });
    }
    out
}

/// 展开集 / 排序 / 目录颜色 / 提示词标记的当前快照（宿主是这几项的真源）。
fn vault_ui_state(app: &App) -> VaultUiState {
    VaultUiState {
        expanded: app.ui.file_explorer_expanded.clone(),
        sort: app.file_sort.clone(),
        folder_colors: app
            .folder_colors
            .iter()
            .map(|(path, color)| (path.clone(), color.clone()))
            .collect(),
        prompt_notes: app.prompt_notes.clone(),
    }
}

/// 读界面状态（`vault.ui`）：状态快照 + 可选一个目录的条目。
/// 目录条目按磁盘现读——宿主自己的 `children` 缓存只在进仓库 / 刷新时补齐，插件展开一个新目录时
/// 那份缓存里没有它，取缓存会让新展开的目录显示为空。
fn vault_ui_read(app: &mut App, request_id: u64, params: VaultUiParams) -> Task<Message> {
    let Some(dir) = params.dir else {
        return resolve_vault(app, request_id, vault_ui_json(app, Vec::new()));
    };
    let rel = match plugin_path(Some(&dir)) {
        Ok(rel) => rel,
        Err(e) => return resolve_vault(app, request_id, Err(e)),
    };
    let sort = app.file_sort.clone();
    match app.repo_kind.clone() {
        crate::RepoKind::Local => match app.vault.root() {
            Ok(root) => {
                let exclude = app.vault.exclude_folders().unwrap_or_default();
                Task::perform(
                    async move {
                        run_blocking(move || {
                            let mut children = vault::read_dir_entries(&root, &rel, &exclude)?;
                            vault::sort_children(&mut children, &sort);
                            Ok(ui_entries(&children))
                        })
                        .await
                    },
                    move |result| Message::Plugins(PluginsMsg::VaultUiDone { request_id, result }),
                )
            }
            Err(e) => resolve_vault(app, request_id, Err(e)),
        },
        crate::RepoKind::Space(space) => {
            let (client, space_id) = (space.client.clone(), space.space_id.clone());
            Task::perform(
                async move {
                    // 空间没有按目录取子项的端点：整树切片后再取该目录的直接子项
                    let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                    let mut children = crate::space_actions::children_of_tree(&tree, &rel);
                    vault::sort_children(&mut children, &sort);
                    Ok(ui_entries(&children))
                },
                move |result| Message::Plugins(PluginsMsg::VaultUiDone { request_id, result }),
            )
        }
    }
}

/// 文件树里的全部目录路径（不含仓库根）。
fn dir_paths(tree: &[vault::FileTreeNode]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(nodes: &[vault::FileTreeNode], out: &mut Vec<String>) {
        for node in nodes.iter().filter(|node| node.is_dir) {
            out.push(node.path.clone());
            walk(&node.children, out);
        }
    }
    walk(tree, &mut out);
    out
}

/// 空间整树里的全部目录路径（不含仓库根）。
fn collect_space_dirs(tree: &[atelyx_core::space_client::TreeNode]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(nodes: &[atelyx_core::space_client::TreeNode], out: &mut Vec<String>) {
        for node in nodes.iter().filter(|node| node.is_dir) {
            out.push(node.path.clone());
            walk(&node.children, out);
        }
    }
    walk(tree, &mut out);
    out
}

/// 目录子项转回执条目（名称取末段，与宿主文件树同一口径）。
fn ui_entries(children: &[vault::DirChild]) -> Vec<UiDirEntry> {
    children
        .iter()
        .map(|child| UiDirEntry {
            name: child.rel.rsplit('/').next().unwrap_or(child.rel.as_str()).to_string(),
            path: child.rel.clone(),
            is_dir: child.is_dir,
            mtime: child.mtime,
        })
        .collect()
}

/// 拼一份完整的 `vault.ui` 回执。
fn vault_ui_json(
    app: &App,
    entries: Vec<atelyx_core::plugin_runtime::UiDirEntry>,
) -> Result<serde_json::Value, String> {
    let result = VaultUiResult {
        state: vault_ui_state(app),
        entries,
        containers: container_entries(app),
        doc_types: app
            .registry
            .doc_types()
            .iter()
            .map(|info| atelyx_core::plugin_runtime::DocTypeEntry {
                extension: info.registration.extension.clone(),
                label: info.registration.label.clone(),
                icon: info.registration.icon.clone(),
            })
            .collect(),
    };
    serde_json::to_value(result).map_err(|e| format!("仓库界面状态不可序列化：{e}"))
}

/// 写界面状态（`vault.updateUi`）：只改给出的键，回执 = 写后的完整快照。
/// 展开集落 `ui-state`；排序 / 目录颜色 / 提示词标记落仓库侧文件，与宿主自身写入同一个出口。
fn vault_ui_write(app: &mut App, request_id: u64, patch: VaultUiPatch) -> Task<Message> {
    let mut task = Task::none();
    if let Some(expanded) = patch.expanded {
        task = task.chain(app.ensure_children_loaded(&expanded));
        app.ui.file_explorer_expanded = expanded;
        app.schedule_persist();
    }
    if let Some(sort) = patch.sort {
        app.file_sort = sort.clone();
        task = task.chain(app.save_file_sort(sort));
    }
    if let Some(colors) = patch.folder_colors {
        app.folder_colors = colors.into_iter().collect();
        task = task.chain(app.save_folder_colors());
    }
    if let Some(notes) = patch.prompt_notes {
        app.prompt_notes = notes;
        task = task.chain(app.save_prompt_notes());
    }
    let result = serde_json::to_value(vault_ui_state(app))
        .map_err(|e| format!("仓库界面状态不可序列化：{e}"));
    app.plugins.resolve_shell_call(request_id, result);
    task
}

fn files_result(app: &App) -> FilesResult {
    FilesResult {
        files: app
            .note_files
            .iter()
            .map(|(path, name)| VaultFileEntry {
                path: path.clone(),
                name: name.clone(),
                icon: layout_view::file_icon(app, name).into_owned(),
                openable: atelyx_core::vault::image_ext_of(name).is_none(),
            })
            .collect(),
    }
}

/// 当前容器与在线成员快照：本端身份只在协作空间给（本地仓库没有设备行），
/// 各端打开的文件带行图标（插件画不出宿主注册表里的领域文档图标）。
fn space_room(app: &App) -> SpaceRoomResult {
    let kind = match app.repo_kind {
        crate::RepoKind::Local => SpaceContainerKind::Local,
        crate::RepoKind::Space(_) => SpaceContainerKind::Space,
    };
    let self_identity = matches!(kind, SpaceContainerKind::Space).then(|| {
        let (name, color) = crate::space_actions::local_identity(app);
        SpaceSelfIdentity {
            name,
            color,
            platform: crate::space_actions::client_platform().to_string(),
            version: crate::space_actions::CLIENT_VERSION.to_string(),
            files: peer_files(app, crate::space_actions::open_file_paths(app)),
        }
    });
    SpaceRoomResult {
        kind,
        connected: app.space_connected,
        self_peer_id: app.space_own_peer_id.clone(),
        self_identity,
        peers: app
            .space_peers
            .iter()
            .map(|peer| SpacePeer {
                peer_id: peer.peer_id.clone(),
                nickname: peer.nickname.clone(),
                device: peer.device_name.clone(),
                color: peer.color.clone(),
                platform: peer.platform.clone(),
                version: peer.version.clone(),
                files: peer_files(
                    app,
                    crate::space_actions::peer_open_files(peer.presence.as_ref()),
                ),
            })
            .collect(),
    }
}

/// 打开的文件清单 → 带图标键的行（图标按文件名走宿主注册表）。
fn peer_files(app: &App, paths: Vec<String>) -> Vec<SpacePeerFile> {
    paths
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(&path);
            SpacePeerFile { icon: layout_view::file_icon(app, name).into_owned(), path }
        })
        .collect()
}

/// 空间成员名册快照（与宿主成员管理弹层共用同一份缓存）。
fn space_members(app: &App) -> SpaceMembersResult {
    SpaceMembersResult {
        ready: app.space_members.is_some(),
        loading: app.space_members_loading,
        error: app.space_members_error.clone(),
        members: app
            .space_members
            .iter()
            .flatten()
            .map(|member| SpaceMember {
                username: member.username.clone(),
                display_name: member.display_name.clone(),
                role: match member.role {
                    atelyx_core::space_client::SpaceRole::Owner => SpaceMemberRole::Owner,
                    atelyx_core::space_client::SpaceRole::Editor => SpaceMemberRole::Editor,
                    atelyx_core::space_client::SpaceRole::Viewer => SpaceMemberRole::Viewer,
                },
            })
            .collect(),
    }
}

/// 当前空间的在场清单：未连接时只给 `connected: false`（名册是连接期快照，断开即失效）。
fn collab_peers(app: &App) -> CollabPeersResult {
    if !app.space_connected {
        return CollabPeersResult::default();
    }
    CollabPeersResult {
        connected: true,
        self_id: app.space_own_peer_id.clone(),
        peers: app
            .space_peers
            .iter()
            .map(|peer| CollabPeer {
                peer_id: peer.peer_id.clone(),
                name: Some(peer.nickname.clone()),
            })
            .collect(),
    }
}

/// 发一条插件消息：未连接时按协议回 `queued: false`（不是错误）；对端 id 非法是可读失败。
/// `qos = ephemeral` 走瞬时通道（不缓存不补投）。
fn collab_send(
    app: &App,
    plugin_id: &str,
    params: CollabSendParams,
) -> Result<serde_json::Value, String> {
    let target = match params.target_peer_id {
        Some(id) => Some(
            id.parse::<u64>()
                .map_err(|_| format!("对端 id 无效：{id}"))?,
        ),
        None => None,
    };
    let ephemeral = params.qos == Some(CollabSendQos::Ephemeral);
    let queued = app.space_connected && app.space_cmds.is_some();
    if queued {
        crate::space_actions::send_plugin_message(
            app,
            plugin_id,
            &params.channel,
            params.payload,
            target,
            ephemeral,
        );
    }
    serde_json::to_value(CollabSendResult { queued })
        .map_err(|e| format!("协作发送结果序列化失败：{e}"))
}

/// 读系统剪贴板里的图片并编成 PNG（阻塞调用，在任务线程池执行）。像素读取走常驻剪贴板线程
/// （与写串行化，系统剪贴板同一时刻只能被一个线程打开）；协议侧拿 PNG，插件不处理像素格式；
/// 剪贴板里没有图片是新常态，按 `None` 回填。
fn read_clipboard_image() -> Result<Option<ClipboardImage>, String> {
    use image::ImageEncoder;

    let Some((width, height, rgba)) = crate::clipboard::read_image()? else {
        return Ok(None);
    };
    let (width, height) = (width as u32, height as u32);
    let mut png: Vec<u8> = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("编码剪贴板图片失败：{e}"))?;
    Ok(Some(ClipboardImage {
        base64: atelyx_core::plugin_runtime::encode_base64(&png),
        width,
        height,
    }))
}

/// 市场缓存的落盘路径（插件根旁；根不可用 = 不缓存只内存展示）。
fn market_cache_path(app: &App) -> Option<std::path::PathBuf> {
    app.market_cache_path.clone()
}

/// 市场缓存不可用（应用数据目录缺失）：索引不缓存、只内存展示，本次刷新中止并提示。
fn market_cache_unavailable(app: &mut App) {
    app.plugins_state.market_loading = false;
    app.notify(
        NoticeLevel::Error,
        "应用数据目录不可用：市场索引无法缓存，刷新中止".to_string(),
    );
}

/// 打开插件页时的市场读取：缓存先行；未过期即不再联网，过期或缺失才刷新。
fn market_auto_load(app: &mut App) -> Task<Message> {
    let Some(cache) = market_cache_path(app) else {
        market_cache_unavailable(app);
        return Task::none();
    };
    if app.plugins_state.market.is_none() {
        app.plugins_state.market = market::read_cache(&cache);
    }
    let fresh = app
        .plugins_state
        .market
        .as_ref()
        .is_some_and(|snapshot| !market::is_stale(snapshot.fetched_at, market::now_ms()));
    if fresh {
        return Task::none();
    }
    start_market_load(app, cache)
}

/// 起一次索引拉取：拉取期间保留既有缓存内容（失败时仍有内容展示，见 `MarketLoaded`）。
fn start_market_load(app: &mut App, cache: PathBuf) -> Task<Message> {
    app.plugins_state.market_loading = true;
    Task::perform(
        async move { market::fetch_snapshot(&cache).await },
        |result| Message::Plugins(PluginsMsg::MarketLoaded(result)),
    )
}

/// 市场安装 / 更新：取 Release 信息（更新先比对版本）→ 下载平台 zip → 既有安装链落位。
/// 与本地安装共用互斥（任意一条安装链在途即拒绝新发起），防两条链并发落位互踩。
/// 安装包清单 id 必须与市场条目 id 一致（否则一份包可以覆盖另一个插件的目录），
/// 下载物只服务本次安装，装完即删。
fn start_market_fetch(app: &mut App, id: String, repo: String, check_update: bool) -> Task<Message> {
    if app.plugins_state.market_busy.is_some() || app.plugins_state.installing {
        return Task::none();
    }
    app.plugins_state.market_busy = Some(id.clone());
    let runtime = app.plugins.clone();
    let id_for_task = id.clone();
    Task::perform(
        async move {
            let release = market::latest_plugin_release(&repo, &id_for_task).await?;
            let installed = runtime
                .list()
                .into_iter()
                .find(|p| p.id == id_for_task)
                .map(|p| p.version);
            if check_update {
                if let Some(installed) = &installed {
                    if atelyx_core::updater::compare_versions(installed, &release.version)
                        != std::cmp::Ordering::Less
                    {
                        return Ok(MarketResult::UpToDate(format!("已是最新版本（v{installed}）")));
                    }
                }
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let zip = market::download_release_asset(
                &release,
                &id_for_task,
                std::env::temp_dir().join("atelyx-market").as_path(),
                cancel,
                Arc::new(|_, _| {}),
            )
            .await?;
            let installer = runtime.clone();
            let expected = id_for_task.clone();
            let install_path = zip.clone();
            let outcome =
                run_blocking(move || installer.install_zip(&install_path, Some(&expected))).await;
            // 下载物只服务本次安装：无论成败都清掉（残留会占容量，且下次复用到旧包更难排查）
            let _ = std::fs::remove_file(&zip);
            Ok(MarketResult::Installed(MarketOutcome {
                result: outcome?,
                version: release.version,
                update: installed.is_some(),
            }))
        },
        move |result| Message::Plugins(PluginsMsg::MarketDone(result)),
    )
}

/// 替插件弹系统对话框，返回用户选定的路径（取消 = `None`）。
async fn pick(request: DialogRequest) -> Option<Vec<String>> {
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Some(title) = &request.title {
        dialog = dialog.set_title(title);
    }
    if let Some(default_path) = &request.default_path {
        // 协议给的是路径：目录部分作起始目录，文件名部分预填进名称栏
        let path = std::path::Path::new(default_path);
        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(name) = path.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
    }
    for filter in &request.filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    match (request.kind, request.save, request.multiple) {
        (DialogKind::File, false, true) => dialog
            .pick_files()
            .await
            .map(|handles| handles.into_iter().map(|handle| handle.path().to_string_lossy().into_owned()).collect()),
        (DialogKind::Directory, _, _) => dialog
            .pick_folder()
            .await
            .map(|handle| vec![handle.path().to_string_lossy().into_owned()]),
        (DialogKind::File, true, _) => dialog
            .save_file()
            .await
            .map(|handle| vec![handle.path().to_string_lossy().into_owned()]),
        (DialogKind::File, false, false) => dialog
            .pick_file()
            .await
            .map(|handle| vec![handle.path().to_string_lossy().into_owned()]),
    }
}

/// 运行时事件：通知 / 未声明调用 / 失败与熔断呈现；只有会改变列表状态的事件才触发刷新。
pub fn on_event(app: &mut App, event: PluginEvent) {
    let list_changed = matches!(
        event,
        PluginEvent::StatusChanged { .. }
            | PluginEvent::PermissionDenied { .. }
    );
    match &event {
        PluginEvent::Notification { id, title, body, level } => {
            let text = match body {
                Some(body) => format!("{title}：{body}"),
                None => title.clone(),
            };
            app.notify(notice_level(*level), format!("[{id}] {text}"));
        }
        PluginEvent::PermissionDenied { id, method, .. } => {
            app.notify(
                NoticeLevel::Warning,
                format!("插件 {id} 调用了未声明的能力：{method}"),
            );
        }
        PluginEvent::StatusChanged { id, status } => {
            // 进程不在跑就不再持有托盘条目（重启后由插件重新提交）
            if *status != PluginStatus::Running {
                app.tray.remove_plugin(id);
            }
            if let PluginStatus::Failed(reason) = status {
                app.notify(NoticeLevel::Error, format!("插件 {id} 启动失败：{reason}"));
            }
        }
        // 界面 / 贡献 / AI 工具 / 服务目录 / 默认布局声明由壳的投影与布局接线统一处理（见 lib.rs 的事件分支）
        PluginEvent::ViewSubmitted { .. }
        | PluginEvent::ContributionsChanged { .. }
        | PluginEvent::AiToolsChanged { .. }
        | PluginEvent::ServiceChanged { .. }
        | PluginEvent::AiChatEvent { .. }
        | PluginEvent::DefaultLayoutDeclared { .. } => {}
    }
    if list_changed && app.settings_open && app.settings_tab == "plugins" {
        refresh(app);
    }
}

fn refresh(app: &mut App) {
    app.plugins_state.list = app.plugins.list();
    if let Some(id) = app.plugins_state.detail.clone() {
        app.plugins_state.detail_audit = app.plugins.audit(&id);
    }
    crate::plugin_actions::rebuild_registry(app);
}

/// 读详情当前插件的日志尾部（异步；回执按插件 id 与纪元对号入座，详见 `LogLoaded`）。
fn load_log(app: &mut App) -> Task<Message> {
    let Some(id) = app.plugins_state.detail.clone() else {
        return Task::none();
    };
    app.plugins_state.log_seq = app.plugins_state.log_seq.wrapping_add(1);
    let seq = app.plugins_state.log_seq;
    app.plugins_state.log_text = None;
    app.plugins_state.log_error = None;
    app.plugins_state.log_clear_ask = false;
    let runtime = app.plugins.clone();
    let for_task = id.clone();
    Task::perform(run_blocking(move || runtime.log_tail(&for_task)), move |result| {
        Message::Plugins(PluginsMsg::LogLoaded { id, seq, result })
    })
}

/// 安装完成提示：内容已落位。重启失败与「还没启用」给出不同下一步——两者都不是安装失败。
fn notify_installed(app: &mut App, outcome: &InstallOutcome, done: &str) {
    let enabled = app
        .plugins_state
        .list
        .iter()
        .any(|p| p.id == outcome.id && p.enabled);
    let (level, text) = match (&outcome.start_error, enabled) {
        (Some(err), _) => (
            NoticeLevel::Warning,
            format!("{done} {}，但启动失败：{err}（可在列表里重试）", outcome.id),
        ),
        (None, true) => (
            NoticeLevel::Success,
            format!("{done} {}，已用新内容重启", outcome.id),
        ),
        (None, false) => (
            NoticeLevel::Success,
            format!("{done} {}，可在列表里启用", outcome.id),
        ),
    };
    app.notify(level, text);
}

/// 市场安装 / 更新完成提示（新装不自动启用，更新按启用态由宿主重启）。
fn notify_market(app: &mut App, done: &MarketOutcome) {
    let head = if done.update {
        format!("已更新 {} 到 v{}", done.result.id, done.version)
    } else {
        format!("已安装 {} v{}", done.result.id, done.version)
    };
    let (level, text) = match &done.result.start_error {
        Some(err) => (
            NoticeLevel::Warning,
            format!("{head}，但启动失败：{err}（可在列表里重试）"),
        ),
        None if done.update => (NoticeLevel::Success, head),
        None => (NoticeLevel::Success, format!("{head}，可在列表里启用")),
    };
    app.notify(level, text);
}

/// 起一次本地安装：先弹文件选择器，再在阻塞线程池里安装（重活不占异步 worker；失败即回执为可读错误）。
/// 与市场安装共用互斥（`installing` 同时挡住两条链），防并发落位互踩。
fn start_install(app: &mut App, from_dir: bool) -> Task<Message> {
    if app.plugins_state.installing || app.plugins_state.market_busy.is_some() {
        return Task::none();
    }
    app.plugins_state.installing = true;
    let runtime = app.plugins.clone();
    Task::perform(
        async move {
            let picked = if from_dir {
                rfd::AsyncFileDialog::new().set_title("选择插件目录").pick_folder().await
            } else {
                rfd::AsyncFileDialog::new()
                    .set_title("选择插件包")
                    .add_filter("插件包", &["zip"])
                    .pick_file()
                    .await
            };
            let Some(handle) = picked else {
                return Ok(None);
            };
            let path = handle.path().to_path_buf();
            let result = if from_dir {
                run_blocking(move || runtime.install_dir(&path)).await
            } else {
                run_blocking(move || runtime.install_zip(&path, None)).await
            };
            result.map(Some)
        },
        |result| Message::Plugins(PluginsMsg::Installed(result)),
    )
}

/// 把阻塞式插件 I/O（安装 / 卸载、文件树递归操作）放到阻塞线程池执行，不冻结 UI 与异步 worker。
async fn run_blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| format!("插件后台任务失败：{e}"))?
}

fn notice_level(level: NotificationLevel) -> NoticeLevel {
    match level {
        NotificationLevel::Info => NoticeLevel::Info,
        NotificationLevel::Success => NoticeLevel::Success,
        NotificationLevel::Warning => NoticeLevel::Warning,
        NotificationLevel::Error => NoticeLevel::Error,
    }
}

/// 插件页视图：模式切换（已安装 / 市场）+ 当前模式内容。
pub fn view(app: &App) -> Element<'static, Message> {
    let active = match app.plugins_state.mode {
        PluginsMode::Installed => "installed",
        PluginsMode::Market => "market",
    };
    let tabs = controls::segmented_tabs(
        &[("installed", "已安装"), ("market", "市场")],
        active,
        |id| {
            Message::Plugins(PluginsMsg::SetMode(if id == "market" {
                PluginsMode::Market
            } else {
                PluginsMode::Installed
            }))
        },
    );
    let body = match app.plugins_state.mode {
        PluginsMode::Installed => installed_group(app),
        PluginsMode::Market => market_group(app),
    };
    let col = Column::new()
        .spacing(12)
        .padding(Padding::ZERO.horizontal(20.0))
        .push(tabs)
        .push(body);
    scroll::v(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 已安装页：安装入口 + 已装列表（卡片含启停 / 详情 / 卸载 / 重试）。
/// 随包预置插件归入内置插件区块，这里只列用户自行安装的插件。
fn installed_group(app: &App) -> Element<'static, Message> {
    let installing = app.plugins_state.installing;
    let installed: Vec<&PluginSummary> = app
        .plugins_state
        .list
        .iter()
        .filter(|summary| !summary.preset)
        .collect();
    let mut col = Column::new().spacing(8);
    col = col.push(group_head("安装", "本地目录 / zip 包", false));
    col = col.push(card(
        row![
            text("从本地目录或压缩包安装插件")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
            space().width(Length::Fill),
            install_button("从本地目录", PluginsMsg::InstallDir, installing),
            install_button("从 zip 安装", PluginsMsg::InstallZip, installing),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center),
    ));
    col = col.push(group_head("内置插件", "随应用分发，可整体停用；贡献与服务见详情", false));
    col = col.push(builtin_group(app));
    col = col.push(group_head("已安装", &format!("共 {} 个", installed.len()), false));
    if installed.is_empty() {
        col = col.push(controls::empty_state(
            "puzzle",
            "暂无插件",
            "从本地目录或 zip 安装插件",
            None,
        ));
    }
    for summary in installed {
        col = col.push(plugin_card(app, summary));
    }
    col.into()
}

/// 内置插件：进程内插件每个一张卡片（插件级启停开关 + 详情），随包预置插件沿用已装插件卡片。
fn builtin_group(app: &App) -> Element<'static, Message> {
    let mut col = Column::new().spacing(6);
    for plugin in app.registry.builtin_plugins() {
        // 同 id 的已装 / 预置插件已有一张带启停的卡片，内置壳不再重复列一次
        if app.plugins_state.list.iter().any(|summary| summary.id == plugin.id) {
            continue;
        }
        col = col.push(stacked_card(vec![builtin_plugin_row(plugin)]));
    }
    for summary in app.plugins_state.list.iter().filter(|summary| summary.preset) {
        col = col.push(plugin_card(app, summary));
    }
    col.into()
}

/// 内置插件行：名称 + id + 详情 + 插件级启停开关（平台插件标不可停用）。
fn builtin_plugin_row(plugin: &BuiltinPluginInfo) -> Element<'static, Message> {
    let mut line = row![
        icon(plugin.icon.as_ref(), 14.0, pal::TEXT_SECONDARY),
        text(plugin.label.clone()).size(pal::fs_body()).color(pal::TEXT_PRIMARY),
        text(plugin.id.clone()).size(pal::fs_caption()).color(pal::TEXT_MUTED),
        space().width(Length::Fill),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    line = line.push(controls::btn(
        "详情",
        &controls::btn_sm(),
        controls::Variant::Ghost,
        Some(Message::Plugins(PluginsMsg::BuiltinDetail(plugin.id.clone()))),
    ));
    line = if plugin.id == PLATFORM_PLUGIN {
        line.push(controls::badge("不可停用", pal::BG_TERTIARY, pal::TEXT_MUTED))
    } else {
        line.push(controls::toggle(
            plugin.enabled,
            Some(Message::Plugins(PluginsMsg::SetBuiltinPluginEnabled(
                plugin.id.clone(),
                !plugin.enabled,
            ))),
        ))
    };
    line.into()
}

/// 预置插件视图行：随所属插件整体启停，只显示名称与 id。
fn builtin_view_row(entry: &Entry) -> Element<'static, Message> {
    row![
        icon(entry.icon.as_ref(), 14.0, pal::TEXT_MUTED),
        text(entry.label.clone()).size(pal::fs_body()).color(pal::TEXT_SECONDARY),
        text(entry.id.clone()).size(pal::fs_caption()).color(pal::TEXT_MUTED),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

/// 服务行：活动提供者与方法数一行显示；候选可切换，外部提供者在任时可回落内置。
fn service_row(entry: &Entry) -> Element<'static, Message> {
    let mut line = row![
        icon(entry.icon.as_ref(), 14.0, pal::TEXT_SECONDARY),
        text(entry.label.clone()).size(pal::fs_body()).color(pal::TEXT_PRIMARY),
        text(entry.detail.clone()).size(pal::fs_caption()).color(pal::TEXT_MUTED),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    if !entry.replaceable {
        line = line.push(controls::badge("不可替换", pal::BG_TERTIARY, pal::TEXT_MUTED));
    }
    line = line.push(space().width(Length::Fill));
    for candidate in &entry.candidates {
        line = line.push(controls::btn(
            format!("切换到 {candidate}"),
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::SetServiceProvider(
                entry.id.clone(),
                Some(candidate.clone()),
            ))),
        ));
    }
    if entry.replaceable && entry.builtin && entry.provider.is_some() {
        line = line.push(controls::btn(
            "回落内置",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::SetServiceProvider(entry.id.clone(), None))),
        ));
    }
    line.into()
}

/// 市场页：搜索 / 刷新 / 类型筛选 + 条目卡片（徽标 / 安装 / 更新）。
fn market_group(app: &App) -> Element<'static, Message> {
    let loading = app.plugins_state.market_loading;
    let mut col = Column::new().spacing(8);
    let search = controls::search_input(
        "搜索插件（名称 / id / 描述 / 仓库）",
        &app.plugins_state.market_query,
        |query| Message::Plugins(PluginsMsg::MarketQuery(query)),
    );
    let refresh = controls::btn(
        if loading { "刷新中…" } else { "刷新索引" },
        &controls::btn_sm(),
        controls::Variant::Ghost,
        (!loading).then_some(Message::Plugins(PluginsMsg::MarketReload)),
    );
    col = col.push(
        row![search, refresh]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
    );
    let types = market_types(app);
    if !types.is_empty() {
        col = col.push(type_filters(app, &types));
    }
    if app.plugins_state.market.is_none() {
        return col
            .push(if loading {
                controls::empty_state("loader-circle", "加载市场…", "正在拉取官方索引", None)
            } else {
                controls::empty_state("puzzle", "市场未加载", "刷新索引后浏览可安装的插件", None)
            })
            .into();
    }
    let entries = filtered_market_entries(app);
    if entries.is_empty() {
        col = col.push(controls::empty_state(
            "puzzle",
            "没有匹配的插件",
            "换个关键词或类型试试",
            None,
        ));
    }
    for entry in entries {
        col = col.push(market_card(app, entry));
    }
    col.into()
}

/// 索引里出现过的类型（去重排序）；空 = 索引不带类型信息，不显示筛选条。
fn market_types(app: &App) -> Vec<String> {
    let Some(snapshot) = &app.plugins_state.market else {
        return Vec::new();
    };
    let mut types: Vec<String> = snapshot.items.iter().filter_map(|e| e.r#type.clone()).collect();
    types.sort();
    types.dedup();
    types
}

/// 类型筛选胶囊条（全部 + 各类型；类型多时单行横向滚动）。
fn type_filters(app: &App, types: &[String]) -> Element<'static, Message> {
    let mut bar = row![].spacing(6);
    bar = bar.push(type_chip("全部", app.plugins_state.market_type.is_none(), None));
    for kind in types {
        let selected = app.plugins_state.market_type.as_deref() == Some(kind.as_str());
        bar = bar.push(type_chip(kind, selected, Some(kind.clone())));
    }
    scrollable(bar)
        .direction(iced::widget::scrollable::Direction::Horizontal(
            scroll::hidden(),
        ))
        .width(Length::Fill)
        .height(Length::Shrink)
        .into()
}

/// 一个类型筛选胶囊（选中 = 强调淡底）。
fn type_chip(label: &str, selected: bool, kind: Option<String>) -> Element<'static, Message> {
    controls::btn(
        label,
        &controls::btn_pill(),
        if selected { controls::Variant::AccentSoft } else { controls::Variant::Outline },
        Some(Message::Plugins(PluginsMsg::MarketType(kind))),
    )
    .into()
}

/// 按搜索词与类型筛选后的市场条目（搜索匹配名称 / id / 仓库 / 简介）。
fn filtered_market_entries(app: &App) -> Vec<&atelyx_core::plugin_runtime::market::MarketEntry> {
    let Some(snapshot) = &app.plugins_state.market else {
        return Vec::new();
    };
    let query = app.plugins_state.market_query.trim().to_lowercase();
    let kind = app.plugins_state.market_type.as_deref();
    snapshot
        .items
        .iter()
        .filter(|entry| {
            if kind.is_some_and(|selected| entry.r#type.as_deref() != Some(selected)) {
                return false;
            }
            query.is_empty()
                || [
                    entry.name.as_str(),
                    entry.id.as_str(),
                    entry.repo.as_str(),
                    entry.tagline.as_deref().unwrap_or_default(),
                    entry.description.as_deref().unwrap_or_default(),
                ]
                .join(" ")
                .to_lowercase()
                .contains(&query)
        })
        .collect()
}

/// 市场条目卡片：名称 / 简介 / 徽标 / 仓库 / 安装或更新动作（在途显示进度态）。
fn market_card(
    app: &App,
    entry: &atelyx_core::plugin_runtime::market::MarketEntry,
) -> Element<'static, Message> {
    let installed = app.plugins_state.list.iter().find(|p| p.id == entry.id);
    let busy = app.plugins_state.market_busy.as_deref() == Some(entry.id.as_str());
    let official = matches!(entry.badge, Some(market::Badge::Official));
    let mut header = row![
        icon("puzzle", 14.0, pal::TEXT_SECONDARY),
        text(entry.name.clone())
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .font(bold()),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    if let Some(badge) = entry.badge {
        header = header.push(controls::badge(badge.label(), pal::BG_TERTIARY, pal::SUCCESS));
    }
    if let Some(installed) = installed {
        header = header.push(controls::badge(
            format!("已安装 v{}", installed.version),
            pal::BG_TERTIARY,
            pal::TEXT_SECONDARY,
        ));
    }
    header = header.push(space().width(Length::Fill));
    let action = if busy {
        controls::btn("处理中…", &controls::btn_sm(), controls::Variant::Ghost, None)
    } else if installed.is_some() {
        controls::btn(
            "检查更新",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::MarketUpdate {
                id: entry.id.clone(),
                repo: entry.repo.clone(),
            })),
        )
    } else {
        controls::btn(
            "安装",
            &controls::btn_sm(),
            controls::Variant::Primary,
            Some(Message::Plugins(PluginsMsg::MarketInstallAsk(MarketInstallAsk {
                id: entry.id.clone(),
                repo: entry.repo.clone(),
                name: entry.name.clone(),
                official,
            }))),
        )
    };
    header = header.push(action);

    let mut rows = vec![header.into()];
    if let Some(blurb) = entry.tagline.as_deref().or(entry.description.as_deref()) {
        rows.push(
            text(blurb.to_string())
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY)
                .into(),
        );
    }
    let mut meta = row![
        text(entry.repo.clone())
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    if let Some(kind) = &entry.r#type {
        meta = meta.push(controls::badge(kind.clone(), pal::BG_TERTIARY, pal::TEXT_SECONDARY));
    }
    if let Some(stars) = entry.stars {
        meta = meta.push(
            text(format!("星标 {stars}"))
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    }
    rows.push(meta.into());
    stacked_card(rows)
}

fn install_button(label: &str, msg: PluginsMsg, disabled: bool) -> Element<'static, Message> {
    let size = controls::btn_sm();
    controls::btn(
        label,
        &size,
        controls::Variant::Ghost,
        (!disabled).then_some(Message::Plugins(msg)),
    )
    .into()
}

/// 单个插件卡片：名称 / 版本 / 状态 / 启用开关 / 详情 / 卸载（含二段确认与失败原因 + 重试入口）。
fn plugin_card(app: &App, summary: &PluginSummary) -> Element<'static, Message> {
    let id = summary.id.clone();
    let (label, color) = status_label(&summary.status);
    // 启动失败：给出重试入口（启用标记仍在，只是没跑起来）
    let retryable = matches!(summary.status, PluginStatus::Failed(_));
    let mut header = row![
        icon("puzzle", 14.0, pal::TEXT_SECONDARY),
        text(summary.name.clone()).size(pal::fs_body()).color(pal::TEXT_PRIMARY),
        text(format!("v{}", summary.version))
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED),
        controls::badge(label, pal::BG_TERTIARY, color),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);

    header = header.push(space().width(Length::Fill));
    // 有安装来源的插件（本地目录安装）可从来源原样重装：构建产物更新后一键替换并重启
    if summary.source.is_some() {
        header = header.push(controls::btn(
            "从来源更新",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            (!app.plugins_state.installing).then_some(Message::Plugins(
                PluginsMsg::ReinstallSource(id.clone()),
            )),
        ));
    }
    if retryable {
        header = header.push(controls::btn(
            "重试",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::Retry(id.clone()))),
        ));
    }
    header = header.push(controls::toggle(
        summary.enabled,
        summary.load_error.is_none().then_some(Message::Plugins(PluginsMsg::Toggle(id.clone(), !summary.enabled))),
    ));
    header = header.push(controls::btn(
        "详情",
        &controls::btn_sm(),
        controls::Variant::Ghost,
        Some(Message::Plugins(PluginsMsg::Detail(id.clone()))),
    ));
    if app.plugins_state.confirm_uninstall.as_deref() == Some(&id) {
        header = header
            .push(controls::btn(
                "确认卸载",
                &controls::btn_sm(),
                controls::Variant::Danger,
                Some(Message::Plugins(PluginsMsg::UninstallConfirm(id.clone()))),
            ))
            .push(controls::btn(
                "取消",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(Message::Plugins(PluginsMsg::UninstallCancel)),
            ));
    } else {
        header = header.push(controls::btn(
            "卸载",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::UninstallAsk(id.clone()))),
        ));
    }

    let mut rows = vec![header.into()];
    if let Some(error) = &summary.load_error {
        rows.push(
            text(format!("不可装载：{error}"))
                .size(pal::fs_caption())
                .color(pal::DANGER)
                .into(),
        );
    } else if let PluginStatus::Failed(reason) = &summary.status {
        rows.push(
            text(format!("启动失败：{reason}"))
                .size(pal::fs_caption())
                .color(pal::DANGER)
                .into(),
        );
    }
    stacked_card(rows)
}

/// 插件页居中模态：市场安装确认（优先）或插件详情。未打开返回 None。
/// 挂根 Stack 追加子（覆盖层显隐不重建内容子树，见 UI 弹层约定）。
pub fn overlay(app: &App, window: iced::window::Id) -> Option<Element<'static, Message>> {
    if !app.settings_open || app.settings_tab != "plugins" {
        return None;
    }
    if let Some(ask) = &app.plugins_state.confirm_install {
        return Some(install_confirm_modal(ask));
    }
    if let Some(id) = app.plugins_state.builtin_detail.as_ref() {
        return Some(builtin_detail_modal(app, window, id));
    }
    let id = app.plugins_state.detail.as_ref()?;
    let summary = app.plugins_state.list.iter().find(|p| &p.id == id)?;
    Some(detail_modal(app, window, summary))
}
/// 插件详情模态：概览（版本 / 声明能力 / 调用记录）与运行日志两页。
fn detail_modal(app: &App, window: iced::window::Id, summary: &PluginSummary) -> Element<'static, Message> {
    let (status, status_color) = status_label(&summary.status);
    let header = modal_title(
        "puzzle",
        summary.name.clone(),
        summary.id.clone(),
        Some((status, status_color)),
        Message::Plugins(PluginsMsg::DetailClose),
        window,
    );
    let tabs = controls::segmented_tabs(
        &[("overview", "概览"), ("log", "日志")],
        match app.plugins_state.detail_tab {
            DetailTab::Overview => "overview",
            DetailTab::Log => "log",
        },
        |id| {
            Message::Plugins(PluginsMsg::DetailTab(if id == "log" {
                DetailTab::Log
            } else {
                DetailTab::Overview
            }))
        },
    );
    let body = match app.plugins_state.detail_tab {
        DetailTab::Overview => detail_overview(app, summary),
        DetailTab::Log => detail_log(app, summary),
    };
    let content = Column::new()
        .spacing(10)
        .padding(12.0)
        .push(header)
        .push(tabs)
        .push(controls::h_divider_subtle())
        .push(body);
    modal_card(DETAIL_CARD_WIDTH, content.into(), Message::Plugins(PluginsMsg::DetailClose))
}

/// 概览页：键值概况 + 声明能力徽标 + 能力调用记录。
fn detail_overview(app: &App, summary: &PluginSummary) -> Element<'static, Message> {
    let mut rows: Vec<Element<'static, Message>> = vec![
        meta_row("版本", summary.version.clone()),
        meta_row("协议", format!("apiVersion {}", summary.api_version)),
        meta_row("界面提交", format!("{} 次", summary.submits)),
        meta_row("未声明调用", format!("{} 条", summary.undeclared)),
    ];
    let mut flags: Vec<&str> = Vec::new();
    if summary.singleton {
        flags.push("单实例");
    }
    if summary.keep_mounted_on_vault_switch {
        flags.push("切仓库保持挂载");
    }
    if !flags.is_empty() {
        rows.push(meta_row("运行声明", flags.join(" · ")));
    }
    if let Some(source) = &summary.source {
        rows.push(meta_row("安装来源", source.clone()));
    }

    rows.push(section_head("声明能力", Some(summary.permissions.len())));
    if summary.permissions.is_empty() {
        rows.push(
            text("未声明任何能力")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED)
                .into(),
        );
    } else {
        for chunk in summary.permissions.chunks(5) {
            let mut line = row![].spacing(6);
            for permission in chunk {
                line = line.push(controls::badge(
                    permission_label(*permission),
                    pal::BG_TERTIARY,
                    pal::TEXT_SECONDARY,
                ));
            }
            rows.push(line.into());
        }
    }

    rows.push(section_head(
        "能力调用记录",
        Some(app.plugins_state.detail_audit.len()),
    ));
    rows.push(
        text("未声明 = manifest 未声明但实际用到")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .into(),
    );
    let mut audit = Column::new().spacing(4);
    if app.plugins_state.detail_audit.is_empty() {
        audit = audit.push(
            text("暂无能力调用记录")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    } else {
        for record in &app.plugins_state.detail_audit {
            let mut line = row![
                text(layout_view::format_ts(record.at.saturating_mul(MS_PER_SEC) as i64))
                    .size(pal::fs_caption())
                    .color(pal::TEXT_MUTED)
                    .font(mono()),
                text(record.method.clone())
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY)
                    .font(mono()),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);
            if !record.declared {
                line = line.push(controls::badge("未声明", pal::BG_TERTIARY, pal::WARNING));
            }
            if !record.detail.is_empty() {
                line = line.push(
                    text(record.detail.clone())
                        .size(pal::fs_caption())
                        .color(pal::TEXT_MUTED),
                );
            }
            audit = audit.push(line);
        }
    }
    rows.push(panel(
        scroll::v(audit).width(Length::Fill).height(Length::Fill).into(),
        DETAIL_AUDIT_MAX_H,
    ));
    Column::with_children(rows).spacing(6).into()
}

/// 日志页：路径 + 刷新 / 清空 + 等宽日志面板。
fn detail_log(app: &App, summary: &PluginSummary) -> Element<'static, Message> {
    let has_log = app
        .plugins_state
        .log_text
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty());
    let mut actions = row![]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center)
        .push(controls::btn(
            "刷新",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::LogRefresh)),
        ));
    actions = if app.plugins_state.log_clear_ask {
        actions
            .push(controls::btn(
                "取消",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(Message::Plugins(PluginsMsg::LogClearCancel)),
            ))
            .push(controls::btn(
                "确认清空",
                &controls::btn_sm(),
                controls::Variant::Danger,
                Some(Message::Plugins(PluginsMsg::LogClear(summary.id.clone()))),
            ))
    } else {
        // 没有日志可清时不亮按钮：空点一下只会让人以为没反应
        actions.push(controls::btn(
            "清空",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            has_log.then_some(Message::Plugins(PluginsMsg::LogClearAsk)),
        ))
    };
    let head = row![
        text("运行日志")
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY),
        space().width(Length::Fill),
        actions,
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    let path = app.plugins.log_path(&summary.id).unwrap_or_default();
    let body: Element<'static, Message> =
        match (&app.plugins_state.log_error, &app.plugins_state.log_text) {
            (Some(err), _) => controls::empty_state(
                "triangle-alert",
                "读取失败",
                err.clone(),
                Some(("重试".to_string(), Message::Plugins(PluginsMsg::LogRefresh))),
            ),
            (None, Some(lines)) if !lines.trim().is_empty() => scroll::v(
                text(lines.clone())
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY)
                    .font(mono()),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            (None, Some(_)) => controls::empty_state(
                "file-text",
                "暂无日志",
                "插件 stderr、协议违规与生命周期失败会写在这里",
                None,
            ),
            (None, None) => controls::empty_state("loader-circle", "读取中…", "正在读取日志尾部", None),
        };
    Column::new()
        .spacing(8)
        .push(head)
        .push(log_path_row(&path))
        .push(panel(body, LOG_PANEL_H))
        .into()
}

/// 日志路径行：单行省略显示，点击复制完整路径（悬停有底色）。
fn log_path_row(path: &str) -> Element<'static, Message> {
    if path.is_empty() {
        return text("日志文件未就绪")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .into();
    }
    let shown = elide_path(path, path_budget_em());
    let label = row![
        icon("copy", 12.0, pal::TEXT_MUTED),
        text(shown)
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .font(mono())
            .wrapping(Wrapping::None),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    button(label)
        .padding(Padding::ZERO.horizontal(4.0).vertical(2.0))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(Message::Plugins(PluginsMsg::CopyLogPath(path.to_string())))
        .into()
}

/// 路径可用的单行宽度（em）：模态内宽扣掉左右内衬、复制图标与间距。
fn path_budget_em() -> f32 {
    (DETAIL_CARD_WIDTH - LOG_PATH_CHROME_W) / pal::fs_caption()
}

/// 路径单行省略：按估算字宽从中间收起，保留盘符附近前缀与文件名。
/// 估算按等宽字推进（ASCII 约 0.62em、CJK 1em），与 UI 层比例字估宽不同源，勿混用。
fn elide_path(path: &str, budget_em: f32) -> String {
    let width = |ch: char| if ch.is_ascii() { 0.62 } else { 1.0 };
    if path.chars().map(width).sum::<f32>() <= budget_em {
        return path.to_string();
    }
    let head_budget = 8.0_f32.min(budget_em / 3.0);
    let tail_budget = (budget_em - head_budget - 0.8).max(0.0);
    let mut head = String::new();
    let mut used = 0.0;
    for ch in path.chars() {
        let w = width(ch);
        if used + w > head_budget {
            break;
        }
        head.push(ch);
        used += w;
    }
    // 两头预算之和小于总预算：若两段重叠即覆盖整串、宽度必不超预算，早已原样返回
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0.0;
    for ch in path.chars().rev() {
        let w = width(ch);
        if used + w > tail_budget {
            break;
        }
        tail.push(ch);
        used += w;
    }
    format!("{head}…{}", tail.into_iter().rev().collect::<String>())
}

/// 模态标题：图标 + 名称（副行 id）+ 可选状态徽标 + 关闭钮。
fn modal_title(
    icon_name: &str,
    title: String,
    subtitle: String,
    status: Option<(&str, iced::Color)>,
    close: Message,
    window: iced::window::Id,
) -> Element<'static, Message> {
    let mut head = row![
        icon(icon_name, 14.0, pal::TEXT_SECONDARY),
        Column::new()
            .spacing(2)
            .push(
                text(title)
                    .size(pal::fs_body())
                    .color(pal::TEXT_PRIMARY)
                    .font(bold()),
            )
            .push(
                text(subtitle)
                    .size(pal::fs_caption())
                    .color(pal::TEXT_MUTED),
            ),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    head = head.push(space().width(Length::Fill));
    if let Some((label, color)) = status {
        head = head.push(controls::badge(label, pal::BG_TERTIARY, color));
    }
    head.push(controls::icon_button(
        "x",
        14.0,
        26.0,
        controls::Variant::Ghost,
        Some(close),
        Some((window, "关闭".into())),
    ))
    .into()
}

/// 概况键值行：标签定宽，值紧跟其后。
fn meta_row(label: &'static str, value: String) -> Element<'static, Message> {
    row![
        text(label)
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .width(Length::Fixed(72.0)),
        text(value)
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY),
    ]
    .spacing(8)
    .into()
}

/// 模态内小节标题：标题 + 可选计数徽标。
fn section_head(label: &'static str, count: Option<usize>) -> Element<'static, Message> {
    let mut line = row![text(label)
        .size(pal::fs_caption())
        .color(pal::TEXT_SECONDARY)]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    if let Some(count) = count {
        line = line.push(controls::badge(
            count.to_string(),
            pal::BG_TERTIARY,
            pal::TEXT_MUTED,
        ));
    }
    line.into()
}

/// 模态内下沉面板：固定高度，内容超出即内部滚动。
fn panel(content: Element<'static, Message>, height: f32) -> Element<'static, Message> {
    container(content)
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .padding(8)
        .style(|_t| Style {
            background: Some(pal::BG_SUNKEN.into()),
            border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
            ..Style::default()
        })
        .into()
}

/// 内置插件（进程内）详情模态：它提供的视图与服务；服务行可就地切换 / 回落提供者。
fn builtin_detail_modal(app: &App, window: iced::window::Id, id: &str) -> Element<'static, Message> {
    let Some(plugin) = app
        .registry
        .builtin_plugins()
        .iter()
        .find(|plugin| plugin.id == id)
    else {
        return row![
            text("内置插件未登记")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        ]
        .into();
    };
    let header = modal_title(
        plugin.icon.as_ref(),
        plugin.label.clone(),
        plugin.id.clone(),
        None,
        Message::Plugins(PluginsMsg::BuiltinDetailClose),
        window,
    );

    let views: Vec<&Entry> = app
        .registry
        .all()
        .filter(|entry| entry.kind == EntryKind::View && entry.source.plugin_id() == plugin.id)
        .collect();
    let services: Vec<&Entry> = app
        .registry
        .services()
        .filter(|entry| entry.source.plugin_id() == plugin.id)
        .collect();
    let mut rows: Vec<Element<'static, Message>> = vec![text(if plugin.id == PLATFORM_PLUGIN {
        "平台自救面：设置入口、通知、对话框与 query 服务一直取用，不可停用。"
    } else {
        "随应用分发：停用后其视图离开选择器、服务离开能力总线。"
    })
    .size(pal::fs_caption())
    .color(pal::TEXT_MUTED)
    .into()];
    if !views.is_empty() {
        rows.push(section_head("提供的视图", Some(views.len())));
        rows.extend(views.into_iter().map(builtin_view_row));
    }
    if !services.is_empty() {
        rows.push(section_head("提供的服务", Some(services.len())));
        rows.extend(services.into_iter().map(service_row));
    }

    let content = Column::with_children(
        std::iter::once(header.into())
            .chain(std::iter::once(controls::h_divider_subtle()))
            .chain(rows)
            .collect::<Vec<Element<'static, Message>>>(),
    )
    .spacing(8)
    .padding(12.0);
    modal_card(
        BUILTIN_CARD_WIDTH,
        content.into(),
        Message::Plugins(PluginsMsg::BuiltinDetailClose),
    )
}

/// 市场安装确认模态（非官方插件；官方插件直接安装，不经此窗）。
fn install_confirm_modal(ask: &MarketInstallAsk) -> Element<'static, Message> {
    let header = row![
        icon("triangle-alert", 14.0, pal::WARNING),
        text(format!("安装「{}」", ask.name))
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .font(bold()),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    let content = Column::new()
        .spacing(10)
        .padding(12.0)
        .push(header)
        .push(
            text(format!(
                "第三方插件未经官方审查，可读写你的文件、运行程序、联网（{}）。仅从你信任的来源安装。",
                ask.repo
            ))
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY),
        )
        .push(
            row![
                space().width(Length::Fill),
                controls::btn(
                    "取消",
                    &controls::btn_sm(),
                    controls::Variant::Ghost,
                    Some(Message::Plugins(PluginsMsg::MarketInstallCancel)),
                ),
                controls::btn(
                    "继续安装",
                    &controls::btn_sm(),
                    controls::Variant::Primary,
                    Some(Message::Plugins(PluginsMsg::MarketInstallConfirm)),
                ),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        );
    modal_card(CONFIRM_CARD_WIDTH, content.into(), Message::Plugins(PluginsMsg::MarketInstallCancel))
}

/// 居中模态卡片外壳：固定宽度 + 浮层底 + 圆角描边；遮罩点击 = `close`。
fn modal_card(width: f32, content: Element<'static, Message>, close: Message) -> Element<'static, Message> {
    let card = container(content)
        .width(Length::Fixed(width))
        .style(|_t| Style {
            background: Some(pal::BG_OVERLAY.into()),
            border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
            ..Style::default()
        });
    crate::ui::popup::modal_overlay_with(close, card.into())
}

/// 多行卡片容器（与设置页卡片同形态，可容纳多行）。
fn stacked_card(rows: Vec<Element<'static, Message>>) -> Element<'static, Message> {
    container(Column::with_children(rows).spacing(6))
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
        .style(|_t| Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_MD),
            ..Style::default()
        })
        .into()
}

fn status_label(status: &PluginStatus) -> (&'static str, iced::Color) {
    match status {
        PluginStatus::Stopped => ("未运行", pal::TEXT_MUTED),
        PluginStatus::Running => ("运行中", pal::SUCCESS),
        PluginStatus::Failed(_) => ("失败", pal::DANGER),
    }
}

fn permission_label(permission: Permission) -> &'static str {
    match permission {
        Permission::Fs => "文件",
        Permission::Http => "网络",
        Permission::Process => "进程",
        Permission::State => "数据",
        Permission::Notification => "通知",
        Permission::Shortcuts => "快捷键",
        Permission::Vault => "仓库",
        Permission::Clipboard => "剪贴板",
        Permission::Window => "窗口",
        Permission::Tray => "托盘",
        Permission::Collab => "协作",
        Permission::Ai => "AI",
        Permission::Dialog => "对话框",
    }
}

#[cfg(test)]
mod tests {
    use super::{elide_path, plugin_path, remove_local, rename_local};

    #[test]
    fn plugin_path_treats_empty_as_vault_root() {
        assert_eq!(plugin_path(None).unwrap(), "");
        assert_eq!(plugin_path(Some("")).unwrap(), "", "空串 = 仓库根，列根目录就是它");
        assert_eq!(plugin_path(Some("dir/a.md")).unwrap(), "dir/a.md");
        assert!(plugin_path(Some("../escape")).is_err(), "越界段仍要拒");
    }

    #[test]
    fn elide_path_keeps_drive_and_file_name() {
        let path = r"C:\Users\35264\AppData\Roaming\com.atelyx.desktop\plugins\com.atelyx.calendar\logs\runtime.log";
        let shown = elide_path(path, 42.0);
        assert!(shown.chars().count() < path.chars().count(), "{shown}");
        assert!(shown.contains('…'), "{shown}");
        assert!(shown.starts_with(r"C:\Users"), "{shown}");
        assert!(shown.ends_with("runtime.log"), "{shown}");
        assert_eq!(elide_path(r"C:\a.log", 42.0), r"C:\a.log", "放得下就原样显示");
    }

    #[test]
    fn elide_path_does_not_repeat_cjk_middle() {
        let path = r"C:\Users\张三\AppData\Roaming\com.atelyx.desktop\plugins\com.atelyx.calendar\logs\runtime.log";
        let shown = elide_path(path, 42.0);
        let (head, tail) = shown.split_once('…').expect("应省略中段");
        assert!(path.starts_with(head) && path.ends_with(tail), "{shown}");
        // 头的字符数加尾的字符数少于原路径：两头不重叠、不出重复片段
        assert!(
            head.chars().count() + tail.chars().count() < path.chars().count(),
            "{shown}"
        );
    }
    use atelyx_core::vault::test_support::TempDir;

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn rename_local_rewrites_links_and_canvas_refs() {
        let tmp = TempDir::new("plugin-rename");
        let root = &*tmp;
        write(root, "a.md", "正文");
        write(root, "b.md", "看 [它](a.md)");
        write(
            root,
            "c.atlx",
            r#"{"schema":"atelyx-canvas/v1","id":"c1","title":"c","nodes":[{"id":"n1","type":"text","x":0,"y":0,"data":{"file":"a.md"}}],"edges":[]}"#,
        );
        let (_, rewritten) = rename_local(root, &[], "a.md", "dir/a.md").unwrap();
        assert_eq!(rewritten, vec!["b.md".to_string()]);
        assert!(root.join("dir/a.md").exists());
        assert!(!root.join("a.md").exists());
        let canvas = std::fs::read_to_string(root.join("c.atlx")).unwrap();
        assert!(canvas.contains(r#""file": "dir/a.md""#), "{canvas}");
    }

    #[test]
    fn rename_local_rejects_occupied_target_and_keeps_source() {
        let tmp = TempDir::new("plugin-rename-occupied");
        let root = &*tmp;
        write(root, "a.md", "源");
        write(root, "dir/a.md", "既有");
        assert!(rename_local(root, &[], "a.md", "dir/a.md").is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), "源");
        assert_eq!(std::fs::read_to_string(root.join("dir/a.md")).unwrap(), "既有");
    }

    #[test]
    fn rename_local_rejects_moving_folder_into_itself() {
        let tmp = TempDir::new("plugin-rename-nested");
        let root = &*tmp;
        write(root, "dir/a.md", "内容");
        assert!(rename_local(root, &[], "dir", "dir/sub").is_err());
        assert!(root.join("dir/a.md").exists());
    }

    #[test]
    fn remove_local_deletes_folder_recursively_and_rejects_root() {
        let tmp = TempDir::new("plugin-remove");
        let root = &*tmp;
        write(root, "dir/sub/a.md", "内容");
        remove_local(root, "dir").unwrap();
        assert!(!root.join("dir").exists());
        assert!(remove_local(root, "").is_err(), "仓库根不可删");
    }
}
