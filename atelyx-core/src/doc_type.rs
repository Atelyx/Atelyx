//! 文档类型查询表：扩展名 → 归属插件（owner）与可承载视图；宿主只查表，不认识具体格式。
//! 扩展名归属规则：预置插件优先，其次按（插件 id、文档类型 id）字典序稳定取胜；落败声明记入冲突。

use std::collections::BTreeMap;

use crate::plugin_runtime::DocTypeInfo;

/// 插件声明的稳定文档类型 id（宿主不解释语义，只用于状态与路由）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocTypeId(String);

impl DocTypeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocTypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一个可承载文档的视图：贡献 id 属于 `plugin_id` 声明的插件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocViewRef {
    pub plugin_id: String,
    pub view_id: String,
}

/// 一条文档类型路由：owner 定内容真源，views 定可绑定视图。
#[derive(Clone, Debug, PartialEq)]
pub struct DocTypeRoute {
    pub id: DocTypeId,
    pub owner_plugin: String,
    /// 这条声明写在哪个插件的 manifest（其 `views` 归该插件）。
    pub declaring_plugin: String,
    pub views: Vec<DocViewRef>,
    /// 小写、不带点的扩展名。
    pub extensions: Vec<String>,
    pub editable: bool,
    pub multi_doc: bool,
    pub label: String,
    pub icon: String,
    pub preset: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocTypeConflict {
    pub extension: String,
    pub kept_plugin: String,
    pub dropped_plugin: String,
}

#[derive(Clone, Debug, Default)]
pub struct DocTypeIndex {
    by_extension: BTreeMap<String, DocTypeRoute>,
    conflicts: Vec<DocTypeConflict>,
}

impl DocTypeIndex {
    /// 摊平所有已安装插件的声明；同一扩展名只保留一条路由。
    pub fn build(infos: &[DocTypeInfo]) -> Self {
        let mut index = Self::default();
        for info in infos {
            let registration = &info.registration;
            let route = DocTypeRoute {
                id: DocTypeId::new(registration.id.clone()),
                owner_plugin: registration.owner_plugin.clone(),
                declaring_plugin: info.plugin_id.clone(),
                views: registration
                    .views
                    .iter()
                    .map(|view| DocViewRef { plugin_id: info.plugin_id.clone(), view_id: view.clone() })
                    .collect(),
                extensions: registration
                    .extensions
                    .iter()
                    .map(|extension| extension.trim().to_ascii_lowercase())
                    .filter(|extension| !extension.is_empty())
                    .collect(),
                editable: registration.editable,
                multi_doc: registration.multi_doc,
                label: registration.label.clone(),
                icon: registration.icon.clone(),
                preset: info.preset,
            };
            for extension in &route.extensions {
                match index.by_extension.get(extension) {
                    Some(current) if !outranks(&route, current) => {
                        index.conflicts.push(DocTypeConflict {
                            extension: extension.clone(),
                            kept_plugin: current.declaring_plugin.clone(),
                            dropped_plugin: route.declaring_plugin.clone(),
                        });
                    }
                    Some(current) => {
                        index.conflicts.push(DocTypeConflict {
                            extension: extension.clone(),
                            kept_plugin: route.declaring_plugin.clone(),
                            dropped_plugin: current.declaring_plugin.clone(),
                        });
                        index.by_extension.insert(extension.clone(), route.clone());
                    }
                    None => {
                        index.by_extension.insert(extension.clone(), route.clone());
                    }
                }
            }
        }
        index
    }

    pub fn route_for_path(&self, path: &str) -> Option<&DocTypeRoute> {
        self.route_for_extension(extension_of(path)?)
    }

    pub fn route_for_extension(&self, extension: &str) -> Option<&DocTypeRoute> {
        self.by_extension.get(&extension.trim().to_ascii_lowercase())
    }

    pub fn routes(&self) -> impl Iterator<Item = &DocTypeRoute> {
        self.by_extension.values()
    }

    /// 该扩展名上落败的声明数量（> 0 表示多个插件争用，宿主可用于提示）。
    pub fn conflicts_for(&self, extension: &str) -> usize {
        let extension = extension.trim().to_ascii_lowercase();
        self.conflicts.iter().filter(|conflict| conflict.extension == extension).count()
    }

    pub fn conflicts(&self) -> &[DocTypeConflict] {
        &self.conflicts
    }
}

/// 路径（`/` 分隔的仓库相对路径）的扩展名；无扩展名或点为末字符时返回 None。
pub fn extension_of(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then_some(extension)
}

fn outranks(candidate: &DocTypeRoute, current: &DocTypeRoute) -> bool {
    if candidate.preset != current.preset {
        return candidate.preset;
    }
    (&candidate.declaring_plugin, candidate.id.as_str())
        < (&current.declaring_plugin, current.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::manifest::DocTypeRegistration;

    fn doc_type_info(plugin_id: &str, preset: bool, registration: DocTypeRegistration) -> DocTypeInfo {
        DocTypeInfo { plugin_id: plugin_id.into(), preset, registration }
    }

    fn registration(id: &str, owner: &str, extensions: &[&str], views: &[&str]) -> DocTypeRegistration {
        DocTypeRegistration {
            id: id.into(),
            extensions: extensions.iter().map(|extension| (*extension).into()).collect(),
            owner_plugin: owner.into(),
            views: views.iter().map(|view| (*view).into()).collect(),
            editable: true,
            multi_doc: false,
            label: id.into(),
            icon: String::new(),
        }
    }

    #[test]
    fn routes_paths_by_extension_case_insensitively() {
        let index = DocTypeIndex::build(&[doc_type_info(
            "atelyx.notes",
            true,
            registration("note", "atelyx.notes", &["md"], &["note"]),
        )]);
        let route = index.route_for_path("notes/hello.MD").expect("扩展名不区分大小写");
        assert_eq!(route.id.as_str(), "note");
        assert_eq!(route.owner_plugin, "atelyx.notes");
        assert_eq!(route.views[0], DocViewRef { plugin_id: "atelyx.notes".into(), view_id: "note".into() });
        assert!(index.route_for_path("notes/hello").is_none());
        assert!(index.route_for_path("notes/hello.").is_none());
    }

    #[test]
    fn preset_wins_extension_conflicts() {
        let index = DocTypeIndex::build(&[
            doc_type_info("com.ext.note", false, registration("note", "com.ext.note", &["md"], &["note"])),
            doc_type_info("atelyx.notes", true, registration("note", "atelyx.notes", &["md"], &["note"])),
        ]);
        let route = index.route_for_extension("md").expect("存在路由");
        assert_eq!(route.declaring_plugin, "atelyx.notes");
        assert_eq!(index.conflicts_for("md"), 1, "落败声明要可判定");
    }

    #[test]
    fn same_source_conflicts_are_ordered_by_plugin_id() {
        let index = DocTypeIndex::build(&[
            doc_type_info("com.z.board", false, registration("board", "com.z.board", &["board"], &["board"])),
            doc_type_info("com.a.board", false, registration("board", "com.a.board", &["board"], &["board"])),
        ]);
        assert_eq!(index.route_for_extension("board").unwrap().declaring_plugin, "com.a.board");
        assert_eq!(index.conflicts_for("board"), 1);
    }

    #[test]
    fn multiple_extensions_share_one_route() {
        let index = DocTypeIndex::build(&[doc_type_info(
            "atelyx.notes",
            true,
            registration("note", "atelyx.notes", &["md", "markdown"], &["note"]),
        )]);
        assert_eq!(index.routes().count(), 2);
        assert_eq!(index.route_for_extension("markdown").unwrap().id.as_str(), "note");
    }
}
