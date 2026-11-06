//! 表格面板：头部行（宿主按插件声明渲染）+ 内容槽里的表格视图面。
//! 内容槽由「视图」下拉选择，缺省 = 表格插件的网格视图；表格数据与交互都在插件侧。

use iced::Element;
use iced::Length;

use crate::Message;

/// 表格面板内容。
pub fn table_view<'a>(app: &'a crate::App, window: iced::window::Id) -> Element<'a, Message> {
    let kind = app
        .panel_view
        .get("table")
        .cloned()
        .flatten()
        .unwrap_or_else(|| crate::plugin_actions::TABLES_VIEW.to_string());
    let body = crate::plugin_view::surface_view(app, window, &kind, "表格视图");
    // 头部归属键：表格插件按面板槽 id `table` 声明；第三方视图按自己的 kind 声明
    let header_target = if kind == crate::plugin_actions::TABLES_VIEW {
        "table"
    } else {
        kind.as_str()
    };
    match crate::panel_header::header_row(app, window, header_target, "table") {
        Some(header) => iced::widget::column![header, body]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => body,
    }
}
