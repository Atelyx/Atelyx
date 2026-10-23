//! 插件界面面的状态与接线：壳侧视图 kind 编解码、贡献查询、输入草稿与多行编辑器、面树的投影刷新、
//! 界面图片按需载入、节点事件回发；接管与插件窗口的壳侧应用、领域文档实例的打开路由。
//!
//! 贡献与面树的投影缓存在 `App`（`plugin_contributions` / `plugin_surfaces` / `plugin_trees`），
//! 由 [`refresh_projection`] 随运行时事件刷新；渲染在 [`crate::plugin_view`]。

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use iced::widget::text_editor;
use iced::{Task, window};

use atelyx_core::layout::{self, LayoutNode};
use atelyx_core::markdown::LinkForm;
use atelyx_core::plugin_runtime::{
    CanvasPointer, Contribution, ContributionSlot, DrawCommand, EventKind, EventValue, ImageSource,
    NodeBody, ProtocolLinkForm, ReportMode, ScrollEdge, ScrollToParams, SurfaceKind, TakeoverScope,
    ViewEvent, ViewNode, WindowOpenParams, WindowUpdateParams,
};
use atelyx_view::support::{declares, format_number, node_value_text, report_mode};

use crate::{App, Message};

/// 壳侧插件视图 kind 前缀（内置视图词汇不被插件占用；插件 id 与贡献 id 都不含 `:`）。
const KIND_PREFIX: &str = "plugin:";

/// 多行输入在 `commit` 档位下的静默提交间隔：编辑器没有提交信号，输入停顿即视为一段输入结束。
const EDITOR_COMMIT_MS: u64 = 600;

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

/// 插件面板的图标键（内置名原样，自备名按插件命名空间限定；未声明 / 未注册回落拼图）。
pub fn contribution_icon(app: &App, plugin_id: &str, contribution_id: &str) -> Cow<'static, str> {
    let name = contribution(app, plugin_id, contribution_id).and_then(|c| c.icon.as_deref());
    plugin_icon_key(plugin_id, name, "puzzle")
}

/// 插件声明图标的解析键：内置名原样返回；自备名返回限定名 `<插件 id>::<名>`，
/// 未注册（插件未定义或已停用）回落 `fallback`。
pub fn plugin_icon_key(
    plugin_id: &str,
    name: Option<&str>,
    fallback: &'static str,
) -> Cow<'static, str> {
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return Cow::Borrowed(fallback);
    };
    match crate::ui::icon::icon_name(name) {
        "" => {}
        builtin => return Cow::Borrowed(builtin),
    }
    let key = format!("{plugin_id}::{name}");
    if crate::ui::icon::is_dynamic(&key) {
        Cow::Owned(key)
    } else {
        Cow::Borrowed(fallback)
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
    /// 上次见到的插件值：非逐键上报时用它判定插件是否主动改值（变了即接管输入）。
    baseline: String,
    mode: ReportMode,
}

impl InputDraft {
    /// 记录一次本地编辑；`forwarded` = 本次文本已回发给插件。
    fn edited(local: String, plugin_value: &str, forwarded: bool, mode: ReportMode) -> Self {
        let reported = if forwarded { local.clone() } else { plugin_value.to_string() };
        Self { local, reported, baseline: plugin_value.to_string(), mode }
    }

    /// 换上报档位（树重发时同步，换档不清本机文本）。
    fn set_mode(&mut self, mode: ReportMode) {
        self.mode = mode;
    }

    /// 插件下发了新值：覆盖本机文本并开启新的编辑会话。
    fn adopt(&mut self, plugin_value: String) {
        self.local = plugin_value.clone();
        self.reported = plugin_value.clone();
        self.baseline = plugin_value;
    }

    /// 本机当前编辑值（防抖到期上报用）。
    fn local(&self) -> &str {
        &self.local
    }

    /// 记下插件的当前值（值未变即不打断输入）。
    fn observe(&mut self, plugin_value: &str) {
        if self.baseline != plugin_value {
            if self.mode.is_deferred() {
                self.adopt(plugin_value.to_string());
            } else {
                self.baseline = plugin_value.to_string();
            }
        }
    }
}

