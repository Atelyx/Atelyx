//! 笔记插件的展示与编辑状态：三档模式、撤销栈、预览编辑的活动块与反链缓存。
//! 正文真源在宿主（`note.*`），这里只持本端副本。

use atelyx_markdown::{parse_markdown, MarkdownBlock, ParseOptions};

/// 面板贡献 id：也是文档实例的 surfaceId（`plugin.json` 的 `docTypeRegistrations` 同值）。
pub const PANEL: &str = "note";
/// 源码编辑面 id。
pub const SOURCE: &str = "note:source";
/// 源码模式外层滚动容器 id。
pub const SOURCE_SCROLL: &str = "note:source-scroll";
/// 只读模式滚动容器 id。
pub const READ_SCROLL: &str = "note:read-scroll";
/// 撤销栈深度（全文快照）。
const UNDO_DEPTH: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Read,
    Edit,
    Source,
}

/// 一条反链（点击经 `editor.openFile` 交宿主打开）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlink {
    pub path: String,
    pub title: String,
}

#[derive(Clone)]
pub struct NoteModel {
    pub path: Option<String>,
    pub text: String,
    pub can_write: bool,
    pub mode: Mode,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 打开失败原因：面板无可显示正文时给出可见原因，不静默。
    pub open_error: Option<String>,
    pub backlinks: Vec<Backlink>,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
    /// 预览编辑的活动块（顶层块下标）与块源区间。
    pub active_block: Option<usize>,
    pub blocks: Vec<(usize, usize)>,
    /// 正文起点（frontmatter 之后）；块区间相对正文。
    pub body_start: usize,
}

impl Default for NoteModel {
    fn default() -> Self {
        Self {
            path: None,
            text: String::new(),
            can_write: true,
            mode: Mode::Read,
            dirty: false,
            save_error: None,
            open_error: None,
            backlinks: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            active_block: None,
            blocks: Vec::new(),
            body_start: 0,
        }
    }
}

impl NoteModel {
    pub fn is_open(&self) -> bool {
        self.path.is_some()
    }

    /// 载入正文（打开文件 / 宿主回填）：撤销历史与活动块全部作废。
    pub fn load(&mut self, path: String, text: String, can_write: bool) {
        self.path = Some(path);
        self.text = text;
        self.can_write = can_write;
        self.mode = Mode::Read;
        self.dirty = false;
        self.save_error = None;
        self.open_error = None;
        self.backlinks.clear();
        self.undo.clear();
        self.redo.clear();
        self.active_block = None;
        self.refresh_blocks();
    }

    /// 关闭当前笔记：正文、撤销历史与活动块清空，回到只读档。
    pub fn clear(&mut self) {
        self.path = None;
        self.text.clear();
        self.dirty = false;
        self.save_error = None;
        self.open_error = None;
        self.backlinks.clear();
        self.undo.clear();
        self.redo.clear();
        self.active_block = None;
        self.blocks.clear();
        self.body_start = 0;
        self.mode = Mode::Read;
    }

    /// 宿主改写正文（协作远端合入 / 属性面板 / 保存合流）：撤销栈作废，
    /// 旧快照回放会把远端内容整体改回去。
    pub fn replace_text(&mut self, text: String) {
        self.text = text;
        self.undo.clear();
        self.redo.clear();
        self.refresh_blocks();
    }

    /// 本端编辑：入撤销栈并重算块区间。
    pub fn edit_text(&mut self, text: String) {
        if self.text == text {
            return;
        }
        self.undo.push(std::mem::replace(&mut self.text, text));
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
    }

    pub fn undo(&mut self) -> Option<String> {
        let previous = self.undo.pop()?;
        self.redo.push(std::mem::replace(&mut self.text, previous));
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
        Some(self.text.clone())
    }

    pub fn redo(&mut self) -> Option<String> {
        let next = self.redo.pop()?;
        self.undo.push(std::mem::replace(&mut self.text, next));
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
        Some(self.text.clone())
    }

