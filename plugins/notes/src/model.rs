//! 笔记插件的展示与编辑状态：三档模式、正文块、撤销栈、属性面板、反链与远端光标。
//! 正文真源在宿主（`doc.*`），这里只持本端副本。

use std::collections::HashMap;

use atelyx_format::frontmatter::{self, Yaml};
use atelyx_format::markdown::{parse_markdown, InlineSpan, MarkdownBlock, ParseOptions};
use atelyx_plugin_protocol::method::EditorPrefs;

/// 面板贡献 id：也是文档实例的 surfaceId（`plugin.json` 的 `docTypeRegistrations` 同值）。
pub const PANEL: &str = "note";
/// 源码编辑面 id。
pub const SOURCE: &str = "note:source";
/// 源码模式外层滚动容器 id。
pub const SOURCE_SCROLL: &str = "note:source-scroll";
/// 只读 / 预览编辑共用的滚动容器 id（切档保留滚动位置）。
pub const BODY_SCROLL: &str = "note:body-scroll";
/// 页面内标题的点击区与改名输入框。
pub const TITLE: &str = "note:title";
pub const TITLE_INPUT: &str = "note:title-input";
/// 空笔记（正文无块）时的编辑面 id。
pub const EMPTY_EDIT: &str = "note:empty";
/// 撤销栈深度（全文快照）。
const UNDO_DEPTH: usize = 200;

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

/// 一位远端协作者的光标（偏移与 `doc_text` 同一坐标系）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCursor {
    pub name: String,
    pub color: String,
    pub anchor: Option<usize>,
    pub head: Option<usize>,
}

/// 独占一段的图片块（`![alt|宽x高](src)` 这类段落按可用宽渲染）。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageBlock {
    pub src: String,
    /// 替代文字：载入失败时只读面回落成 `[图片：alt]`。
    pub alt: String,
    /// 声明的像素高度（缺省由阅读面回落成替代文字占位）。
    pub height: Option<f32>,
}

/// 顶层块的源区间（相对正文）与图片信息。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockInfo {
    pub from: usize,
    pub to: usize,
    pub image: Option<ImageBlock>,
    /// 列表块的项区间（非列表为空）：每项是独立揭示单元。
    pub items: Vec<ItemInfo>,
}

/// 撤销快照：正文与当时的选区（全文绝对偏移）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoEntry {
    pub text: String,
    pub selection: Option<(usize, usize)>,
}

/// 列表项的源区间（相对正文）：标记起点到项内容尾，不含行首缩进与项后空行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    pub from: usize,
    pub to: usize,
}

pub use atelyx_plugin_props::{PropsHost, PropsState, Slot};

#[derive(Clone, Debug, Default)]
pub struct NoteModel {
    pub path: Option<String>,
    pub text: String,
    pub can_write: bool,
    pub mode: Mode,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 打开失败原因：面板无可显示正文时给出可见原因，不静默。
    pub open_error: Option<String>,
    pub prefs: EditorPrefs,
    /// 全仓库标签清单（属性面板的标签候选）。
    pub tag_vocab: Vec<String>,
    pub backlinks: Vec<Backlink>,
    pub remote: Vec<RemoteCursor>,
    pub props: PropsState,
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// 最近一次提交的选区（全文绝对偏移）：随撤销快照存取，撤销后光标回到编辑处。
    pub selection: Option<(usize, usize)>,
    /// 预览编辑的活动块（顶层块下标）；与 `active_item` 互斥。
    pub active_block: Option<usize>,
    /// 预览编辑的活动列表项（块下标，项下标）；与 `active_block` 互斥。
    pub active_item: Option<(usize, usize)>,
    /// 页面内标题的改名草稿（`Some` = 输入框在途）。
    pub renaming: Option<String>,
    /// `…` 菜单是否展开。
    pub menu_open: bool,
    /// 改名草稿在等保存落地（保存回执到达后发起改名）。
    pub pending_rename: Option<String>,
    /// 最近一次按下位置（面本地像素）：类型菜单这类就地弹层按它定位。
    pub pointer: (f32, f32),
    /// 正文图片的可用性（路径 → 能否解码）：失败的在只读面回落替代文字。
    pub image_probe: HashMap<String, bool>,
    pub blocks: Vec<BlockInfo>,
    /// 正文起点（frontmatter 之后）；块区间相对正文。
    pub body_start: usize,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Read
    }
}

