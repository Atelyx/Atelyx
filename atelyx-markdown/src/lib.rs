//! Markdown 解析：纯文本 → 源偏移标注块规格。纯逻辑、无 I/O，宿主与插件共用同一份语义。
//!
//! 语义契约由对拍快照 `fixtures/markdown/parse.json` 锁死，
//! 解析实现以快照为逐值期望（`tests/conformance.rs`）。

mod inline;
mod parse;
mod types;

pub use parse::{block_from, parse_inline, parse_markdown};
pub use types::{
    InlineSpan, LinkForm, MarkdownBlock, MarkdownDocument, MarkdownListItem, MarkdownTableCell,
    MentionCandidate, ParseOptions, RangeInfo,
};
