//! 插件面的状态与接线：壳侧视图 kind 编解码、贡献查询、整面帧的投影刷新、界面图片按需载入；
//! 接管与插件窗口的壳侧应用、领域文档实例的打开路由。
//!
//! 贡献与帧的投影缓存在 `App`（`plugin_contributions` / `plugin_surfaces` / `plugin_frames`），
//! 由 [`refresh_projection`] 随运行时事件刷新；渲染在 [`crate::plugin_view`]。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use iced::{Task, window};

use atelyx_core::layout::{self, LayoutNode, LayoutOp};
use atelyx_core::plugin_runtime::{
    Contribution, ContributionSlot, DrawCommand, FrameInput, ImageSource, SurfaceKind,
    TakeoverScope, WindowOpenParams, WindowUpdateParams,
};

use crate::registry::{Registry, Snapshot};
use crate::{App, Message};

/// 壳侧插件视图 kind 前缀（预置插件视图词汇不被插件占用；插件 id 与贡献 id 都不含 `:`）。
const KIND_PREFIX: &str = "plugin:";

// ===== 视图 kind 编解码 =====

/// 贡献成的壳侧视图 kind（`plugin:<插件 id>:<贡献 id>`）。
pub fn view_kind(plugin_id: &str, contribution_id: &str) -> String {
    format!("{KIND_PREFIX}{plugin_id}:{contribution_id}")
}

/// 解析壳侧插件视图 kind →（插件 id, 贡献 id）；非插件 kind 或形态不合法 = `None`。
pub fn parse_view_kind(view: &str) -> Option<(&str, &str)> {
    let rest = view.strip_prefix(KIND_PREFIX)?;
    let (plugin_id, contribution_id) = rest.split_once(':')?;
    let valid =
        !plugin_id.is_empty() && !contribution_id.is_empty() && !contribution_id.contains(':');
    valid.then_some((plugin_id, contribution_id))
}

/// 某贡献的注册信息（菜单与面板渲染用）。
pub fn contribution<'a>(
    app: &'a App,
    plugin_id: &str,
    contribution_id: &str,
) -> Option<&'a Contribution> {
    app.plugin_contributions
        .iter()
        .find(|c| c.plugin_id == plugin_id && c.id == contribution_id)
}

/// 指定槽位的全部贡献（运行时汇总已按插件 id + 贡献 id 排序）。
pub fn contributions_of(app: &App, slot: ContributionSlot) -> impl Iterator<Item = &Contribution> {
    app.plugin_contributions.iter().filter(move |c| c.slot == slot)
}

/// 插件声明图标的解析键：内置名原样返回；未声明 / 未知名回落 `fallback`。
pub fn plugin_icon_key(name: Option<&str>, fallback: &'static str) -> Cow<'static, str> {
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return Cow::Borrowed(fallback);
    };
    match crate::ui::icon::icon_name(name) {
        "" => Cow::Borrowed(fallback),
        builtin => Cow::Borrowed(builtin),
    }
}

// ===== 布局里的面与投影 =====

/// 当前呈现布局里现存的插件面（激活布局的面板标签 + 撕裂窗口标签）。
pub fn surfaces_in_layout(app: &App) -> HashSet<(String, String)> {
    fn collect(node: &LayoutNode, found: &mut HashSet<(String, String)>) {
        match node {
            LayoutNode::Panel { tabs, .. } => {
                for tab in tabs {
                    if let Some((plugin_id, id)) = parse_view_kind(&tab.view) {
                        found.insert((plugin_id.to_string(), id.to_string()));
                    }
                }
            }
            LayoutNode::Split { children, .. } => {
                for child in children {
                    collect(child, found);
                }
            }
        }
    }
    let mut found = HashSet::new();
    collect(&layout::active_layout(&app.ui).tree, &mut found);
    for window in &app.ui.detached_windows {
        for tab in &window.tabs {
            if let Some((plugin_id, id)) = parse_view_kind(&tab.view) {
                found.insert((plugin_id.to_string(), id.to_string()));
            }
        }
    }
    found
}

/// 布局变化后重算已挂面并差额通知插件：新出现的面发 attached，消失的面发 detached。
/// 返回已挂面集合是否变化——未变化（改分割比例等）时调用方无需重投影。
pub fn sync_surfaces(app: &mut App) -> bool {
    let now = surfaces_in_layout(app);
    if now == app.plugin_surfaces {
        return false;
    }
    for (plugin_id, surface_id) in now.difference(&app.plugin_surfaces) {
        app.plugins
            .notify_surface_attached(plugin_id, surface_id, SurfaceKind::Panel);
    }
    let removed: Vec<(String, String)> = app.plugin_surfaces.difference(&now).cloned().collect();
    for (plugin_id, surface_id) in &removed {
        app.plugins.notify_surface_detached(plugin_id, surface_id);
    }
    app.plugin_surfaces = now;
    true
}

