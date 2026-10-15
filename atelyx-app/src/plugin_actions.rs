//! 插件界面面的状态与接线：壳侧视图 kind 编解码、贡献查询、输入草稿与多行编辑器、面树的投影刷新、
//! 界面图片按需载入、节点事件回发；接管与插件窗口的壳侧应用、领域文档实例的打开路由。
//!
//! 贡献与面树的投影缓存在 `App`（`plugin_contributions` / `plugin_surfaces` / `plugin_trees`），
//! 由 [`refresh_projection`] 随运行时事件刷新；渲染在 [`crate::plugin_view`]。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use iced::widget::text_editor;
use iced::{Task, window};

use atelyx_core::layout::{self, LayoutNode};
use atelyx_core::plugin_runtime::{
    CanvasPointer, Contribution, ContributionSlot, DrawCommand, EventKind, EventValue, ImageSource,
    NodeBody, SurfaceKind, TakeoverScope, ViewEvent, ViewNode, WindowOpenParams,
};

use crate::{App, Message};

/// 壳侧插件视图 kind 前缀（内置视图词汇不被插件占用；插件 id 与贡献 id 都不含 `:`）。
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

/// 插件面板的图标名（归一为形状表里的 'static 名；贡献未声明 / 名字未知回落拼图）。
pub fn contribution_icon(app: &App, plugin_id: &str, contribution_id: &str) -> &'static str {
    let name = contribution(app, plugin_id, contribution_id)
        .and_then(|c| c.icon.as_deref())
        .unwrap_or("puzzle");
    normalize_icon(name, "puzzle")
}

/// 图标名归一为宿主形状表里的 'static 名；未知 / 空名回落指定兜底。
pub fn normalize_icon(name: &str, fallback: &'static str) -> &'static str {
    match crate::ui::icon::icon_name(name) {
        "" => fallback,
        resolved => resolved,
    }
}

// ===== 输入草稿 =====

/// 输入草稿键（插件 id + 面 id + 节点 id；节点 id 在树内唯一）。
pub type InputKey = (String, String, String);

/// 单行 / 数字输入的宿主草稿：`local` = 本机当前编辑值，`reported` = 本机认知中插件持有的值
/// （声明了 `change` 的节点每次回发即更新；未声明时保持插件现值）。
///
/// 显示值取 `reported == 插件当前值 ? local : 插件当前值`：插件回显本机刚发的值不改变显示
/// （打字不被回退），插件主动改值即接管显示。插件乱序提交旧值时本机文本会被覆盖，
/// 回发顺序由插件侧保证。
#[derive(Clone, Debug)]
pub struct InputDraft {
    local: String,
    reported: String,
}

impl InputDraft {
    /// 记录一次本地编辑；`forwarded` = 该节点声明了 `change`（本次文本已回发给插件）。
    fn edited(local: String, plugin_value: &str, forwarded: bool) -> Self {
        let reported = if forwarded { local.clone() } else { plugin_value.to_string() };
        Self { local, reported }
    }
}

/// 草稿下的显示值（无草稿 = 插件的值）。
pub fn input_display(plugin_value: &str, draft: Option<&InputDraft>) -> String {
    match draft {
        Some(draft) if draft.reported == plugin_value => draft.local.clone(),
        _ => plugin_value.to_string(),
    }
}

/// 多行输入的本机编辑器状态：内容与光标 / 选区都归它所有（插件不改值即原地保留），
/// `reported` 语义同 [`InputDraft`]。
pub struct EditorState {
    pub(crate) content: text_editor::Content,
    reported: String,
}

/// 输入节点的值文本（单行取原值、数字按无多余零的十进制；草稿比对与事件回发共用它）。
pub fn node_value_text(body: &NodeBody) -> Option<String> {
    match body {
        NodeBody::TextInput { value, .. } => Some(value.clone()),
        NodeBody::NumberInput { value, .. } => value.map(format_number),
        _ => None,
    }
}

/// 数字的规范文本（整数不带小数点，其余取最短表示）。
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// 输入类节点（文本 / 数字）：事件携带文本，需先写宿主草稿再上行。
fn is_input(body: &NodeBody) -> bool {
    matches!(body, NodeBody::TextInput { .. } | NodeBody::NumberInput { .. })
}

/// 数字输入的文本 → 事件值：不可解析为有限数值时不发事件，越界按声明的 min / max 钳制。
pub fn number_event_value(body: &NodeBody, text: &str) -> Option<EventValue> {
    let NodeBody::NumberInput { min, max, .. } = body else {
        return None;
    };
    let parsed = text.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
    let clamped = min.map_or(parsed, |lo| parsed.max(lo));
    let clamped = max.map_or(clamped, |hi| clamped.min(hi));
    Some(EventValue::Number(clamped))
}

/// 树里按 id 找节点（草稿维护与事件处理共用；查找实现在协议类型 [`ViewNode::find`]）。
pub fn find_node<'a>(tree: &'a ViewNode, node_id: &str) -> Option<&'a ViewNode> {
    tree.find(node_id)
}