/// 草稿下的显示值（无草稿 = 插件的值）。非逐键上报时本机值是唯一显示来源：
/// 插件主动改值已由 [`sync_drafts`] 在树更新时写回草稿，渲染路径不再做比较。
pub fn input_display(plugin_value: &str, draft: Option<&InputDraft>) -> String {
    match draft {
        Some(draft) if draft.mode.is_deferred() => draft.local.clone(),
        Some(draft) if draft.reported == plugin_value => draft.local.clone(),
        _ => plugin_value.to_string(),
    }
}

/// 多行输入的本机编辑器状态：内容与光标 / 选区都归它所有（插件不改值即原地保留），
/// `reported` 语义同 [`InputDraft`]。
pub struct EditorState {
    pub(crate) content: text_editor::Content,
    reported: String,
    /// 上次见到的插件值：非逐键上报时用它判定插件是否主动改值。
    baseline: String,
    mode: ReportMode,
}

/// markdown 节点的解析投影：`source` 用于变更比对（源码不变不重解析）。
pub struct MarkdownNode {
    pub(crate) source: String,
    pub(crate) doc: atelyx_core::markdown::MarkdownDocument,
}

/// 输入类节点（文本 / 数字）：事件携带文本，需先写宿主草稿再上行。
fn is_input(body: &NodeBody) -> bool {
    matches!(body, NodeBody::TextInput { .. } | NodeBody::NumberInput { .. })
}

/// 输入节点是否接受编辑（只读节点保留外观但不建草稿、不收事件）。
fn is_editable_input(body: &NodeBody) -> bool {
    match body {
        NodeBody::TextInput { read_only, .. } | NodeBody::NumberInput { read_only, .. } => {
            !matches!(read_only, Some(true))
        }
        _ => false,
    }
}

/// 数字输入的文本 → 事件值：清空上报 `Null`（与 0 区分），不可解析为有限数值时不发事件，
/// 越界按声明的 min / max 钳制。
pub fn number_event_value(body: &NodeBody, text: &str) -> Option<EventValue> {
    let NodeBody::NumberInput { min, max, .. } = body else {
        return None;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(EventValue::Null);
    }
    let parsed = trimmed.parse::<f64>().ok().filter(|v| v.is_finite())?;
    let clamped = min.map_or(parsed, |lo| parsed.max(lo));
    let clamped = max.map_or(clamped, |hi| clamped.min(hi));
    Some(EventValue::Number(clamped))
}

/// 树里按 id 找节点（草稿维护与事件处理共用；查找实现在协议类型 [`ViewNode::find`]）。
pub fn find_node<'a>(tree: &'a ViewNode, node_id: &str) -> Option<&'a ViewNode> {
    tree.find(node_id)
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
        ViewEvent::LinkClick { .. } => Some(EventKind::LinkClick),
        ViewEvent::Scroll { .. } => Some(EventKind::Scroll),
        ViewEvent::Canvas { .. } => None,
    }
}

/// 画布指针事件是否由节点声明。
fn declares_pointer(node_body: &NodeBody, pointer: &CanvasPointer) -> bool {
    let NodeBody::Canvas { mouse_events, .. } = node_body else {
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
        drop_node_state(app, plugin_id, surface_id);
    }
    app.plugin_table_surfaces = now;
    true
}

