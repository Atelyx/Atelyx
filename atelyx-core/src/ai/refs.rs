//! # 引用与上下文注入的纯逻辑：类型词表、标签生成、命中扫描、注入块拼装。

/// 引用目标的形态（决定标签前缀与路径块的目录标注）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Note,
    Table,
    Canvas,
    Dir,
    /// 画布内已有卡（正文注入按卡类型另行映射，不进「引用文件」路径块）。
    Card,
}

impl RefKind {
    /// 标签前缀（与文件名以全角冒号相接）。
    pub fn prefix(self) -> &'static str {
        match self {
            RefKind::Note => "笔记",
            RefKind::Table => "表格",
            RefKind::Canvas => "画布",
            RefKind::Dir => "文件夹",
            RefKind::Card => "卡片",
        }
    }

    /// 消息 refs 字段的序列化键。
    pub fn key(self) -> &'static str {
        match self {
            RefKind::Note => "note",
            RefKind::Table => "table",
            RefKind::Canvas => "canvas",
            RefKind::Dir => "dir",
            RefKind::Card => "card",
        }
    }

    /// 序列化键解析（未知值 = None，调用方按不可引用丢弃）。
    pub fn parse_key(raw: &str) -> Option<Self> {
        match raw {
            "note" => Some(RefKind::Note),
            "table" => Some(RefKind::Table),
            "canvas" => Some(RefKind::Canvas),
            "dir" => Some(RefKind::Dir),
            "card" => Some(RefKind::Card),
            _ => None,
        }
    }
}

/// 按扩展名判定文件引用形态；目录由调用方直接给定 [`RefKind::Dir`]。
pub fn kind_of_file(path: &str) -> Option<RefKind> {
    let name = file_name(path);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "md" => Some(RefKind::Note),
        "atb" => Some(RefKind::Table),
        "atlx" => Some(RefKind::Canvas),
        _ => None,
    }
}

/// 路径末段（文件名或目录名；两种分隔符都认，尾随 `/` 忽略）。
fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

/// 标签内文件名的字符上限（超出截为前缀 + `...`）。
const LABEL_NAME_MAX: usize = 24;

/// 字符数超限的截断：保留头部，尾部补 `...`。
fn truncate_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

/// 标签内文件名的候选串：先文件名，重名逐级补父目录，兜底完整路径（保证唯一）。
fn label_candidates(path: &str) -> Vec<String> {
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let n = segs.len();
    // 从「仅文件名」开始，逐级向根方向补父目录，最后一段候选是完整路径。
    (0..n)
        .rev()
        .map(|from| segs[from..].join("/"))
        .collect()
}

/// 生成某引用的标签：文件名超长截断；候选串与既有标签撞车时逐级补父目录消歧，
/// 全部候选撞车（嵌套路径互为前后缀的病态构造）则以完整路径追加序号兜底。
pub fn make_label(path: &str, taken: &[(&str, &str)]) -> String {
    let conflict =
        |cand: &str| taken.iter().any(|(p, l)| *p != path && *l == cand);
    let candidates = label_candidates(path);
    for cand in &candidates {
        let truncated = truncate_tail(cand, LABEL_NAME_MAX);
        if !conflict(&truncated) {
            return truncated;
        }
    }
    // 兜底：未截断的完整路径仍撞车时追加序号（路径唯一 + 序号递增，必然收敛）。
    let base = candidates
        .last()
        .cloned()
        .unwrap_or_else(|| path.to_string());
    for n in 2u32.. {
        let cand = format!("{base} ({n})");
        if !conflict(&cand) {
            return cand;
        }
    }
    unreachable!("conflict 集合有限，序号递增必收敛")
}

/// 一条已登记引用（消息 refs 字段的内存形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    /// 文件 / 目录路径；画布内文本卡无文件时取节点 id（只作去重身份）。
    pub path: String,
    pub label: String,
    pub kind: RefKind,
    /// 画布卡引用：来源节点 id（消息 refs 落盘为 `nodeId`，消费判定按它命中）。
    /// 文本卡引用笔记文件时同样带 node_id（标签按「笔记」注入路径块）。
    pub node_id: Option<String>,
}

impl RefEntry {
    /// 输入框与正文中的完整标签文本。
    pub fn tag_text(&self) -> String {
        format!("#{}：{}", self.kind.prefix(), self.label)
    }
}

/// 一次正文命中（每条引用至多一处：首个实例）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefHit {
    pub path: String,
    pub kind: RefKind,
    /// 画布卡引用的来源节点 id（文件引用为 None）。
    pub node_id: Option<String>,
    /// 命中文本的字节起点（`#` 处）。
    pub start: usize,
    /// 命中文本的字节长度。
    pub len: usize,
}