/// 事件是否由节点声明（未声明不上行）。画布节点的指针事件按 `mouseEvents` 声明子集判定。
pub(crate) fn declares(node: &ViewNode, kind: EventKind) -> bool {
    node.events.contains(&kind)
}

/// 事件对应的声明种类（上行闸门按它判定）。画布指针事件有独立的声明集（`mouseEvents`），
/// 不经此映射（返回 `None`）。
fn event_kind(event: &ViewEvent) -> Option<EventKind> {
    match event {
        ViewEvent::Click => Some(EventKind::Click),
        ViewEvent::Change { .. } => Some(EventKind::Change),
        ViewEvent::Submit { .. } => Some(EventKind::Submit),
        ViewEvent::Focus => Some(EventKind::Focus),
        ViewEvent::Blur => Some(EventKind::Blur),
        ViewEvent::Canvas { .. } => None,
    }
}

/// 画布指针事件是否由节点声明。
fn declares_pointer(node_body: &NodeBody, pointer: &CanvasPointer) -> bool {
    let NodeBody::Canvas { mouse_events } = node_body else {
        return false;
    };
    mouse_events.iter().any(|kind| kind.matches(pointer))
}

/// 画布指针事件构造（画布部件回发用）。
pub(crate) fn view_event_canvas(pointer: CanvasPointer) -> ViewEvent {
    ViewEvent::Canvas { pointer }
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

/// 布局变化后重算已挂面并差额通知插件：新出现的面发 attached，消失的面发 detached 并撤掉本地草稿与树。
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
        drop_node_state(app, plugin_id, surface_id);
    }
    app.plugin_surfaces = now;
    true
}

/// 设置贡献的挂载差分：注册即挂载（宿主立即回 attached，插件常驻设置树），
/// 贡献撤销 / 插件停用随差分回 detached 并撤掉本地投影。运行中的插件才有贡献，
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
        drop_node_state(app, plugin_id, surface_id);
    }
    app.plugin_settings_surfaces = now;
}

/// 把贡献与已挂面的树投影进壳状态，并按需发起界面图片载入（返回的任务由调用方链出）。
/// 投影面 = 布局面板 + 设置面 + 插件窗口 + 接管面 + 文档实例（后三类不在布局里，键在此补齐）。
pub fn refresh_projection(app: &mut App) -> Task<Message> {
    let contributions = app.plugins.contributions();
    app.plugin_contributions = contributions;
    app.plugin_doc_types = app.plugins.doc_types();
    sync_settings_surfaces(app);
    let mut surfaces: Vec<(String, String)> = app.plugin_surfaces.iter().cloned().collect();
    surfaces.extend(app.plugin_settings_surfaces.iter().cloned());
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
    let mut canvas_keys = HashSet::new();
    for (plugin_id, surface_id) in surfaces {
        let key = (plugin_id.clone(), surface_id.clone());
        match app.plugins.surface_tree(&plugin_id, &surface_id) {
            Some(tree) => {
                sync_editors(app, &plugin_id, &surface_id, &tree);
                prune_node_state(app, &plugin_id, &surface_id, &tree);
                app.plugin_trees.insert(key.clone(), tree);
            }
            None => {
                app.plugin_trees.remove(&key);
                drop_node_state(app, &plugin_id, &surface_id);
            }
        }
        // 画布清单投影：树里每个 canvas 节点取最新清单（收尾 / 树重发的孤儿清单不投影）
        let mut canvases = Vec::new();
        if let Some(tree) = app.plugin_trees.get(&key) {
            collect_canvas_nodes(tree, &mut canvases);
        }
        for node_id in canvases {
            if let Some(list) = app.plugins.surface_canvas(&plugin_id, &surface_id, &node_id) {
                let canvas_key = (plugin_id.clone(), surface_id.clone(), node_id);
                canvas_keys.insert(canvas_key.clone());
                app.plugin_canvases.insert(canvas_key, list);
            }
        }
    }
    app.plugin_canvases.retain(|key, _| canvas_keys.contains(key));
    ensure_images(app)
}

