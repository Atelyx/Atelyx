//! 壳内原生视图表：视图 id / 名称 / 图标与渲染函数。布局层只按 id 查注册表；
//! 渲染函数按 id 分派，单测锁住两张表 id 一一对应。

use iced::window;
use iced::Element;

use crate::registry::NativeView;
use crate::{App, Message};

/// 原生视图渲染函数签名。
type ViewProvider = for<'a> fn(&'a App, window::Id) -> Element<'a, Message>;

/// 壳内原生视图：顺序即选择器里视图的出现序。
pub(crate) const NATIVE_VIEWS: &[NativeView] = &[
    NativeView { id: "canvas", label: "画布", icon: "palette" },
    NativeView { id: "note", label: "笔记", icon: "file-text" },
    NativeView { id: "table", label: "表格", icon: "table" },
    NativeView { id: "files", label: "文件", icon: "files" },
    NativeView { id: "search", label: "搜索", icon: "search" },
    NativeView { id: "inspector", label: "属性", icon: "info" },
    NativeView { id: "aichat", label: "AI 对话", icon: "sparkles" },
    NativeView { id: "collabroom", label: "协作房间", icon: "users" },
    NativeView { id: "repohistory", label: "仓库历史", icon: "history" },
    NativeView { id: "recent", label: "最近打开", icon: "clock" },
    NativeView { id: "calendar", label: "日历", icon: "calendar-days" },
];

/// 视图 id → 渲染函数。与 [`NATIVE_VIEWS`] 的 id 对齐（单测锁住）。
const RENDERERS: &[(&str, ViewProvider)] = &[
    ("canvas", crate::canvas_view::canvas_view),
    ("note", crate::layout_view::note_view),
    ("table", crate::table_view::table_view),
    ("files", crate::layout_view::files_view),
    ("search", crate::layout_view::search_view),
    ("inspector", crate::layout_view::inspector_view),
    ("aichat", crate::chat_view::chat_view),
    ("collabroom", crate::layout_view::collab_room_view),
    ("repohistory", crate::repohistory_view::repohistory_view),
    ("recent", crate::recent_view::recent_view),
    ("calendar", crate::calendar_view::calendar_view),
];

/// 按 id 取原生视图内容；不属于原生表时 `None`（调用方落插件面或占位）。
pub(crate) fn render<'a>(
    id: &str,
    app: &'a App,
    window: window::Id,
) -> Option<Element<'a, Message>> {
    RENDERERS
        .iter()
        .find(|(view, _)| *view == id)
        .map(|(_, render)| render(app, window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 每条原生视图都有渲染函数，且没有指向不存在条目的渲染分派。
    #[test]
    fn native_view_entries_and_renderers_align() {
        let entry_ids: HashSet<&str> = NATIVE_VIEWS.iter().map(|view| view.id).collect();
        let renderer_ids: HashSet<&str> = RENDERERS.iter().map(|(id, _)| *id).collect();
        assert_eq!(entry_ids.len(), NATIVE_VIEWS.len(), "原生视图 id 重复");
        assert_eq!(renderer_ids.len(), RENDERERS.len(), "渲染分派 id 重复");
        for view in NATIVE_VIEWS {
            assert!(!view.id.is_empty(), "原生视图 id 不能为空");
            assert!(renderer_ids.contains(view.id), "原生视图 {} 没有渲染函数", view.id);
        }
        for (id, _) in RENDERERS {
            assert!(entry_ids.contains(id), "渲染分派 {id} 没有对应的视图条目");
        }
    }
}