/// 扫描正文中的引用命中：每条引用取首个实例，重叠命中丢弃，按位置升序。
pub fn scan_hits(text: &str, refs: &[RefEntry]) -> Vec<RefHit> {
    let mut found: Vec<RefHit> = refs
        .iter()
        .filter_map(|r| {
            text.find(&r.tag_text()).map(|start| RefHit {
                path: r.path.clone(),
                kind: r.kind,
                node_id: r.node_id.clone(),
                start,
                len: r.tag_text().len(),
            })
        })
        .collect();
    found.sort_by_key(|h| h.start);
    // 重叠丢弃（前一条命中盖住后一条的开头时后者让位）。
    let mut out: Vec<RefHit> = Vec::new();
    let mut prev_end = 0usize;
    for h in found {
        if h.start >= prev_end {
            prev_end = h.start + h.len;
            out.push(h);
        }
    }
    out
}

/// 路径块标注：目录引用以 `/` 结尾。
fn block_path(kind: RefKind, path: &str) -> String {
    if kind == RefKind::Dir {
        format!("{}/", path.trim_end_matches('/'))
    } else {
        path.to_string()
    }
}

/// 消息开头「引用文件」路径块（含尾随空行；命中按出现顺序、同路径去重）。
/// 画布卡引用由调用方按卡类型另行注入，不出现在路径块。
pub fn reference_path_block(hits: &[RefHit]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for h in hits {
        if h.kind == RefKind::Card || seen.contains(&h.path.as_str()) {
            continue;
        }
        seen.push(&h.path);
        lines.push(format!("- {}", block_path(h.kind, &h.path)));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("[引用文件：\n{}\n]\n\n", lines.join("\n"))
}

/// 一个打开中文件的注入标注（`viewing` = 对话面板所在窗口布局中正在查看）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFileCtx {
    pub path: String,
    pub viewing: bool,
}

/// 当前打开文件块（拼在末条 user 消息正文之后；无打开文件时不注入）。
pub fn open_files_context_block(files: &[OpenFileCtx]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut lines = vec!["<context>".to_string(), "用户当前打开的文件：".to_string()];
    for f in files {
        let marker = if f.viewing { "正在查看" } else { "其他窗口打开" };
        lines.push(format!("- {marker}：`{}`", f.path));
    }
    lines.push(
        "若与本次对话相关，用 read_file 按路径读取正文；读取之前不要声称已查看过该文件。"
            .to_string(),
    );
    lines.push("</context>".to_string());
    Some(lines.join("\n"))
}

/// 系统提示词引导段（Agent 工具名册含 read_file 时追加；与路径块、context 块配套）。
pub const FILE_REFERENCE_PROMPT: &str = "以 # 前缀引用的文件是用户明确指定的内容，其相对仓库根路径列在消息开头「引用文件」列表中。需要其内容时用 read_file 工具读取，在读取之前不要声称已查看过该文件。以 / 结尾的引用是目录：先用 list_dir 工具列出其中的内容，再按需 read_file；glob 用于按模式检索文件。以 . 开头的目录/文件（如 .git、.atelyx）不可被 AI 工具访问。已读取过的文件内容在对话历史中，无需重复读取；仅在需要确认内容是否变更时重读。";