/// 撤掉某面的本地草稿、多行编辑器、画布清单与树投影（面移除 / 插件停用）。
pub fn drop_node_state(app: &mut App, plugin_id: &str, surface_id: &str) {
    app.plugin_trees
        .remove(&(plugin_id.to_string(), surface_id.to_string()));
    app.plugin_drafts
        .retain(|key, _| !(key.0 == plugin_id && key.1 == surface_id));
    app.plugin_editors
        .retain(|key, _| !(key.0 == plugin_id && key.1 == surface_id));
    app.plugin_canvases
        .retain(|key, _| !(key.0 == plugin_id && key.1 == surface_id));
}

/// 撤掉树里已不存在的节点上的草稿与编辑器。
fn prune_node_state(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let alive = |key: &InputKey| {
        key.0 != plugin_id || key.1 != surface_id || find_node(tree, key.2.as_str()).is_some()
    };
    app.plugin_drafts.retain(|key, _| alive(key));
    app.plugin_editors.retain(|key, _| alive(key));
}

/// 多行输入节点的本机编辑器同步：仅当插件值既不同于本机当前文本、也不同于已上报值时接管
/// （同文重建内容会丢光标与选区，故同文不动）。
fn sync_editors(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let mut wanted: Vec<(InputKey, String)> = Vec::new();
    collect_multiline(tree, &mut |node, value| {
        wanted.push((
            (plugin_id.to_string(), surface_id.to_string(), node.to_string()),
            value,
        ));
    });
    for (key, plugin_value) in wanted {
        match app.plugin_editors.get_mut(&key) {
            Some(state) => {
                if plugin_value != state.content.text() && plugin_value != state.reported {
                    state.content = text_editor::Content::with_text(&plugin_value);
                    state.reported = plugin_value;
                }
            }
            None => {
                app.plugin_editors.insert(
                    key,
                    EditorState {
                        content: text_editor::Content::with_text(&plugin_value),
                        reported: plugin_value,
                    },
                );
            }
        }
    }
}

/// 收集树里的多行输入节点（节点 id + 值文本）。
fn collect_multiline<'a>(node: &'a ViewNode, out: &mut impl FnMut(&'a str, String)) {
    if matches!(node.body, NodeBody::TextInput { multiline: Some(true), .. }) {
        if let Some(value) = node_value_text(&node.body) {
            out(node.id.as_str(), value);
        }
    }
    for child in &node.children {
        collect_multiline(child, out);
    }
}

/// 收集树里的画布节点 id（清单投影按它对齐）。
fn collect_canvas_nodes(node: &ViewNode, out: &mut Vec<String>) {
    if matches!(node.body, NodeBody::Canvas { .. }) {
        out.push(node.id.clone());
    }
    for child in &node.children {
        collect_canvas_nodes(child, out);
    }
}

// ===== 界面图片 =====

