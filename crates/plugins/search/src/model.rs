//! 面板状态：宿主给的仓库文件清单副本 + 查询词与本地命中排序。
//! 插件不持磁盘真源：清单每次由 `vault.files` 整份替换，过滤与排序都在这一份副本上做。

use atelyx_plugin_protocol::method::{FilesResult, Radii};

/// 单次渲染的结果行上限：列表非虚拟化，上限即渲染预算（超出在计数行说明）。
pub const MAX_ROWS: usize = 200;

/// 清单里的一行：图标键与能否打开都由宿主判定，插件原样呈现。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 仓库相对路径（`/` 分隔）。
    pub path: String,
    pub name: String,
    /// 宿主注册表的图标键。
    pub icon: String,
    /// 有面板内打开面（图片附件这类 = false，行只呈现、不挂点击）。
    pub openable: bool,
}

/// 一条要画的结果行；`index` 是清单下标（命中区 id 用它定位点击目标）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub index: usize,
    /// 文件名。
    pub name: String,
    /// 所在文件夹（仓库根下的文件为空串 = 不出次行）。
    pub parent: String,
    pub icon: String,
    pub openable: bool,
}

/// 空态形态：无可搜索文件 / 有文件但无命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Empty {
    None,
    NoFiles,
    NoMatch,
}

/// 一帧要画的内容（与 `Model` 解耦，渲染期间不持锁）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// 清单世代：行命中区 id 带着它，过期帧上的点击据此作废。
    pub gen: u64,
    /// 首份清单（或切仓库后的首份）尚未取到：只画加载态，不判定空态。
    pub loading: bool,
    /// 取数失败的原因（保留上次成功的清单；空清单 + 失败给可读空态）。
    pub error: Option<String>,
    pub rows: Vec<Row>,
    /// 计数行文字（`None` = 不出这一行）。
    pub count: Option<String>,
    pub empty: Empty,
}

/// 面板状态：清单副本 + 查询词。
#[derive(Clone)]
pub struct Model {
    entries: Vec<Entry>,
    query: String,
    /// 行命中区世代：清单或查询变化即 +1，过期帧上的点击据此作废。
    gen: u64,
    /// 圆角档取自宿主主题。
    pub radii: Radii,
    /// 首份清单是否已到：未到时视图画加载态，不判空态。
    loaded: bool,
    /// 取数失败的原因（清单保持上次成功的值）。
    error: Option<String>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            query: String::new(),
            gen: 0,
            radii: Radii::default(),
            loaded: false,
            error: None,
        }
    }

    pub fn set_radii(&mut self, radii: Radii) {
        self.radii = radii;
    }

    /// 整份清单替换（宿主每次给的就是当前仓库的完整清单，树序）。
    pub fn apply(&mut self, result: FilesResult) {
        self.entries = result
            .files
            .into_iter()
            .map(|file| Entry {
                path: file.path,
                name: file.name,
                icon: file.icon,
                openable: file.openable,
            })
            .collect();
        self.gen = self.gen.wrapping_add(1);
        self.loaded = true;
        self.error = None;
    }

    /// 记一次取数失败：保留已有清单，空清单时由视图给可读空态。
    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.gen = self.gen.wrapping_add(1);
        self.loaded = true;
    }

    /// 切仓库：丢掉旧仓库的清单，回到未就绪（取到首份前只画加载态）。
    pub fn begin_load(&mut self) {
        self.entries.clear();
        self.gen = self.gen.wrapping_add(1);
        self.loaded = false;
        self.error = None;
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// 改写查询词；内容真的变了才推进世代。
    pub fn set_query(&mut self, query: String) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.gen = self.gen.wrapping_add(1);
    }

    pub fn gen(&self) -> u64 {
        self.gen
    }

    /// 按清单下标取条目（点击回执的编号与当前世代不符 = 过期帧）。
    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// 当前要画的一帧：命中排序后取前 [`MAX_ROWS`] 条 + 计数行 + 空态判定。
    pub fn view(&self) -> View {
        let hits = hits(&self.entries, &self.query);
        let total = hits.len();
        let rows: Vec<Row> = hits
            .into_iter()
            .take(MAX_ROWS)
            .map(|index| {
                let entry = &self.entries[index];
                Row {
                    index,
                    name: entry.name.clone(),
                    parent: entry
                        .path
                        .rsplit_once('/')
                        .map(|(dir, _)| dir.to_string())
                        .unwrap_or_default(),
                    icon: entry.icon.clone(),
                    openable: entry.openable,
                }
            })
            .collect();
        let empty = if self.entries.is_empty() {
            Empty::NoFiles
        } else if total == 0 {
            Empty::NoMatch
        } else {
            Empty::None
        };
        View {
            gen: self.gen,
            loading: !self.loaded,
            error: self.error.clone(),
            count: count_label(total, rows.len(), &self.query),
            rows,
            empty,
        }
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// 命中档位 = 排序键（文件名前缀 > 文件名包含 > 仅路径命中）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    NamePrefix,
    NameContains,
    PathContains,
}