/// 设置贡献的挂载差分：注册即挂载（宿主立即回 attached，插件常驻设置面），
/// 贡献撤销 / 插件停用随差分回 detached。运行中的插件才有贡献，
/// 重启后随贡献恢复自动重新挂载。
pub fn sync_settings_surfaces(app: &mut App) {
    let now: HashSet<(String, String)> = contributions_of(app, ContributionSlot::Settings)
        .map(|c| (c.plugin_id.clone(), c.id.clone()))
        .collect();
    if now == app.plugin_settings_surfaces {
        return;
    }
    for (plugin_id, surface_id) in now.difference(&app.plugin_settings_surfaces) {
        app.plugins
            .notify_surface_attached(plugin_id, surface_id, SurfaceKind::Settings);
    }
    let removed: Vec<(String, String)> =
        app.plugin_settings_surfaces.difference(&now).cloned().collect();
    for (plugin_id, surface_id) in &removed {
        app.plugins.notify_surface_detached(plugin_id, surface_id);
    }
    app.plugin_settings_surfaces = now;
}

/// 表格视图面挂载差分：面板里当前选中的插件表格视图即挂在表格面板上的面
/// （换视图 / 关表格 / 切文件即旧面 detached）。返回是否变化——变化才需重投影。
pub fn sync_table_surfaces(app: &mut App) -> bool {
    let now: HashSet<(String, String)> = app
        .table
        .as_ref()
        .and_then(|session| session.view.active_view.as_deref())
        .and_then(parse_view_kind)
        .map(|(plugin_id, id)| (plugin_id.to_string(), id.to_string()))
        .into_iter()
        .collect();
    if now == app.plugin_table_surfaces {
        return false;
    }
    for (plugin_id, surface_id) in now.difference(&app.plugin_table_surfaces) {
        app.plugins
            .notify_surface_attached(plugin_id, surface_id, SurfaceKind::TableView);
    }
    let removed: Vec<(String, String)> =
        app.plugin_table_surfaces.difference(&now).cloned().collect();
    for (plugin_id, surface_id) in &removed {
        app.plugins.notify_surface_detached(plugin_id, surface_id);
    }
    app.plugin_table_surfaces = now;
    true
}

/// 唯一注册表的唯一重建入口：内置插件表 + 插件贡献 + 文档类型 + 在册服务 + 内置停用表。
/// 选择器、标签与设置页都读这张表，任何一侧变化后都必须经此整体重建。
pub fn rebuild_registry(app: &mut App) {
    let services = app.plugins.service_list();
    let plugins = app.plugins.list();
    let doc_types = app.plugins.doc_types();
    let registry = Registry::build(Snapshot {
        builtin_plugins: crate::pack::BUILTIN_PLUGINS,
        contributions: &app.plugin_contributions,
        doc_types: &doc_types,
        services: &services.services,
        plugins: &plugins,
        disabled_plugins: &app.ui.disabled_plugins,
    });
    app.registry = registry;
}

/// 整面帧投影 + 注册表重建（贡献 / 状态 / 服务变化后走这条）。
pub fn refresh_projection(app: &mut App) -> Task<Message> {
    let contributions = app.plugins.contributions();
    app.plugin_contributions = contributions;
    rebuild_registry(app);
    refresh_frames(app)
}

/// 只重取已挂面的帧：帧提交不改注册表，帧率驱动下走这条，避免逐帧重建表。
pub fn refresh_frames(app: &mut App) -> Task<Message> {
    sync_settings_surfaces(app);
    sync_table_surfaces(app);
    let mut surfaces: Vec<(String, String)> = app.plugin_surfaces.iter().cloned().collect();
    surfaces.extend(app.plugin_settings_surfaces.iter().cloned());
    surfaces.extend(app.plugin_table_surfaces.iter().cloned());
    surfaces.extend(
        app.plugin_windows
            .values()
            .map(|w| (w.plugin_id.clone(), w.window_key.clone())),
    );
    surfaces.extend(
        app.plugin_takeovers
            .values()
            .map(|t| (t.plugin_id.clone(), t.surface_id.clone())),
    );
    if let Some(doc) = &app.doc_open {
        surfaces.push((doc.plugin_id.clone(), doc.surface_id.clone()));
    }
    let mut frames = HashMap::new();
    for (plugin_id, surface_id) in surfaces {
        let key = (plugin_id.clone(), surface_id.clone());
        let Some(frame) = app.plugins.surface_frame(&plugin_id, &surface_id) else {
            continue;
        };
        // 帧换了才发新序号：帧对象地址会被分配器复用，缓存必须按序号失效（否则滚动 / 重排后画面不刷新）
        let changed = app.plugin_frames.get(&key).is_none_or(|old| !std::sync::Arc::ptr_eq(old, &frame));
        if changed {
            app.plugin_frame_stamp_seq += 1;
            app.plugin_frame_stamps.insert(key.clone(), app.plugin_frame_stamp_seq);
        }
        frames.insert(key, frame);
    }
    app.plugin_frames = frames;
    app.plugin_frame_stamps
        .retain(|key, _| app.plugin_frames.contains_key(key));
    sync_frame_focus(app);
    ensure_images(app)
}

/// 帧焦点槽只对仍在投影里的帧有效：面不再有帧即释放并回发失焦。
fn sync_frame_focus(app: &mut App) {
    let Some(focus) = app.plugin_frame_focus.clone() else { return };
    let mounted = app
        .plugin_frames
        .contains_key(&(focus.plugin_id.clone(), focus.surface_id.clone()));
    if !mounted {
        app.plugin_frame_focus = None;
        notify_frame_focus(app, &focus.plugin_id, &focus.surface_id, false);
    }
}

