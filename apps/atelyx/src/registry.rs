//! 唯一注册表：内置插件的视图 / 服务、外部插件贡献、领域文档类型收在同一份快照里。
//! 条目是纯数据，渲染留在 [`crate::pack`] 的渲染表按 id 分派；本模块不依赖 `App`，
//! 测试直接喂快照。

use std::borrow::Cow;

use atelyx_core::plugin_runtime::service::ServiceInfo;
use atelyx_core::plugin_runtime::{
    Contribution, ContributionSlot, DocTypeInfo, PluginSummary, BUILTIN_SERVICES,
};

/// 条目类别：视图与服务各成一片，查询与展示共用同一结构。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// 可停靠的视图（内置插件与外部插件的面板 / 表格视图 / 设置页贡献）。
    View,
    /// 能力总线服务（内置插件实现与外部提供者）。
    Service,
}

/// 条目归属：哪个插件提供它，以及该插件是随应用分发还是外部安装。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// 预置插件（随应用分发）；视图由壳内渲染表出图。
    Builtin(String),
    /// 外部安装的插件。
    Plugin(String),
}

impl Source {
    pub fn is_builtin(&self) -> bool {
        matches!(self, Source::Builtin(_))
    }

    /// 提供该条目的插件 id。
    pub fn plugin_id(&self) -> &str {
        match self {
            Source::Builtin(plugin) | Source::Plugin(plugin) => plugin,
        }
    }
}

/// 注册表条目。
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// 视图 = 壳侧视图 kind（内置 `"note"`，插件 `"plugin:<插件 id>:<贡献 id>"`）；服务 = 服务名。
    pub id: String,
    pub kind: EntryKind,
    pub label: String,
    /// 壳内形状表里的图标名；未知已回落到 `puzzle`。静态名不分配（渲染每帧都读）。
    pub icon: Cow<'static, str>,
    pub source: Source,
    /// 有槽位的视图（面板 / 表格视图 / 设置页）；预置插件视图与其它类别为 `None`。
    pub slot: Option<ContributionSlot>,
    /// 可选 / 可用：内置插件条目看 `ui.disabled_plugins` 与所在服务是否在目录里，外部插件看启用态。
    pub enabled: bool,
    /// 能否被外部提供者替换。平台自救面（`query` 服务）与视图 / 文档类型恒 false——
    /// 视图没有提供者切换面。
    pub replaceable: bool,
    /// 服务：活动提供者 + 方法数，供设置页一行显示；其它类别为空串。
    pub detail: String,
    /// 服务：活动提供者（`None` = 内置实现）；其它类别为 `None`。
    pub provider: Option<String>,
    /// 服务：其余候选提供者（切换目标，不含活动提供者）；其它类别为空。
    pub candidates: Vec<String>,
    /// 服务：是否有可回落的内置实现（归属插件停用即 false）；其它类别为 false。
    pub builtin: bool,
}

/// 内置插件的一个视图贡献。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinView {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

/// 内置插件条目的登记数据（`pack` 的表用它声明，注册表把它摊平进条目）。
/// 与外部插件同构：启停按插件整体生效，视图只是它提供的一种贡献。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinPlugin {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub views: &'static [BuiltinView],
}

/// 注册表里的内置插件（设置页按它分组显示）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinPluginInfo {
    pub id: String,
    pub label: String,
    pub icon: Cow<'static, str>,
    pub enabled: bool,
}

/// 建表快照：注册表只认这些数据，不认 `App` 的字段布局。
pub struct Snapshot<'a> {
    pub builtin_plugins: &'a [BuiltinPlugin],
    pub contributions: &'a [Contribution],
    pub doc_types: &'a [DocTypeInfo],
    pub services: &'a [ServiceInfo],
    pub plugins: &'a [PluginSummary],
    pub disabled_plugins: &'a [String],
}

/// 注册表：条目按登记序（视图在前、服务在后），装配期与投影刷新时整体重建，运行期只读。
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
    builtin_plugins: Vec<BuiltinPluginInfo>,
    /// 领域文档类型按扩展名路由，不进选择器；保存完整注册以便打开 / 新建 / 图标共用一处。
    doc_types: Vec<DocTypeInfo>,
}

