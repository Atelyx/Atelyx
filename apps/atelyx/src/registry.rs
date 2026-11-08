//! 唯一注册表：原生视图、外部插件贡献、领域文档类型收在同一份快照里。
//! 条目是纯数据，渲染留在 [`crate::pack`] 的渲染表按 id 分派；本模块不依赖 `App`，
//! 测试直接喂快照。

use std::borrow::Cow;

use atelyx_core::plugin_runtime::service::ServiceInfo;
use atelyx_core::plugin_runtime::{Contribution, ContributionSlot, DocTypeInfo, PluginSummary};

/// 条目类别：视图与服务各成一片，查询与展示共用同一结构。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// 可停靠的视图（壳内原生视图与外部插件贡献）。
    View,
    /// 能力总线服务（宿主能力与外部插件提供）。
    Service,
}

/// 条目归属：壳内原生实现，或外部插件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// 壳内原生实现（视图 / 宿主服务）。
    Native,
    /// 外部安装的插件。
    Plugin(String),
}

impl Source {
    /// 外部插件 id；原生实现为 `None`。
    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Source::Native => None,
            Source::Plugin(plugin) => Some(plugin),
        }
    }
}

/// 注册表条目。
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// 视图 = 壳侧视图 kind（原生 `"note"`，插件 `"plugin:<插件 id>:<贡献 id>"`）；服务 = 服务名。
    pub id: String,
    pub kind: EntryKind,
    pub label: String,
    /// 壳内形状表里的图标名；未知已回落到 `puzzle`。静态名不分配（渲染每帧都读）。
    pub icon: Cow<'static, str>,
    pub source: Source,
    /// 有槽位的视图（面板 / 表格视图 / 设置页）；原生视图与其它类别为 `None`。
    pub slot: Option<ContributionSlot>,
    /// 可选 / 可用：原生视图恒可用，外部插件看启用态。
    pub enabled: bool,
    /// 能否被外部提供者替换。原生视图与内置服务恒 false。
    pub replaceable: bool,
    /// 服务：活动提供者 + 方法数，供设置页一行显示；其它类别为空串。
    pub detail: String,
    /// 服务：活动提供者（`None` = 宿主实现）；其它类别为 `None`。
    pub provider: Option<String>,
    /// 服务：是否有宿主实现可回落；其它类别为 false。
    pub builtin: bool,
}

/// 壳内原生视图（渲染函数在 [`crate::pack`] 的渲染表里按 id 分派）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeView {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

/// 建表快照：注册表只认这些数据，不认 `App` 的字段布局。
pub struct Snapshot<'a> {
    pub views: &'a [NativeView],
    pub contributions: &'a [Contribution],
    pub doc_types: &'a [DocTypeInfo],
    pub services: &'a [ServiceInfo],
    pub plugins: &'a [PluginSummary],
}

/// 注册表：条目按登记序（原生视图 → 插件贡献 → 服务），装配期与投影刷新时整体重建，运行期只读。
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
    /// 领域文档类型按扩展名路由，不进选择器；保存完整注册以便打开 / 新建 / 图标共用一处。
    doc_types: Vec<DocTypeInfo>,
}

impl Registry {
    /// 按快照建表：原生视图 → 插件贡献 → 服务；文档类型另存一份。
    pub fn build(snapshot: Snapshot<'_>) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for view in snapshot.views {
            entries.push(Entry {
                id: view.id.to_string(),
                kind: EntryKind::View,
                label: view.label.to_string(),
                icon: Cow::Borrowed(view.icon),
                source: Source::Native,
                slot: None,
                enabled: true,
                replaceable: false,
                detail: String::new(),
                provider: None,
                builtin: false,
            });
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
                builtin: false,
            });
        }
        for info in snapshot.services {
            entries.push(Entry {
                id: info.service.clone(),
                kind: EntryKind::Service,
                label: info.service.clone(),
                icon: Cow::Borrowed("layers"),
                source: match &info.provider {
                    Some(provider) => Source::Plugin(provider.clone()),
                    None => Source::Native,
                },
                slot: None,
                enabled: true,
                replaceable: false,
                detail: service_detail(info),
                provider: info.provider.clone(),
                builtin: info.provider.is_none(),
            });
        }
        Self { entries, doc_types: snapshot.doc_types.to_vec() }
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

