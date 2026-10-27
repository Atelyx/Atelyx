//! 视图注册表：视图元数据（id / 显示名 / 图标）的唯一索引，登记顺序即呈现序。
//! 只放元数据——渲染由 Pack 按 id 提供，注册表不认识任何具体视图。

/// 一条视图元数据。`id` 同时是布局标签、`ui-state.json` 与视图选择器里使用的标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

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

    fn desc(id: &'static str) -> ViewDescriptor {
        ViewDescriptor { id, label: "样例", icon: "circle" }
    }

    fn sample() -> ViewRegistry {
        let mut registry = ViewRegistry::default();
        for id in ["note", "table"] {
            registry.register(desc(id)).expect("样例 id 唯一");
        }
        registry
    }

    /// 登记顺序即呈现序（菜单与选择器依赖该顺序）。
    #[test]
    fn register_keeps_insertion_order() {
        let registry = sample();
        let ids: Vec<&str> = registry.iter().map(|d| d.id).collect();
        assert_eq!(ids, ["note", "table"]);
    }

    #[test]
    fn register_rejects_duplicate_and_empty_id() {
        let mut registry = sample();
        assert_eq!(registry.register(desc("note")), Err(RegisterError::DuplicateId));
        assert_eq!(registry.len(), 2, "被拒的登记不得改变表");
        assert_eq!(registry.register(desc("")), Err(RegisterError::EmptyId));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn register_appends_unknown_id() {
        let mut registry = sample();
        let extra = desc("timer");
        assert_eq!(registry.register(extra), Ok(()));
        assert_eq!(registry.get("timer"), Some(&extra));
        assert_eq!(registry.iter().last(), Some(&extra));
    }

    #[test]
    fn unknown_id_lookup_is_none() {
        assert!(ViewRegistry::default().get("plugin:a:b").is_none());
    }
}
