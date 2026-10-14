//! 插件界面面的状态与接线：壳侧视图 kind 编解码、贡献查询、输入草稿与多行编辑器、面树的投影刷新、
//! 界面图片按需载入、节点事件回发。
//!
//! 贡献与面树的投影缓存在 `App`（`plugin_contributions` / `plugin_surfaces` / `plugin_trees`），
//! 由 [`refresh_projection`] 随运行时事件刷新；渲染在 [`crate::plugin_view`]。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use iced::widget::text_editor;
use iced::Task;

use atelyx_core::layout::{self, LayoutNode};
use atelyx_core::plugin_runtime::{
    Contribution, ContributionSlot, EventKind, EventValue, ImageSource, NodeBody, SurfaceKind,
    ViewEvent, ViewNode,
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
    match crate::ui::icon::icon_name(name) {
        "" => "puzzle",
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

/// 树里按 id 找节点（草稿维护与事件处理共用）。
pub fn find_node<'a>(tree: &'a ViewNode, node_id: &str) -> Option<&'a ViewNode> {
    if tree.id == node_id {
        return Some(tree);
    }
    tree.children.iter().find_map(|child| find_node(child, node_id))
}

/// 事件是否由节点声明（未声明不上行）。
pub(crate) fn declares(node: &ViewNode, kind: EventKind) -> bool {
    node.events.contains(&kind)
}

/// 事件对应的声明种类（上行闸门按它判定）。
fn event_kind(event: &ViewEvent) -> EventKind {
    match event {
        ViewEvent::Click => EventKind::Click,
        ViewEvent::Change { .. } => EventKind::Change,
        ViewEvent::Submit { .. } => EventKind::Submit,
        ViewEvent::Focus => EventKind::Focus,
        ViewEvent::Blur => EventKind::Blur,
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

/// 把贡献与已挂面的树投影进壳状态，并按需发起界面图片载入（返回的任务由调用方链出）。
pub fn refresh_projection(app: &mut App) -> Task<Message> {
    let contributions = app.plugins.contributions();
    app.plugin_contributions = contributions;
    let surfaces: Vec<(String, String)> = app.plugin_surfaces.iter().cloned().collect();
    for (plugin_id, surface_id) in surfaces {
        let key = (plugin_id.clone(), surface_id.clone());
        match app.plugins.surface_tree(&plugin_id, &surface_id) {
            Some(tree) => {
                sync_editors(app, &plugin_id, &surface_id, &tree);
                prune_node_state(app, &plugin_id, &surface_id, &tree);
                app.plugin_trees.insert(key, tree);
            }
            None => {
                app.plugin_trees.remove(&key);
                drop_node_state(app, &plugin_id, &surface_id);
            }
        }
    }
    ensure_images(app)
}

/// 撤掉某面的本地草稿、多行编辑器与树投影（面移除 / 插件停用）。
pub fn drop_node_state(app: &mut App, plugin_id: &str, surface_id: &str) {
    app.plugin_trees
        .remove(&(plugin_id.to_string(), surface_id.to_string()));
    app.plugin_drafts
        .retain(|key, _| !(key.0 == plugin_id && key.1 == surface_id));
    app.plugin_editors
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
    if !events.contains(&event_kind(&event)) {
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
        assert_eq!(event_kind(&ViewEvent::Submit { value: EventValue::Text("v".into()) }), EventKind::Submit);
        assert_eq!(event_kind(&ViewEvent::Click), EventKind::Click);
    }
}