/// 界面图片来源的内容键：私有目录取绝对路径（与仓库相对路径取值域不重叠，仓库来源沿仓库口径）。
pub(crate) fn image_key(app: &App, plugin_id: &str, source: &ImageSource) -> Option<String> {
    match source {
        ImageSource::Private { path } => {
            let dir = app.plugins.plugin_data_dir(plugin_id)?;
            Some(safe_private_path(&dir, path)?.to_string_lossy().into_owned())
        }
        ImageSource::Vault { path } => (!path.is_empty()).then(|| path.clone()),
    }
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
fn ensure_images(app: &mut App) -> Task<Message> {
    let mut pending: Vec<(String, ImageSource)> = Vec::new();
    for ((plugin_id, _), tree) in &app.plugin_trees {
        let mut sources = Vec::new();
        collect_image_sources(tree, &mut sources);
        for source in sources {
            if let Some(key) = image_key(app, plugin_id, source) {
                pending.push((key, source.clone()));
            }
        }
    }
    for ((plugin_id, _, _), list) in &app.plugin_canvases {
        for command in list.iter() {
            if let DrawCommand::Image { source, .. } = command {
                if let Some(key) = image_key(app, plugin_id, source) {
                    pending.push((key, source.clone()));
                }
            }
        }
    }
    let gen = app.epoch();
    let mut tasks = Vec::new();
    for (key, source) in pending {
        if app.note_images.contains_key(&key)
            || app.note_images_failed.contains(&key)
            || !app.note_images_inflight.insert(key.clone())
        {
            continue;
        }
        tasks.push(load_image(app, gen, key, source));
    }
    Task::batch(tasks)
}

/// 单张界面图片的载入任务：私有来源直接读文件，仓库来源沿用笔记图片的本地 / 空间分派。
fn load_image(app: &App, gen: u64, key: String, source: ImageSource) -> Task<Message> {
    match source {
        ImageSource::Private { .. } => {
            let path = PathBuf::from(&key);
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
    }
}

fn collect_image_sources<'a>(node: &'a ViewNode, out: &mut Vec<&'a ImageSource>) {
    if let NodeBody::Image { source, .. } = &node.body {
        out.push(source);
    }
    for child in &node.children {
        collect_image_sources(child, out);
    }
}

// ===== 接管与插件窗口 =====

/// 一个窗口的接管态（按窗生效；布局真源不动，退出即恢复原视图）。
#[derive(Clone, Debug)]
pub struct TakeoverActive {
    pub plugin_id: String,
    /// 接管的贡献 id（与面板共享同一棵提交树）。
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
    pub hide_on_blur: bool,
}

/// 插件接管请求：应用到当前聚焦窗口。贡献未注册 / 目标不可接管（插件窗口 / 已关闭窗口 /
/// 已在接管中）都以可读原因回填失败。接管不占权限声明——它是渲染编排（只呈现插件自己注册
/// 的贡献树），且退出入口由宿主常驻提供。
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
        // 面就绪通知：贡献面未挂载时插件尚未提交树，接管成功即告知（与面板挂载同一通知形态）
        let kind = if full { SurfaceKind::Takeover } else { SurfaceKind::Page };
        app.plugins.notify_surface_attached(&plugin_id, &surface_id, kind);
        Ok(())
    })();
    app.plugins.resolve_ui_call(request_id, result);
    refresh_projection(app)
}

/// 退出某窗口的接管（宿主常驻退出入口 / Esc / 窗口关闭随清理）。未在接管中 = 空操作。
pub fn exit_takeover(app: &mut App, os_id: window::Id) -> Task<Message> {
    if app.plugin_takeovers.remove(&os_id).is_some() {
        // 布局事件（拖拽命中带等）在接管期间被抑制，退出即恢复
        app.schedule_persist();
    }
    Task::none()
}

