//! 打开文件集合：全部已打开文档实例的有序表（应用级唯一，运行时不落盘）。
//! 纯逻辑、无 I/O；容器（主要面板 / 类型槽 / 撕裂窗口）的呈现由壳侧按集合投影。

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

/// 实例宿主：主窗口（主要面板或类型槽投影）或某个撕裂窗口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    Main,
    Detached(String),
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
    pub host: Host,
}

/// 打开文件集合：有序项 + 激活项。
#[derive(Clone, Debug, Default)]
pub struct OpenFiles {
    items: Vec<DocInstance>,
    active: Option<InstanceId>,
}

impl OpenFiles {
    /// 打开路径：已在集合则激活并返回既有 id（不新建，kind 不变）；否则追加到尾部并激活。
    pub fn open(&mut self, path: &str, kind: DocKind) -> InstanceId {
        if let Some(existing) = self.items.iter().find(|d| d.path == path) {
            let id = existing.id.clone();
            self.active = Some(id.clone());
            return id;
        }
        let id = nanoid!();
        self.items.push(DocInstance { id: id.clone(), path: path.to_string(), kind, host: Host::Main });
        self.active = Some(id.clone());
        id
    }

    /// 关闭实例：被关闭的是激活项时按标签组同规转移（右邻 → 前邻 → None）。
    pub fn close(&mut self, id: &str) -> Option<DocInstance> {
        let index = self.items.iter().position(|d| d.id == id)?;
        let removed = self.items.remove(index);
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
            true
        } else {
            false
        }
    }

    /// 换宿主（实例不存在 = 忽略）。
    pub fn set_host(&mut self, id: &str, host: Host) {
        if let Some(item) = self.items.iter_mut().find(|d| d.id == id) {
            item.host = host;
        }
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

    /// 按类型与宿主过滤（集合顺序）。
    pub fn of_kind(&self, kind: &DocKind, host: &Host) -> Vec<&DocInstance> {
        self.items
            .iter()
            .filter(|d| &d.kind == kind && &d.host == host)
            .collect()
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
    fn of_kind_filters_by_kind_and_host() {
        let mut set = OpenFiles::default();
        set.open("a.md", note());
        set.open("b.md", note());
        set.open("c.atb", DocKind::Table);
        let detached = set.open("d.md", note());
        set.set_host(&detached, Host::Detached("w1".into()));
        let main_notes = set.of_kind(&note(), &Host::Main);
        assert_eq!(main_notes.len(), 2);
        assert_eq!(main_notes[0].path, "a.md");
        let detached_notes = set.of_kind(&note(), &Host::Detached("w1".into()));
        assert_eq!(detached_notes.len(), 1);
        assert_eq!(set.of_kind(&DocKind::Plugin("x".into()), &Host::Main).len(), 0);
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
}