impl Registry {
    /// 按快照建表：内置插件 → 插件贡献 → 服务；文档类型另存一份。
    pub fn build(snapshot: Snapshot<'_>) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        let mut builtin_plugins = Vec::new();
        for plugin in snapshot.builtin_plugins {
            let enabled = !snapshot.disabled_plugins.iter().any(|id| id == plugin.id);
            builtin_plugins.push(BuiltinPluginInfo {
                id: plugin.id.to_string(),
                label: plugin.label.to_string(),
                icon: Cow::Borrowed(plugin.icon),
                enabled,
            });
            for view in plugin.views {
                entries.push(Entry {
                    id: view.id.to_string(),
                    kind: EntryKind::View,
                    label: view.label.to_string(),
                    icon: Cow::Borrowed(view.icon),
                    source: Source::Builtin(plugin.id.to_string()),
                    slot: None,
                    enabled,
                    replaceable: false,
                    detail: String::new(),
                    provider: None,
                    candidates: Vec::new(),
                    builtin: false,
                });
            }
        }
        for contribution in snapshot.contributions {
            entries.push(Entry {
                id: crate::plugin_actions::view_kind(&contribution.plugin_id, &contribution.id),
                kind: EntryKind::View,
                label: contribution.label.clone(),
                icon: crate::plugin_actions::plugin_icon_key(contribution.icon.as_deref(), "puzzle"),
                source: Source::Plugin(contribution.plugin_id.clone()),
                slot: Some(contribution.slot),
                enabled: plugin_enabled(snapshot.plugins, &contribution.plugin_id),
                replaceable: false,
                detail: String::new(),
                provider: None,
                candidates: Vec::new(),
                builtin: false,
            });
        }
        for info in snapshot.services {
            let owner = service_owner(&info.service);
            let builtin = owner.is_some_and(|owner| {
                !snapshot.disabled_plugins.iter().any(|id| id == owner)
            });
            entries.push(Entry {
                id: info.service.clone(),
                kind: EntryKind::Service,
                label: info.service.clone(),
                icon: Cow::Borrowed("layers"),
                source: match owner {
                    Some(owner) => Source::Builtin(owner.to_string()),
                    // 外部插件带进来的服务：归属在册的提供者
                    None => Source::Plugin(info.provider.clone().unwrap_or_default()),
                },
                slot: None,
                enabled: true,
                replaceable: info.replaceable,
                detail: service_detail(info),
                provider: info.provider.clone(),
                candidates: info.candidates.clone(),
                builtin,
            });
        }
        Self { entries, builtin_plugins, doc_types: snapshot.doc_types.to_vec() }
    }

    /// 按视图 id 查条目（不看启用态：停用视图的标签仍要显示名称与图标）。
    /// 服务名与视图 id 是两个命名空间（`table` 既是视图也是服务），服务请用 [`Registry::service`]。
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.kind == EntryKind::View && entry.id == id)
    }

    /// 按服务名查条目（不看启用态）。
    pub fn service(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.kind == EntryKind::Service && entry.id == name)
    }

    /// 领域文档类型注册（扩展名路由 / 新建菜单 / 文件树图标共用这一份）。
    pub fn doc_types(&self) -> &[DocTypeInfo] {
        &self.doc_types
    }

    /// 内置插件（设置页按它分组：每组带自己的视图与服务条目）。
    pub fn builtin_plugins(&self) -> &[BuiltinPluginInfo] {
        &self.builtin_plugins
    }

    /// 条目标签；未登记时回落到插件 id（贡献随插件停用消失），再回落原始 id。
    pub fn label(&self, id: &str) -> String {
        if let Some(entry) = self.get(id) {
            return entry.label.clone();
        }
        match crate::plugin_actions::parse_view_kind(id) {
            Some((plugin_id, _)) => plugin_id.to_string(),
            None => id.to_string(),
        }
    }

    /// 条目图标名；未登记回落拼图。
    pub fn icon(&self, id: &str) -> Cow<'static, str> {
        match self.get(id) {
            Some(entry) => entry.icon.clone(),
            None => Cow::Borrowed("puzzle"),
        }
    }

    /// 可选视图（选择器与标签菜单的清单）：启用中的面板视图，按登记序。
    pub fn views(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| {
            entry.kind == EntryKind::View
                && entry.enabled
                && matches!(entry.slot, None | Some(ContributionSlot::Panel))
        })
    }

    /// 指定槽位的启用视图（表格视图下拉 / 设置页侧栏）。
    pub fn views_in(&self, slot: ContributionSlot) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |entry| {
                entry.kind == EntryKind::View && entry.enabled && entry.slot == Some(slot)
            })
    }

    /// 在册服务（按服务名排序，与目录快照一致）。
    pub fn services(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.kind == EntryKind::Service)
    }

    /// 全部条目（设置页按类别自行筛选）。
    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }
}

