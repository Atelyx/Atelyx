//! 插件面渲染适配：按 surfaceId 取整面帧交给 [`crate::plugin_frame`]，未提交 / 插件不可用时给
//! 可读占位。面外壳内边距取自贡献声明（缺省用宿主默认），本模块不参与布局与命中判定。

use iced::{Element, Padding};

use atelyx_core::plugin_runtime::PaddingSpec;

use crate::App;
use crate::Message;
use crate::plugin_actions::contribution;
use crate::ui::controls;

/// 插件面板内容：取投影缓存里的帧渲染；贡献缺失 / 帧未提交时给可读占位。
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
    let key = (entry.plugin_id.clone(), entry.window_key.clone());
    match app.plugin_frames.get(&key) {
        Some(frame) => crate::plugin_frame::frame_view(
            app,
            window,
            &entry.plugin_id,
            &entry.window_key,
            frame,
            Padding::ZERO,
        ),
        None => placeholder("puzzle", "插件窗口", "等待插件提交界面（插件未运行或尚未就绪）"),
    }
}

/// 接管面内容：与面板同一套帧渲染，无内边距铺满可用区域（接管态由 layout_view 组装）。
pub fn takeover_content<'a>(
    app: &'a App,
    window: iced::window::Id,
    plugin_id: &str,
    surface_id: &str,
) -> Element<'a, Message> {
    let key = (plugin_id.to_string(), surface_id.to_string());
    match app.plugin_frames.get(&key) {
        Some(frame) => {
            crate::plugin_frame::frame_view(app, window, plugin_id, surface_id, frame, Padding::ZERO)
        }
        None => placeholder("puzzle", "接管面", "等待插件提交界面"),
    }
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
    let key = (plugin_id.to_string(), surface_id.to_string());
    let outer_pad = match contribution.padding {
        Some(pad) => padding_of(pad),
        None => Padding::ZERO.horizontal(12.0).vertical(10.0),
    };
    match app.plugin_frames.get(&key) {
        Some(frame) => {
            crate::plugin_frame::frame_view(app, window, plugin_id, surface_id, frame, outer_pad)
        }
        None => {
            let icon_key =
                crate::plugin_actions::plugin_icon_key(contribution.icon.as_deref(), "puzzle");
            placeholder(&icon_key, &contribution.label, "等待插件提交界面（插件未运行或尚未就绪）")
        }
    }
}

/// 面外壳内边距 → iced 内边距（上 / 右 / 下 / 左）。
pub(crate) fn padding_of(spec: PaddingSpec) -> Padding {
    let [top, right, bottom, left] = spec.edges();
    Padding { top, right, bottom, left }
}

fn placeholder<'a>(icon_name: &str, title: &str, desc: &str) -> Element<'a, Message> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}