/// 整面帧的焦点变化回发插件（`host:frame.input`）。
fn notify_frame_focus(app: &App, plugin_id: &str, surface_id: &str, focused: bool) {
    app.plugins
        .notify_frame_input(plugin_id, surface_id, &FrameInput::Focus { focused });
}

/// 整面帧的键盘焦点（全局单槽：OS 只给聚焦窗口发键盘，窗口失焦即清除）。
#[derive(Clone, Debug, PartialEq)]
pub struct FrameFocus {
    pub window: window::Id,
    pub plugin_id: String,
    pub surface_id: String,
}

/// 整面帧的焦点请求：设 / 清帧焦点。
/// 取焦通知由帧部件在按下前直接回发（先于 `PointerDown`），这里只同步宿主焦点槽。
pub fn on_frame_focus(
    app: &mut App,
    window: window::Id,
    plugin_id: String,
    surface_id: String,
    focused: bool,
) {
    let current = app.plugin_frame_focus.clone();
    if !focused {
        let holds = current
            .as_ref()
            .is_some_and(|focus| focus.plugin_id == plugin_id && focus.surface_id == surface_id);
        if holds {
            app.plugin_frame_focus = None;
            notify_frame_focus(app, &plugin_id, &surface_id, false);
        }
        return;
    }
    let next = FrameFocus { window, plugin_id: plugin_id.clone(), surface_id: surface_id.clone() };
    if current.as_ref() == Some(&next) {
        return;
    }
    if let Some(previous) = current {
        notify_frame_focus(app, &previous.plugin_id, &previous.surface_id, false);
    }
    app.plugin_frame_focus = Some(next);
}

/// 清掉属于该窗口的键盘焦点（窗口失焦 / 关闭）。
pub fn release_plugin_focus(app: &mut App, window: window::Id) {
    let Some(focus) = app.plugin_frame_focus.clone() else { return };
    if focus.window != window {
        return;
    }
    app.plugin_frame_focus = None;
    notify_frame_focus(app, &focus.plugin_id, &focus.surface_id, false);
}

/// 整面帧是否收全部按键（含命令组合）：持有焦点的面按最新帧的 `keys` 声明判定。
pub fn frame_takes_keyboard(app: &App) -> bool {
    app.plugin_frame_focus.as_ref().is_some_and(|focus| {
        app.plugin_frames
            .get(&(focus.plugin_id.clone(), focus.surface_id.clone()))
            .is_some_and(|frame| frame.keys)
    })
}

// ===== 界面图片 =====

/// 界面图片来源的内容键：各来源取值域互不重叠——私有目录与单机绝对路径都取绝对路径
/// （后者加 `file:` 前缀，与相对路径隔开），仓库来源沿仓库相对路径口径；远程来源以 URL 作键，
/// 本机策略加 `local:` 前缀——同一地址在两种策略下走不同的出网守卫，共用一个键会让内网图
/// 被公网口径命中（笔记外链图片走公网口径）。
pub(crate) fn image_key(app: &App, plugin_id: &str, source: &ImageSource) -> Option<String> {
    match source {
        ImageSource::Private { path } => {
            let dir = app.plugins.plugin_data_dir(plugin_id)?;
            Some(safe_private_path(&dir, path)?.to_string_lossy().into_owned())
        }
        ImageSource::Vault { path } => (!path.is_empty()).then(|| path.clone()),
        ImageSource::Url { url, local } => is_remote_url(url).then(|| match local {
            Some(true) => format!("local:{url}"),
            _ => url.clone(),
        }),
        // 前缀隔开取值域：相对路径（`vault`）与绝对路径（`file` / 私有目录）不得互为键
        ImageSource::File { path } => (!path.is_empty()).then(|| format!("file:{path}")),
    }
}

fn is_remote_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// 私有文件路径（拒收绝对路径与 `..` 段，限制在插件私有目录内）。
fn safe_private_path(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// 界面图片按需载入：缺句柄且未在途 / 未失败的来源发起读盘或下载任务。
/// 句柄与笔记图片共用同一缓存（同来源即同字节，键不重叠）。
pub(crate) fn ensure_images(app: &mut App) -> Task<Message> {
    let mut pending: Vec<(String, ImageSource)> = Vec::new();
    for ((plugin_id, _), frame) in &app.plugin_frames {
        collect_draw_images(&frame.commands, &mut |source| {
            if let Some(key) = image_key(app, plugin_id, source) {
                pending.push((key, source.clone()));
            }
        });
    }
    let gen = app.epoch();
    let mut tasks = Vec::new();
    for (key, source) in pending {
        if app.note_images.contains_key(&key)
            || !app.can_load_image(&key)
            || !app.note_images_inflight.insert(key.clone())
        {
            continue;
        }
        tasks.push(load_image(app, gen, key, source));
    }
    Task::batch(tasks)
}

/// 单张界面图片的载入任务：私有目录来源与单机绝对路径直接读文件，仓库来源沿用笔记图片的
/// 本地 / 空间分派，远程来源按声明的策略下载并缓存。
fn load_image(app: &App, gen: u64, key: String, source: ImageSource) -> Task<Message> {
    match source {
        ImageSource::Private { .. } => {
            let path = PathBuf::from(&key);
            Task::perform(
                async move { std::fs::read(&path).map_err(|e| format!("读插件图片失败：{e}")) },
                move |result| Message::ImageLoaded(gen, key, result),
            )
        }
        // 键带 `file:` 前缀（与仓库相对路径隔开取值域），读盘用来源里的原始路径
        ImageSource::File { path } => {
            let path = PathBuf::from(path);
            Task::perform(
                async move { std::fs::read(&path).map_err(|e| format!("读插件图片失败：{e}")) },
                move |result| Message::ImageLoaded(gen, key, result),
            )
        }
        ImageSource::Vault { path } => {
            if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
                return crate::space_actions::space_load_image(app, gen, path);
            }
            match app.vault.root().ok() {
                Some(root) => {
                    let rel = path.clone();
                    Task::perform(
                        async move { atelyx_core::vault::read_file_bytes(&root, &rel) },
                        move |result| Message::ImageLoaded(gen, key.clone(), result),
                    )
                }
                None => Task::none(),
            }
        }
        ImageSource::Url { url, local } => {
            let local = local.unwrap_or(false);
            Task::perform(
                async move {
                    if local {
                        atelyx_core::remote_image::fetch_local_image(&url).await
                    } else {
                        atelyx_core::remote_image::fetch_external_image(&url).await
                    }
                },
                move |result| Message::ImageLoaded(gen, key, result),
            )
        }
    }
}

