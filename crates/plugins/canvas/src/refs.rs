//! `#` 引用与卡内容注入的纯逻辑：标签生成、命中扫描、路径块拼装、按卡类型注入正文。
//! 与宿主同一口径（同一标签字面量、同一路径块格式），插件侧不读盘。

/// 引用目标的形态（决定标签前缀与路径块的目录标注）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Note,
    Table,
    Canvas,
    Dir,
    /// 画布内已有卡：正文按卡类型注入，不进「引用文件」路径块。
    Card,
}

impl RefKind {
    pub fn prefix(self) -> &'static str {
        match self {
            RefKind::Note => "笔记",
            RefKind::Table => "表格",
            RefKind::Canvas => "画布",
            RefKind::Dir => "文件夹",
            RefKind::Card => "卡片",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            RefKind::Note => "note",
            RefKind::Table => "table",
            RefKind::Canvas => "canvas",
            RefKind::Dir => "dir",
            RefKind::Card => "card",
        }
    }

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

fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

const LABEL_NAME_MAX: usize = 24;

fn truncate_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn label_candidates(path: &str) -> Vec<String> {
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let n = segs.len();
    (0..n).rev().map(|from| segs[from..].join("/")).collect()
}

/// 生成引用标签：超长截断；同名逐级补父目录消歧，全部候选撞车时以完整路径追加序号兜底。
pub fn make_label(path: &str, taken: &[(&str, &str)]) -> String {
    let conflict = |cand: &str| taken.iter().any(|(p, l)| *p != path && *l == cand);
    let candidates = label_candidates(path);
    for cand in &candidates {
        let truncated = truncate_tail(cand, LABEL_NAME_MAX);
        if !conflict(&truncated) {
            return truncated;
        }
    }
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

/// 一条已登记引用（草稿与消息 refs 的共同内存形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub path: String,
    pub label: String,
    pub kind: RefKind,
    /// 画布卡引用的来源节点 id；笔记卡引用同样带节点 id（标签仍按「笔记」注入路径块）。
    pub node_id: Option<String>,
}

impl RefEntry {
    pub fn tag_text(&self) -> String {
        format!("#{}：{}", self.kind.prefix(), self.label)
    }
}

/// 一次正文命中（每条引用至多一处：首个实例）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefHit {
    pub path: String,
    pub kind: RefKind,
    pub node_id: Option<String>,
    pub start: usize,
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

fn block_path(kind: RefKind, path: &str) -> String {
    if kind == RefKind::Dir {
        format!("{}/", path.trim_end_matches('/'))
    } else {
        path.to_string()
    }
}

/// 消息开头的「引用文件」路径块（含尾随空行；同路径去重，卡引用不进块）。
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

/// `#` 是否构成引用触发（行首或前一字符是空白才算）。
pub fn is_trigger_position(before: &str) -> bool {
    before.chars().next_back().is_none_or(|c| c.is_whitespace())
}

/// 一张被引用卡的可用内容形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardSource {
    InlineText { title: String, body: String },
    TableText { title: String, body: String },
    MediaText { title: String, body: String },
    Image { title: String, file: String },
    Binary { title: String },
    Missing { title: String },
}

/// 一张卡的引用身份与内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardContent {
    pub node_id: String,
    pub source: CardSource,
}

/// 命中扫描结果 → 卡片注入块与图片附件路径（按命中顺序，同卡只取一次）。
pub fn card_injection(hits: &[RefHit], contents: &[CardContent]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut images: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for hit in hits {
        if hit.kind != RefKind::Card {
            continue;
        }
        let Some(node_id) = hit.node_id.as_deref() else {
            continue;
        };
        if seen.contains(&node_id) {
            continue;
        }
        seen.push(node_id);
        let Some(content) = contents.iter().find(|c| c.node_id == node_id) else {
            text.push_str("## 画布卡「已不在画布上」\n\n");
            continue;
        };
        match &content.source {
            CardSource::InlineText { title, body }
            | CardSource::TableText { title, body }
            | CardSource::MediaText { title, body } => {
                text.push_str(&format!("## 画布卡「{title}」\n{body}\n\n"));
            }
            CardSource::Image { title, file } => {
                if !images.contains(file) {
                    images.push(file.clone());
                }
                text.push_str(&format!(
                    "## 画布卡「{title}」（图片，作为附件随消息发送）\n\n"
                ));
            }
            CardSource::Binary { title } => {
                text.push_str(&format!(
                    "## 画布卡「{title}」（二进制附件，无法以文本读取）\n\n"
                ));
            }
            CardSource::Missing { title } => {
                text.push_str(&format!("## 画布卡「{title}」（内容缺失）\n\n"));
            }
        }
    }
    (text, images)
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
    fn path_block_dedupes_and_marks_dirs() {
        let hits = vec![
            RefHit {
                path: "a.md".into(),
                kind: RefKind::Note,
                node_id: None,
                start: 0,
                len: 1,
            },
            RefHit {
                path: "素材".into(),
                kind: RefKind::Dir,
                node_id: None,
                start: 1,
                len: 1,
            },
            RefHit {
                path: "a.md".into(),
                kind: RefKind::Note,
                node_id: None,
                start: 2,
                len: 1,
            },
            RefHit {
                path: "node-1".into(),
                kind: RefKind::Card,
                node_id: Some("node-1".into()),
                start: 3,
                len: 1,
            },
        ];
        assert_eq!(
            reference_path_block(&hits),
            "[引用文件：\n- a.md\n- 素材/\n]\n\n"
        );
    }

    #[test]
    fn scan_hits_are_position_sorted_and_overlap_dropped() {
        let refs = vec![
            entry("a.md", RefKind::Note, "a"),
            entry("a.md", RefKind::Note, "a.md"),
        ];
        let hits = scan_hits("#笔记：a.md", &refs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].len, "#笔记：a".len());
    }

    #[test]
    fn card_injection_orders_by_hits_and_keeps_images_out_of_text() {
        let hits = vec![
            RefHit {
                path: "b".into(),
                kind: RefKind::Card,
                node_id: Some("b".into()),
                start: 0,
                len: 1,
            },
            RefHit {
                path: "a".into(),
                kind: RefKind::Card,
                node_id: Some("a".into()),
                start: 3,
                len: 1,
            },
            RefHit {
                path: "img".into(),
                kind: RefKind::Card,
                node_id: Some("img".into()),
                start: 6,
                len: 1,
            },
        ];
        let contents = vec![
            CardContent {
                node_id: "a".into(),
                source: CardSource::InlineText {
                    title: "A".into(),
                    body: "正文".into(),
                },
            },
            CardContent {
                node_id: "b".into(),
                source: CardSource::TableText {
                    title: "B".into(),
                    body: "列".into(),
                },
            },
            CardContent {
                node_id: "img".into(),
                source: CardSource::Image {
                    title: "图".into(),
                    file: "a.png".into(),
                },
            },
        ];
        let (text, images) = card_injection(&hits, &contents);
        assert!(text.find("B").unwrap() < text.find("A").unwrap());
        assert_eq!(images, vec!["a.png".to_string()]);
        assert!(!text.contains("a.png"));
    }
}