/// 服务的内置归属插件；`None` = 目录里没有该服务的内置实现。
fn service_owner(name: &str) -> Option<&'static str> {
    BUILTIN_SERVICES
        .iter()
        .find(|(service, _, _)| *service == name)
        .map(|(_, owner, _)| *owner)
}

/// 插件是否启用：注册表里没有该插件（贡献与启用标记不同步的瞬间）按可用处理，
/// 宁可多列一项，也不让标签在选择器里闪断。
fn plugin_enabled(plugins: &[PluginSummary], plugin_id: &str) -> bool {
    plugins
        .iter()
        .find(|summary| summary.id == plugin_id)
        .is_none_or(|summary| summary.enabled)
}

/// 服务条目的一行说明：活动提供者 + 方法数。
fn service_detail(info: &ServiceInfo) -> String {
    let provider = info.provider.as_deref().unwrap_or("内置");
    format!("{provider} · {} 个方法", info.methods.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::plugin_runtime::PluginStatus;

    fn builtin_plugins() -> [BuiltinPlugin; 2] {
        [
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
        ]
    }

    fn contribution() -> Contribution {
        Contribution {
            plugin_id: "com.a".into(),
            slot: ContributionSlot::Panel,
            id: "models".into(),
            label: "模型面板".into(),
            icon: Some("layers".into()),
            padding: None,
        }
    }

    fn plugin_summary(enabled: bool) -> PluginSummary {
        PluginSummary {
            id: "com.a".into(),
            name: "样例插件".into(),
            version: "1.0.0".into(),
            api_version: 1,
            permissions: Vec::new(),
            singleton: false,
            keep_mounted_on_vault_switch: false,
            enabled,
            status: PluginStatus::Running,
            load_error: None,
            undeclared: 0,
            submits: 0,
            source: None,
            preset: false,
        }
    }

    fn service(name: &str, provider: Option<&str>, replaceable: bool) -> ServiceInfo {
        ServiceInfo {
            service: name.into(),
            version: 1,
            methods: vec!["read".into(), "write".into()],
            provider: provider.map(str::to_string),
            replaceable,
            candidates: vec!["com.b".into()],
        }
    }

    fn registry(enabled: bool, disabled_plugins: &[String]) -> Registry {
        let builtin_plugins = builtin_plugins();
        let contributions = [contribution()];
        let plugins = [plugin_summary(enabled)];
        let services = [
            service("vault", None, true),
            service("query", None, false),
            service("table", None, true),
        ];
        Registry::build(Snapshot {
            builtin_plugins: &builtin_plugins,
            contributions: &contributions,
            doc_types: &[],
            services: &services,
            plugins: &plugins,
            disabled_plugins,
        })
    }

    #[test]
    fn builtin_and_plugin_entries_share_one_shape() {
        let registry = registry(true, &[]);
        let builtin = registry.get("note").expect("预置插件视图在册");
        let plugin = registry.get("plugin:com.a:models").expect("插件视图在册");
        assert_eq!(builtin.kind, EntryKind::View);
        assert_eq!(plugin.kind, EntryKind::View);
        assert_eq!(builtin.slot, None);
        assert_eq!(plugin.slot, Some(ContributionSlot::Panel));
        assert!(builtin.enabled && plugin.enabled);
        // 同一查询路径：选择器、标签与图标都不区分来源
        let ids: Vec<&str> = registry.views().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["note", "table", "plugin:com.a:models"]);
        assert_eq!(registry.label("plugin:com.a:models"), "模型面板");
        assert_eq!(registry.icon("plugin:com.a:models"), "layers");
        assert!(matches!(plugin.source, Source::Plugin(ref id) if id == "com.a"));
        assert_eq!(builtin.source.plugin_id(), "atelyx.notes");
        assert!(builtin.source.is_builtin());
    }

    #[test]
    fn disabled_plugin_hides_its_views_and_leaves_other_plugins_intact() {
        let registry = registry(true, &["atelyx.notes".to_string()]);
        let ids: Vec<&str> = registry.views().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["table", "plugin:com.a:models"], "停用插件不再进选择器");
        assert!(!registry.get("note").expect("条目仍保留").enabled);
        assert_eq!(registry.label("note"), "笔记", "停用后标签仍可读");
        assert!(registry.get("table").expect("同组外的视图不受影响").enabled);
        assert_eq!(registry.services().count(), 3, "停用视图不动服务区");
    }

    #[test]
    fn unknown_id_falls_back() {
        let registry = registry(true, &[]);
        assert!(registry.get("nope").is_none());
        assert!(registry.service("nope").is_none());
        assert_eq!(registry.label("nope"), "nope");
        assert_eq!(registry.icon("nope"), "puzzle");
        assert_eq!(registry.label("plugin:com.b:gone"), "com.b");
        assert_eq!(registry.icon("plugin:com.b:gone"), "puzzle");
    }

    #[test]
    fn view_and_service_namespaces_stay_separate() {
        let registry = registry(true, &[]);
        // `table` 既是预置插件视图也是服务：两个查询各取本位，互不遮蔽
        assert_eq!(registry.get("table").expect("视图").kind, EntryKind::View);
        assert_eq!(registry.service("table").expect("服务").kind, EntryKind::Service);
        assert!(registry.get("vault").is_none(), "服务名不冒充视图");
        assert!(registry.service("note").is_none(), "视图 id 不冒充服务");
    }

    #[test]
    fn platform_entries_are_not_replaceable() {
        let registry = registry(true, &[]);
        assert!(!registry.get("note").expect("预置插件视图").replaceable);
        assert!(!registry.get("plugin:com.a:models").expect("插件视图").replaceable);
        let query = registry.service("query").expect("query 在册");
        assert!(!query.replaceable, "平台自救面不可替换");
        let vault = registry.service("vault").expect("vault 在册");
        assert!(vault.replaceable);
        assert!(vault.builtin);
        assert_eq!(vault.detail, "内置 · 2 个方法");
    }

    #[test]
    fn disabled_plugin_entries_stay_registered_but_marked() {
        let disabled = registry(false, &[]);
        assert!(!disabled.get("plugin:com.a:models").expect("贡献仍在册").enabled);
        assert!(disabled.views().all(|entry| entry.id != "plugin:com.a:models"));
        assert!(disabled.get("note").expect("预置插件视图不受影响").enabled);
    }

    #[test]
    fn disabled_plugins_are_marked_at_build_time() {
        let registry = registry(true, &["atelyx.notes".to_string()]);
        let notes = registry
            .builtin_plugins()
            .iter()
            .find(|plugin| plugin.id == "atelyx.notes")
            .expect("内置插件在册");
        assert!(!notes.enabled);
        assert!(!registry.get("note").expect("在册").enabled);
        assert!(registry.views().all(|entry| entry.id != "note"));
        assert!(registry.builtin_plugins().iter().any(|plugin| plugin.id == "atelyx.tables" && plugin.enabled));
    }

    #[test]
    fn services_belong_to_their_builtin_plugin() {
        let registry = registry(true, &[]);
        let markdown = registry.service("markdown");
        // 目录里没有 markdown（本测试快照只喂了三项服务），归属仍从内置目录取
        assert!(markdown.is_none());
        let table = registry.service("table").expect("table 在册");
        assert_eq!(table.source, Source::Builtin("atelyx.tables".into()));
        let query = registry.service("query").expect("query 在册");
        assert_eq!(query.source, Source::Builtin("atelyx.platform".into()));
        let vault = registry.service("vault").expect("vault 在册");
        assert_eq!(vault.source.plugin_id(), "atelyx.platform");
    }
}
