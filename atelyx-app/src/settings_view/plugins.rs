//! 设置-插件：已安装列表 / 启用停用 / 卸载 / 本地目录与 zip 安装 / 详情（版本、权限、失败与被拒原因）。
//! 列表与状态来自 `atelyx_core::plugin_runtime`（运行时为进程内单例，壳持有 `Arc`）。

use std::sync::Arc;

use iced::widget::{container, row, scrollable, space, text, Column};
use iced::{border, widget::container::Style, Element, Length, Padding, Task};

use atelyx_core::plugin_runtime::{
    AuditRecord, DialogKind, DialogRequest, HostRequest, NotificationLevel, Permission, PluginEvent,
    PluginRuntime, PluginStatus, PluginSummary,
};

use crate::settings_view::{card, group_head};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::{layout_view, App, Message};

/// 数据落盘时刻格式化用的毫秒口径（审计记录为 unix 秒）。
const MS_PER_SEC: u64 = 1000;

/// 插件页状态（列表缓存 + 详情 + 卸载二段确认 + 安装在途）。
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
    Installed(Result<Option<String>, String>),
    /// 卸载回执（携带目标 id 供提示文案）。
    Uninstalled(String, Result<(), String>),
    /// 打开 / 关闭详情。
    Detail(String),
    DetailClose,
    /// 壳侧服务请求：替插件弹系统对话框（选定的路径经 `resolve_dialog` 回填给等待中的调用）。
    DialogAsk { request_id: u64, dialog: DialogRequest },
    DialogDone { request_id: u64, path: Option<String> },
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

/// 壳侧服务请求流：运行时用无界通道送请求（对话框），这里接进 Elm update。
pub fn host_requests_stream(spec: &RuntimeSpec) -> impl iced::futures::Stream<Item = Message> + use<> {
    let runtime = spec.0.clone();
    iced::stream::channel(16, async move |mut output| {
        while let Some(request) = runtime.recv_host_request().await {
            let message = match request {
                HostRequest::Dialog { request_id, dialog, .. } => {
                    Message::Plugins(PluginsMsg::DialogAsk { request_id, dialog })
                }
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
            match result {
                Ok(Some(id)) => {
                    app.notify(NoticeLevel::Success, format!("已安装 {id}，可在列表里启用"));
                }
                Ok(None) => {}
                Err(e) => app.notify(NoticeLevel::Error, format!("安装失败：{e}")),
            }
            refresh(app);
            Task::none()
        }
        PluginsMsg::Uninstalled(id, result) => {
            app.plugins_state.installing = false;
            match result {
                Ok(()) => {
                    app.plugins_state.detail = None;
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
        PluginsMsg::DialogAsk { request_id, dialog } => Task::perform(pick(dialog), move |path| {
            Message::Plugins(PluginsMsg::DialogDone { request_id, path })
        }),
        PluginsMsg::DialogDone { request_id, path } => {
            app.plugins.resolve_dialog(request_id, path);
            Task::none()
        }
    }
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
        PluginEvent::StatusChanged { .. } | PluginEvent::PermissionDenied { .. }
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
        PluginEvent::StatusChanged { id, status } => match status {
            PluginStatus::Failed(reason) => {
                app.notify(NoticeLevel::Error, format!("插件 {id} 启动失败：{reason}"));
            }
            PluginStatus::CircuitBroken => app.notify(
                NoticeLevel::Error,
                format!("插件 {id} 连续崩溃，已熔断；可在设置中重试"),
            ),
            _ => {}
        },
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

/// 起一次安装：先弹文件选择器，再在阻塞线程池里安装（重活不占异步 worker；失败即回执为可读错误）。
fn start_install(app: &mut App, from_dir: bool) -> Task<Message> {
    if app.plugins_state.installing {
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
                run_blocking(move || runtime.install_zip(&path)).await
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

/// 插件页视图：安装入口 + 已装列表 + 详情。
pub fn view(app: &App) -> Element<'static, Message> {
    let installing = app.plugins_state.installing;
    let mut col = Column::new()
        .spacing(8)
        .padding(Padding::ZERO.horizontal(20.0));
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

    let installed = &app.plugins_state.list;
    col = col.push(group_head(
        "已安装",
        &format!("共 {} 个", installed.len()),
        false,
    ));
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

    if let Some(id) = app.plugins_state.detail.as_ref() {
        if let Some(summary) = installed.iter().find(|p| &p.id == id) {
            col = col.push(detail_card(app, summary));
        }
    }

    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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

/// 详情卡片：版本 / 协议版本 / 权限清单 / 提交与被拒计数 / 被拒调用记录。
fn detail_card(app: &App, summary: &PluginSummary) -> Element<'static, Message> {
    let mut rows: Vec<Element<'static, Message>> = Vec::new();
    rows.push(
        row![
            text(format!("{}（{}）", summary.name, summary.id))
                .size(pal::fs_body())
                .color(pal::TEXT_PRIMARY),
            space().width(Length::Fill),
            controls::btn(
                "关闭详情",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(Message::Plugins(PluginsMsg::DetailClose)),
            ),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center)
        .into(),
    );
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

    let mut perms = row![
        text("声明能力")
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY)
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

    if app.plugins_state.detail_audit.is_empty() {
        rows.push(
            text("暂无被拒调用记录")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED)
                .into(),
        );
    } else {
        for record in &app.plugins_state.detail_audit {
            let detail = if record.detail.is_empty() {
                String::new()
            } else {
                format!("（{}）", record.detail)
            };
            rows.push(
                text(format!(
                    "{}  {}{}",
                    layout_view::format_ts(record.at.saturating_mul(MS_PER_SEC) as i64),
                    record.method,
                    detail
                ))
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY)
                .into(),
            );
        }
    }
    stacked_card(rows)
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
        Permission::Clipboard => "剪贴板",
        Permission::Window => "窗口",
        Permission::Tray => "托盘",
        Permission::Collab => "协作",
        Permission::Ai => "AI",
        Permission::Dialog => "对话框",
    }
}