/// 插件开窗请求：校验后建 OS 窗口（重复 id 以可读原因拒绝；窗口参数声明尺寸 / 置顶 /
/// 不进任务栏 / 失焦收起）。`window::open` 返回的 OS id 即壳侧映射键。
pub fn on_window_open(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    params: WindowOpenParams,
) -> Task<Message> {
    let dup = app
        .plugin_windows
        .values()
        .any(|w| w.plugin_id == plugin_id && w.window_key == params.id);
    if dup {
        app.plugins
            .resolve_ui_call(request_id, Err(format!("窗口 id 已存在：{}", params.id)));
        return Task::none();
    }
    let mut settings = iced::window::Settings {
        size: iced::Size::new(
            params.width.unwrap_or(480.0).max(160.0),
            params.height.unwrap_or(640.0).max(120.0),
        ),
        exit_on_close_request: true,
        icon: crate::ui::window_icon(),
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
            hide_on_blur,
        },
    );
    app.plugins.resolve_ui_call(request_id, Ok(()));
    // 面就绪通知先于窗口可见（进程在跑时插件即刻可提交树；树投影由 ViewSubmitted 事件刷新）
    app.plugins.notify_surface_attached(&plugin_id, &params.id, SurfaceKind::Window);
    let level = if params.always_on_top.unwrap_or(false) {
        iced::window::Level::AlwaysOnTop
    } else {
        iced::window::Level::Normal
    };
    open.map(move |_| Message::WindowOpened(os_id))
        .chain(iced::window::set_level(os_id, level))
        .chain(iced::window::scale_factor(os_id).map(move |sf| Message::ScaleKnown(os_id, sf)))
        .chain(refresh_projection(app))
}

/// 插件请求关闭自己的一个窗口。
pub fn on_window_close(app: &mut App, request_id: u64, plugin_id: String, window_key: String) -> Task<Message> {
    let found = app
        .plugin_windows
        .iter()
        .find(|(_, w)| w.plugin_id == plugin_id && w.window_key == window_key)
        .map(|(os, _)| *os);
    match found {
        Some(os_id) => {
            app.plugins.resolve_ui_call(request_id, Ok(()));
            window::close(os_id)
        }
        None => {
            app.plugins
                .resolve_ui_call(request_id, Err(format!("窗口未打开：{window_key}")));
            Task::none()
        }
    }
}

/// OS 窗口已关闭（用户关闭插件窗口）：摘记账、撤投影并通知插件（`host:window.closed`）；
/// 若该窗口在接管中（不可达路径，防御性清理）一并摘除接管条目。
pub fn on_plugin_window_closed(app: &mut App, os_id: window::Id) -> Task<Message> {
    app.plugin_takeovers.remove(&os_id);
    if app.focused_window == Some(os_id) {
        app.focused_window = None;
    }
    let Some(entry) = app.plugin_windows.remove(&os_id) else {
        return refresh_projection(app);
    };
    app.plugins
        .notify_surface_detached(&entry.plugin_id, &entry.window_key);
    drop_node_state(app, &entry.plugin_id, &entry.window_key);
    refresh_projection(app)
}

/// 插件窗口失焦：声明了 hide_on_blur 的隐藏窗口（浮窗形态）。
pub fn on_plugin_window_blur(app: &mut App, os_id: window::Id) -> Task<Message> {
    let hide = app
        .plugin_windows
        .get(&os_id)
        .is_some_and(|w| w.hide_on_blur);
    if hide {
        iced::window::set_mode(os_id, iced::window::Mode::Hidden)
    } else {
        Task::none()
    }
}

/// 插件停用 / 崩溃后重启就绪：按当前壳侧状态重发面就绪通知（面板 / 设置 / 插件窗口 / 接管面），
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

// ===== 领域文档类型 =====

/// 一个打开中的插件文档实例（单实例：同一时间至多一个打开的插件文档，切换文件即换实例）。
#[derive(Clone, Debug)]
pub struct DocOpenInstance {
    pub plugin_id: String,
    /// 承载文档内容的贡献 id（manifest docTypeRegistrations.surface）；
    /// 实例面标识 = 贡献 id 本身（单实例，与该贡献的面板树同一键空间）。
    pub contribution_id: String,
    pub path: String,
    /// 实例面标识（= contribution_id；插件据此提交文档界面树）。
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
        .plugins
        .doc_types()
        .into_iter()
        .filter(|info| info.registration.extension.eq_ignore_ascii_case(extension))
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
        drop_node_state(app, &old.plugin_id, &old.surface_id);
    }
    // 实例面标识 = 贡献 id：渲染 / 事件 / 清理与面板树共用同一键空间（单实例语义）
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
    drop_node_state(app, &doc.plugin_id, &doc.surface_id);
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