/// 绘图指令里要预载的图片来源。
pub(crate) fn collect_draw_images<'a>(
    commands: &'a [DrawCommand],
    visit: &mut impl FnMut(&'a ImageSource),
) {
    for command in commands {
        if let DrawCommand::Image { source, .. } = command {
            visit(source);
        }
    }
}

// ===== 接管与插件窗口 =====

/// 一个窗口的接管态（按窗生效；布局真源不动，退出即恢复原视图）。
#[derive(Clone, Debug)]
pub struct TakeoverActive {
    pub plugin_id: String,
    /// 接管的贡献 id（与面板共用同一面 id）。
    pub surface_id: String,
    /// true = 全接管（含标题栏区域：拖动 / 缩放 / 关闭由宿主代执行）；false = 只接管内容区。
    pub full: bool,
}

/// 一个插件窗口的壳侧记账（os 窗口 id → 条目）。
#[derive(Clone, Debug)]
pub struct PluginWindowActive {
    pub plugin_id: String,
    /// 插件声明的窗口 id（= 该窗口面的 surfaceId）。
    pub window_key: String,
    /// 窗口标题（`window.open` 声明），OS 标题栏按它取。
    pub title: String,
    /// 系统窗口边框；`false` 时宿主补标题栏与窗口控制。
    pub decorations: bool,
    pub hide_on_blur: bool,
    /// 最近一次已知窗口尺寸（开窗声明 + `window.update` 与 OS 调整事件共同维护）：
    /// `window.update` 只改一维时另一维取它。
    pub size: iced::Size,
    /// 是否置顶（`window.update` 与开窗声明共同维护）。
    pub always_on_top: bool,
    /// 插件已请求关闭、OS 窗口尚未销毁：同 id 可立即重新开窗，销毁事件到达时按是否已有新窗
    /// 决定要不要通知插件（见 [`on_plugin_window_closed`]）。
    pub closing: bool,
}

/// 插件接管请求：应用到当前聚焦窗口。贡献未注册 / 目标不可接管（插件窗口 / 已关闭窗口 /
/// 已在接管中）都以可读原因回填失败。接管不占权限声明——它是渲染编排（只呈现插件自己注册
/// 的贡献面），且退出入口由宿主常驻提供。
pub fn on_takeover_request(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    scope: TakeoverScope,
    surface_id: String,
) -> Task<Message> {
    let result = (|| -> Result<(), String> {
        let Some(os_id) = app.focused_window else {
            return Err("没有可接管的窗口".to_string());
        };
        // 接管只作用于工作区窗口（主窗 / 撕裂窗）：插件窗口有自己的承载语义
        if app.plugin_windows.contains_key(&os_id) {
            return Err("插件窗口不可被接管".to_string());
        }
        let is_workspace_window =
            app.main_id == Some(os_id) || app.detached_os.contains_key(&os_id);
        if !is_workspace_window {
            return Err("目标窗口已关闭".to_string());
        }
        if app.plugin_takeovers.contains_key(&os_id) {
            return Err("该窗口已在接管中".to_string());
        }
        if !app
            .plugin_contributions
            .iter()
            .any(|c| c.plugin_id == plugin_id && c.id == surface_id)
        {
            return Err(format!("接管的贡献未注册：{surface_id}"));
        }
        let full = matches!(scope, TakeoverScope::Full);
        app.plugin_takeovers.insert(
            os_id,
            TakeoverActive { plugin_id: plugin_id.clone(), surface_id: surface_id.clone(), full },
        );
        // 面就绪通知：贡献面未挂载时插件尚未提交帧，接管成功即告知（与面板挂载同一通知形态）
        let kind = if full { SurfaceKind::Takeover } else { SurfaceKind::Page };
        app.plugins.notify_surface_attached(&plugin_id, &surface_id, kind);
        Ok(())
    })();
    app.plugins.resolve_ui_call(request_id, result);
    refresh_projection(app)
}

