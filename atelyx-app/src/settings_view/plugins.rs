//! 设置-插件：已安装列表 / 启停 / 卸载 / 本地与 zip 安装 / 详情（版本、权限、失败与被拒原因）
//! / 市场（官方索引浏览 / 安装 / 更新）。
//! 列表与状态来自 `atelyx_core::plugin_runtime`（运行时为进程内单例，壳持有 `Arc`）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use iced::widget::{container, row, scrollable, space, Column};
use iced::{border, widget::container::Style, Element, Length, Padding, Task};

use atelyx_core::plugin_runtime::market::{self, MarketSnapshot};
use atelyx_core::plugin_runtime::{
    AuditRecord, ClipboardImage, CollabPeer, CollabPeersResult, CollabSendParams, CollabSendQos,
    CollabSendResult, DialogKind, DialogRequest, HostRequest, ImageInfoResult, InstallOutcome,
    NotificationLevel, Permission, PluginEvent, PluginRuntime, PluginStatus, PluginSummary,
    SetTrayItemsParams, MAX_VAULT_BINARY_BYTES,
};

use crate::settings_view::{card, group_head};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::{bold, text};
use crate::{layout_view, vault, App, Message};

/// 数据落盘时刻格式化用的毫秒口径（审计记录为 unix 秒）。
const MS_PER_SEC: u64 = 1000;

/// 插件详情模态卡宽。
const DETAIL_CARD_WIDTH: f32 = 460.0;
/// 被拒调用记录区固定滚动高度（避免长列表把模态撑出窗口）。
const DETAIL_AUDIT_MAX_H: f32 = 200.0;
/// 市场安装确认模态卡宽。
const CONFIRM_CARD_WIDTH: f32 = 380.0;
/// 日志模态卡宽与日志区固定高度。
const LOG_CARD_WIDTH: f32 = 620.0;
const LOG_MAX_H: f32 = 360.0;