// ===== 交互 =====

/// 插件菜单 / 下拉的弹层条目：标签 + 选中标记 + 点击要回发的消息。
#[derive(Clone, Debug)]
pub(crate) struct PopupEntry {
    pub(crate) label: String,
    pub(crate) current: bool,
    pub(crate) disabled: bool,
    pub(crate) danger: bool,
    pub(crate) on_press: Message,
}

/// 节点事件处理：输入类先写本机草稿（含回显抑制），再按该事件的声明决定是否上行。
pub fn on_view_event(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    event: ViewEvent,
) -> Task<Message> {
    let key = (plugin_id.clone(), surface_id.clone(), node_id.clone());
    let found = app
        .plugin_trees
        .get(&(plugin_id.clone(), surface_id.clone()))
        .and_then(|tree| find_node(tree, &node_id))
        .map(|node| (node.body.clone(), node.events.clone()));
    let Some((body, events)) = found else {
        return Task::none();
    };
    let event = match event {
        // 输入类节点：文本先进本机草稿（显示值来源），数字节点转成数值型事件值
        ViewEvent::Change { value: EventValue::Text(text) } if is_input(&body) => {
            let plugin_value = node_value_text(&body).unwrap_or_default();
            let forwarded = events.contains(&EventKind::Change);
            app.plugin_drafts
                .insert(key, InputDraft::edited(text.clone(), &plugin_value, forwarded));
            match body {
                NodeBody::NumberInput { .. } => match number_event_value(&body, &text) {
                    Some(value) => ViewEvent::Change { value },
                    None => return Task::none(),
                },
                _ => ViewEvent::Change { value: EventValue::Text(text) },
            }
        }
        // 提交：数字节点把控件里的文本转成数值，其余原样上行
        ViewEvent::Submit { value: EventValue::Text(text) }
            if matches!(body, NodeBody::NumberInput { .. }) =>
        {
            match number_event_value(&body, &text) {
                Some(value) => ViewEvent::Submit { value },
                None => return Task::none(),
            }
        }
        other => other,
    };
    if !match &event {
        ViewEvent::Canvas { pointer } => declares_pointer(&body, pointer),
        other => event_kind(other).is_some_and(|kind| events.contains(&kind)),
    } {
        return Task::none();
    }
    app.plugins
        .send_view_event(&plugin_id, &surface_id, &node_id, event);
    Task::none()
}