impl NoteModel {
    pub fn is_open(&self) -> bool {
        self.path.is_some()
    }

    /// 文件主名（页面内标题与工具条显示用）。
    pub fn title(&self) -> String {
        let Some(path) = &self.path else { return "笔记".to_string() };
        let name = path.rsplit('/').next().unwrap_or(path);
        name.strip_suffix(".md")
            .or_else(|| name.strip_suffix(".MD"))
            .unwrap_or(name)
            .to_string()
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
        self.remote.clear();
        self.undo.clear();
        self.redo.clear();
        self.selection = None;
        self.active_block = None;
        self.active_item = None;
        self.renaming = None;
        self.pending_rename = None;
        self.menu_open = false;
        self.image_probe.clear();
        // 上一份文档的槽位下标对新正文无意义：草稿与候选态一并丢弃
        atelyx_plugin_props::discard_edit(self);
        self.refresh_blocks();
        self.refresh_props();
    }

    /// 关闭当前笔记：正文、撤销历史与活动块清空，回到只读档。
    pub fn clear(&mut self) {
        self.path = None;
        self.text.clear();
        self.dirty = false;
        self.save_error = None;
        self.open_error = None;
        self.tag_vocab.clear();
        self.backlinks.clear();
        self.remote.clear();
        self.undo.clear();
        self.redo.clear();
        self.selection = None;
        self.active_block = None;
        self.active_item = None;
        self.blocks.clear();
        self.image_probe.clear();
        self.body_start = 0;
        self.props = PropsState::default();
        self.renaming = None;
        self.pending_rename = None;
        self.menu_open = false;
        self.mode = Mode::Read;
    }

    /// 宿主改写正文（协作远端合入 / 属性面板 / 保存合流）：撤销栈作废，
    /// 旧快照回放会把远端内容整体改回去。
    pub fn replace_text(&mut self, text: String) {
        if self.text == text {
            return;
        }
        self.text = text;
        self.undo.clear();
        self.redo.clear();
        self.selection = None;
        self.refresh_blocks();
        self.refresh_props();
    }

    /// 本端编辑：入撤销栈并重算块区间（快照带当时的选区）。
    pub fn edit_text(&mut self, text: String) {
        if self.text == text {
            return;
        }
        self.undo.push(UndoEntry {
            text: std::mem::replace(&mut self.text, text),
            selection: self.selection,
        });
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
        self.refresh_props();
    }

    /// 撤销：返回恢复后的正文与当时的选区。
    pub fn undo(&mut self) -> Option<(String, Option<(usize, usize)>)> {
        let previous = self.undo.pop()?;
        self.redo.push(UndoEntry {
            text: std::mem::replace(&mut self.text, previous.text),
            selection: self.selection,
        });
        self.selection = previous.selection;
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
        self.refresh_props();
        Some((self.text.clone(), self.selection))
    }

    /// 重做：返回恢复后的正文与当时的选区。
    pub fn redo(&mut self) -> Option<(String, Option<(usize, usize)>)> {
        let next = self.redo.pop()?;
        self.undo.push(UndoEntry {
            text: std::mem::replace(&mut self.text, next.text),
            selection: self.selection,
        });
        self.selection = next.selection;
        self.dirty = true;
        self.save_error = None;
        self.refresh_blocks();
        self.refresh_props();
        Some((self.text.clone(), self.selection))
    }

