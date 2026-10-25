//! 插件面渲染适配：面取树、空态占位与宿主状态读取在此，树解释与控件绑定在 [`atelyx_view::render`]。
//! 宿主经 [`ViewHost`] 提供节点瞬时状态与外部元素（Markdown / 图片 / 画布），经 [`ViewMessage`]
//! 把渲染期消息接回壳的消息类型；本模块不参与布局与命中判定。

use std::borrow::Cow;

use iced::widget::text;
use iced::{Element, Padding};

use atelyx_core::plugin_runtime::ImageSource;
use atelyx_view::render::{
    self, padding_of, CanvasRequest, MarkdownRequest, NodeKey, NodeState, StateNeed, ViewHost,
    ViewMessage, ViewMsg,
};

use crate::plugin_actions::{contribution, image_key, input_display};
use crate::ui::controls;
use crate::{App, Message};

/// 插件面板内容：取投影缓存里的树渲染；贡献缺失 / 树未提交时给可读占位。
pub fn panel_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件面板")
}

/// 设置页里的插件面（tab 内容）：与面板同一套面树渲染，仅占位文案不同。
pub fn settings_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件设置")
}

/// 插件窗口内容：独立 OS 窗口承载的面树（无面板内边距，铺满窗口）。
pub fn window_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    entry: &crate::plugin_actions::PluginWindowActive,
) -> Element<'a, Message> {
    let key = (entry.plugin_id.clone(), entry.window_key.clone());
    match app.plugin_trees.get(&key) {
        Some(tree) => render::surface(
            app,
            window,
            &entry.plugin_id,
            &entry.window_key,
            tree,
            Padding::ZERO,
        ),
        None => placeholder(
            "puzzle",
            "插件窗口",
            "等待插件提交界面（插件未运行或尚未就绪）",
        ),
    }
}

/// 接管面内容：与面板同一套面树渲染，无内边距铺满可用区域（接管态由 layout_view 组装）。
pub fn takeover_content<'a>(
    app: &'a App,
    window: iced::window::Id,
    plugin_id: &str,
    surface_id: &str,
) -> Element<'a, Message> {
    let key = (plugin_id.to_string(), surface_id.to_string());
    match app.plugin_trees.get(&key) {
        Some(tree) => {
            render::surface(app, window, plugin_id, surface_id, tree, Padding::ZERO)
        }
        None => placeholder("puzzle", "接管面", "等待插件提交界面"),
    }
}

/// 面内容解释渲染（面板 / 表格视图共用）：按壳侧视图 kind 取插件贡献与树。
pub(crate) fn surface_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    view: &str,
    kind_label: &str,
) -> Element<'a, Message> {
    let Some((plugin_id, surface_id)) = crate::plugin_actions::parse_view_kind(view) else {
        return placeholder("puzzle", kind_label, "视图 kind 非法");
    };
    let Some(contribution) = contribution(app, plugin_id, surface_id) else {
        return placeholder(
            "puzzle",
            &format!("{kind_label} {plugin_id}"),
            "插件不可用：未安装、未启用或已卸载",
        );
    };
    let key = (plugin_id.to_string(), surface_id.to_string());
    let outer_pad = match contribution.padding {
        Some(pad) => padding_of(pad),
        None => Padding::ZERO.horizontal(12.0).vertical(10.0),
    };
    match app.plugin_trees.get(&key) {
        Some(tree) => {
            render::surface(app, window, plugin_id, surface_id, tree, outer_pad)
        }
        None => {
            let icon_key =
                crate::plugin_actions::plugin_icon_key(plugin_id, contribution.icon.as_deref(), "puzzle");
            placeholder(
                &icon_key,
                &contribution.label,
                "等待插件提交界面（插件未运行或尚未就绪）",
            )
        }
    }
}

fn placeholder<'a>(icon_name: &str, title: &str, desc: &str) -> Element<'a, Message> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}

