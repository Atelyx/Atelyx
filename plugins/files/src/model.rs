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
    /// 展开指示线的 x 坐标（每层一条）。
    pub guides: Vec<f32>,
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

/// 一帧要画的内容（与 [`Model`] 解耦，渲染期间不持锁）。
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub error: Option<String>,
    pub rows: Vec<Row>,
    /// 容器列表（全部最近仓库 + 全部空间，激活项在前）。
    pub containers: Vec<ContainerRow>,
    /// 「展开全部」按钮当前状态（有任一展开即显示「收起全部」）。
    pub any_expanded: bool,
    /// 当前容器是否可用（未进入仓库时给空态）。
    pub in_repo: bool,
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
    /// 插件注册的文档类型（扩展名 + 菜单标签）。
    doc_types: Vec<(String, String)>,
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
        }
    }

    pub fn set_radii(&mut self, radii: Radii) {
        self.radii = radii;
    }

    /// 整份状态替换（宿主给的就是当前仓库的完整状态）。
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

    /// 插件注册的文档类型（扩展名 + 菜单标签），供「新建」菜单条目使用。
    pub fn set_doc_types(&mut self, doc_types: Vec<(String, String)>) {
        self.doc_types = doc_types;
    }

    pub fn doc_types(&self) -> Vec<(String, String)> {
        self.doc_types.clone()
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
            rows,
            containers,
            // 容器行（仓库根 = 空路径）不算「有展开的目录」：全部展开/收起只作用于仓库内的条目
            any_expanded: self.expanded.iter().any(|dir| !dir.is_empty()),
            in_repo: self.ready,
            vault_drop_target: root_target,
            vault_drop_rejected: root_target && rejected,
            inline,
            dragging: self.drag_target.is_some() || self.drag_reject.is_some(),
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
                guides: (1..=depth).map(|level| 15.0 + level as f32 * 16.0).collect(),
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