    /// 块的可编辑区间（绝对偏移）：块自身区间 + 其后到下一块行首之间的空行。
    /// 终点止于下一块行首前一个换行：末尾换行是下一块的行终止符，算进来会让文本框多出一行。
    pub fn block_source(&self, index: usize) -> Option<(usize, usize)> {
        let block = self.blocks.get(index)?;
        let body = self.body();
        let end = if let Some(next) = self.blocks.get(index + 1) {
            body[..next.from].rfind('\n').map_or(0, |line| line + 1)
        } else {
            body.len()
        };
        let from = self.body_start + block.from;
        let mut to = self.body_start + end;
        if to > from && self.text.as_bytes().get(to - 1) == Some(&b'\n') {
            to -= 1;
        }
        Some((from, to.max(from)))
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

    /// 列表项的源区间（绝对偏移）：行首（含缩进）到项内容尾，不含项后空行。
    /// 含缩进才能让 Tab / 续项按整行判定嵌套，缩进也就随项一起显示与回写。
    pub fn item_source(&self, block: usize, item: usize) -> Option<(usize, usize)> {
        let info = self.blocks.get(block)?.items.get(item)?;
        let body = self.body();
        let line_from = body[..info.from.min(body.len())]
            .rfind('\n')
            .map_or(0, |line| line + 1);
        Some((self.body_start + line_from, self.body_start + info.to))
    }

    /// 列表项内某偏移所属的（块序，项序）。
    pub fn item_at(&self, offset: usize) -> Option<(usize, usize)> {
        let rel = offset.checked_sub(self.body_start)?;
        for (index, block) in self.blocks.iter().enumerate() {
            if rel < block.from || rel > block.to {
                continue;
            }
            if let Some(item) = block
                .items
                .iter()
                .position(|info| rel >= info.from && rel <= info.to)
            {
                return Some((index, item));
            }
        }
        None
    }

    /// 用编辑后的项正文替换原项区间，返回新全文。
    pub fn replace_item(&self, block: usize, item: usize, source: &str) -> Option<String> {
        let (from, to) = self.item_source(block, item)?;
        let mut out = String::with_capacity(self.text.len() + source.len());
        out.push_str(&self.text[..from]);
        out.push_str(source);
        out.push_str(&self.text[to..]);
        Some(out)
    }

    /// 列表项之间的空行数（渲染时折算间距；与 Markdown 版面口径一致）。
    pub fn item_gap_lines(&self, block: usize, item: usize) -> usize {
        if item == 0 {
            return 0;
        }
        let Some(info) = self.blocks.get(block) else { return 0 };
        let (Some(prev), Some(next)) = (info.items.get(item - 1), info.items.get(item)) else {
            return 0;
        };
        self.body()[prev.to..next.from].matches('\n').count().saturating_sub(1)
    }

    /// 正文（frontmatter 之后）。
    pub fn body(&self) -> &str {
        &self.text[self.body_start.min(self.text.len())..]
    }

    /// 第 `index` 个块间空行区的各行行首（绝对偏移）：`index` = 其后块序，末块之后用 `blocks.len()`。
    /// 空行区只含空行；块间首行行首取自上一块行尾换行之后，文首空行区从正文起点算起。
    pub fn gap_lines(&self, index: usize) -> Vec<usize> {
        if index > self.blocks.len() {
            return Vec::new();
        }
        let body = self.body();
        let from = if index == 0 { 0 } else { self.blocks[index - 1].to };
        // 下一块的 `from` 是内容起点（首行缩进已剥掉）：空行区止于该行行首，不把缩进算成一根空行
        let to = if index == self.blocks.len() {
            body.len()
        } else {
            let at = self.blocks[index].from;
            body[..at].rfind('\n').map_or(0, |line| line + 1)
        };
        if to <= from {
            return Vec::new();
        }
        let mut starts = Vec::new();
        if index == 0 {
            starts.push(self.body_start);
        }
        for (offset, ch) in body[from..to].char_indices() {
            if ch != '\n' {
                continue;
            }
            let next = from + offset + 1;
            if next < to {
                starts.push(self.body_start + next);
            }
        }
        starts
    }

    /// 光标所在顶层块（绝对偏移）。
    pub fn block_at(&self, offset: usize) -> Option<usize> {
        let rel = offset.checked_sub(self.body_start)?;
        self.blocks.iter().position(|block| rel >= block.from && rel <= block.to)
    }

    /// 空行区某行的内容区间（绝对偏移）：`from` = 行首，`to` = 行尾换行位（无换行取文末）。
    pub fn gap_line_range(&self, gap: usize, line: usize) -> Option<(usize, usize)> {
        let start = *self.gap_lines(gap).get(line)?;
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |offset| start + offset);
        Some((start, end))
    }