/// 渲染期消息 → 壳消息（面身份与节点 id 已由渲染层填好）。
impl ViewMessage for Message {
    fn view(msg: ViewMsg<Self>) -> Self {
        match msg {
            ViewMsg::Event { plugin_id, surface_id, node_id, event } => {
                Message::PluginView { plugin_id, surface_id, node_id, event }
            }
            ViewMsg::Draft { plugin_id, surface_id, node_id, text, commit } => {
                Message::PluginDraft { plugin_id, surface_id, node_id, text, commit }
            }
            ViewMsg::Step { plugin_id, surface_id, node_id, up } => {
                Message::PluginStep { plugin_id, surface_id, node_id, up }
            }
            ViewMsg::TextEdit { plugin_id, surface_id, node_id, action } => {
                Message::PluginTextEdit { plugin_id, surface_id, node_id, action }
            }
            ViewMsg::EditorSubmit { plugin_id, surface_id, node_id } => {
                Message::PluginEditorSubmit { plugin_id, surface_id, node_id }
            }
            ViewMsg::Hover { plugin_id, surface_id, node_id, entered } => {
                Message::PluginHover { plugin_id, surface_id, node_id, entered }
            }
            ViewMsg::Focus { window, plugin_id, surface_id, node_id, focused } => {
                Message::PluginFocus { window, plugin_id, surface_id, node_id, focused }
            }
            ViewMsg::WindowDrag(window) => Message::PluginWindowDrag(window),
            ViewMsg::OpenList { window, entries } => Message::PluginList { window, entries },
        }
    }
}

/// 节点状态键（存储侧按三元组索引）。
fn state_key(plugin_id: &str, surface_id: &str, node_id: &str) -> (String, String, String) {
    (plugin_id.to_string(), surface_id.to_string(), node_id.to_string())
}

impl ViewHost<Message> for App {
    fn node_state(&self, key: &NodeKey<'_>, need: StateNeed<'_>) -> NodeState<'_> {
        let empty = NodeState { hovered: false, focused: false, text: None, editor: None };
        let key = state_key(key.plugin_id, key.surface_id, key.node_id);
        match need {
            StateNeed::Interaction => NodeState {
                hovered: self.plugin_hovered.contains(&key),
                focused: self.plugin_focus.as_ref().is_some_and(|focus| {
                    focus.plugin_id == key.0 && focus.surface_id == key.1 && focus.node_id == key.2
                }),
                ..empty
            },
            StateNeed::TextInput { plugin_value } => NodeState {
                text: self
                    .plugin_drafts
                    .get(&key)
                    .map(|draft| Cow::Owned(input_display(plugin_value, Some(draft)))),
                ..empty
            },
            StateNeed::Multiline => NodeState {
                editor: self.plugin_editors.get(&key).map(|editor| &editor.content),
                ..empty
            },
        }
    }

    fn markdown<'a>(&'a self, request: MarkdownRequest<'_>) -> Element<'a, Message> {
        let key = state_key(request.plugin_id, request.surface_id, request.node_id);
        match self.plugin_markdown.get(&key) {
            Some(entry) => crate::markdown_view::document_body_styled(
                &entry.doc,
                &self.theme,
                &self.note_images,
                self.global.soft_wrap,
                request.scale,
                crate::markdown_view::BodyStyle {
                    font: request.font,
                    color: request.color,
                    link_scope: Some(crate::markdown_view::PluginLinkScope {
                        plugin_id: request.plugin_id.to_string(),
                        surface_id: request.surface_id.to_string(),
                        node_id: request.node_id.to_string(),
                    }),
                },
            ),
            None => text("").into(),
        }
    }

    fn image(&self, plugin_id: &str, source: &ImageSource) -> Option<iced::widget::image::Handle> {
        image_key(self, plugin_id, source).and_then(|key| self.note_images.get(&key).cloned())
    }

    fn canvas<'a>(&'a self, request: CanvasRequest<'_>) -> Element<'a, Message> {
        let key = state_key(request.plugin_id, request.surface_id, request.node_id);
        crate::plugin_canvas::PluginCanvas {
            app: self,
            window: request.window,
            plugin_id: request.plugin_id.to_string(),
            surface_id: request.surface_id.to_string(),
            node_id: request.node_id.to_string(),
            list: self.plugin_canvases.get(&key),
            mouse_events: request.mouse_events.to_vec(),
        }
        .into()
    }
}

