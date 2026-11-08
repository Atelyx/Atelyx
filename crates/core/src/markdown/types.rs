//! Markdown 内核类型：纯文本 → 源偏移标注块规格（框架无关）。
//!
//! 所有 from/to 均为源文本绝对偏移；serde 形状即对拍快照
//! （`fixtures/markdown/parse.json`）的 JSON 形状（`tag = "kind"`、camelCase 字段、
//! 可选字段缺值出 `null`）。

use serde::{Deserialize, Serialize};

/// 通用区间（源文本绝对偏移）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeInfo {
    pub from: usize,
    pub to: usize,
}

/// 链接形态：外链 / wiki / 仓库路径 / 可新建 / 未识别（保持原文）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkForm {
    External,
    Wiki,
    Path,
    Create,
    Plain,
}

/// 行内片段。from/to 为源文本绝对偏移（编辑面靠它做 offset ↔ 渲染位置映射）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InlineSpan {
    #[serde(rename = "text")]
    Text { from: usize, to: usize, text: String },
    #[serde(rename = "strong")]
    Strong {
        from: usize,
        to: usize,
        children: Vec<InlineSpan>,
    },
    #[serde(rename = "em")]
    Em {
        from: usize,
        to: usize,
        children: Vec<InlineSpan>,
    },
    #[serde(rename = "strike")]
    Strike {
        from: usize,
        to: usize,
        children: Vec<InlineSpan>,
    },
    #[serde(rename = "highlight")]
    Highlight {
        from: usize,
        to: usize,
        children: Vec<InlineSpan>,
    },
    #[serde(rename = "code")]
    Code { from: usize, to: usize, text: String },
    #[serde(rename = "mathInline")]
    MathInline { from: usize, to: usize, tex: String },
    #[serde(rename = "comment")]
    Comment { from: usize, to: usize },
    #[serde(rename = "link")]
    Link {
        from: usize,
        to: usize,
        href: String,
        label: String,
        form: LinkForm,
        title: Option<String>,
    },
    #[serde(rename = "autolink")]
    Autolink { from: usize, to: usize, href: String, label: String },
    #[serde(rename = "wiki")]
    Wiki { from: usize, to: usize, target: String, label: String },
    #[serde(rename = "image")]
    Image {
        from: usize,
        to: usize,
        src: String,
        alt: String,
        width: Option<String>,
        height: Option<String>,
        title: Option<String>,
    },
    #[serde(rename = "tag")]
    Tag { from: usize, to: usize, tag: String },
    #[serde(rename = "footnoteRef")]
    FootnoteRef { from: usize, to: usize, label: String },
    #[serde(rename = "mention")]
    Mention {
        from: usize,
        to: usize,
        key: String,
        label: String,
        #[serde(rename = "char")]
        trigger: char,
    },
    #[serde(rename = "html")]
    Html { from: usize, to: usize, html: String },
    #[serde(rename = "hardBreak")]
    HardBreak { from: usize, to: usize },
}

/// 列表项：from/to 为项的源区间；children 为项内嵌套块（嵌套列表、松散项的后续段落等）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkdownListItem {
    pub from: usize,
    pub to: usize,
    /// 任务项（源文本带 `[ ]`/`[x]` 勾选框标记）。
    pub task: bool,
    pub checked: bool,
    pub children: Vec<MarkdownBlock>,
}

/// 表格单元格：内容以源区间承载（单元格行内渲染按区间重解析，与正文同一套行内语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownTableCell {
    pub from: usize,
    pub to: usize,
}

/// 块级规格（顶层块；引用块与列表项递归持有内层块）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MarkdownBlock {
    #[serde(rename = "heading")]
    Heading {
        level: u8,
        from: usize,
        to: usize,
        inline: Vec<InlineSpan>,
    },
    #[serde(rename = "paragraph")]
    Paragraph {
        from: usize,
        to: usize,
        inline: Vec<InlineSpan>,
    },
    #[serde(rename = "blockquote")]
    Blockquote {
        from: usize,
        to: usize,
        callout: Option<String>,
        children: Vec<MarkdownBlock>,
    },
    #[serde(rename = "list")]
    List {
        ordered: bool,
        from: usize,
        to: usize,
        items: Vec<MarkdownListItem>,
    },
    #[serde(rename = "fencedCode")]
    FencedCode {
        from: usize,
        to: usize,
        lang: String,
        code: String,
        #[serde(rename = "contentFrom")]
        content_from: usize,
        #[serde(rename = "contentTo")]
        content_to: usize,
    },
    #[serde(rename = "indentedCode")]
    IndentedCode { from: usize, to: usize, code: String },
    #[serde(rename = "hr")]
    Hr { from: usize, to: usize },
    #[serde(rename = "table")]
    Table {
        from: usize,
        to: usize,
        header: Vec<MarkdownTableCell>,
        aligns: Vec<String>,
        rows: Vec<Vec<MarkdownTableCell>>,
    },
    #[serde(rename = "mathBlock")]
    MathBlock { from: usize, to: usize, tex: String },
    #[serde(rename = "htmlBlock")]
    HtmlBlock { from: usize, to: usize, html: String },
    #[serde(rename = "footnoteDef")]
    FootnoteDef {
        from: usize,
        to: usize,
        label: String,
        inline: Vec<InlineSpan>,
    },
}

/// 解析产物：原文 + 顶层块序列。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkdownDocument {
    pub source: String,
    pub blocks: Vec<MarkdownBlock>,
}

/// 链接形态判定闭包。
pub type ResolveLinkFn = Box<dyn Fn(&str) -> LinkForm + 'static>;

/// 解析选项。
#[derive(Default)]
pub struct ParseOptions {
    /// 链接形态判定（external/create 由内核内置判定，此项只决定其余 href 的形态；缺省全 "plain"）。
    pub resolve_link: Option<ResolveLinkFn>,
    /// mention 胶囊候选（`@` 或 `#` 触发）。
    pub mentions: Vec<MentionCandidate>,
}

impl std::fmt::Debug for ParseOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseOptions")
            .field("resolve_link", &self.resolve_link.as_ref().map(|_| "fn"))
            .field("mentions", &self.mentions)
            .finish()
    }
}

/// mention 候选。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionCandidate {
    pub key: String,
    pub label: String,
}