    /// 块的可编辑区间（绝对偏移）：块自身区间 + 其后到下一块开始之间的空行。
    /// 不含行尾空行时，块内回车打进分隔区，重新解析又被剥掉，表现为按键无反应。
    pub fn block_source(&self, index: usize) -> Option<(usize, usize)> {
        let (from, _) = *self.blocks.get(index)?;
        let to = self
            .blocks
            .get(index + 1)
            .map_or(self.text.len(), |(next, _)| self.body_start + *next);
        Some((self.body_start + from, to))
    }

    /// 用编辑后的块正文替换原块区间，返回新全文。
    pub fn replace_block(&self, index: usize, source: &str) -> Option<String> {
        let (from, to) = self.block_source(index)?;
        let mut out = String::with_capacity(self.text.len() + source.len());
        out.push_str(&self.text[..from]);
        out.push_str(source);
        out.push_str(&self.text[to..]);
        Some(out)
    }

    /// 活动块的源文（绝对偏移区间）。
    pub fn active_source(&self) -> Option<&str> {
        let (from, to) = self.block_source(self.active_block?)?;
        Some(&self.text[from..to])
    }

    fn refresh_blocks(&mut self) {
        self.body_start = body_start(&self.text);
        let body = &self.text[self.body_start..];
        let doc = parse_markdown(body, &ParseOptions::default());
        self.blocks = doc.blocks.iter().map(block_range).collect();
        if self.active_block.is_some_and(|index| index >= self.blocks.len()) {
            self.active_block = None;
        }
    }
}

/// frontmatter 结束位置（闭合行行尾，含换行）；无闭合块 = 0。
fn body_start(text: &str) -> usize {
    let Some(open_end) = text.find('\n') else { return 0 };
    if !text[..open_end].starts_with("---") {
        return 0;
    }
    let mut pos = open_end;
    for line in text[open_end..].split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        if trimmed.starts_with("---") {
            return pos + line.len();
        }
        pos += line.len();
    }
    0
}

fn block_range(block: &MarkdownBlock) -> (usize, usize) {
    match block {
        MarkdownBlock::Heading { from, to, .. }
        | MarkdownBlock::Paragraph { from, to, .. }
        | MarkdownBlock::Blockquote { from, to, .. }
        | MarkdownBlock::List { from, to, .. }
        | MarkdownBlock::FencedCode { from, to, .. }
        | MarkdownBlock::IndentedCode { from, to, .. }
        | MarkdownBlock::Hr { from, to }
        | MarkdownBlock::Table { from, to, .. }
        | MarkdownBlock::MathBlock { from, to, .. }
        | MarkdownBlock::HtmlBlock { from, to, .. }
        | MarkdownBlock::FootnoteDef { from, to, .. } => (*from, *to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_starts_after_closed_frontmatter_only() {
        assert_eq!(body_start("---\ntitle: a\n---\n正文"), 17);
        assert_eq!(body_start("---\ntitle: a\n正文"), 0, "未闭合不剥");
        assert_eq!(body_start("正文"), 0);
    }

    #[test]
    fn block_replacement_keeps_rest_of_note() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\nt: x\n---\n# 标题\n\n段落\n".into(), true);
        assert_eq!(model.blocks.len(), 2);
        let edited = model.replace_block(0, "# 新标题\n\n").expect("块在");
        assert_eq!(edited, "---\nt: x\n---\n# 新标题\n\n段落\n");
        let edited = model.replace_block(1, "新段落\n").expect("块在");
        assert_eq!(edited, "---\nt: x\n---\n# 标题\n\n新段落\n");
    }

    #[test]
    fn block_source_keeps_trailing_blank_lines() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "段落\n".into(), true);
        let (from, to) = model.block_source(0).expect("块在");
        assert_eq!(&model.text[from..to], "段落\n", "块尾空行归编辑面，回车才留得住");
    }

    #[test]
    fn remote_replace_clears_undo_history() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "一".into(), true);
        model.edit_text("一二".into());
        assert_eq!(model.undo.len(), 1);
        model.replace_text("三".into());
        assert!(model.undo.is_empty() && model.redo.is_empty());
    }
}
