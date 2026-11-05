//! 面板状态：宿主给的界面状态快照 + 按展开集摊平出的可见行 + 就地编辑与拖拽的瞬时态。
//! 插件不持磁盘真源：目录条目、排序、目录颜色与提示词标记都由宿主给，写回也经宿主。

use std::collections::HashMap;

use atelyx_plugin_protocol::method::{ContainerEntry, Radii, UiDirEntry, VaultUiState};

/// 一条可见行（树摊平后的结果）。
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// 仓库相对路径。
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// 缩进层数（仓库根下的条目不缩进）。
    pub depth: usize,
    /// 目录是否展开。
    pub expanded: bool,
    /// 目录的图标颜色（hex）。
    pub color: Option<String>,
    /// 当前打开的文件（强调色高亮）。
    pub active: bool,
    /// 该行是拖拽落点（高亮底色）。
    pub drop_target: bool,
    /// 落点被拒（警示底色）。
    pub drop_rejected: bool,
    /// 条目图标名（插件注册的文档类型给；`None` = 按扩展名兜底）。
    pub icon: Option<String>,
}

/// 就地输入会话（重命名 / 新建草稿行）。
#[derive(Clone, Debug, PartialEq)]
pub enum InlineEdit {
    /// 重命名一个已存在条目。
    Rename { path: String, is_dir: bool, value: String },
    /// 在某个目录后新建一行草稿。
    Draft { dir: String, kind: DraftKind, value: String },
}

impl InlineEdit {
    pub fn value(&self) -> &str {
        match self {
            InlineEdit::Rename { value, .. } | InlineEdit::Draft { value, .. } => value,
        }
    }

    pub fn set_value(&mut self, next: String) {
        match self {
            InlineEdit::Rename { value, .. } | InlineEdit::Draft { value, .. } => *value = next,
        }
    }
}

/// 新建草稿的类型。
#[derive(Clone, Debug, PartialEq)]
pub enum DraftKind {
    Note,
    Table,
    Canvas,
    Folder,
    Doc(String),
}

impl DraftKind {
    /// 交给宿主的 `vault.create` 类型串。
    pub fn wire(&self) -> &str {
        match self {
            DraftKind::Note => "note",
            DraftKind::Table => "table",
            DraftKind::Canvas => "canvas",
            DraftKind::Folder => "folder",
            DraftKind::Doc(extension) => extension,
        }
    }

    /// 菜单里的标签。
    pub fn label(&self) -> String {
        match self {
            DraftKind::Note => "新建笔记".into(),
            DraftKind::Table => "新建表格".into(),
            DraftKind::Canvas => "新建画布".into(),
            DraftKind::Folder => "新建文件夹".into(),
            DraftKind::Doc(extension) => format!("新建 .{extension}"),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            DraftKind::Note => "sticky-note",
            DraftKind::Table => "table",
            DraftKind::Canvas => "palette",
            DraftKind::Folder => "folder-plus",
            DraftKind::Doc(_) => "file-text",
        }
    }
}

/// 容器列表里的一行（本地仓库 / 协作空间；激活行就地展开文件树）。
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerRow {
    /// [`ContainerEntry::id`] 的原值（切换时原样交回宿主）。
    pub id: String,
    pub label: String,
    /// 本地仓库 = 硬盘图标，协作空间 = 云图标。
    pub is_local: bool,
    /// 该容器是当前激活容器。
    pub active: bool,
    /// 激活容器的文件树是否展开。
    pub open: bool,
    /// 悬停提示：本地给仓库根路径，空间给「空间名 · 服务器」。
    pub hint: String,
    pub drop_target: bool,
    pub drop_rejected: bool,
}

/// 插件注册的文档类型（扩展名不带点）：菜单条目与文件树条目的图标都取自这里。
#[derive(Clone, Debug, PartialEq)]
pub struct DocType {
    pub extension: String,
    pub label: String,
    /// 宿主图标形状名（空 = 按扩展名兜底）。
    pub icon: String,
}

