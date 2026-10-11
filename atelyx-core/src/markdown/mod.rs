//! Markdown 内核：纯文本 → 源偏移标注块规格。
//!
//! 语义契约由对拍快照 `fixtures/markdown/parse.json` 锁死，
//! Rust 解析实现以快照为逐值期望（`tests/markdown_conformance.rs`）。

mod inline;
mod parse;
mod types;

pub use parse::{parse_inline, parse_markdown};
pub use types::{
    InlineSpan, LinkForm, MarkdownBlock, MarkdownDocument, MarkdownListItem, MarkdownTableCell,
    MentionCandidate, ParseOptions, RangeInfo,
};
