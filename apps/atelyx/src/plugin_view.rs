//! 插件面渲染适配：把这一面最近一拍的帧交给 [`crate::plugin_frame`]，插件不可用时给可读占位。
//! 面外壳内边距取自贡献声明（缺省贴边），本模块不参与布局与命中判定。

use iced::{Element, Padding};

use atelyx_core::plugin_runtime::PaddingSpec;

use crate::App;
use crate::Message;
use crate::plugin_actions::contribution;
use crate::ui::controls;

/// 插件面板内容：交出这一面最近一拍的帧；贡献缺失时给可读占位。
pub fn panel_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件面板")
}

/// 设置页里的插件面（tab 内容）：与面板同一套帧渲染，仅占位文案不同。
pub fn settings_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件设置")
}

/// 插件窗口内容：独立 OS 窗口承载的面（无面板内边距，铺满窗口）。
pub fn window_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    entry: &crate::plugin_actions::PluginWindowActive,
) -> Element<'a, Message> {
    // 部件必须无条件构造：帧是它在宿主重绘拍里画出来的，拿帧当构造前提就永远等不到第一帧
    let frame = app
        .plugins
        .surface_frame(&entry.plugin_id, &entry.window_key);
    crate::plugin_frame::frame_view(
        app,
        window,
        &entry.plugin_id,
        &entry.window_key,
        frame,
        Padding::ZERO,
    )
}

/// 面内容渲染（面板 / 表格视图共用）：按壳侧视图 kind 取插件贡献与帧。
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
    let outer_pad = contribution.padding.map(padding_of).unwrap_or(Padding::ZERO);
    // 部件必须无条件构造：帧是它在宿主重绘拍里画出来的，拿帧当构造前提就永远等不到第一帧
    let frame = app.plugins.surface_frame(plugin_id, surface_id);
    crate::plugin_frame::frame_view(app, window, plugin_id, surface_id, frame, outer_pad)
}

/// 面外壳内边距 → iced 内边距（上 / 右 / 下 / 左）。
pub(crate) fn padding_of(spec: PaddingSpec) -> Padding {
    let [top, right, bottom, left] = spec.edges();
    Padding { top, right, bottom, left }
}

fn placeholder<'a>(icon_name: &str, title: &str, desc: &str) -> Element<'a, Message> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}
