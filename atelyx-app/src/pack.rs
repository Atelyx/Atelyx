//! 内置插件：预置插件表（id / 名称 / 图标 / 视图贡献）是注册表的输入数据，
//! 渲染函数按视图 id 分派。渲染函数另立一张表（[`RENDERERS`]），单测锁住两张表 id 一一对应。
//! 布局层只按 id 查注册表；视图内部按 id 关联是渲染细节，不外泄到布局。

use iced::window;
use iced::Element;

use crate::registry::{BuiltinPlugin, BuiltinView};
use crate::{App, Message};

/// 预置插件视图渲染函数签名。
type ViewProvider = for<'a> fn(&'a App, window::Id) -> Element<'a, Message>;

/// 预置插件：顺序即选择器里视图的出现序（组内按声明序）。
pub(crate) const BUILTIN_PLUGINS: &[BuiltinPlugin] = &[
    BuiltinPlugin {
        id: "atelyx.canvas",
        label: "画布",
        icon: "palette",
        views: &[BuiltinView { id: "canvas", label: "画布", icon: "palette" }],
    },
    BuiltinPlugin {
        id: "atelyx.notes",
        label: "笔记",
        icon: "file-text",
        views: &[BuiltinView { id: "note", label: "笔记", icon: "file-text" }],
    },
    BuiltinPlugin {
        id: "atelyx.tables",
        label: "表格",
        icon: "table",
        views: &[BuiltinView { id: "table", label: "表格", icon: "table" }],
    },
    BuiltinPlugin {
        id: "atelyx.files",
        label: "文件",
        icon: "files",
        views: &[BuiltinView { id: "files", label: "文件", icon: "files" }],
    },
    BuiltinPlugin {
        id: "atelyx.search",
        label: "搜索",
        icon: "search",
        views: &[BuiltinView { id: "search", label: "搜索", icon: "search" }],
    },
    BuiltinPlugin {
        id: "atelyx.inspector",
        label: "属性",
        icon: "info",
        views: &[BuiltinView { id: "inspector", label: "属性", icon: "info" }],
    },
    BuiltinPlugin {
        id: "atelyx.ai",
        label: "AI 对话",
        icon: "sparkles",
        views: &[BuiltinView { id: "aichat", label: "AI 对话", icon: "sparkles" }],
    },
    BuiltinPlugin {
        id: "atelyx.collab",
        label: "协作房间",
        icon: "users",
        views: &[BuiltinView { id: "collabroom", label: "协作房间", icon: "users" }],
    },
    BuiltinPlugin {
        id: "atelyx.history",
        label: "仓库历史",
        icon: "history",
        views: &[
            BuiltinView { id: "repohistory", label: "仓库历史", icon: "history" },
            BuiltinView { id: "recent", label: "最近打开", icon: "clock" },
        ],
    },
    BuiltinPlugin {
        id: atelyx_core::plugin_runtime::PLATFORM_PLUGIN,
        label: "平台基础",
        icon: "layers",
        views: &[],
    },
];

/// 预置插件视图 id → 渲染函数。与 [`BUILTIN_PLUGINS`] 的视图 id 对齐（单测锁住）。
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
];

/// 按 id 取预置插件视图内容；不属于内置表时 `None`（调用方落占位）。
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

    fn builtin_view_ids() -> HashSet<&'static str> {
        BUILTIN_PLUGINS
            .iter()
            .flat_map(|plugin| plugin.views.iter().map(|view| view.id))
            .collect()
    }

    /// 每条预置插件视图条目都有渲染函数，且没有指向不存在条目的渲染分派。
    #[test]
    fn builtin_view_entries_and_renderers_align() {
        let entry_ids = builtin_view_ids();
        let renderer_ids: HashSet<&str> = RENDERERS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            entry_ids.len(),
            BUILTIN_PLUGINS.iter().map(|plugin| plugin.views.len()).sum::<usize>(),
            "预置插件视图 id 重复"
        );
        assert_eq!(renderer_ids.len(), RENDERERS.len(), "渲染分派 id 重复");
        for view in BUILTIN_PLUGINS.iter().flat_map(|plugin| plugin.views) {
            assert!(!view.id.is_empty(), "预置插件视图 id 不能为空");
            assert!(renderer_ids.contains(view.id), "预置插件视图 {} 没有渲染函数", view.id);
        }
        for (id, _) in RENDERERS {
            assert!(entry_ids.contains(id), "渲染分派 {id} 没有对应的注册表条目");
        }
    }

    /// 内置服务声明的归属插件必须真实存在（或就是平台自身）：归属写错会让服务永远不可用。
    #[test]
    fn every_builtin_service_owner_is_a_declared_plugin() {
        use atelyx_core::plugin_runtime::{BUILTIN_SERVICES, PLATFORM_PLUGIN};
        let ids: HashSet<&str> = BUILTIN_PLUGINS.iter().map(|plugin| plugin.id).collect();
        assert!(ids.contains(PLATFORM_PLUGIN));
        for (service, owner, _) in BUILTIN_SERVICES {
            assert!(ids.contains(owner), "服务 {service} 的归属插件 {owner} 未声明");
        }
    }
}