/// 一帧要画的内容（与 [`Model`] 解耦，渲染期间不持锁）。
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub error: Option<String>,
    /// 上一次操作失败的可读原因（下一次操作开始时清掉）。
    pub notice: Option<String>,
    pub rows: Vec<Row>,
    /// 容器列表（全部最近仓库 + 全部空间，激活项在前）。
    pub containers: Vec<ContainerRow>,
    /// 「展开全部」按钮当前状态（有任一展开即显示「收起全部」）。
    pub any_expanded: bool,
    /// 当前容器是否可用（未进入仓库时给空态）。
    pub in_repo: bool,
    /// 首份状态（或切容器后的首份）尚未取到：只画加载态，不判定空态。
    pub loading: bool,
    pub vault_drop_target: bool,
    pub vault_drop_rejected: bool,
    /// 就地输入会话与它的插入位置（`Draft` 时为该行之后；重命名时被替换行的下标）。
    pub inline: Option<(InlineEdit, usize)>,
    /// 拖拽会话进行中（ghost 由宿主画，插件只画落点高亮）。
    pub dragging: bool,
    /// 拖拽落点被拒的可读原因。
    pub drop_hint: Option<String>,
    /// 排序键（菜单勾选态）。
    pub sort: String,
}

/// 面板状态。
#[derive(Clone)]
pub struct Model {
    expanded: Vec<String>,
    state: VaultUiState,
    containers: Vec<ContainerEntry>,
    error: Option<String>,
    ready: bool,
    /// 拖拽落点（宿主回推）。
    drag_target: Option<String>,
    drag_reject: Option<String>,
    /// 拖拽会话进行中（宿主已确认发起）。
    dragging: bool,
    /// 当前打开的文件集合（笔记 / 表格 / 画布）：命中行给强调色。
    active_files: Vec<String>,
    inline: Option<InlineEdit>,
    /// 圆角档取自宿主主题。
    pub radii: Radii,
    /// 插件注册的文档类型（扩展名 + 菜单标签 + 条目图标）。
    doc_types: Vec<DocType>,
    /// 文档类型扩展名（小写）→ 图标名：摊平行时按扩展名取图标。
    doc_icons: HashMap<String, String>,
    /// 上一次操作失败的可读原因。
    notice: Option<String>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            expanded: Vec::new(),
            state: VaultUiState::default(),
            containers: Vec::new(),
            error: None,
            ready: false,
            drag_target: None,
            drag_reject: None,
            dragging: false,
            active_files: Vec::new(),
            inline: None,
            radii: Radii::default(),
            doc_types: Vec::new(),
            doc_icons: HashMap::new(),
            notice: None,
        }
    }

    pub fn set_radii(&mut self, radii: Radii) {
        self.radii = radii;
    }

    /// 整份状态替换（宿主给的就是当前仓库的完整状态）；不动顶部提示，由重取入口自行清理。
    pub fn apply(&mut self, state: VaultUiState) {
        self.expanded = state.expanded.clone();
        self.state = state;
        self.ready = true;
        self.error = None;
    }

    pub fn set_containers(&mut self, containers: Vec<ContainerEntry>) {
        self.containers = containers;
    }

    pub fn containers(&self) -> &[ContainerEntry] {
        &self.containers
    }

    /// 当前容器是本地仓库（协作空间的内容在服务端，没有本机路径）。
    pub fn in_local_container(&self) -> bool {
        self.containers.iter().any(|entry| entry.active && entry.root.is_some())
    }

    /// 插件注册的文档类型（扩展名 + 菜单标签 + 条目图标），供「新建」菜单与行图标使用。
    pub fn set_doc_types(&mut self, doc_types: Vec<DocType>) {
        self.doc_icons = doc_types
            .iter()
            .filter(|kind| !kind.icon.is_empty())
            .map(|kind| (kind.extension.to_ascii_lowercase(), kind.icon.clone()))
            .collect();
        self.doc_types = doc_types;
    }

    pub fn doc_types(&self) -> &[DocType] {
        &self.doc_types
    }

    /// 记一条用户可见的失败原因（面板顶部提示，下一次操作开始时清掉）。
    pub fn warn(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn prompt_notes(&self) -> &[String] {
        &self.state.prompt_notes
    }

    pub fn folder_colors(&self) -> &std::collections::BTreeMap<String, String> {
        &self.state.folder_colors
    }

    pub fn sort(&self) -> &str {
        &self.state.sort
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }

    /// 切容器：丢掉旧容器的展开集 / 即时态，回到未就绪（取到首份前只画加载态）。
    pub fn begin_load(&mut self) {
        self.ready = false;
        self.error = None;
        self.notice = None;
        self.expanded.clear();
        self.inline = None;
        self.active_files.clear();
        self.clear_drag();
        self.dragging = false;
        let sort = self.state.sort.clone();
        self.state = VaultUiState { sort, ..VaultUiState::default() };
    }

    pub fn expanded(&self) -> &[String] {
        &self.expanded
    }

    pub fn expand(&mut self, dir: String) {
        if !self.expanded.contains(&dir) {
            self.expanded.push(dir);
        }
    }

    /// 整份替换展开集（「全部展开 / 收起」用）。
    pub fn set_expanded(&mut self, expanded: Vec<String>) {
        self.expanded = expanded;
    }

    /// 切换一个目录的展开态，返回切换后是否展开。
    pub fn toggle(&mut self, dir: &str) -> bool {
        if let Some(index) = self.expanded.iter().position(|item| item == dir) {
            self.expanded.remove(index);
            false
        } else {
            self.expanded.push(dir.to_string());
            true
        }
    }

    /// 当前打开的文件集合（宿主广播 `editor:open.changed` 后重取）。
    pub fn set_active_files(&mut self, files: Vec<String>) {
        self.active_files = files;
    }

    pub fn set_drag_state(&mut self, target: Option<String>, reject: Option<String>) {
        self.drag_target = target;
        self.drag_reject = reject;
    }

    pub fn clear_drag(&mut self) {
        self.drag_target = None;
        self.drag_reject = None;
    }

    /// 拖拽会话进行中（宿主已发起、尚未结束）。
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// 标记拖拽会话开始 / 结束（落点状态另由 `set_drag_state` 给）。
    pub fn set_dragging(&mut self, dragging: bool) {
        self.dragging = dragging;
        if !dragging {
            self.clear_drag();
        }
    }

    pub fn begin_inline(&mut self, edit: InlineEdit) {
        self.inline = Some(edit);
    }

    pub fn inline(&self) -> Option<&InlineEdit> {
        self.inline.as_ref()
    }

    pub fn inline_mut(&mut self) -> Option<&mut InlineEdit> {
        self.inline.as_mut()
    }

    pub fn take_inline(&mut self) -> Option<InlineEdit> {
        self.inline.take()
    }

    /// 按展开集把目录缓存摊平成可见行（目录在前、文件在后，顺序即宿主给的排序）。
    pub fn view(&self, children: &HashMap<String, Vec<UiDirEntry>>) -> View {
        let mut rows = Vec::new();
        self.push_rows(children, "", 0, &mut rows);
        let inline = self.inline.as_ref().and_then(|edit| {
            let at = match edit {
                InlineEdit::Draft { dir, .. } => rows
                    .iter()
                    .position(|row| &row.path == dir)
                    .map(|index| index + 1)
                    .unwrap_or(0),
                InlineEdit::Rename { path, .. } => {
                    rows.iter().position(|row| &row.path == path)?
                }
            };
            Some((edit.clone(), at))
        });
        let root_open = self.expanded.iter().any(String::is_empty);
        let root_target = self.drag_target.as_deref() == Some("");
        let rejected = self.drag_reject.is_some();
        let containers = self
            .containers
            .iter()
            .map(|entry| {
                let is_local = entry.root.is_some();
                let active = entry.active;
                let hint = match entry.root.as_deref() {
                    Some(root) => root.to_string(),
                    None => match entry.server_url.as_deref() {
                        Some(server) => format!("{} · {}", entry.label, server),
                        None => entry.label.clone(),
                    },
                };
                ContainerRow {
                    id: entry.id.clone(),
                    label: entry.label.clone(),
                    is_local,
                    active,
                    open: active && root_open,
                    hint,
                    // 落点只作用在激活容器上：别的容器不可落（切过去才能放）
                    drop_target: active && root_target && !rejected,
                    drop_rejected: active && root_target && rejected,
                }
            })
            .collect();
        View {
            error: self.error.clone(),
            notice: self.notice.clone(),
            rows,
            containers,
            // 容器行（仓库根 = 空路径）不算「有展开的目录」：全部展开/收起只作用于仓库内的条目
            any_expanded: self.expanded.iter().any(|dir| !dir.is_empty()),
            in_repo: self.ready,
            loading: !self.ready && self.error.is_none(),
            vault_drop_target: root_target,
            vault_drop_rejected: root_target && rejected,
            inline,
            dragging: self.dragging,
            drop_hint: self.drag_reject.clone(),
            sort: self.state.sort.clone(),
        }
    }

    fn push_rows(
        &self,
        children: &HashMap<String, Vec<UiDirEntry>>,
        dir: &str,
        depth: usize,
        rows: &mut Vec<Row>,
    ) {
        let Some(entries) = children.get(dir) else { return };
        for entry in entries {
            let expanded = self.expanded.iter().any(|item| item == &entry.path);
            let rejected = self.drag_reject.is_some();
            let is_target = self.drag_target.as_deref() == Some(entry.path.as_str());
            rows.push(Row {
                path: entry.path.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                active: !entry.is_dir && self.active_files.iter().any(|f| f == &entry.path),
                depth,
                expanded,
                color: self.state.folder_colors.get(&entry.path).cloned(),
                drop_target: is_target && !rejected && entry.is_dir,
                drop_rejected: is_target && rejected,
                icon: if entry.is_dir {
                    None
                } else {
                    extension_of(&entry.name)
                        .and_then(|ext| self.doc_icons.get(&ext.to_ascii_lowercase()).cloned())
                },
            });
            if entry.is_dir && expanded {
                self.push_rows(children, &entry.path, depth + 1, rows);
            }
        }
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// 文件名的扩展名（不带点；无扩展名与 `.` 开头的隐藏文件都没有）。
pub(crate) fn extension_of(name: &str) -> Option<&str> {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext),
        _ => None,
    }
}