/// 退出某窗口的接管（宿主常驻退出入口 / Esc / 窗口关闭随清理）。未在接管中 = 空操作。
/// 该面没有其他挂载点（布局 / 设置 / 插件窗口 / 接管面 / 文档实例）时一并撤投影并通知插件。
pub fn exit_takeover(app: &mut App, os_id: window::Id) -> Task<Message> {
    let Some(entry) = app.plugin_takeovers.remove(&os_id) else {
        return Task::none();
    };
    // 布局事件（拖拽命中带等）在接管期间被抑制，退出即恢复
    app.schedule_persist();
    let mounted_elsewhere = app
        .plugin_surfaces
        .contains(&(entry.plugin_id.clone(), entry.surface_id.clone()))
        || app
            .plugin_settings_surfaces
            .contains(&(entry.plugin_id.clone(), entry.surface_id.clone()))
        || app
            .plugin_windows
            .values()
            .any(|w| w.plugin_id == entry.plugin_id && w.window_key == entry.surface_id)
        || app
            .plugin_takeovers
            .values()
            .any(|t| t.plugin_id == entry.plugin_id && t.surface_id == entry.surface_id)
        || app.doc_open.as_ref().is_some_and(|doc| {
            doc.plugin_id == entry.plugin_id && doc.surface_id == entry.surface_id
        });
    if !mounted_elsewhere {
        app.plugins
            .notify_surface_detached(&entry.plugin_id, &entry.surface_id);
    }
    refresh_projection(app)
}

/// 插件开窗请求：校验后建 OS 窗口（重复 id 以可读原因拒绝；窗口参数声明标题 / 尺寸 / 置顶 /
/// 不进任务栏 / 失焦收起）。`window::open` 返回的 OS id 即壳侧映射键。
///
/// 关闭在途的同 id 窗口不算重复：插件关窗后可以立刻重开，不必等 OS 销毁完成。
pub fn on_window_open(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    params: WindowOpenParams,
) -> Task<Message> {
    let dup = app
        .plugin_windows
        .values()
        .any(|w| w.plugin_id == plugin_id && w.window_key == params.id && !w.closing);
    if dup {
        app.plugins
            .resolve_ui_call(request_id, Err(format!("窗口 id 已存在：{}", params.id)));
        return Task::none();
    }
    let size = iced::Size::new(
        params.width.unwrap_or(480.0).max(160.0),
        params.height.unwrap_or(640.0).max(120.0),
    );
    let always_on_top = params.always_on_top.unwrap_or(false);
    let decorations = params.decorations.unwrap_or(true);
    let mut settings = iced::window::Settings {
        size,
        decorations,
        exit_on_close_request: true,
        icon: crate::app_icon::window_icon(),
        ..iced::window::Settings::default()
    };
    settings.min_size = Some(iced::Size::new(160.0, 120.0));
    #[cfg(windows)]
    {
        settings.platform_specific.skip_taskbar = params.skip_taskbar.unwrap_or(false);
    }
    let hide_on_blur = params.hide_on_blur.unwrap_or(false);
    let (os_id, open) = iced::window::open(settings);
    app.plugin_windows.insert(
        os_id,
        PluginWindowActive {
            plugin_id: plugin_id.clone(),
            window_key: params.id.clone(),
            title: params.title.clone(),
            decorations,
            hide_on_blur,
            size,
            always_on_top,
            closing: false,
        },
    );
    app.plugins.resolve_ui_call(request_id, Ok(()));
    // 面就绪通知先于窗口可见（进程在跑时插件即刻可提交帧；帧投影由 ViewSubmitted 事件刷新）
    app.plugins.notify_surface_attached(&plugin_id, &params.id, SurfaceKind::Window);
    let level = if always_on_top {
        iced::window::Level::AlwaysOnTop
    } else {
        iced::window::Level::Normal
    };
    open.map(move |_| Message::WindowOpened(os_id))
        .chain(iced::window::set_level(os_id, level))
        .chain(iced::window::scale_factor(os_id).map(move |sf| Message::ScaleKnown(os_id, sf)))
        .chain(refresh_projection(app))
}

/// 插件请求关闭自己的一个窗口。应答语义 = 关闭已受理（OS 窗口随后销毁），插件可立即用同 id
/// 重新开窗；重复关闭同一窗口是幂等成功。
pub fn on_window_close(app: &mut App, request_id: u64, plugin_id: String, window_key: String) -> Task<Message> {
    // 只找还活着的条目：关闭在途的旧窗不可再当目标（否则回执了「已受理」却关不掉新窗）
    let mut live: Option<window::Id> = None;
    let mut closing_only = false;
    for (os, w) in &app.plugin_windows {
        if w.plugin_id != plugin_id || w.window_key != window_key {
            continue;
        }
        if w.closing {
            closing_only = true;
        } else {
            live = Some(*os);
        }
    }
    match live {
        Some(os_id) => {
            app.plugins.resolve_ui_call(request_id, Ok(()));
            if let Some(entry) = app.plugin_windows.get_mut(&os_id) {
                entry.closing = true;
            }
            window::close(os_id)
        }
        // 全部条目都已在途：这次关闭无事可做，按幂等成功回执
        None if closing_only => {
            app.plugins.resolve_ui_call(request_id, Ok(()));
            Task::none()
        }
        None => {
            app.plugins
                .resolve_ui_call(request_id, Err(format!("窗口未打开：{window_key}")));
            Task::none()
        }
    }
}

