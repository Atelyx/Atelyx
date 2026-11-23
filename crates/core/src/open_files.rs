//! 打开文件集合：全部已打开文档实例的有序表（应用级唯一，运行时不落盘）。
//! 纯逻辑、无 I/O；主要面板与普通面板的类型槽由壳侧按集合投影，文档只在主窗口承载。

use nanoid::nanoid;

/// 文档实例 id（进程内唯一；同一路径全应用只有一项）。
pub type InstanceId = String;

/// 文档类型：内置三类 + 插件文档（按注册的视图 kind 区分）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DocKind {
    Note,
    Table,
    Canvas,
    Plugin(String),
}

impl DocKind {
    /// 该视图 kind 是否为文档类（内置三类 + 插件文档视图）。
    pub fn from_view(view: &str, plugin_doc: bool) -> Option<Self> {
        match view {
            "note" => Some(DocKind::Note),
            "table" => Some(DocKind::Table),
            "canvas" => Some(DocKind::Canvas),
            _ if plugin_doc => Some(DocKind::Plugin(view.to_string())),
            _ => None,
        }
    }
}

/// 一个已打开的文档实例。
#[derive(Clone, Debug, PartialEq)]
pub struct DocInstance {
    pub id: InstanceId,
    pub path: String,
    pub kind: DocKind,
}

/// 打开文件集合：有序项 + 激活项 + 最近激活序（类型槽回退用）。
#[derive(Clone, Debug, Default)]
pub struct OpenFiles {
    items: Vec<DocInstance>,
    active: Option<InstanceId>,
    /// 最近激活序（新在前的实例 id）：同类型多实例时「最近激活」的判定依据。
    activated: Vec<InstanceId>,
}

impl OpenFiles {
    /// 打开路径：已在集合则激活并返回既有 id（不新建，kind 不变）；否则追加到尾部并激活。
    pub fn open(&mut self, path: &str, kind: DocKind) -> InstanceId {
        if let Some(existing) = self.items.iter().find(|d| d.path == path) {
            let id = existing.id.clone();
            self.activate(&id);
            return id;
        }
        let id = nanoid!();
        self.items.push(DocInstance { id: id.clone(), path: path.to_string(), kind });
        self.active = Some(id.clone());
        self.touch(&id);
        id
    }

    /// 关闭实例：被关闭的是激活项时按标签组同规转移（右邻 → 前邻 → None）。
    pub fn close(&mut self, id: &str) -> Option<DocInstance> {
        let index = self.items.iter().position(|d| d.id == id)?;
        let removed = self.items.remove(index);
        self.activated.retain(|a| a != id);
        if self.active.as_deref() == Some(id) {
            self.active = self
                .items
                .get(index)
                .or_else(|| index.checked_sub(1).and_then(|i| self.items.get(i)))
                .map(|d| d.id.clone());
        }
        Some(removed)
    }

    /// 移除路径对应的实例（外部删除）：激活转移同 `close`。
    pub fn close_path(&mut self, path: &str) -> Option<DocInstance> {
        let id = self.find_by_path(path)?.id.clone();
        self.close(&id)
    }

    /// 激活实例（不存在返回 false）。
    pub fn activate(&mut self, id: &str) -> bool {
        if self.items.iter().any(|d| d.id == id) {
            self.active = Some(id.to_string());
            self.touch(id);
            true
        } else {
            false
        }
    }

    /// 记为最近激活（已在表首则不动）。
    fn touch(&mut self, id: &str) {
        self.activated.retain(|a| a != id);
        self.activated.insert(0, id.to_string());
    }

    /// 调整集合顺序（前移后移均按移除后下标处理；不存在 = 忽略）。
    pub fn reorder(&mut self, id: &str, to_index: usize) {
        let Some(from) = self.items.iter().position(|d| d.id == id) else { return };
        let moved = self.items.remove(from);
        let target = if from < to_index { to_index.saturating_sub(1) } else { to_index };
        self.items.insert(target.min(self.items.len()), moved);
    }

    /// 路径改名 / 移动后更新集合内路径（旧路径已不存在时忽略）。
    pub fn rename_path(&mut self, from: &str, to: &str) {
        if let Some(item) = self.items.iter_mut().find(|d| d.path == from) {
            item.path = to.to_string();
        }
    }

    /// 目录改名 / 移动后按前缀改写集合内路径（文件精确改写也走这里）。
    pub fn rename_prefix(&mut self, from: &str, to: &str) {
        for item in &mut self.items {
            if let Some(next) = crate::path::remap_prefix(&item.path, from, to) {
                item.path = next;
            }
        }
    }

    /// 按类型过滤（集合顺序）。
    pub fn of_kind(&self, kind: &DocKind) -> Vec<&DocInstance> {
        self.items.iter().filter(|d| &d.kind == kind).collect()
    }