/// 在清单上按查询词检索：大小写不敏感，同档保持清单顺序（清单本身即树序）。
/// 空白查询 = 全部命中、原序返回。
fn hits(entries: &[Entry], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut ranked: Vec<(Rank, usize)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let name = entry.name.to_lowercase();
        let rank = if name.starts_with(&needle) {
            Rank::NamePrefix
        } else if name.contains(&needle) {
            Rank::NameContains
        } else if entry.path.to_lowercase().contains(&needle) {
            Rank::PathContains
        } else {
            continue;
        };
        ranked.push((rank, index));
    }
    ranked.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    ranked.into_iter().map(|(_, index)| index).collect()
}

/// 计数行：空白查询给文件总数，有查询给命中数；超渲染上限说明只列前几条。
/// 无文件 / 无匹配由空态承载，此处不出行。
fn count_label(total: usize, shown: usize, query: &str) -> Option<String> {
    if total == 0 {
        return None;
    }
    Some(if query.trim().is_empty() {
        format!("共 {total} 个文件")
    } else if shown < total {
        format!("{total} 个结果，仅列前 {shown} 个")
    } else {
        format!("{total} 个结果")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_at(path: &str) -> Entry {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Entry { path: path.to_string(), name, icon: "sticky-note".into(), openable: true }
    }

    fn entries(paths: &[&str]) -> Vec<Entry> {
        paths.iter().map(|path| entry_at(path)).collect()
    }

    fn ordered(items: &[Entry], query: &str) -> Vec<String> {
        hits(items, query).into_iter().map(|index| items[index].path.clone()).collect()
    }

    fn model_with(paths: &[&str]) -> Model {
        let mut model = Model::new();
        model.apply(FilesResult {
            files: entries(paths)
                .into_iter()
                .map(|entry| atelyx_plugin_protocol::method::VaultFileEntry {
                    path: entry.path,
                    name: entry.name,
                    icon: entry.icon,
                    openable: entry.openable,
                })
                .collect(),
        });
        model
    }

    /// 空白查询 = 全部命中、保持清单（树）序。
    #[test]
    fn blank_query_keeps_every_entry_in_order() {
        let items = entries(&["a.md", "docs/b.md", "z.atb"]);
        assert_eq!(ordered(&items, ""), ["a.md", "docs/b.md", "z.atb"]);
        assert_eq!(ordered(&items, "   "), ["a.md", "docs/b.md", "z.atb"]);
    }

    /// 档位顺序：文件名前缀 > 文件名包含 > 仅路径命中；同档保持清单序。
    #[test]
    fn ranks_name_prefix_then_name_contains_then_path_hit() {
        let items = entries(&["plan/note.md", "notes/my-plan.md", "planning.md", "docs/plan.md"]);
        assert_eq!(
            ordered(&items, "plan"),
            ["planning.md", "docs/plan.md", "notes/my-plan.md", "plan/note.md"]
        );
    }

    #[test]
    fn matches_case_insensitively() {
        let items = entries(&["Notes/Plan.md"]);
        assert_eq!(ordered(&items, "PLAN"), ["Notes/Plan.md"]);
        assert_eq!(ordered(&items, "notes/"), ["Notes/Plan.md"]);
    }

    /// 中文词与完整树序：前缀命中在前，仅路径命中在后，无关项剔除。
    #[test]
    fn ranks_chinese_queries_in_tree_order() {
        let items = entries(&[
            "归档/会议纪要.md",
            "周报/2026-10.md",
            "项目/周报归档/2025.md",
            "周报汇总.md",
            "杂项/关于周报的思考.md",
            "其它/无关.md",
        ]);
        assert_eq!(
            ordered(&items, "周报"),
            ["周报汇总.md", "杂项/关于周报的思考.md", "周报/2026-10.md", "项目/周报归档/2025.md"]
        );
    }

    /// 文件名没命中才看路径：只出现在路径里的词算最低档。
    #[test]
    fn path_hit_only_when_name_misses() {
        let items = entries(&["a.md", "archive/b.md"]);
        assert_eq!(ordered(&items, "archive"), ["archive/b.md"]);
    }

    /// 渲染上限只截断行，不影响命中总数与计数文案。
    #[test]
    fn render_cap_truncates_rows_but_not_total() {
        let paths: Vec<String> = (0..MAX_ROWS + 5).map(|i| format!("note-{i}.md")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut model = model_with(&refs);
        model.set_query("note".into());
        let view = model.view();
        assert_eq!(view.rows.len(), MAX_ROWS);
        assert_eq!(
            view.count.as_deref(),
            Some(format!("{} 个结果，仅列前 {MAX_ROWS} 个", MAX_ROWS + 5).as_str())
        );
    }

    /// 计数行：空白查询给文件总数，命中数不多于上限时只说结果数；无命中时不出行。
    #[test]
    fn count_label_switches_between_total_and_results() {
        let mut model = model_with(&["a.md", "b.md"]);
        assert_eq!(model.view().count.as_deref(), Some("共 2 个文件"));
        model.set_query("a".into());
        assert_eq!(model.view().count.as_deref(), Some("1 个结果"));
        model.set_query("missing".into());
        assert_eq!(model.view().count, None);
        assert_eq!(model.view().empty, Empty::NoMatch);
    }

    /// 行带所在文件夹（根下的文件不出次行）。
    #[test]
    fn rows_carry_parent_folder() {
        let model = model_with(&["docs/plan.md", "root.md"]);
        let view = model.view();
        assert_eq!(view.rows[0].parent, "docs");
        assert_eq!(view.rows[1].parent, "");
    }

    /// 首份结果未到（初始 / 切仓库）只画加载态；成功或失败回执到了才允许判空态。
    #[test]
    fn loading_gate_closes_until_first_result() {
        let mut model = Model::new();
        assert!(model.view().loading, "初始未取数");
        assert_eq!(model.view().empty, Empty::NoFiles, "空态结论先不给视图用");

        model.apply(FilesResult::default());
        assert!(!model.view().loading, "首份清单到了");

        model.begin_load();
        let view = model.view();
        assert!(view.loading, "切仓库回到未就绪");
        assert!(view.rows.is_empty(), "旧仓库清单不能留在新仓库里");
        assert_eq!(view.error, None);

        model.fail("宿主未响应".into());
        assert!(!model.view().loading, "失败回执也结束等待（给失败态）");
    }

    /// 空清单分两种：没有可搜索文件 / 取数失败（失败态由视图给可读原因与重试）。
    #[test]
    fn empty_view_distinguishes_no_files_from_failure() {
        let mut model = Model::new();
        assert_eq!(model.view().empty, Empty::NoFiles);
        assert_eq!(model.view().error, None);
        model.fail("宿主未响应".into());
        let view = model.view();
        assert_eq!(view.empty, Empty::NoFiles);
        assert_eq!(view.error.as_deref(), Some("宿主未响应"));
        model.apply(FilesResult::default());
        assert_eq!(model.view().error, None, "成功取数清掉失败态");
    }

    /// 查询与清单变化都推进世代（过期帧上的点击据此作废）；同样的查询不推进。
    #[test]
    fn generation_moves_on_list_or_query_change() {
        let mut model = Model::new();
        model.apply(FilesResult::default());
        let after_list = model.gen();
        model.set_query("a".into());
        assert_ne!(model.gen(), after_list);
        let after_query = model.gen();
        model.set_query("a".into());
        assert_eq!(model.gen(), after_query);
    }
}