/// OS 窗口已关闭（用户关闭或插件请求关闭的窗口真正销毁）：摘记账。同 id 还有其他条目时只摘记账
/// （面仍挂着，不发卸下通知），否则发 `host:surface.detached` 并清节点态。用户关窗（条目非
/// 关闭在途）另发 `host:window.closed`——包括「同 id 的旧窗还在途、用户关掉最后一个活窗」的序列。
/// 若该窗口在接管中（不可达路径，防御性清理）一并摘除接管条目。
pub fn on_plugin_window_closed(app: &mut App, os_id: window::Id) -> Task<Message> {
    app.plugin_takeovers.remove(&os_id);
    if app.focused_window == Some(os_id) {
        app.focused_window = None;
    }
    let Some(entry) = app.plugin_windows.remove(&os_id) else {
        return refresh_projection(app);
    };
    let mut any_elsewhere = false;
    let mut live_elsewhere = false;
    for w in app.plugin_windows.values() {
        if w.plugin_id == entry.plugin_id && w.window_key == entry.window_key {
            any_elsewhere = true;
            live_elsewhere |= !w.closing;
        }
    }
    if !entry.closing && !live_elsewhere {
        // 用户关窗才发「窗口没了」：插件自己请求的关闭不必回报（它知道，且关闭回执已受理）
        app.plugins.notify_window_closed(&entry.plugin_id, &entry.window_key);
    }
    if !any_elsewhere {
        app.plugins
            .notify_surface_detached(&entry.plugin_id, &entry.window_key);
    }
    refresh_projection(app)
}

/// 插件请求运行时改一个已打开窗口的属性：只改给出的项，其余保持现值。
/// 尺寸只给一维时另一维取记账里的现值；窗口未打开（或关闭在途）以可读原因回填失败。
pub fn on_window_update(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    params: WindowUpdateParams,
) -> Task<Message> {
    let Some(os_id) = live_plugin_window(app, &plugin_id, &params.id) else {
        app.plugins
            .resolve_ui_call(request_id, Err(format!("窗口未打开：{}", params.id)));
        return Task::none();
    };
    app.plugins.resolve_ui_call(request_id, Ok(()));
    let mut task = Task::none();
    if let Some(entry) = app.plugin_windows.get_mut(&os_id) {
        let mut size = entry.size;
        if let Some(width) = params.width {
            size.width = width.max(160.0);
        }
        if let Some(height) = params.height {
            size.height = height.max(120.0);
        }
        if size != entry.size {
            entry.size = size;
            task = task.chain(iced::window::resize(os_id, size));
        }
        if let Some(on_top) = params.always_on_top {
            if on_top != entry.always_on_top {
                entry.always_on_top = on_top;
                let level = if on_top {
                    iced::window::Level::AlwaysOnTop
                } else {
                    iced::window::Level::Normal
                };
                task = task.chain(iced::window::set_level(os_id, level));
            }
        }
    }
    task
}

/// 插件请求重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
pub fn on_window_show(app: &mut App, request_id: u64, plugin_id: String, window_key: String) -> Task<Message> {
    let Some(os_id) = live_plugin_window(app, &plugin_id, &window_key) else {
        app.plugins
            .resolve_ui_call(request_id, Err(format!("窗口未打开：{window_key}")));
        return Task::none();
    };
    app.plugins.resolve_ui_call(request_id, Ok(()));
    iced::window::set_mode(os_id, iced::window::Mode::Windowed)
}

/// 拖动承载某个面的窗口：面先按接管面 / 插件窗口解析出口，解析不到即用当前聚焦窗口。
pub fn on_window_begin_drag(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    surface_id: String,
) -> Task<Message> {
    let target = app
        .plugin_takeovers
        .iter()
        .find(|(_, t)| t.plugin_id == plugin_id && t.surface_id == surface_id)
        .map(|(id, _)| *id)
        .or_else(|| live_plugin_window(app, &plugin_id, &surface_id))
        .or(app.focused_window);
    let Some(window) = target else {
        app.plugins.resolve_ui_call(request_id, Err("没有可拖动的窗口".to_string()));
        return Task::none();
    };
    app.plugins.resolve_ui_call(request_id, Ok(()));
    iced::window::drag(window)
}

/// 按插件 id + 窗口 key 找活窗（关闭在途的不算）。
fn live_plugin_window(app: &App, plugin_id: &str, window_key: &str) -> Option<window::Id> {
    app.plugin_windows
        .iter()
        .find(|(_, w)| w.plugin_id == plugin_id && w.window_key == window_key && !w.closing)
        .map(|(id, _)| *id)
}

/// 插件窗口失焦：声明了 hide_on_blur 的隐藏窗口（浮窗形态），并通知插件（可用 `window.show` 唤起）。
pub fn on_plugin_window_blur(app: &mut App, os_id: window::Id) -> Task<Message> {
    let Some(entry) = app.plugin_windows.get(&os_id) else {
        return Task::none();
    };
    if !entry.hide_on_blur {
        return Task::none();
    }
    let (plugin_id, window_key) = (entry.plugin_id.clone(), entry.window_key.clone());
    app.plugins.notify_window_hidden(&plugin_id, &window_key);
    iced::window::set_mode(os_id, iced::window::Mode::Hidden)
}

