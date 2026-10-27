//! 装配根：宿主认识的全部功能模块。
//!
//! 加一个 Pack = 在 [`Packs`] 加一个字段，并在 `views` / `view` 各加一条分派。
//! 这里是唯一允许出现领域视图名的地方——布局层只按 id 查，不认识任何具体视图。

use atelyx_pack::{HostCtx, Pack, ViewDescriptor};
use iced::window;
use iced::Element;

use crate::layout_view::{collab_room_view, files_view, inspector_view, note_view, search_view};
use crate::{App, Message};

/// 内置视图渲染函数签名。
type ViewProvider = for<'a> fn(&'a App, window::Id) -> Element<'a, Message>;

/// 内置视图：元数据与渲染函数同一行，顺序即呈现序。
const BUILTIN_VIEWS: &[(ViewDescriptor, ViewProvider)] = &[
    (
        ViewDescriptor { id: "canvas", label: "画布", icon: "palette" },
        crate::canvas_view::canvas_view,
    ),
    (ViewDescriptor { id: "note", label: "笔记", icon: "file-text" }, note_view),
    (ViewDescriptor { id: "table", label: "表格", icon: "table" }, crate::table_view::table_view),
    (ViewDescriptor { id: "files", label: "文件", icon: "files" }, files_view),
    (ViewDescriptor { id: "search", label: "搜索", icon: "search" }, search_view),
    (ViewDescriptor { id: "inspector", label: "属性", icon: "info" }, inspector_view),
    (
        ViewDescriptor { id: "aichat", label: "AI 对话", icon: "sparkles" },
        crate::chat_view::chat_view,
    ),
    (ViewDescriptor { id: "collabroom", label: "协作房间", icon: "users" }, collab_room_view),
    (
        ViewDescriptor { id: "calendar", label: "日历", icon: "calendar-days" },
        crate::calendar_view::calendar_view,
    ),
    (
        ViewDescriptor { id: "repohistory", label: "仓库历史", icon: "history" },
        crate::repohistory_view::repohistory_view,
    ),
    (ViewDescriptor { id: "recent", label: "最近打开", icon: "clock" }, crate::recent_view::recent_view),
];

/// 内置功能模块：视图全部编译在壳内，`view` 只做 id → 渲染函数的分派。
struct BuiltinPack {
    /// 元数据从 [`BUILTIN_VIEWS`] 摊平，呈现序与表一致。
    views: Vec<ViewDescriptor>,
}

impl BuiltinPack {
    fn new() -> Self {
        Self { views: BUILTIN_VIEWS.iter().map(|(desc, _)| *desc).collect() }
    }
}

/// 宿主类型即 `HostCtx` 的实现者：内置 Pack 拿到的 `&App` 是它的能力面全集。
impl HostCtx for App {}

impl Pack<App> for BuiltinPack {
    type Msg = Message;

    fn views(&self) -> &[ViewDescriptor] {
        &self.views
    }

    fn view<'a>(
        &'a self,
        id: &str,
        host: &'a App,
        window: window::Id,
    ) -> Option<Element<'a, Message>> {
        BUILTIN_VIEWS.iter().find(|(desc, _)| desc.id == id).map(|(_, render)| render(host, window))
    }
}

/// 装配根：宿主向各模块取视图的唯一入口。
pub(crate) struct Packs {
    builtin: BuiltinPack,
}

impl Packs {
    pub(crate) fn new() -> Self {
        Self { builtin: BuiltinPack::new() }
    }

    /// 各模块贡献的视图元数据（按模块登记序拼接）。
    pub(crate) fn views(&self) -> &[ViewDescriptor] {
        self.builtin.views()
    }

    /// 按 id 取视图内容；不属于任何模块时 `None`（调用方落占位）。
    pub(crate) fn view<'a>(
        &'a self,
        id: &str,
        host: &'a App,
        window: window::Id,
    ) -> Option<Element<'a, Message>> {
        self.builtin.view(id, host, window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_pack::ViewRegistry;

    /// 内置视图 id 非空且唯一：装配期重复登记会被静默跳过（菜单里少一项），此处按数量锁住。
    #[test]
    fn builtin_views_register_cleanly() {
        let pack = BuiltinPack::new();
        let mut registry = ViewRegistry::default();
        for desc in pack.views() {
            registry.register(*desc).unwrap_or_else(|_| panic!("视图 id 重复或为空：{}", desc.id));
        }
        assert_eq!(registry.len(), pack.views().len());
    }
}
