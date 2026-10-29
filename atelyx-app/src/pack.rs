//! 内置视图：条目（id / 名称 / 图标）是注册表的输入数据，渲染函数按同一 id 分派。
//! 注册表条目是纯数据，渲染函数另立一张表（[`RENDERERS`]），单测锁住两张表 id 一一对应。
//! 布局层只按 id 查注册表；视图内部按 id 关联是渲染细节，不外泄到布局。

use iced::window;
use iced::Element;

use crate::registry::BuiltinView;
use crate::{App, Message};

/// 内置视图渲染函数签名。
type ViewProvider = for<'a> fn(&'a App, window::Id) -> Element<'a, Message>;

/// 内置视图条目：顺序即选择器与标签菜单的呈现序。
pub(crate) const BUILTIN_VIEWS: &[BuiltinView] = &[
    BuiltinView { id: "canvas", label: "画布", icon: "palette" },
    BuiltinView { id: "note", label: "笔记", icon: "file-text" },
    BuiltinView { id: "table", label: "表格", icon: "table" },
    BuiltinView { id: "files", label: "文件", icon: "files" },
    BuiltinView { id: "search", label: "搜索", icon: "search" },
    BuiltinView { id: "inspector", label: "属性", icon: "info" },
    BuiltinView { id: "aichat", label: "AI 对话", icon: "sparkles" },
    BuiltinView { id: "collabroom", label: "协作房间", icon: "users" },
    BuiltinView { id: "calendar", label: "日历", icon: "calendar-days" },
    BuiltinView { id: "repohistory", label: "仓库历史", icon: "history" },
    BuiltinView { id: "recent", label: "最近打开", icon: "clock" },
];

/// 内置视图 id → 渲染函数。与 [`BUILTIN_VIEWS`] 同 id 对齐（单测锁住）。
const RENDERERS: &[(&str, ViewProvider)] = &[
    ("canvas", crate::canvas_view::canvas_view),
    ("note", crate::layout_view::note_view),
    ("table", crate::table_view::table_view),
    ("files", crate::layout_view::files_view),
    ("search", crate::layout_view::search_view),
    ("inspector", crate::layout_view::inspector_view),
    ("aichat", crate::chat_view::chat_view),
    ("collabroom", crate::layout_view::collab_room_view),
    ("calendar", crate::calendar_view::calendar_view),
    ("repohistory", crate::repohistory_view::repohistory_view),
    ("recent", crate::recent_view::recent_view),
];

/// 按 id 取内置视图内容；不属于内置表时 `None`（调用方落占位）。
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

    /// 每条内置视图条目都有渲染函数，且没有指向不存在条目的渲染分派。
    #[test]
    fn builtin_view_entries_and_renderers_align() {
        let entry_ids: HashSet<&str> = BUILTIN_VIEWS.iter().map(|view| view.id).collect();
        let renderer_ids: HashSet<&str> = RENDERERS.iter().map(|(id, _)| *id).collect();
        assert_eq!(entry_ids.len(), BUILTIN_VIEWS.len(), "内置视图 id 重复");
        assert_eq!(renderer_ids.len(), RENDERERS.len(), "渲染分派 id 重复");
        for view in BUILTIN_VIEWS {
            assert!(!view.id.is_empty(), "内置视图 id 不能为空");
            assert!(renderer_ids.contains(view.id), "内置视图 {} 没有渲染函数", view.id);
        }
        for (id, _) in RENDERERS {
            assert!(entry_ids.contains(id), "渲染分派 {id} 没有对应的注册表条目");
        }
    }
}