/// 文件名的主名（无扩展名与隐藏文件原样返回）。
pub(crate) fn stem_of(name: &str) -> &str {
    match extension_of(name) {
        Some(ext) => &name[..name.len() - ext.len() - 1],
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(extension: &str, icon: &str) -> DocType {
        DocType { extension: extension.into(), label: extension.into(), icon: icon.into() }
    }

    /// 首份状态未到（初始 / 切容器）只画加载态；取到状态后空树才交给空态。
    #[test]
    fn loading_gate_closes_until_the_first_state() {
        let children = HashMap::new();
        let mut model = Model::new();
        assert!(model.view(&children).loading, "初始未取数");

        model.apply(VaultUiState::default());
        assert!(!model.view(&children).loading, "首份状态到了");

        model.begin_load();
        let view = model.view(&children);
        assert!(view.loading, "切容器回到未就绪");
        assert!(!view.in_repo, "未就绪不当成空仓库");
        assert!(view.rows.is_empty());
    }

    /// 第三方文档类型的条目图标按注册表声明取，大小写与扩展名无关。
    #[test]
    fn row_icon_follows_the_registered_doc_type() {
        let mut model = Model::new();
        model.set_doc_types(vec![kind("board", "layout-dashboard")]);
        let mut children = HashMap::new();
        children.insert(
            String::new(),
            vec![
                UiDirEntry { name: "白板.BOARD".into(), path: "白板.BOARD".into(), is_dir: false, mtime: 0 },
                UiDirEntry { name: "笔记.md".into(), path: "笔记.md".into(), is_dir: false, mtime: 0 },
            ],
        );
        let view = model.view(&children);
        assert_eq!(view.rows[0].icon.as_deref(), Some("layout-dashboard"));
        assert_eq!(view.rows[1].icon, None, "未注册的扩展名交给扩展名兜底");
    }

    /// 隐藏文件没有扩展名，不该被当成后缀徽标或文档类型。
    #[test]
    fn hidden_files_have_no_extension() {
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("a.md"), Some("md"));
        assert_eq!(extension_of("无扩展名"), None);
    }
}
