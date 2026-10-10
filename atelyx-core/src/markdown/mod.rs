//! Markdown 内核（Rust 侧）：纯文本 → 源偏移标注块规格。
//!
//! 语义契约 = 前端内核（`src/utils/markdownCore`）；对拍快照
//! `fixtures/markdown/parse.json`（`MD_DUMP=1` 重新生成）锁死语义，
//! Rust 解析实现以快照为逐值期望（`tests/markdown_conformance.rs`）。

mod inline;
mod parse;
mod types;

pub use parse::parse_markdown;
pub use types::{
    InlineSpan, LinkForm, MarkdownBlock, MarkdownDocument, MarkdownListItem, MarkdownTableCell,
    MentionCandidate, ParseOptions, RangeInfo,
};