/// 插件停用 / 崩溃后重启就绪：按当前壳侧状态重发面就绪通知（面板 / 设置 / 表格视图 / 插件窗口 / 接管面），
/// 插件据此重建全部界面。
pub fn resync_surfaces_on_running(app: &mut App, plugin_id: &str) {
    let takeovers: Vec<(String, String, bool)> = app
        .plugin_takeovers
        .values()
        .filter(|t| t.plugin_id == plugin_id)
        .map(|t| (t.plugin_id.clone(), t.surface_id.clone(), t.full))
        .collect();
    for (pid, surface_id, full) in takeovers {
        let kind = if full { SurfaceKind::Takeover } else { SurfaceKind::Page };
        app.plugins.notify_surface_attached(&pid, &surface_id, kind);
    }
    // 表格视图面：挂载状态由壳侧会话选择决定、不随贡献增删差分，重启后须显式重发
    let table_surfaces: Vec<(String, String)> = app.plugin_table_surfaces.iter().cloned().collect();
    for (pid, surface_id) in table_surfaces {
        if pid == plugin_id {
            app.plugins.notify_surface_attached(&pid, &surface_id, SurfaceKind::TableView);
        }
    }
    let windows: Vec<(String, String)> = app
        .plugin_windows
        .values()
        .filter(|w| w.plugin_id == plugin_id)
        .map(|w| (w.plugin_id.clone(), w.window_key.clone()))
        .collect();
    for (plugin_id, surface_id) in windows {
        app.plugins.notify_surface_attached(&plugin_id, &surface_id, SurfaceKind::Window);
    }
    // 打开中的领域文档实例：重启后实例面已清空，按当前文档重新告知
    if let Some(doc) = &app.doc_open {
        if doc.plugin_id == plugin_id {
            app.plugins.notify_doc_open(plugin_id, &doc.surface_id, &doc.path);
        }
    }
}

// ===== 插件默认布局 =====

/// 首次启用插件时把其声明的默认布局摆进当前场景：每插件只摆一次（`plugin_layouts_applied`
/// 记账并随 ui-state 落盘，用户改名 / 删除后不再补回）。当前场景任一布局或撕裂窗口已含
/// 规格中的视图则只记账不新建，避免与用户已摆好的插件面重复。
pub fn apply_declared_default_layout(app: &mut App, plugin_id: &str) -> Task<Message> {
    if app.ui.plugin_layouts_applied.iter().any(|id| id == plugin_id) {
        return Task::none();
    }
    let Some((name, spec)) = app.plugins.declared_default_layout(plugin_id) else {
        return Task::none();
    };
    app.ui.plugin_layouts_applied.push(plugin_id.to_string());
    app.schedule_persist();
    if layout::layout_spec_views(&spec).iter().any(|view| scene_has_view(app, view)) {
        return Task::none();
    }
    app.apply(LayoutOp::AddLayoutFromSpec { name, spec })
}

/// 视图是否已在当前场景的任一布局（含主页）或撕裂窗口中。
fn scene_has_view(app: &App, view: &str) -> bool {
    let scene = layout::active_scene(&app.ui);
    let trees = std::iter::once(&scene.home_layout.tree)
        .chain(scene.layouts.iter().map(|l| &l.tree));
    trees.into_iter().any(|tree| {
        let mut tabs = Vec::new();
        layout::collect_tabs(tree, &mut tabs);
        tabs.iter().any(|t| t.view == view)
    }) || app
        .ui
        .detached_windows
        .iter()
        .flat_map(|w| &w.tabs)
        .any(|t| t.view == view)
}

// ===== 领域文档类型 =====

/// 一个打开中的插件文档实例（单实例：同一时间至多一个打开的插件文档，切换文件即换实例）。
#[derive(Clone, Debug)]
pub struct DocOpenInstance {
    pub plugin_id: String,
    /// 承载文档内容的贡献 id（manifest docTypeRegistrations.surface）；
    /// 实例面标识 = 贡献 id 本身（单实例）。
    pub contribution_id: String,
    pub path: String,
    /// 实例面标识（= contribution_id；插件据此提交文档面）。
    pub surface_id: String,
}

/// 文档视图 kind：插件领域文档视图沿用插件视图 kind 命名空间。
pub fn doc_view_kind(plugin_id: &str, contribution_id: &str) -> String {
    view_kind(plugin_id, contribution_id)
}

/// 按扩展名找文档类型注册（多个插件注册同一扩展名时按插件 id 排序取第一个，
/// 返回的候选数供调用方提示）。
pub fn doc_type_for_extension(
    app: &App,
    extension: &str,
) -> Option<(String, atelyx_core::plugin_runtime::DocTypeRegistration, usize)> {
    let matches: Vec<atelyx_core::plugin_runtime::DocTypeInfo> = app
        .registry
        .doc_types()
        .iter()
        .filter(|info| info.registration.extension.eq_ignore_ascii_case(extension))
        .cloned()
        .collect();
    let count = matches.len();
    let first = matches.into_iter().next()?;
    Some((first.plugin_id, first.registration, count))
}