    /// 光标落在哪个空行区的哪一行（绝对偏移）：行首 ≤ 光标 ≤ 行尾。
    pub fn gap_line_at(&self, offset: usize) -> Option<(usize, usize)> {
        for gap in 0..=self.blocks.len() {
            for line in 0..self.gap_lines(gap).len() {
                if let Some((from, to)) = self.gap_line_range(gap, line) {
                    if offset >= from && offset <= to {
                        return Some((gap, line));
                    }
                }
            }
        }
        None
    }

    /// 用一段文本替换一个区间（返回新全文与新光标绝对位，光标按行内相对位落在替换段内）。
    pub fn replace_range(&self, from: usize, to: usize, value: &str, caret: usize) -> (String, usize) {
        let from = from.min(self.text.len());
        let to = to.min(self.text.len()).max(from);
        let mut out = String::with_capacity(self.text.len() + value.len());
        out.push_str(&self.text[..from]);
        out.push_str(value);
        out.push_str(&self.text[to..]);
        (out, from + caret.min(value.len()))
    }

    /// 删除一个换行（Delete 在行尾与下一行合并），返回新全文与新光标位。
    pub fn remove_newline(&self, at: usize) -> Option<(String, usize)> {
        if self.text.get(at..at + 1)? != "\n" {
            return None;
        }
        let mut out = String::with_capacity(self.text.len() - 1);
        out.push_str(&self.text[..at]);
        out.push_str(&self.text[at + 1..]);
        Some((out, at))
    }

    /// 退格合并空行：删掉该行前的换行，返回新全文与新光标位。
    pub fn join_gap_line(&self, offset: usize) -> Option<(String, usize)> {
        let cut = offset.checked_sub(1)?;
        if self.text.get(cut..offset)? != "\n" {
            return None;
        }
        let mut out = String::with_capacity(self.text.len() - 1);
        out.push_str(&self.text[..cut]);
        out.push_str(&self.text[offset..]);
        Some((out, cut))
    }

    fn refresh_blocks(&mut self) {
        self.body_start = frontmatter::block_end(&self.text).unwrap_or(0);
        let body = &self.text[self.body_start..];
        let doc = parse_markdown(body, &ParseOptions::default());
        self.blocks = doc.blocks.iter().map(block_info).collect();
        // 活动块只指向可整块揭示的块：列表块按项揭示，不保留块级活动态
        if self
            .active_block
            .is_some_and(|index| self.blocks.get(index).is_none_or(|info| !info.items.is_empty()))
        {
            self.active_block = None;
        }
        if self
            .active_item
            .is_some_and(|(block, item)| self.blocks.get(block).is_none_or(|info| item >= info.items.len()))
        {
            self.active_item = None;
        }
    }

    /// 重建属性面板解析缓存（载入 / 正文替换 / 属性提交后调用）。
    pub fn refresh_props(&mut self) {
        let parsed = frontmatter::parse(&self.text);
        let open = frontmatter::block_end(&self.text).is_some();
        self.props.data = if open {
            let pairs = parsed.data.as_map().map(<[(String, Yaml)]>::to_vec).unwrap_or_default();
            Some((pairs, parsed.ok))
        } else if self.text.starts_with("---") {
            Some((Vec::new(), false))
        } else {
            None
        };
    }

    /// 用属性数据重写 frontmatter，返回新全文（空映射 = 删块回到无 frontmatter 态）。
    /// 属性面板写回：整篇正文替换由调用方提交给宿主。
    fn props_write_text(&self, pairs: Vec<(String, Yaml)>) -> String {
        self.text_with_props(pairs)
    }
    pub fn text_with_props(&self, pairs: Vec<(String, Yaml)>) -> String {
        let parsed = frontmatter::parse(&self.text);
        frontmatter::stringify(&Yaml::Map(pairs), &parsed.body)
    }
}