/// 把贡献与已挂面的树投影进壳状态，并按需发起界面图片载入（返回的任务由调用方链出）。
/// 投影面 = 布局面板 + 设置面 + 插件窗口 + 接管面 + 文档实例（后三类不在布局里，键在此补齐）。
pub fn refresh_projection(app: &mut App) -> Task<Message> {
    let contributions = app.plugins.contributions();
    app.plugin_contributions = contributions;
    app.plugin_doc_types = app.plugins.doc_types();
    sync_plugin_icons(app);
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
    let mut canvas_keys = HashSet::new();
    for (plugin_id, surface_id) in surfaces {
        let key = (plugin_id.clone(), surface_id.clone());
        match app.plugins.surface_tree(&plugin_id, &surface_id) {
            Some(tree) => {
                sync_drafts(app, &plugin_id, &surface_id, &tree);
                sync_editors(app, &plugin_id, &surface_id, &tree);
                prune_node_state(app, &plugin_id, &surface_id, &tree);
                sync_markdown(app, &plugin_id, &surface_id, &tree);
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

/// 重建自备图标注册表：先清空再按当前有界面的插件全量注册（插件停用 / 卸载后不留孤儿）。
/// 每次投影刷新整体重建，避免维护一份跨事件的生命周期表。
fn sync_plugin_icons(app: &App) {
    crate::ui::icon::reset_dynamic();
    let mut ids: HashSet<String> = app.plugin_trees.keys().map(|(plugin_id, _)| plugin_id.clone()).collect();
    for contribution in &app.plugin_contributions {
        ids.insert(contribution.plugin_id.clone());
    }
    for info in &app.plugin_doc_types {
        ids.insert(info.plugin_id.clone());
    }
    for id in ids {
        for (name, path) in app.plugins.plugin_icons(&id) {
            crate::ui::icon::register_dynamic(&format!("{id}::{name}"), &path);
        }
    }
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
    app.plugin_markdown
        .retain(|key, _| !(key.0 == plugin_id && key.1 == surface_id));
    app.plugin_hovered
        .retain(|key| !(key.0 == plugin_id && key.1 == surface_id));
}

/// 撤掉树里已不存在的节点上的草稿、编辑器与悬停态。
fn prune_node_state(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let alive = |key: &InputKey| {
        key.0 != plugin_id || key.1 != surface_id || find_node(tree, key.2.as_str()).is_some()
    };
    app.plugin_drafts.retain(|key, _| alive(key));
    app.plugin_draft_seq.retain(|key, _| alive(key));
    app.plugin_editors.retain(|key, _| alive(key));
    app.plugin_hovered.retain(|key| alive(key));
}

/// 单行 / 数字输入的草稿同步：记下插件当前值，非逐键上报时值变了即接管本机文本。
/// 与 [`sync_editors`] 同在树投影刷新时执行，保证插件改值先于渲染生效。
fn sync_drafts(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let mut wanted: Vec<(String, String, ReportMode)> = Vec::new();
    collect_drafts(tree, &mut |node, value, mode| {
        wanted.push((node.to_string(), value, mode));
    });
    for (node_id, plugin_value, mode) in wanted {
        let key = (plugin_id.to_string(), surface_id.to_string(), node_id);
        if let Some(draft) = app.plugin_drafts.get_mut(&key) {
            draft.set_mode(mode);
            draft.observe(&plugin_value);
        }
    }
}

/// 收集树里走草稿的输入节点（单行文本与数字；多行由编辑器状态承担）。
fn collect_drafts<'a>(node: &'a ViewNode, out: &mut impl FnMut(&'a str, String, ReportMode)) {
    let single_line = matches!(node.body, NodeBody::TextInput { multiline, .. } if !matches!(multiline, Some(true)));
    if single_line || matches!(node.body, NodeBody::NumberInput { .. }) {
        if is_editable_input(&node.body) {
            if let Some(value) = node_value_text(&node.body) {
                out(node.id.as_str(), value, report_mode(&node.body));
            }
        }
    }
    for child in &node.children {
        collect_drafts(child, out);
    }
}

/// 多行输入节点的本机编辑器同步：仅当插件值既不同于本机当前文本、也不同于已上报值时接管
/// （同文重建内容会丢光标与选区，故同文不动）。
fn sync_editors(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let mut wanted: Vec<(InputKey, String, ReportMode)> = Vec::new();
    collect_multiline(tree, &mut |node, value, mode| {
        wanted.push((
            (plugin_id.to_string(), surface_id.to_string(), node.to_string()),
            value,
            mode,
        ));
    });
    for (key, plugin_value, mode) in wanted {
        match app.plugin_editors.get_mut(&key) {
            Some(state) => {
                state.mode = mode;
                if mode.is_deferred() {
                    // 非逐键：只有插件主动改值才重建，输入期间不打断光标与选区
                    if plugin_value != state.baseline {
                        state.content = text_editor::Content::with_text(&plugin_value);
                        state.reported = plugin_value.clone();
                        state.baseline = plugin_value;
                    }
                } else if plugin_value != state.content.text() && plugin_value != state.reported {
                    state.content = text_editor::Content::with_text(&plugin_value);
                    state.reported = plugin_value.clone();
                    state.baseline = plugin_value;
                } else {
                    state.baseline = plugin_value;
                }
            }
            None => {
                app.plugin_editors.insert(
                    key,
                    EditorState {
                        content: text_editor::Content::with_text(&plugin_value),
                        reported: plugin_value.clone(),
                        baseline: plugin_value,
                        mode,
                    },
                );
            }
        }
    }
}

/// 收集树里的可编辑多行输入节点（节点 id + 值文本）：只读多行画静态框，不建编辑器状态。
fn collect_multiline<'a>(node: &'a ViewNode, out: &mut impl FnMut(&'a str, String, ReportMode)) {
    let editable = matches!(
        node.body,
        NodeBody::TextInput { multiline: Some(true), read_only: None | Some(false), .. }
    );
    if editable {
        if let Some(value) = node_value_text(&node.body) {
            out(node.id.as_str(), value, report_mode(&node.body));
        }
    }
    for child in &node.children {
        collect_multiline(child, out);
    }
}

/// markdown 节点解析投影同步：源码变化才重解析；树里已移除的节点随面清退。
fn sync_markdown(app: &mut App, plugin_id: &str, surface_id: &str, tree: &ViewNode) {
    let mut wanted: Vec<(&str, &str)> = Vec::new();
    collect_markdown(tree, &mut wanted);
    let mut alive = HashSet::new();
    for (node_id, content) in wanted {
        let key = (plugin_id.to_string(), surface_id.to_string(), node_id.to_string());
        alive.insert(key.clone());
        let cached = app.plugin_markdown.get(&key);
        if cached.is_none_or(|entry| entry.source != content) {
            app.plugin_markdown.insert(
                key,
                MarkdownNode {
                    source: content.to_string(),
                    doc: crate::markdown_view::parse_document(content),
                },
            );
        }
    }
    app.plugin_markdown
        .retain(|key, _| key.0 != plugin_id || key.1 != surface_id || alive.contains(key));
}

fn collect_markdown<'a>(node: &'a ViewNode, out: &mut Vec<(&'a str, &'a str)>) {
    if let NodeBody::Markdown { content, .. } = &node.body {
        out.push((node.id.as_str(), content.as_str()));
    }
    for child in &node.children {
        collect_markdown(child, out);
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
    // markdown 节点的块级图片走笔记图片同一条载入链（渲染端按 src 取同一句柄表）
    let markdown_srcs: Vec<String> = app
        .plugin_markdown
        .values()
        .flat_map(|entry| {
            crate::markdown_view::collect_image_srcs(&entry.doc.source, &entry.doc.blocks)
        })
        .collect();
    app.ensure_image_srcs(markdown_srcs);
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
    /// 窗口标题（`window.open` 声明），OS 标题栏按它取。
    pub title: String,
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
        drop_node_state(app, &entry.plugin_id, &entry.surface_id);
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
    let mut settings = iced::window::Settings {
        size,
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
            hide_on_blur,
            size,
            always_on_top,
            closing: false,
        },
    );
    app.plugins.resolve_ui_call(request_id, Ok(()));
    // 面就绪通知先于窗口可见（进程在跑时插件即刻可提交树；树投影由 ViewSubmitted 事件刷新）
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
        drop_node_state(app, &entry.plugin_id, &entry.window_key);
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

/// 插件请求把某可滚动容器滚到一端。目标须在当前树里且为 `list` / `scroll` 容器。
/// 该请求由运行时登记为**壳侧服务调用**（应答走 [`PluginRuntime::resolve_shell_call`]）。
pub fn on_scroll_to(
    app: &mut App,
    request_id: u64,
    plugin_id: String,
    params: ScrollToParams,
) -> Task<Message> {
    let node = app
        .plugin_trees
        .get(&(plugin_id.clone(), params.surface_id.clone()))
        .and_then(|tree| find_node(tree, &params.node_id))
        .map(|node| node.body.clone());
    let scrollable = matches!(node, Some(NodeBody::List { .. }) | Some(NodeBody::Scroll { .. }));
    if !scrollable {
        app.plugins.resolve_shell_call(
            request_id,
            Err(format!("目标不是可滚动容器：{}", params.node_id)),
        );
        return Task::none();
    }
    app.plugins.resolve_shell_call(request_id, Ok(serde_json::json!({})));
    let id = atelyx_view::render::scroll_id(&plugin_id, &params.surface_id, &params.node_id);
    match params.edge {
        // `RelativeOffset` 按比例定位：START = 0（顶端 / 最左）、END = 1（底端 / 最右），由控件钳到真实末端
        ScrollEdge::Start => iced::widget::operation::snap_to(
            id,
            iced::widget::operation::RelativeOffset::START,
        ),
        ScrollEdge::End => iced::widget::operation::snap_to_end(id),
    }
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

/// 节点事件处理：输入类先写本机草稿（含回显抑制），再按该事件的声明决定是否上行。
/// 输入框本地编辑（非逐键上报档位）：写草稿；`commit` 为真时按声明上报。
/// 防抖档位在未提交时排一次到期上报，同节点以最新序号为准（过期拍丢弃）。
pub fn on_draft(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    text: Option<String>,
    commit: bool,
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
    if !is_editable_input(&body) {
        return Task::none();
    }
    let plugin_value = node_value_text(&body).unwrap_or_default();
    let mode = report_mode(&body);
    let text = text
        .or_else(|| app.plugin_drafts.get(&key).map(|draft| draft.local().to_string()))
        .unwrap_or_else(|| plugin_value.clone());
    // 这条路径只在非逐键档位绑定到控件上；逐键档位由事件回发路径维护草稿
    let forwarded = commit && mode.is_deferred() && events.contains(&EventKind::Change);
    app.plugin_drafts.insert(
        key.clone(),
        InputDraft::edited(text.clone(), &plugin_value, forwarded, mode),
    );
    if commit {
        app.plugin_draft_seq.remove(&key);
        return report_input_value(app, &plugin_id, &surface_id, &node_id, &body, &events, &text, mode);
    }
    match mode {
        ReportMode::Debounced(ms) if ms > 0 => {
            let seq = app.plugin_draft_seq.entry(key).or_insert(0);
            *seq += 1;
            let seq = *seq;
            Task::perform(
                tokio::time::sleep(std::time::Duration::from_millis(u64::from(ms))),
                move |_| Message::PluginDraftFlush { plugin_id, surface_id, node_id, seq },
            )
        }
        _ => Task::none(),
    }
}

/// 数字输入步进：空值直接落到 min（缺省 0），其余按 step 增减后夹在 min / max 内。
/// 点击是离散动作，不受逐键上报档位影响——结果立即按 change 上行；已在该方向边界时
/// 不上行重复值。
pub fn on_step(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    up: bool,
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
    if !matches!(body, NodeBody::NumberInput { .. }) || !is_editable_input(&body) {
        return Task::none();
    }
    if !events.contains(&EventKind::Change) {
        return Task::none();
    }
    let plugin_value = node_value_text(&body).unwrap_or_default();
    let current = app
        .plugin_drafts
        .get(&key)
        .map(|draft| input_display(&plugin_value, Some(draft)))
        .unwrap_or_else(|| plugin_value.clone());
    let Some(text) = stepped_number_text(&body, &current, up) else {
        return Task::none();
    };
    if same_number(&current, &text) {
        return Task::none();
    }
    // 先按本机步进值显示（插件回显前不回退），插件回显后以它的值为准
    app.plugin_drafts.insert(
        key.clone(),
        InputDraft::edited(text.clone(), &plugin_value, false, report_mode(&body)),
    );
    app.plugin_draft_seq.remove(&key);
    if let Some(value) = number_event_value(&body, &text) {
        app.plugins
            .send_view_event(&plugin_id, &surface_id, &node_id, ViewEvent::Change { value });
    }
    Task::none()
}

/// 步进后的数值文本：解析失败（空 / 非数值）时落到 min（缺省 0）而不是叠加一步。
fn stepped_number_text(body: &NodeBody, text: &str, up: bool) -> Option<String> {
    let NodeBody::NumberInput { min, max, step, .. } = body else {
        return None;
    };
    let next = match text.trim().parse::<f64>().ok().filter(|v| v.is_finite()) {
        Some(value) => {
            let delta = step.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
            if up { value + delta } else { value - delta }
        }
        None => min.unwrap_or(0.0),
    };
    let next = min.map_or(next, |lo| next.max(lo));
    let next = max.map_or(next, |hi| next.min(hi));
    next.is_finite().then(|| format_number(next))
}

/// 两次输入是否表示同一个数值（步进落到边界时不再重复上行）。
fn same_number(left: &str, right: &str) -> bool {
    match (left.trim().parse::<f64>(), right.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => a.is_finite() && b.is_finite() && (a - b).abs() < f64::EPSILON,
        _ => false,
    }
}

/// 防抖到期：序号仍是最新才把草稿上报（编辑期间落后的拍次直接丢弃）。
pub fn on_draft_flush(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    seq: u64,
) -> Task<Message> {
    let key = (plugin_id.clone(), surface_id.clone(), node_id.clone());
    if app.plugin_draft_seq.get(&key).copied() != Some(seq) {
        return Task::none();
    }
    app.plugin_draft_seq.remove(&key);
    let Some(text) = app.plugin_drafts.get(&key).map(|draft| draft.local().to_string()) else {
        return Task::none();
    };
    let found = app
        .plugin_trees
        .get(&(plugin_id.clone(), surface_id.clone()))
        .and_then(|tree| find_node(tree, &node_id))
        .map(|node| (node.body.clone(), node.events.clone()));
    let Some((body, events)) = found else {
        return Task::none();
    };
    let mode = report_mode(&body);
    report_input_value(app, &plugin_id, &surface_id, &node_id, &body, &events, &text, mode)
}

/// 多行编辑器静默到期：序号仍是最新才把整段内容上报（期间又有输入则本次丢弃）。
pub fn on_editor_flush(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    seq: u64,
) -> Task<Message> {
    let key = (plugin_id.clone(), surface_id.clone(), node_id.clone());
    if app.plugin_draft_seq.get(&key).copied() != Some(seq) {
        return Task::none();
    }
    app.plugin_draft_seq.remove(&key);
    let Some(state) = app.plugin_editors.get_mut(&key) else {
        return Task::none();
    };
    let text = state.content.text();
    if text == state.reported {
        return Task::none();
    }
    state.reported = text.clone();
    state.baseline = text.clone();
    app.plugins.send_view_event(
        &plugin_id,
        &surface_id,
        &node_id,
        ViewEvent::Change { value: EventValue::Text(text) },
    );
    Task::none()
}

/// 把输入文本上报为事件：延迟档位发 `change`（数字转数值），按声明发 `submit`。
/// 不可解析为数值时不发事件（与逐键路径同口径）。
fn report_input_value(
    app: &mut App,
    plugin_id: &str,
    surface_id: &str,
    node_id: &str,
    body: &NodeBody,
    events: &[EventKind],
    text: &str,
    mode: ReportMode,
) -> Task<Message> {
    let event_value = |body: &NodeBody| match body {
        NodeBody::NumberInput { .. } => number_event_value(body, text),
        _ => Some(EventValue::Text(text.to_string())),
    };
    if mode.is_deferred() && events.contains(&EventKind::Change) {
        if let Some(value) = event_value(body) {
            app.plugins.send_view_event(plugin_id, surface_id, node_id, ViewEvent::Change { value });
        }
    }
    if events.contains(&EventKind::Submit) {
        if let Some(value) = event_value(body) {
            app.plugins.send_view_event(plugin_id, surface_id, node_id, ViewEvent::Submit { value });
        }
    }
    Task::none()
}

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
            app.plugin_drafts.insert(
                key,
                InputDraft::edited(text.clone(), &plugin_value, forwarded, report_mode(&body)),
            );
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

/// 插件正文链接点击：按节点声明回发 `linkClick`，同时走宿主默认的打开行为。
pub fn on_link_click(
    app: &mut App,
    plugin_id: String,
    surface_id: String,
    node_id: String,
    href: String,
    form: LinkForm,
) -> Task<Message> {
    let event = ViewEvent::LinkClick { href: href.clone(), form: protocol_link_form(form) };
    let task = on_view_event(app, plugin_id, surface_id, node_id, event);
    task.chain(app.open_link(&href, form))
}

/// 核心链接形态 → 协议链接形态（两端词汇一一对应）。
fn protocol_link_form(form: LinkForm) -> ProtocolLinkForm {
    match form {
        LinkForm::External => ProtocolLinkForm::External,
        LinkForm::Wiki => ProtocolLinkForm::Wiki,
        LinkForm::Path => ProtocolLinkForm::Path,
        LinkForm::Create => ProtocolLinkForm::Create,
        LinkForm::Plain => ProtocolLinkForm::Plain,
    }
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
    let mode = state.mode;
    let edited = matches!(action, text_editor::Action::Edit(_));
    state.content.perform(action);
    let text = state.content.text();

    if mode.is_deferred() {
        // 非逐键上报：输入过程只更新编辑器内容，静默一段后整段提交
        if !declares_change || !edited {
            return Task::none();
        }
        let delay = match mode {
            ReportMode::Debounced(ms) if ms > 0 => u64::from(ms),
            _ => EDITOR_COMMIT_MS,
        };
        let seq = app.plugin_draft_seq.entry(key).or_insert(0);
        *seq += 1;
        let seq = *seq;
        return Task::perform(
            tokio::time::sleep(std::time::Duration::from_millis(delay)),
            move |_| Message::PluginEditorFlush { plugin_id, surface_id, node_id, seq },
        );
    }

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
            interaction: None,
            grow: None,
            body: NodeBody::TextInput {
                value: value.into(),
                placeholder: None,
                multiline: None,
                password: None,
                read_only: None,
                report: None,
            },
            children: Vec::new(),
            events: vec![EventKind::Change],
        }
    }

    fn number_input(value: Option<f64>) -> NodeBody {
        NodeBody::NumberInput {
            value,
            min: Some(1.0),
            max: Some(10.0),
            step: None,
            placeholder: None,
            read_only: None,
            report: None,
        }
    }

    fn stepped_input(min: Option<f64>, max: Option<f64>, step: Option<f64>) -> NodeBody {
        NodeBody::NumberInput {
            value: Some(1.0),
            min,
            max,
            step,
            placeholder: None,
            read_only: None,
            report: None,
        }
    }

    #[test]
    fn step_uses_declared_step_and_clamps_bounds() {
        let body = stepped_input(Some(1.0), Some(5.0), Some(2.0));
        assert_eq!(stepped_number_text(&body, "1", true).as_deref(), Some("3"));
        assert_eq!(stepped_number_text(&body, "5", true).as_deref(), Some("5"));
        assert_eq!(stepped_number_text(&body, "1", false).as_deref(), Some("1"));
    }

    #[test]
    fn step_starts_from_min_for_empty_value() {
        let body = stepped_input(Some(2.5), None, Some(0.5));
        assert_eq!(stepped_number_text(&body, "", true).as_deref(), Some("2.5"));
        assert_eq!(stepped_number_text(&body, "  ", false).as_deref(), Some("2.5"));
    }

    #[test]
    fn step_defaults_to_one_and_formats_fraction() {
        let body = stepped_input(None, None, Some(0.25));
        assert_eq!(stepped_number_text(&body, "1", true).as_deref(), Some("1.25"));
        let body = stepped_input(None, None, Some(0.1));
        assert_eq!(stepped_number_text(&body, "0.2", true).as_deref(), Some("0.3"));
        let body = stepped_input(None, None, Some(0.0));
        assert_eq!(stepped_number_text(&body, "2.5", false).as_deref(), Some("1.5"));
    }

    #[test]
    fn step_ignores_pathological_values() {
        let body = stepped_input(None, None, Some(f64::INFINITY));
        assert_eq!(stepped_number_text(&body, "1", true).as_deref(), Some("2"));
        let body = stepped_input(Some(f64::MAX), None, Some(f64::MAX));
        assert_eq!(stepped_number_text(&body, &f64::MAX.to_string(), true), None);
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

    /// 非逐键上报：插件不回显也必须显示本机文本（逐键档位下这一条会退回插件值）。
    #[test]
    fn deferred_draft_shows_local_text_without_plugin_echo() {
        let draft = InputDraft::edited("xy".into(), "x", false, ReportMode::Commit);
        assert_eq!(input_display("x", Some(&draft)), "xy", "插件值停在 x，本机文本不得被回退");
        assert_eq!(input_display("", Some(&draft)), "xy", "插件值为空同样不打断输入");
    }

    /// 非逐键上报：插件值未变不打断输入，变了才接管并开启新会话。
    #[test]
    fn deferred_draft_adopts_only_fresh_plugin_values() {
        let mut draft = InputDraft::edited("xy".into(), "x", false, ReportMode::Commit);
        draft.observe("x");
        assert_eq!(input_display("x", Some(&draft)), "xy", "值未变：保持本机文本");
        draft.observe("z");
        assert_eq!(input_display("z", Some(&draft)), "z", "插件主动改值：接管");
        draft.observe("xy");
        assert_eq!(input_display("xy", Some(&draft)), "xy", "插件接受本机值：仍显示本机文本");
    }

    /// 逐键上报保持既有语义：不回显时返回插件值（这条锁住缺省行为不被改掉）。
    #[test]
    fn immediate_draft_still_compares_reported_value() {
        let draft = InputDraft::edited("xy".into(), "x", true, ReportMode::Immediate);
        assert_eq!(input_display("x", Some(&draft)), "x", "逐键档位以插件值为准");
    }

    /// 步进的草稿把「待上报」标为已上报：插件回显前逐键档位也显示本机步进值。
    #[test]
    fn step_draft_shows_local_value_before_plugin_echo() {
        let draft = InputDraft::edited("3".into(), "1", false, ReportMode::Immediate);
        assert_eq!(input_display("1", Some(&draft)), "3");
    }

    /// 草稿显示规则：插件回显本机刚发的值不改变显示，插件主动改值即接管。
    #[test]
    fn draft_display_prefers_local_until_plugin_overrides() {
        let draft = InputDraft::edited("xy".into(), "x", true, ReportMode::Immediate);
        assert_eq!(input_display("xy", Some(&draft)), "xy", "回显本机已上报值：显示本机文本");
        assert_eq!(input_display("z", Some(&draft)), "z", "插件改值：接管显示");
        assert_eq!(input_display("x", None), "x", "无草稿：显示插件值");
    }

    /// 未声明 change 的输入：已上报值保持插件现值，本机文本原地保留；插件改值仍接管。
    #[test]
    fn draft_without_forwarding_keeps_local_text() {
        let first = InputDraft::edited("a".into(), "", false, ReportMode::Immediate);
        assert_eq!(first.reported, "", "未回发时已上报值 = 插件现值");
        assert_eq!(input_display("", Some(&first)), "a");
        let second = InputDraft::edited("ab".into(), "", false, ReportMode::Immediate);
        assert_eq!(input_display("", Some(&second)), "ab");
        assert_eq!(input_display("z", Some(&second)), "z", "插件改值即接管");
    }

    #[test]
    fn number_input_parses_and_clamps() {
        let body = number_input(None);
        assert_eq!(number_event_value(&body, " 4 "), Some(EventValue::Number(4.0)));
        assert_eq!(number_event_value(&body, "0.5"), Some(EventValue::Number(1.0)));
        assert_eq!(number_event_value(&body, "99"), Some(EventValue::Number(10.0)));
        assert_eq!(number_event_value(&body, "   "), Some(EventValue::Null), "清空上报 null");
        for bad in ["abc", "1e999", "NaN"] {
            assert!(number_event_value(&body, bad).is_none(), "应不发事件：{bad}");
        }
    }

    #[test]
    fn number_text_and_value_agree_on_roundtrip() {
        let body = NodeBody::NumberInput {
            value: Some(3.5),
            min: None,
            max: None,
            step: None,
            placeholder: None,
            read_only: None,
            report: None,
        };
        assert_eq!(node_value_text(&body).as_deref(), Some("3.5"));
        assert_eq!(number_event_value(&body, "3.5"), Some(EventValue::Number(3.5)));
        let whole = NodeBody::NumberInput {
            value: Some(7.0),
            min: None,
            max: None,
            step: None,
            placeholder: None,
            read_only: None,
            report: None,
        };
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
            interaction: None,
            grow: None,
            body: NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
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
            interaction: None,
            grow: None,
            body: NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
            children: vec![
                text_input("single", "s"),
                ViewNode {
                    id: "multi".into(),
                    interaction: None,
                    grow: None,
                    body: NodeBody::TextInput {
                        value: "m".into(),
                        placeholder: None,
                        multiline: Some(true),
                        password: None,
                        read_only: None,
                        report: None,
                    },
                    children: Vec::new(),
                    events: vec![EventKind::Change],
                },
            ],
            events: Vec::new(),
        };
        let mut found = Vec::new();
        collect_multiline(&tree, &mut |id, value, _mode| found.push((id.to_string(), value)));
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