/// 打开一个插件领域文档（文件打开路由分派）：登记实例、确保文档视图标签存在并激活、
/// 通知插件（`host:doc.open`）。实例面标识换新（旧实例先关闭通知）。
pub fn open_doc(app: &mut App, rel: &str) -> Task<Message> {
    let extension = rel.rsplit('.').next().unwrap_or_default().to_string();
    let Some((plugin_id, reg, candidates)) = doc_type_for_extension(app, &extension) else {
        app.notify(
            crate::ui::notify::Level::Error,
            format!("没有插件注册 .{extension} 文档类型"),
        );
        return Task::none();
    };
    if candidates > 1 {
        app.notify(
            crate::ui::notify::Level::Info,
            format!(".{extension} 由多个插件注册，已用 {plugin_id} 打开"),
        );
    }
    // 换文件即换实例：旧实例先通知关闭（同插件同贡献时插件可按路径自辨，无需重建面）
    if let Some(old) = app.doc_open.take() {
        app.plugins.notify_doc_close(&old.plugin_id, &old.surface_id, &old.path);
    }
    // 实例面标识 = 贡献 id：渲染与清理共用同一键空间（单实例语义）
    let surface_id = reg.surface.clone();
    app.plugins.notify_doc_open(&plugin_id, &surface_id, rel);
    app.doc_open = Some(DocOpenInstance {
        plugin_id: plugin_id.clone(),
        contribution_id: surface_id.clone(),
        path: rel.to_string(),
        surface_id,
    });
    app.ui.last_doc_file = Some(rel.to_string());
    app.schedule_persist();
    ensure_doc_tab(app, &plugin_id, &reg.surface)
}

/// 关闭当前打开的插件文档（文档标签关闭 / 文件删除 / 换仓库）。
pub fn close_doc(app: &mut App) -> Task<Message> {
    let Some(doc) = app.doc_open.take() else {
        return Task::none();
    };
    app.ui.last_doc_file = None;
    app.plugins.notify_doc_close(&doc.plugin_id, &doc.surface_id, &doc.path);
    app.schedule_persist();
    refresh_projection(app)
}

/// 确保文档视图标签存在并激活（复用「添加面板」的布局通道；视图被占用时仅激活）。
fn ensure_doc_tab(app: &mut App, plugin_id: &str, contribution_id: &str) -> Task<Message> {
    let view = doc_view_kind(plugin_id, contribution_id);
    // 文档视图已存在（主树或撕裂窗口）= 只需激活所在标签
    if let Some(task) = activate_existing_view(app, &view) {
        return task;
    }
    let panel_id = app
        .ui
        .focused_panel_id
        .clone()
        .or_else(|| first_panel_id(&layout::active_layout(&app.ui).tree));
    let Some(panel_id) = panel_id else {
        app.notify(
            crate::ui::notify::Level::Error,
            "没有可放置文档视图的面板".to_string(),
        );
        return Task::none();
    };
    app.apply(layout::LayoutOp::AddView { panel_id, view })
}

/// 布局里已存在某视图标签时激活它（主树 / 撕裂窗口各查一层）；未找到 = `None`。
fn activate_existing_view(app: &mut App, view: &str) -> Option<Task<Message>> {
    fn find_in(node: &LayoutNode, view: &str) -> Option<(String, String)> {
        match node {
            LayoutNode::Panel { id, tabs, .. } => tabs
                .iter()
                .find(|t| t.view == view)
                .map(|t| (id.clone(), t.id.clone())),
            LayoutNode::Split { children, .. } => {
                children.iter().find_map(|child| find_in(child, view))
            }
        }
    }
    if let Some((panel_id, tab_id)) = find_in(&layout::active_layout(&app.ui).tree, view) {
        return Some(app.apply(layout::LayoutOp::SetActive { panel_id, tab_id }));
    }
    for window in &app.ui.detached_windows {
        if let Some(tab) = window.tabs.iter().find(|t| t.view == view) {
            return Some(app.apply(layout::LayoutOp::DetachedSetActive {
                window_id: window.id.clone(),
                tab_id: tab.id.clone(),
            }));
        }
    }
    None
}

fn first_panel_id(node: &LayoutNode) -> Option<String> {
    match node {
        LayoutNode::Panel { id, .. } => Some(id.clone()),
        LayoutNode::Split { children, .. } => children.iter().find_map(first_panel_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_view_kind_roundtrip() {
        let kind = view_kind("com.a.panel", "models");
        assert_eq!(kind, "plugin:com.a.panel:models");
        assert_eq!(parse_view_kind(&kind), Some(("com.a.panel", "models")));
    }

    #[test]
    fn parse_view_kind_rejects_non_plugin_and_malformed_kinds() {
        for bad in [
            "note",
            "plugin:",
            "plugin:com.a",
            "plugin::x",
            "plugin:com.a:",
            "plugin:a:b:c",
            "pluginfoo:a",
        ] {
            assert!(parse_view_kind(bad).is_none(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn private_paths_stay_inside_plugin_dir() {
        let dir = Path::new("/plugins/com.a.panel/data");
        assert_eq!(
            safe_private_path(dir, "shots/last.png"),
            Some(dir.join("shots").join("last.png"))
        );
        assert_eq!(safe_private_path(dir, "./a.png"), Some(dir.join("a.png")));
        for bad in ["/etc/passwd", "../escape.png", "a/../../b.png"] {
            assert!(safe_private_path(dir, bad).is_none(), "应拒绝越出私有目录：{bad}");
        }
    }
}