/// 插件是否启用：注册表里没有该插件（贡献与启用标记不同步的瞬间）按可用处理，
/// 宁可多列一项，也不让选择器里的标签闪断。
fn plugin_enabled(plugins: &[PluginSummary], plugin_id: &str) -> bool {
    plugins
        .iter()
        .find(|summary| summary.id == plugin_id)
        .is_none_or(|summary| summary.enabled)
}

/// 服务条目的一行说明：活动提供者 + 方法数。
fn service_detail(info: &ServiceInfo) -> String {
    let provider = info.provider.as_deref().unwrap_or("宿主");
    format!("{provider} · {} 个方法", info.methods.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::plugin_runtime::PluginStatus;

    fn views() -> [NativeView; 2] {
        [
            NativeView { id: "note", label: "笔记", icon: "file-text" },
            NativeView { id: "table", label: "表格", icon: "table" },
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
        }
    }

    fn service(name: &str, provider: Option<&str>) -> ServiceInfo {
        ServiceInfo {
            service: name.into(),
            version: 1,
            methods: vec!["read".into(), "write".into()],
            provider: provider.map(str::to_string),
        }
    }

    fn registry(enabled: bool) -> Registry {
        let views = views();
        let contributions = [contribution()];
        let plugins = [plugin_summary(enabled)];
        let services = [
            service("vault", None),
            service("query", None),
            service("table", None),
        ];
        Registry::build(Snapshot {
            views: &views,
            contributions: &contributions,
            doc_types: &[],
            services: &services,
            plugins: &plugins,
        })
    }

    #[test]
    fn native_and_plugin_entries_share_one_shape() {
        let registry = registry(true);
        let native = registry.get("note").expect("原生视图在册");
        let plugin = registry.get("plugin:com.a:models").expect("插件视图在册");
        assert_eq!(native.kind, EntryKind::View);
        assert_eq!(plugin.kind, EntryKind::View);
        assert_eq!(native.slot, None);
        assert_eq!(plugin.slot, Some(ContributionSlot::Panel));
        assert!(native.enabled && plugin.enabled);
        // 同一查询路径：选择器、标签与图标都不区分来源
        let ids: Vec<&str> = registry.views().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["note", "table", "plugin:com.a:models"]);
        assert_eq!(registry.label("plugin:com.a:models"), "模型面板");
        assert_eq!(registry.icon("plugin:com.a:models"), "layers");
        assert!(matches!(plugin.source, Source::Plugin(ref id) if id == "com.a"));
        assert_eq!(native.source, Source::Native);
    }

    #[test]
    fn disabled_plugin_hides_its_contributions_only() {
        let registry = registry(false);
        let ids: Vec<&str> = registry.views().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["note", "table"], "停用插件不再进选择器");
        assert!(!registry.get("plugin:com.a:models").expect("贡献仍在册").enabled);
        assert!(registry.get("note").expect("原生视图不受影响").enabled);
    }

    #[test]
    fn unknown_id_falls_back() {
        let registry = registry(true);
        assert!(registry.get("nope").is_none());
        assert!(registry.service("nope").is_none());
        assert_eq!(registry.label("nope"), "nope");
        assert_eq!(registry.icon("nope"), "puzzle");
        assert_eq!(registry.label("plugin:com.b:gone"), "com.b");
        assert_eq!(registry.icon("plugin:com.b:gone"), "puzzle");
    }

    #[test]
    fn view_and_service_namespaces_stay_separate() {
        let registry = registry(true);
        assert_eq!(registry.get("table").expect("视图").kind, EntryKind::View);
        assert_eq!(registry.service("table").expect("服务").kind, EntryKind::Service);
        assert!(registry.get("vault").is_none(), "服务名不冒充视图");
        assert!(registry.service("note").is_none(), "视图 id 不冒充服务");
    }

    #[test]
    fn nothing_in_the_registry_is_replaceable() {
        let registry = registry(true);
        assert!(!registry.get("note").expect("原生视图").replaceable);
        assert!(!registry.get("plugin:com.a:models").expect("插件视图").replaceable);
        let vault = registry.service("vault").expect("服务");
        assert!(!vault.replaceable, "内置服务归宿主，不可被插件顶替");
        assert!(vault.builtin);
        assert_eq!(vault.detail, "宿主 · 2 个方法");
    }
}