/// `#` 是否构成引用触发（行首或前一个字符是空白才弹选择器；`#` 前是正文则只是普通文本）。
pub fn is_trigger_position(before: &str) -> bool {
    before.chars().next_back().is_none_or(|c| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: RefKind, label: &str) -> RefEntry {
        RefEntry {
            path: path.into(),
            label: label.into(),
            kind,
            node_id: None,
        }
    }

    #[test]
    fn kind_by_extension() {
        assert_eq!(kind_of_file("a/笔记.MD"), Some(RefKind::Note));
        assert_eq!(kind_of_file("b/表.atb"), Some(RefKind::Table));
        assert_eq!(kind_of_file("画布.atlx"), Some(RefKind::Canvas));
        assert_eq!(kind_of_file("图.png"), None);
    }

    #[test]
    fn label_truncates_long_names() {
        let long = "这是一个特别特别特别特别特别长的笔记文件名称超出上限.md";
        let label = make_label(long, &[]);
        assert_eq!(label.chars().count(), LABEL_NAME_MAX);
        assert!(label.ends_with("..."));
    }

    #[test]
    fn label_disambiguates_same_filename_with_parent() {
        let taken = [("a/plan.md", "plan.md")];
        assert_eq!(make_label("b/plan.md", &taken), "b/plan.md");
        // 独立文件同名不冲突（taken 只约束不同路径）
        assert_eq!(make_label("a/plan.md", &taken), "plan.md");
    }

    #[test]
    fn label_falls_back_to_full_path_when_all_collide() {
        // 两个长名共享前 21 字符：截断版互相撞车，靠补父目录消歧
        let long_a = "a/这是一个用来测试标签截断消歧的特别长的文件名甲乙丙丁.md";
        let long_b = "b/这是一个用来测试标签截断消歧的特别长的文件名子丑寅卯.md";
        let taken_a = make_label(long_a, &[]);
        assert!(taken_a.ends_with("..."));
        let taken = [(long_a, taken_a.as_str())];
        let label_b = make_label(long_b, &taken);
        assert_ne!(label_b, taken_a);
        assert!(label_b.ends_with("..."));
    }

    #[test]
    fn label_suffix_fallback_when_path_candidates_exhausted() {
        // 嵌套路径互为前后缀：所有候选撞车时以完整路径 + 序号兜底（不产生重复标签）
        let taken = [("y/b", "b"), ("x/a/b", "a/b")];
        let label = make_label("a/b", &taken);
        assert!(!taken.iter().any(|(_, l)| *l == label));
        assert!(label.starts_with("a/b"));
    }

    #[test]
    fn scan_hits_match_exact_tags_only() {
        let refs = vec![entry("a/笔记.md", RefKind::Note, "笔记.md")];
        let text = "看下#笔记：笔记.md 和 #随手记 还有 #笔记：笔记.md";
        let hits = scan_hits(text, &refs);
        // 每条引用取首个实例；未登记的 #随手记 不是引用
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a/笔记.md");
    }

    #[test]
    fn card_hits_carry_source_node_id() {
        let mut card = entry("node-1", RefKind::Card, "需求卡");
        card.node_id = Some("node-1".into());
        let hits = scan_hits("看 #卡片：需求卡 的内容", &[card]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, RefKind::Card);
        assert_eq!(hits[0].node_id.as_deref(), Some("node-1"));
        // 纯文件引用没有节点身份
        let hits = scan_hits("#笔记：a.md", &[entry("a.md", RefKind::Note, "a.md")]);
        assert_eq!(hits[0].node_id, None);
    }

    #[test]
    fn scan_hits_drop_unregistered_hash_text() {
        let refs = vec![entry("a/笔记.md", RefKind::Note, "笔记.md")];
        let hits = scan_hits("正文里的话题 #随手记 不是引用", &refs);
        assert!(hits.is_empty());
    }

    #[test]
    fn scan_hits_overlap_keeps_first() {
        // "#笔记：a.md" 既是「标签 a.md」的完整 needle，又以「标签 a」的 needle 为前缀延伸；
        // 短命中先到、长命中重叠让位
        let refs = vec![
            entry("a.md", RefKind::Note, "a"),
            entry("a.md", RefKind::Note, "a.md"),
        ];
        let hits = scan_hits("#笔记：a.md", &refs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].len, "#笔记：a".len());
    }

    #[test]
    fn path_block_dedupes_and_marks_dirs() {
        let hits = vec![
            RefHit { path: "a.md".into(), kind: RefKind::Note, node_id: None, start: 0, len: 1 },
            RefHit { path: "素材".into(), kind: RefKind::Dir, node_id: None, start: 1, len: 1 },
            RefHit { path: "a.md".into(), kind: RefKind::Note, node_id: None, start: 2, len: 1 },
            // 画布卡引用不进路径块（正文已按卡类型注入）
            RefHit { path: "node-1".into(), kind: RefKind::Card, node_id: Some("node-1".into()), start: 3, len: 1 },
        ];
        let block = reference_path_block(&hits);
        assert_eq!(block, "[引用文件：\n- a.md\n- 素材/\n]\n\n");
    }

    #[test]
    fn context_block_marks_viewing_and_other_windows() {
        let files = vec![
            OpenFileCtx { path: "a.md".into(), viewing: true },
            OpenFileCtx { path: "b.atb".into(), viewing: true },
            OpenFileCtx { path: "c.md".into(), viewing: false },
        ];
        let block = open_files_context_block(&files).unwrap();
        assert!(block.starts_with("<context>\n用户当前打开的文件：\n"));
        assert!(block.contains("- 正在查看：`a.md`"));
        assert!(block.contains("- 正在查看：`b.atb`"));
        assert!(block.contains("- 其他窗口打开：`c.md`"));
        assert!(block.trim_end().ends_with("</context>"));
    }

    #[test]
    fn context_block_absent_without_open_files() {
        assert!(open_files_context_block(&[]).is_none());
    }

    #[test]
    fn trigger_requires_line_start_or_whitespace() {
        assert!(is_trigger_position(""));
        assert!(is_trigger_position("hello "));
        assert!(is_trigger_position("第一行\n"));
        assert!(!is_trigger_position("abc#"));
        assert!(!is_trigger_position("C#"));
    }
}