/// 插件页模式：已安装列表 / 市场浏览。
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsMode {
    /// 已装插件列表（安装入口 / 启停 / 卸载 / 详情）。
    #[default]
    Installed,
    /// 官方索引浏览（搜索 / 类型筛选 / 安装 / 更新）。
    Market,
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

/// 插件页状态（列表缓存 + 详情 + 卸载二段确认 + 安装在途 + 市场快照与筛选）。
#[derive(Default)]
pub struct PluginsState {
    /// 已装插件摘要（打开插件页 / 收到事件 / 动作后刷新）。
    pub list: Vec<PluginSummary>,
    /// 详情打开的插件 id。
    pub detail: Option<String>,
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
    /// 日志模态的目标插件 id（`Some` = 日志模态打开）与读到的内容。
    pub log_target: Option<String>,
    pub log_text: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PluginsMsg {
    /// 打开插件页 / 动作后刷新列表。
    Refresh,
    /// 启用 / 停用。
    Toggle(String, bool),
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
    /// 日志模态：打开并读尾部 / 关闭。
    LogOpen(String),
    LogClose,
    /// 日志读取回执（带目标 id：连开两个插件时先发的读取不能覆盖后一个的显示）。
    LogLoaded { id: String, text: String },
    /// 壳侧服务请求：替插件弹系统对话框（选定的路径经 `resolve_dialog` 回填给等待中的调用）。
    DialogAsk { request_id: u64, dialog: DialogRequest },
    DialogDone { request_id: u64, path: Option<String> },
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
    /// 剪贴板读写（系统剪贴板只能经壳的 iced 任务访问）。
    ClipboardWrite { request_id: u64, text: String },
    ClipboardWritten { request_id: u64 },
    ClipboardRead { request_id: u64 },
    ClipboardReadDone { request_id: u64, text: Option<String> },
    /// 剪贴板图片（arboard 在阻塞线程池读，编成 PNG 后 base64 回填）。
    ClipboardReadImage { request_id: u64 },
    ClipboardImageDone { request_id: u64, result: Result<Option<ClipboardImage>, String> },
    /// 仓库读写（本地仓库直接落盘；协作空间转发服务端）。`VaultCallDone` 统一回填。
    VaultRead { request_id: u64, path: String },
    VaultWrite { request_id: u64, path: String, content: String },
    VaultReadBinary { request_id: u64, path: String },
    VaultWriteBinary { request_id: u64, path: String, bytes: Vec<u8> },
    VaultList { request_id: u64, path: String },
    VaultExists { request_id: u64, path: String },
    /// 图片固有像素尺寸（只读文件头，不送回内容）。
    VaultImageInfo { request_id: u64, path: String },
    VaultCallDone { request_id: u64, result: Result<serde_json::Value, String> },
    /// 表格只读快照（读壳的表格会话；写走表格消息流）。
    TableSnapshot { request_id: u64 },
    /// 系统托盘条目整发整换（空清单 = 收起该插件条目）。
    TraySetItems { request_id: u64, plugin_id: String, params: SetTrayItemsParams },
    /// 协作空间在场成员（未连接 = `connected: false`）。
    CollabPeers { request_id: u64 },
    /// 向同空间对等插件发消息（通道加插件前缀，未连接时回 `queued: false`）。
    CollabSend { request_id: u64, plugin_id: String, params: CollabSendParams },
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
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = iced::futures::SinkExt::send(
                        &mut output,
                        Message::PluginEvent(Box::new(event)),
                    )
                    .await;
                }
                // 广播落后只丢中间事件、连接不失效：继续收后续事件
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
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
                HostRequest::ScrollTo { plugin_id, request_id, params } => {
                    Message::PluginScrollTo { request_id, plugin_id, params }
                }
                HostRequest::ChatSend { plugin_id, request_id, params } => Message::Chat(
                    crate::chat_actions::ChatMsg::PluginSend { request_id, plugin_id, params },
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
                HostRequest::ClipboardWrite { request_id, text, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardWrite { request_id, text })
                }
                HostRequest::ClipboardRead { request_id, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardRead { request_id })
                }
                HostRequest::ClipboardReadImage { request_id, .. } => {
                    Message::Plugins(PluginsMsg::ClipboardReadImage { request_id })
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultRead { request_id, path })
                }
                HostRequest::VaultWriteText { request_id, path, content, .. } => {
                    Message::Plugins(PluginsMsg::VaultWrite { request_id, path, content })
                }
                HostRequest::VaultReadBinary { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultReadBinary { request_id, path })
                }
                HostRequest::VaultWriteBinary { request_id, path, bytes, .. } => {
                    Message::Plugins(PluginsMsg::VaultWriteBinary { request_id, path, bytes })
                }
                HostRequest::VaultList { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultList { request_id, path })
                }
                HostRequest::VaultExists { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultExists { request_id, path })
                }
                HostRequest::VaultImageInfo { request_id, path, .. } => {
                    Message::Plugins(PluginsMsg::VaultImageInfo { request_id, path })
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
                // 表格读写：读走本页回填；写出壳的表格消息流承担（与本地编辑同一出口，
                // 自动保存期限与 table:changed 广播都由该流统一处理）
                HostRequest::TableSnapshot { request_id, .. } => {
                    Message::Plugins(PluginsMsg::TableSnapshot { request_id })
                }
                HostRequest::TableUpdateCell { request_id, row_id, field_id, value, .. } => {
                    Message::Table(crate::table_view::TableMsg::PluginSetCell {
                        request_id,
                        row_id,
                        field_id,
                        value,
                    })
                }
                HostRequest::TableAddRow { request_id, .. } => {
                    Message::Table(crate::table_view::TableMsg::PluginAddRow { request_id })
                }
                HostRequest::TableRemoveRow { request_id, row_id, .. } => {
                    Message::Table(crate::table_view::TableMsg::PluginRemoveRow { request_id, row_id })
                }
                HostRequest::TableSelectRow { request_id, row_id, .. } => {
                    Message::Table(crate::table_view::TableMsg::PluginSelectRow { request_id, row_id })
                }
                HostRequest::TableSetCellImage { request_id, params, .. } => Message::Table(
                    crate::table_view::TableMsg::PluginSetCellImage {
                        request_id,
                        row_id: params.row_id,
                        field_id: params.field_id,
                        images: params.images,
                        display: params.display,
                    },
                ),
            };
            let _ = iced::futures::SinkExt::send(&mut output, message).await;
        }
    })
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
            if let Err(e) = app.plugins.set_enabled(&id, on) {
                app.notify(NoticeLevel::Error, format!("插件操作失败：{e}"));
            }
            refresh(app);
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
                    // 日志模态跟着插件走：已卸载的插件没有日志可看
                    if app.plugins_state.log_target.as_deref() == Some(id.as_str()) {
                        app.plugins_state.log_target = None;
                        app.plugins_state.log_text = None;
                    }
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
            Task::none()
        }
        PluginsMsg::DetailClose => {
            app.plugins_state.detail = None;
            Task::none()
        }
        PluginsMsg::LogOpen(id) => {
            app.plugins_state.log_target = Some(id.clone());
            app.plugins_state.log_text = None;
            let runtime = app.plugins.clone();
            let for_task = id.clone();
            Task::perform(
                async move {
                    run_blocking(move || Ok(runtime.log_tail(&for_task)))
                        .await
                        .unwrap_or_default()
                },
                move |text| Message::Plugins(PluginsMsg::LogLoaded { id, text }),
            )
        }
        PluginsMsg::LogClose => {
            app.plugins_state.log_target = None;
            app.plugins_state.log_text = None;
            Task::none()
        }
        PluginsMsg::LogLoaded { id, text } => {
            // 目标已换（连开另一个插件）或模态已关：这次读取的结果不再对应屏幕上的内容
            if app.plugins_state.log_target.as_deref() == Some(id.as_str()) {
                app.plugins_state.log_text = Some(text);
            }
            Task::none()
        }
        PluginsMsg::DialogAsk { request_id, dialog } => Task::perform(pick(dialog), move |path| {
            Message::Plugins(PluginsMsg::DialogDone { request_id, path })
        }),
        PluginsMsg::DialogDone { request_id, path } => {
            app.plugins.resolve_dialog(request_id, path);
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
        PluginsMsg::ClipboardWrite { request_id, text } => iced::clipboard::write(text)
            .map(move |_: ()| Message::Plugins(PluginsMsg::ClipboardWritten { request_id })),
        PluginsMsg::ClipboardWritten { request_id } => {
            app.plugins
                .resolve_shell_call(request_id, Ok(serde_json::json!({})));
            Task::none()
        }
        PluginsMsg::ClipboardRead { request_id } => iced::clipboard::read().map(move |text| {
            Message::Plugins(PluginsMsg::ClipboardReadDone { request_id, text })
        }),
        PluginsMsg::ClipboardReadDone { request_id, text } => {
            let text = text.unwrap_or_default();
            app.plugins
                .resolve_shell_call(request_id, Ok(serde_json::json!({ "text": text })));
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
        PluginsMsg::VaultRead { request_id, path } => {
            let rel = match tool_path(Some(&path)) {
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
        PluginsMsg::VaultWrite { request_id, path, content } => {
            let rel = match tool_path(Some(&path)) {
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
                        Task::perform(
                            async move {
                                let target = run_blocking(move || vault::safe_join(&root, &rel, true)).await?;
                                writes
                                    .write(target, content)
                                    .await
                                    .map(|_| serde_json::json!({}))
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
                            client
                                .put_file_text(&space_id, &rel, &content)
                                .await
                                .map(|_| serde_json::json!({}))
                                .map_err(|e| e.to_string())
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultReadBinary { request_id, path } => {
            let rel = match tool_path(Some(&path)) {
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
            let rel = match tool_path(Some(&path)) {
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
                        Task::perform(
                            async move {
                                let target =
                                    run_blocking(move || vault::safe_join(&root, &rel, true)).await?;
                                writes
                                    .write_bytes(target, bytes)
                                    .await
                                    .map(|_| serde_json::json!({}))
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
                            client
                                .put_file_bytes(&space_id, &rel, &bytes)
                                .await
                                .map(|_| serde_json::json!({}))
                                .map_err(|e| e.to_string())
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultList { request_id, path } => {
            let rel = match tool_path((!path.is_empty()).then_some(path.as_str())) {
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
                                    let children = vault::read_dir_entries(&root, &rel, &exclude)?;
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
                            let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                            Ok(dir_entries_json(&crate::space_actions::children_of_tree(&tree, &rel)))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultExists { request_id, path } => {
            let rel = match tool_path(Some(&path)) {
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
                            let tree = client.tree(&space_id).await.map_err(|e| e.to_string())?;
                            let exists = crate::space_actions::tree_has_file(&tree, &rel);
                            Ok(serde_json::json!({ "exists": exists }))
                        },
                        move |result| Message::Plugins(PluginsMsg::VaultCallDone { request_id, result }),
                    )
                }
            }
        }
        PluginsMsg::VaultImageInfo { request_id, path } => {
            let rel = match tool_path(Some(&path)) {
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
        PluginsMsg::VaultCallDone { request_id, result } => {
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
        PluginsMsg::TableSnapshot { request_id } => {
            let snapshot: atelyx_core::plugin_runtime::TableSnapshot = match &app.table {
                Some(session) => atelyx_core::plugin_runtime::table::snapshot(
                    &session.file,
                    &session.data,
                    session.view.selection.as_ref(),
                ),
                None => atelyx_core::plugin_runtime::TableSnapshot::default(),
            };
            let result = serde_json::to_value(snapshot)
                .map_err(|e| format!("表格快照序列化失败：{e}"));
            app.plugins.resolve_shell_call(request_id, result);
            Task::none()
        }
    }
}

/// 仓库相对路径校验：`None` = 仓库根（仅列目录允许根路径）。
fn tool_path(path: Option<&str>) -> Result<String, String> {
    match path {
        None => Ok(String::new()),
        Some(path) => vault::validate_tool_path(path),
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
fn resolve_vault(
    app: &App,
    request_id: u64,
    result: Result<serde_json::Value, String>,
) -> Task<Message> {
    app.plugins.resolve_shell_call(request_id, result);
    Task::none()
}

/// 写入目标是否正由编辑器持有：壳的会话里保留着该文件的内存副本，插件覆盖会与之竞争。
/// 比较归一化相对路径（`\` 与 `/` 同义）。
fn open_file_conflict(app: &App, rel: &str) -> Option<String> {
    let target = rel.replace('\\', "/");
    let open = [
        app.ui.last_note_file.as_deref(),
        app.table.as_ref().map(|t| t.file.as_str()),
        app.canvas.as_ref().map(|c| c.file.as_str()),
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
    let (width, height) = crate::canvas_actions::image_dimensions(bytes);
    if width == 0 || height == 0 {
        return Err(format!("不是可识别的图片（png / jpeg / gif / bmp / webp）：{rel}"));
    }
    serde_json::to_value(ImageInfoResult { width, height })
        .map_err(|e| format!("图片尺寸序列化失败：{e}"))
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

    let Some((width, height, rgba)) = crate::image_clipboard::read_image()? else {
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
async fn pick(request: DialogRequest) -> Option<String> {
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
    let picked = match (request.kind, request.save) {
        (DialogKind::Directory, _) => dialog.pick_folder().await,
        (DialogKind::File, true) => dialog.save_file().await,
        (DialogKind::File, false) => dialog.pick_file().await,
    };
    picked.map(|handle| handle.path().to_string_lossy().into_owned())
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
            match status {
                PluginStatus::Failed(reason) => {
                    app.notify(NoticeLevel::Error, format!("插件 {id} 启动失败：{reason}"));
                }
                PluginStatus::CircuitBroken => app.notify(
                    NoticeLevel::Error,
                    format!("插件 {id} 连续崩溃，已熔断；可在设置中重试"),
                ),
                _ => {}
            }
        }
        // 界面 / 贡献 / AI 工具变化由壳的投影刷新统一处理（见 main.rs 的事件分支）
        PluginEvent::ViewSubmitted { .. }
        | PluginEvent::ContributionsChanged { .. }
        | PluginEvent::AiToolsChanged { .. } => {}
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
fn installed_group(app: &App) -> Element<'static, Message> {
    let installing = app.plugins_state.installing;
    let installed = &app.plugins_state.list;
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
    // 熔断 / 启动失败：给出重试入口（与启用开关无关——熔断时启用标记仍在）
    let retryable = matches!(
        summary.status,
        PluginStatus::CircuitBroken | PluginStatus::Failed(_)
    );
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
    if let Some(id) = app.plugins_state.log_target.clone() {
        return Some(log_modal(app, window, &id));
    }
    let id = app.plugins_state.detail.as_ref()?;
    let summary = app.plugins_state.list.iter().find(|p| &p.id == id)?;
    Some(detail_modal(app, window, summary))
}

/// 插件详情模态：版本 / 协议版本 / 权限清单 / 提交与被拒计数 / 被拒调用记录。
fn detail_modal(app: &App, window: iced::window::Id, summary: &PluginSummary) -> Element<'static, Message> {
    let header = row![
        icon("puzzle", 14.0, pal::TEXT_SECONDARY),
        text(format!("{}（{}）", summary.name, summary.id))
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .font(bold()),
        space().width(Length::Fill),
        controls::btn(
            "日志",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::LogOpen(summary.id.clone()))),
        ),
        controls::icon_button(
            "x",
            14.0,
            26.0,
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::DetailClose)),
            Some((window, "关闭".into())),
        ),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);

    let mut rows: Vec<Element<'static, Message>> = Vec::new();
    rows.push(
        text(format!(
            "版本 {} · 协议 apiVersion {} · 已提交界面 {} 次 · 被拒调用 {} 次",
            summary.version, summary.api_version, summary.view_submits, summary.denied
        ))
        .size(pal::fs_caption())
        .color(pal::TEXT_MUTED)
        .into(),
    );

    let mut flags: Vec<&str> = Vec::new();
    if summary.singleton {
        flags.push("单实例");
    }
    if summary.keep_mounted_on_vault_switch {
        flags.push("切仓库保持挂载");
    }
    if !flags.is_empty() {
        rows.push(
            text(format!("声明：{}", flags.join(" · ")))
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED)
                .into(),
        );
    }
    if let Some(source) = &summary.source {
        rows.push(
            text(format!("安装来源：{source}"))
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED)
                .into(),
        );
    }

    let mut perms = row![
        text("声明能力").size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    if summary.permissions.is_empty() {
        perms = perms.push(controls::badge("未声明", pal::BG_TERTIARY, pal::TEXT_MUTED));
    } else {
        for permission in &summary.permissions {
            perms = perms.push(controls::badge(permission_label(*permission), pal::BG_TERTIARY, pal::TEXT_SECONDARY));
        }
    }
    rows.push(perms.into());

    // 被拒调用记录条数不定：定高内滚动，避免长列表把模态撑出窗口
    let mut audit = Column::new().spacing(4);
    if app.plugins_state.detail_audit.is_empty() {
        audit = audit.push(
            text("暂无被拒调用记录")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    } else {
        for record in &app.plugins_state.detail_audit {
            let detail = if record.detail.is_empty() {
                String::new()
            } else {
                format!("（{}）", record.detail)
            };
            audit = audit.push(
                text(format!(
                    "{}  {}{}",
                    layout_view::format_ts(record.at.saturating_mul(MS_PER_SEC) as i64),
                    record.method,
                    detail
                ))
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
            );
        }
    }
    rows.push(
        scroll::v(audit)
            .width(Length::Fill)
            .height(Length::Fixed(DETAIL_AUDIT_MAX_H))
            .into(),
    );

    let content = Column::with_children(
        std::iter::once(header.into())
            .chain(std::iter::once(controls::h_divider_subtle()))
            .chain(rows)
            .collect::<Vec<Element<'static, Message>>>(),
    )
    .spacing(10)
    .padding(12.0);
    modal_card(DETAIL_CARD_WIDTH, content.into(), Message::Plugins(PluginsMsg::DetailClose))
}

/// 插件日志模态：插件 stderr、协议违规与生命周期失败（宿主没有控制台，这里是唯一可查处）。
fn log_modal(app: &App, window: iced::window::Id, id: &str) -> Element<'static, Message> {
    let title = app
        .plugins_state
        .list
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string());
    let header = row![
        icon("file-text", 14.0, pal::TEXT_SECONDARY),
        text(format!("{title} 日志"))
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .font(bold()),
        space().width(Length::Fill),
        controls::btn(
            "刷新",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::LogOpen(id.to_string()))),
        ),
        controls::icon_button(
            "x",
            14.0,
            26.0,
            controls::Variant::Ghost,
            Some(Message::Plugins(PluginsMsg::LogClose)),
            Some((window, "关闭".into())),
        ),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);

    let body: Element<'static, Message> = match &app.plugins_state.log_text {
        Some(lines) if !lines.trim().is_empty() => scroll::v(
            text(lines.clone())
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
        )
        .width(Length::Fill)
        .height(Length::Fixed(LOG_MAX_H))
        .into(),
        Some(_) => text("暂无日志")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .into(),
        None => text("读取中…")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .into(),
    };

    let content = Column::new()
        .spacing(10)
        .padding(12.0)
        .push(header)
        .push(controls::h_divider_subtle())
        .push(
            text(app.plugins.log_path(id).unwrap_or_default())
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        )
        .push(body);
    modal_card(LOG_CARD_WIDTH, content.into(), Message::Plugins(PluginsMsg::LogClose))
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
        PluginStatus::Restarting => ("重启中", pal::WARNING),
        PluginStatus::CircuitBroken => ("已熔断", pal::DANGER),
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
        Permission::Table => "表格",
        Permission::Clipboard => "剪贴板",
        Permission::Window => "窗口",
        Permission::Tray => "托盘",
        Permission::Collab => "协作",
        Permission::Ai => "AI",
        Permission::Dialog => "对话框",
    }
}