fn block_info(block: &MarkdownBlock) -> BlockInfo {
    let (from, to) = block_range(block);
    let image = match block {
        MarkdownBlock::Paragraph { inline, .. } => image_only(inline).map(|span| {
            let InlineSpan::Image { src, alt, height, .. } = span else { unreachable!() };
            ImageBlock {
                src: src.clone(),
                alt: alt.clone(),
                height: height.as_deref().and_then(parse_len),
            }
        }),
        _ => None,
    };
    let items = match block {
        MarkdownBlock::List { items, .. } => items
            .iter()
            .map(|item| ItemInfo { from: item.from, to: item.to })
            .collect(),
        _ => Vec::new(),
    };
    BlockInfo { from, to, image, items }
}

fn image_only(inline: &[InlineSpan]) -> Option<&InlineSpan> {
    match inline {
        [span @ InlineSpan::Image { .. }] => Some(span),
        _ => None,
    }
}

/// 尺寸声明（`![x](p.png =120x80)` 那类）→ 像素；只认高度，宽随可用宽等比。
fn parse_len(value: &str) -> Option<f32> {
    let raw = value.trim().trim_end_matches(['%', 'x', 'X']);
    raw.parse::<f32>().ok().filter(|value| *value > 0.0)
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

/// markdown 的图片 src 与仓库相对路径是否指同一个文件：src 可能带 `./`、`../` 或目录前缀。
pub fn image_src_matches(src: &str, path: &str) -> bool {
    if src == path {
        return true;
    }
    src.ends_with(&format!("/{path}")) || path.ends_with(&format!("/{src}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_starts_after_closed_frontmatter_only() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\ntitle: a\n---\n正文".into(), true);
        assert_eq!(model.body_start, 17);
        assert_eq!(model.body(), "正文");
        model.load("b.md".into(), "---\ntitle: a\n正文".into(), true);
        assert_eq!(model.body_start, 0, "未闭合不剥");
    }

    #[test]
    fn block_replacement_keeps_rest_of_note() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\nt: x\n---\n# 标题\n\n段落\n".into(), true);
        assert_eq!(model.blocks.len(), 2);
        let edited = model.replace_block(0, "# 新标题\n").expect("块在");
        assert_eq!(edited, "---\nt: x\n---\n# 新标题\n\n段落\n");
        let edited = model.replace_block(1, "新段落").expect("块在");
        assert_eq!(edited, "---\nt: x\n---\n# 标题\n\n新段落\n");
    }

    #[test]
    fn block_source_keeps_blank_lines_but_not_the_next_line_break() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "甲\n\n乙\n".into(), true);
        let (from, to) = model.block_source(0).expect("块在");
        assert_eq!(&model.text[from..to], "甲\n", "块间空行归编辑面，末尾换行归下一块");
        let (from, to) = model.block_source(1).expect("块在");
        assert_eq!(&model.text[from..to], "乙", "文末换行不算一行");
    }

    #[test]
    fn block_source_skips_the_next_block_indent() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "甲\n\n  乙\n".into(), true);
        let (from, to) = model.block_source(0).expect("块在");
        assert_eq!(&model.text[from..to], "甲\n", "下一块首行的缩进不属于本块");
    }

    #[test]
    fn list_items_are_individual_source_ranges() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "- 甲\n\n- 乙\n".into(), true);
        assert_eq!(model.blocks[0].items.len(), 2);
        let (from, to) = model.item_source(0, 0).expect("项在");
        assert_eq!(&model.text[from..to], "- 甲");
        let (from, to) = model.item_source(0, 1).expect("项在");
        assert_eq!(&model.text[from..to], "- 乙");
        assert_eq!(model.item_gap_lines(0, 1), 1);
        assert_eq!(model.item_at(model.body_start + 1), Some((0, 0)));
        assert_eq!(model.item_at(model.body_start + 7), Some((0, 1)));
    }

    /// 缩进项的源区间从行首算起：Tab / 续项按整行判定，编辑框也照这个区间绘制。
    #[test]
    fn nested_item_source_includes_the_leading_indent() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "  - 甲\n  - 乙\n".into(), true);
        let (from, to) = model.item_source(0, 0).expect("项在");
        assert_eq!(&model.text[from..to], "  - 甲", "缩进随项一起显示与回写");
    }

    /// 图片探测缓存按 src 记，变更事件给的是仓库相对路径：两种写法都要能对上。
    #[test]
    fn image_src_matches_repository_paths() {
        assert!(image_src_matches("图.png", "图.png"));
        assert!(image_src_matches("./图.png", "图.png"));
        assert!(image_src_matches("../附件/图.png", "附件/图.png"));
        assert!(image_src_matches("附件/图.png", "附件/图.png"));
        assert!(!image_src_matches("图.png", "附件/图2.png"));
        assert!(!image_src_matches("附件/图.png", "附件/图2.png"));
    }

    #[test]
    fn active_item_is_dropped_when_the_list_shrinks() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "- 甲\n- 乙\n".into(), true);
        model.active_item = Some((0, 1));
        model.edit_text("正文\n".into());
        assert!(model.active_item.is_none());
    }

    #[test]
    fn gap_lines_cover_head_between_and_tail() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "\n甲\n\n乙\n\n".into(), true);
        let head = model.gap_lines(0);
        assert_eq!(head, vec![model.body_start], "文首空行也算一根");
        let between = model.gap_lines(1);
        assert_eq!(between.len(), 1, "甲、乙之间一根空行");
        assert_eq!(&model.text[between[0]..between[0] + 1], "\n");
        let tail = model.gap_lines(model.blocks.len());
        assert_eq!(tail.len(), 1, "文末一根空行");
    }

    #[test]
    fn gap_line_range_keeps_the_line_content() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "甲\n  \n乙\n".into(), true);
        let (from, to) = model.gap_line_range(1, 0).expect("块间空行");
        assert_eq!(&model.text[from..to], "  ", "空格属于该空行内容");
    }

    #[test]
    fn gap_lines_do_not_treat_the_next_block_indent_as_a_blank_line() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "甲\n\n  乙\n".into(), true);
        let lines = model.gap_lines(1);
        assert_eq!(lines.len(), 1, "下一块首行的两个空格不是空行");
        let (from, to) = model.gap_line_range(1, 0).expect("块间空行");
        assert_eq!(from, to, "该空行内容为空，不含下一块的缩进");
    }

    #[test]
    fn load_and_clear_drop_a_pending_rename() {
        let mut model = NoteModel::default();
        model.pending_rename = Some("改名".into());
        model.load("a.md".into(), "正文".into(), true);
        assert!(model.pending_rename.is_none(), "换篇不得把旧改名带到新笔记");
        model.pending_rename = Some("改名".into());
        model.clear();
        assert!(model.pending_rename.is_none());
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

    #[test]
    fn title_drops_the_markdown_suffix() {
        let mut model = NoteModel::default();
        model.load("dir/方案.md".into(), String::new(), true);
        assert_eq!(model.title(), "方案");
        model.load("dir/方案.MD".into(), String::new(), true);
        assert_eq!(model.title(), "方案");
    }

    #[test]
    fn image_only_paragraphs_carry_declared_height() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "![图|120x80](p.png)\n".into(), true);
        let image = model.blocks[0].image.as_ref().expect("图片块");
        assert_eq!(image.src, "p.png");
        assert_eq!(image.height, Some(80.0));
    }
}

impl PropsHost for NoteModel {
    fn props(&self) -> &PropsState {
        &self.props
    }

    fn props_mut(&mut self) -> &mut PropsState {
        &mut self.props
    }

    fn editable(&self) -> bool {
        self.can_write
    }

    fn tags(&self) -> &[String] {
        &self.tag_vocab
    }

    fn write_pairs(&mut self, pairs: Vec<(String, Yaml)>) -> Option<String> {
        Some(self.props_write_text(pairs))
    }

    fn refresh(&mut self) {
        self.refresh_props();
    }
}