    /// 该类型最近激活的实例（类型槽回退依据）。
    pub fn recent_of_kind(&self, kind: &DocKind) -> Option<&DocInstance> {
        self.activated.iter().find_map(|id| self.get(id).filter(|d| &d.kind == kind))
    }

    /// 最近激活的实例（跨类型；激活项缺失时的回落）。
    pub fn recent(&self) -> Option<&DocInstance> {
        self.activated.iter().find_map(|id| self.get(id))
    }

    /// 按路径查找（全集合唯一）。
    pub fn find_by_path(&self, path: &str) -> Option<&DocInstance> {
        self.items.iter().find(|d| d.path == path)
    }

    pub fn get(&self, id: &str) -> Option<&DocInstance> {
        self.items.iter().find(|d| d.id == id)
    }

    pub fn active(&self) -> Option<&DocInstance> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// 集合内的路径清单（引用同步 / 退出关闭等批量处理用）。
    pub fn items(&self) -> &[DocInstance] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.active = None;
        self.activated.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> DocKind {
        DocKind::Note
    }

    #[test]
    fn open_same_path_is_single_instance() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        let b = set.open("a.md", note());
        assert_eq!(a, b);
        assert_eq!(set.len(), 1);
        assert_eq!(set.active_id(), Some(a.as_str()));
    }

    #[test]
    fn open_appends_tail_and_activates() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        let b = set.open("b.atb", DocKind::Table);
        assert_ne!(a, b);
        assert_eq!(set.items().iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), vec!["a.md", "b.atb"]);
        assert_eq!(set.active_id(), Some(b.as_str()));
        // 重新打开既有项只激活，不改变顺序
        set.open("a.md", note());
        assert_eq!(set.items()[0].path, "a.md");
    }

    #[test]
    fn rename_prefix_rewrites_nested_items() {
        let mut set = OpenFiles::default();
        set.open("dir/a.md", note());
        set.open("dir/sub/b.atb", DocKind::Table);
        set.open("other.md", note());
        set.rename_prefix("dir", "docs");
        let paths: Vec<&str> = set.items().iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["docs/a.md", "docs/sub/b.atb", "other.md"]);
    }

    #[test]
    fn close_active_moves_to_right_neighbor_then_left_then_none() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        let b = set.open("b.md", note());
        let c = set.open("c.md", note());
        set.activate(&b);
        set.close(&b);
        assert_eq!(set.active_id(), Some(c.as_str()));
        // 关掉末尾激活项 → 前邻
        set.close(&c);
        assert_eq!(set.active_id(), Some(a.as_str()));
        // 全部关完 → None
        set.close(&a);
        assert_eq!(set.active_id(), None);
        assert!(set.is_empty());
    }

    #[test]
    fn closing_inactive_keeps_active() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        let _b = set.open("b.md", note());
        set.activate(&a);
        set.close(&_b);
        assert_eq!(set.active_id(), Some(a.as_str()));
    }

    #[test]
    fn of_kind_filters_by_kind() {
        let mut set = OpenFiles::default();
        set.open("a.md", note());
        set.open("b.md", note());
        set.open("c.atb", DocKind::Table);
        let notes = set.of_kind(&note());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].path, "a.md");
        assert_eq!(set.of_kind(&DocKind::Plugin("x".into())).len(), 0);
    }

    #[test]
    fn reorder_moves_item() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        set.open("b.md", note());
        set.open("c.md", note());
        set.reorder(&a, 3);
        assert_eq!(set.items().iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), vec!["b.md", "c.md", "a.md"]);
        set.reorder(&a, 0);
        assert_eq!(set.items()[0].path, "a.md");
    }

    #[test]
    fn rename_and_close_path() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        set.rename_path("a.md", "dir/a.md");
        assert_eq!(set.find_by_path("a.md"), None);
        assert_eq!(set.find_by_path("dir/a.md").map(|d| d.id.as_str()), Some(a.as_str()));
        set.close_path("dir/a.md");
        assert!(set.is_empty());
    }

    #[test]
    fn recent_of_kind_follows_activation() {
        let mut set = OpenFiles::default();
        let a = set.open("a.md", note());
        let b = set.open("b.md", note());
        assert_eq!(set.recent_of_kind(&note()).map(|d| d.path.as_str()), Some("b.md"));
        set.activate(&a);
        assert_eq!(set.recent_of_kind(&note()).map(|d| d.path.as_str()), Some("a.md"));
        // 关闭最近项 → 回退到剩下的最近项；全部关闭 → None
        set.close(&a);
        assert_eq!(set.recent_of_kind(&note()).map(|d| d.path.as_str()), Some("b.md"));
        set.close(&b);
        assert!(set.recent_of_kind(&note()).is_none());
        assert!(set.recent().is_none());
    }
}
