//! 打开文件集合的壳侧接线：实例同步、主要面板投影、类型槽显示文件解析。
//! 集合模型在 `atelyx_core::open_files`；本模块只把会话态与会话容器接上。

use atelyx_core::layout::{self, LayoutNode, TabItem};
use atelyx_core::open_files::{DocKind, Host};

use crate::layout_view::file_label;
use crate::App;

/// 内置文档视图（插件文档视图不进集合：仍按普通面板标签呈现）。
pub fn doc_kind_of_view(view: &str) -> Option<DocKind> {
    if !layout::is_builtin_doc_view(view) {
        return None;
    }
    Some(match view {
        "table" => DocKind::Table,
        "canvas" => DocKind::Canvas,
        _ => DocKind::Note,
    })
}

/// 文档类型对应的视图 kind（投影标签与槽都用它）。
pub fn view_of_kind(kind: &DocKind) -> &str {
    match kind {
        DocKind::Note => "note",
        DocKind::Table => "table",
        DocKind::Canvas => "canvas",
        DocKind::Plugin(view) => view,
    }
}

/// 文件已打开（会话载入时调用）：集合登记并激活。本地仓库文档多实例（其余打开项的
/// 编辑态停放在 `App.notes` / `App.tables` / `App.canvases`）；协作空间每类型至多一项，
/// 先把同类型别的实例移出集合（其会话已被新文件替换）。
pub fn doc_opened(app: &mut App, path: &str, kind: DocKind) {
    let multi = matches!(app.repo_kind, crate::RepoKind::Local)
        && matches!(kind, DocKind::Note | DocKind::Table | DocKind::Canvas);
    if !multi {
        let others: Vec<String> = app
            .open_files
            .of_kind(&kind, &Host::Main)
            .into_iter()
            .filter(|d| d.path != path)
            .map(|d| d.id.clone())
            .collect();
        for id in others {
            app.notes.remove(&id);
            app.tables.remove(&id);
            app.canvases.remove(&id);
            app.open_files.close(&id);
        }
    }
    app.open_files.open(path, kind);
}

/// 文件已关闭：集合移除该类型的全部实例（关文件 = 销毁实例，撕裂窗口同样失去内容）。
/// 装配层（会话文件已清空）之后调用；空间 / 本地两套关闭路径共用。
pub fn doc_closed(app: &mut App, kind: &DocKind) {
    let ids: Vec<String> = app
        .open_files
        .items()
        .iter()
        .filter(|d| &d.kind == kind)
        .map(|d| d.id.clone())
        .collect();
    for id in ids {
        app.notes.remove(&id);
        app.tables.remove(&id);
        app.canvases.remove(&id);
        app.open_files.close(&id);
    }
}

/// 切仓库 / 离开空间：集合清空（打开的会话随后全部关闭）。
pub fn clear_all(app: &mut App) {
    app.open_files.clear();
    app.notes.clear();
    app.tables.clear();
    app.canvases.clear();
}

/// 主要面板投影标签：宿主在主窗口的实例按集合顺序生成（id = 实例 id）。
pub fn primary_tabs(app: &App) -> Vec<TabItem> {
    app.open_files
        .items()
        .iter()
        .filter(|d| d.host == Host::Main)
        .map(|d| TabItem {
            id: d.id.clone(),
            view: view_of_kind(&d.kind).to_string(),
            locked: false,
            file: Some(d.path.clone()),
        })
        .collect()
}

/// 主要面板当前激活标签：集合激活项在主窗口则用它，否则回退最近激活的主窗口实例。
pub fn primary_active(app: &App) -> Option<String> {
    if let Some(d) = app.open_files.active().filter(|d| d.host == Host::Main) {
        return Some(d.id.clone());
    }
    app.open_files.recent_of_host(&Host::Main).map(|d| d.id.clone())
}

/// 类型槽当前显示文件：手动选择（集合内同类、宿主在主窗口）> 集合该类型最近激活 > 空态。
pub fn slot_path(app: &App, tab: &TabItem) -> Option<String> {
    let kind = doc_kind_of_view(&tab.view)?;
    if let Some(file) = &tab.file {
        if let Some(d) = app.open_files.find_by_path(file) {
            if d.kind == kind && d.host == Host::Main {
                return Some(file.clone());
            }
        }
    }
    doc_path(app, &kind)
}

/// 某类型在主窗口的当前文档（集合该类型最近激活）。
pub fn doc_path(app: &App, kind: &DocKind) -> Option<String> {
    app.open_files.recent_of_kind(kind, &Host::Main).map(|d| d.path.clone())
}

/// 笔记视图当前显示的笔记路径。
pub fn note_path(app: &App) -> Option<String> {
    doc_path(app, &DocKind::Note)
}

/// 标签是否显示 ×（视图有打开中的文件）。
pub fn tab_has_file(app: &App, tab: &TabItem) -> bool {
    if let Some(file) = &tab.file {
        if app.open_files.find_by_path(file).is_some_and(|d| d.host == Host::Main) {
            return true;
        }
    }
    doc_kind_of_view(&tab.view).is_some_and(|kind| app.open_files.recent_of_kind(&kind, &Host::Main).is_some())
}

/// 标签文字：文档槽/投影 = 文件名；工具视图 = 视图名。
pub fn tab_label(app: &App, tab: &TabItem) -> String {
    if let Some(path) = slot_path(app, tab) {
        return file_label(&path);
    }
    crate::layout_view::view_label(app, &tab.view)
}

/// 取消主要面板：集合塌缩成类型槽（每类型一个，优先该类型最近激活项）。
pub fn collapse_slots(app: &App) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for kind in [DocKind::Note, DocKind::Table, DocKind::Canvas] {
        if let Some(d) = app.open_files.recent_of_kind(&kind, &Host::Main) {
            out.push((view_of_kind(&kind).to_string(), Some(d.path.clone())));
        }
    }
    out
}

/// 当前布局内全部文档槽与其有效显示文件（设为主要面板前解析，迁入集合）。
pub fn layout_doc_paths(app: &App) -> Vec<(String, Option<String>)> {
    let tree = layout::active_layout(&app.ui).tree;
    let mut out = Vec::new();
    collect_doc_tabs(&tree, app, &mut out);
    out
}

fn collect_doc_tabs(node: &LayoutNode, app: &App, out: &mut Vec<(String, Option<String>)>) {
    match node {
        LayoutNode::Panel { tabs, .. } => {
            for tab in tabs {
                if doc_kind_of_view(&tab.view).is_some() {
                    out.push((tab.view.clone(), slot_path(app, tab)));
                }
            }
        }
        LayoutNode::Split { children, .. } => {
            for child in children {
                collect_doc_tabs(child, app, out);
            }
        }
    }
}
