//! 视图注册表：视图元数据（id / 显示名 / 图标）的唯一来源，登记顺序即呈现序。
//! 只放元数据——渲染函数要读壳状态，由壳侧按 id 分派。

/// 一条视图元数据。`id` 同时是布局标签、`ui-state.json` 与视图选择器里使用的标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

/// 内置视图（呈现序 = 标签菜单与视图选择器的顺序）。
const BUILTIN: &[ViewDescriptor] = &[
    ViewDescriptor { id: "canvas", label: "画布", icon: "palette" },
    ViewDescriptor { id: "note", label: "笔记", icon: "file-text" },
    ViewDescriptor { id: "table", label: "表格", icon: "table" },
    ViewDescriptor { id: "files", label: "文件", icon: "files" },
    ViewDescriptor { id: "search", label: "搜索", icon: "search" },
    ViewDescriptor { id: "inspector", label: "属性", icon: "info" },
    ViewDescriptor { id: "aichat", label: "AI 对话", icon: "sparkles" },
    ViewDescriptor { id: "collabroom", label: "协作房间", icon: "users" },
    ViewDescriptor { id: "calendar", label: "日历", icon: "calendar-days" },
    ViewDescriptor { id: "repohistory", label: "仓库历史", icon: "history" },
    ViewDescriptor { id: "recent", label: "最近打开", icon: "clock" },
];

/// 登记被拒的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// id 为空。
    EmptyId,
    /// id 已被先登记者占用（先登记者与既有顺序不变）。
    DuplicateId,
}

/// 视图注册表：id 唯一，登记顺序即呈现序。
#[derive(Debug, Default)]
pub struct ViewRegistry {
    views: Vec<ViewDescriptor>,
}

impl ViewRegistry {
    /// 内置视图注册表。
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        for desc in BUILTIN {
            // 内置条目 id 唯一性由 `builtin_views_are_all_registered` 锁定
            let _ = registry.register(*desc);
        }
        registry
    }

    /// 追加一条视图元数据。
    pub fn register(&mut self, desc: ViewDescriptor) -> Result<(), RegisterError> {
        if desc.id.is_empty() {
            return Err(RegisterError::EmptyId);
        }
        if self.get(desc.id).is_some() {
            return Err(RegisterError::DuplicateId);
        }
        self.views.push(desc);
        Ok(())
    }

    /// 按 id 查元数据；未登记为 `None`。
    pub fn get(&self, id: &str) -> Option<&ViewDescriptor> {
        self.views.iter().find(|desc| desc.id == id)
    }

    /// 按呈现序遍历。
    pub fn iter(&self) -> impl Iterator<Item = &ViewDescriptor> {
        self.views.iter()
    }

    pub fn len(&self) -> usize {
        self.views.len()
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置条目一条不落地进表：登记若被拒（重复 id）会静默少一项，此处按数量锁住。
    #[test]
    fn builtin_views_are_all_registered() {
        let builtin = ViewRegistry::builtin();
        assert_eq!(builtin.len(), BUILTIN.len());
        for desc in BUILTIN {
            assert_eq!(builtin.get(desc.id), Some(desc), "内置视图缺失：{}", desc.id);
        }
    }

    /// 登记顺序即呈现序（菜单与选择器依赖该顺序）。
    #[test]
    fn register_keeps_insertion_order() {
        let registry = ViewRegistry::builtin();
        let ids: Vec<&str> = registry.iter().map(|desc| desc.id).collect();
        let expected: Vec<&str> = BUILTIN.iter().map(|desc| desc.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn register_rejects_duplicate_and_empty_id() {
        let mut registry = ViewRegistry::builtin();
        let dup = ViewDescriptor { id: "note", label: "笔记副本", icon: "file-text" };
        assert_eq!(registry.register(dup), Err(RegisterError::DuplicateId));
        assert_eq!(registry.len(), BUILTIN.len(), "被拒的登记不得改变表");
        let empty = ViewDescriptor { id: "", label: "空", icon: "circle-x" };
        assert_eq!(registry.register(empty), Err(RegisterError::EmptyId));
    }

    #[test]
    fn register_appends_unknown_id() {
        let mut registry = ViewRegistry::builtin();
        let extra = ViewDescriptor { id: "timer", label: "计时器", icon: "clock" };
        assert_eq!(registry.register(extra), Ok(()));
        assert_eq!(registry.get("timer"), Some(&extra));
        assert_eq!(registry.iter().last(), Some(&extra));
    }

    #[test]
    fn unknown_id_lookup_is_none() {
        assert!(ViewRegistry::builtin().get("plugin:a:b").is_none());
    }
}