/// 多行输入的编辑器动作：应用到本机内容，文本有变化才按节点声明上行。
pub fn on_text_edit(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    action: text_editor::Action,
) -> Task<Message> {
    let key = (plugin_id.clone(), surface_id.clone(), node_id.clone());
    let declares_change = app
        .plugin_trees
        .get(&(plugin_id.clone(), surface_id.clone()))
        .and_then(|tree| find_node(tree, &node_id))
        .is_some_and(|node| declares(node, EventKind::Change));
    let Some(state) = app.plugin_editors.get_mut(&key) else {
        return Task::none();
    };
    state.content.perform(action);
    let text = state.content.text();
    if text == state.reported {
        return Task::none();
    }
    state.reported = text.clone();
    if !declares_change {
        return Task::none();
    }
    app.plugins.send_view_event(
        &plugin_id,
        &surface_id,
        &node_id,
        ViewEvent::Change { value: EventValue::Text(text) },
    );
    Task::none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_input(id: &str, value: &str) -> ViewNode {
        ViewNode {
            id: id.into(),
            body: NodeBody::TextInput {
                value: value.into(),
                placeholder: None,
                multiline: None,
                password: None,
            },
            children: Vec::new(),
            events: vec![EventKind::Change],
        }
    }

    fn number_input(value: Option<f64>) -> NodeBody {
        NodeBody::NumberInput { value, min: Some(1.0), max: Some(10.0), step: None, placeholder: None }
    }

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

    /// 草稿显示规则：插件回显本机刚发的值不改变显示，插件主动改值即接管。
    #[test]
    fn draft_display_prefers_local_until_plugin_overrides() {
        let draft = InputDraft::edited("xy".into(), "x", true);
        assert_eq!(input_display("xy", Some(&draft)), "xy", "回显本机已上报值：显示本机文本");
        assert_eq!(input_display("z", Some(&draft)), "z", "插件改值：接管显示");
        assert_eq!(input_display("x", None), "x", "无草稿：显示插件值");
    }

    /// 未声明 change 的输入：已上报值保持插件现值，本机文本原地保留；插件改值仍接管。
    #[test]
    fn draft_without_forwarding_keeps_local_text() {
        let first = InputDraft::edited("a".into(), "", false);
        assert_eq!(first.reported, "", "未回发时已上报值 = 插件现值");
        assert_eq!(input_display("", Some(&first)), "a");
        let second = InputDraft::edited("ab".into(), "", false);
        assert_eq!(input_display("", Some(&second)), "ab");
        assert_eq!(input_display("z", Some(&second)), "z", "插件改值即接管");
    }

    #[test]
    fn number_input_parses_and_clamps() {
        let body = number_input(None);
        assert_eq!(number_event_value(&body, " 4 "), Some(EventValue::Number(4.0)));
        assert_eq!(number_event_value(&body, "0.5"), Some(EventValue::Number(1.0)));
        assert_eq!(number_event_value(&body, "99"), Some(EventValue::Number(10.0)));
        for bad in ["", "abc", "1e999", "NaN"] {
            assert!(number_event_value(&body, bad).is_none(), "应不发事件：{bad}");
        }
    }

    #[test]
    fn number_text_and_value_agree_on_roundtrip() {
        let body = NodeBody::NumberInput { value: Some(3.5), min: None, max: None, step: None, placeholder: None };
        assert_eq!(node_value_text(&body).as_deref(), Some("3.5"));
        assert_eq!(number_event_value(&body, "3.5"), Some(EventValue::Number(3.5)));
        let whole = NodeBody::NumberInput { value: Some(7.0), min: None, max: None, step: None, placeholder: None };
        assert_eq!(node_value_text(&whole).as_deref(), Some("7"));
        // 未给值时不产生值文本（输入草稿按空串起算）
        assert_eq!(node_value_text(&number_input(None)), None);
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

    #[test]
    fn find_node_locates_nested_children() {
        let tree = ViewNode {
            id: "root".into(),
            body: NodeBody::Column { gap: None, padding: None, cross_align: None },
            children: vec![text_input("query", "x")],
            events: Vec::new(),
        };
        assert!(find_node(&tree, "query").is_some());
        assert!(find_node(&tree, "missing").is_none());
    }

    #[test]
    fn multiline_collection_only_takes_multiline_inputs() {
        let tree = ViewNode {
            id: "root".into(),
            body: NodeBody::Column { gap: None, padding: None, cross_align: None },
            children: vec![
                text_input("single", "s"),
                ViewNode {
                    id: "multi".into(),
                    body: NodeBody::TextInput {
                        value: "m".into(),
                        placeholder: None,
                        multiline: Some(true),
                        password: None,
                    },
                    children: Vec::new(),
                    events: vec![EventKind::Change],
                },
            ],
            events: Vec::new(),
        };
        let mut found = Vec::new();
        collect_multiline(&tree, &mut |id, value| found.push((id.to_string(), value)));
        assert_eq!(found, vec![("multi".to_string(), "m".to_string())]);
    }

    /// 上行闸门按事件自己的声明判定：只声明 submit 的输入仍能上行提交（不被 change 声明拖累）。
    #[test]
    fn event_gate_uses_the_event_own_kind() {
        let mut node = text_input("query", "x");
        node.events = vec![EventKind::Submit];
        assert!(!declares(&node, EventKind::Change));
        assert!(declares(&node, EventKind::Submit));
        assert_eq!(
            event_kind(&ViewEvent::Submit { value: EventValue::Text("v".into()) }),
            Some(EventKind::Submit)
        );
        assert_eq!(event_kind(&ViewEvent::Click), Some(EventKind::Click));
        assert_eq!(event_kind(&ViewEvent::Canvas { pointer: CanvasPointer::Leave }), None);
    }
}
