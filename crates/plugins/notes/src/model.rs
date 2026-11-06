//! 笔记插件的展示与编辑状态：三档模式、正文块、撤销栈、属性面板、反链与远端光标。
//! 正文缓冲在插件，宿主只按 `doc.state` / `doc.save` 记会话状态并落盘。

use std::collections::HashMap;

use atelyx_format::frontmatter::{self, Yaml};
use atelyx_format::markdown::{parse_markdown, InlineSpan, MarkdownBlock, MarkdownListItem, ParseOptions};
use atelyx_plugin_protocol::method::{EditorPrefs, VaultFileEntry};
use atelyx_plugin_sdk::frame_ui::Rect;

/// 面板贡献 id：也是文档实例的 surfaceId（`plugin.json` 的 `docTypeRegistrations` 同值）。
pub const PANEL: &str = "note";
/// 笔记文件扩展名（与 `plugin.json` 的 `docTypeRegistrations` 一致）。
pub const EXTENSION: &str = "md";
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

/// 一位远端协作者的光标（偏移与模型正文同一坐标系）。
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
    /// 声明的像素宽度（缺省取原图宽）。
    pub width: Option<f32>,
    /// 声明的像素高度（缺省取原图高）。
    pub height: Option<f32>,
}

/// `[[` 候选浮层：`input` = 所在输入面，`start` = `[[` 在输入值里的字节位，`caret` = 光标在输入值里的字节位。
#[derive(Clone, Debug, PartialEq)]
pub struct WikiSuggest {
    pub input: String,
    pub start: usize,
    pub query: String,
    pub caret: usize,
    pub candidates: Vec<String>,
    /// 光标矩形（面本地坐标）：浮层按它定位；输入框尚未落位时为 `None`（本帧不画）。
    pub caret_rect: Option<Rect>,
}

/// 图片的显示尺寸（像素）：声明值优先，缺边按原图比例补齐。
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)]
pub struct ImageSize {
    pub width: f32,
    pub height: f32,
}

/// 顶层块的源区间（相对正文）与图片信息。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockInfo {
    pub from: usize,
    pub to: usize,
    pub image: Option<ImageBlock>,
    /// 块内的揭示单元（列表项逐层展开、引用内各子块）：每项独立替换，空表示整块揭示。
    pub items: Vec<ItemInfo>,
}

/// 撤销快照：正文与当时的选区（全文绝对偏移）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoEntry {
    pub text: String,
    pub selection: Option<(usize, usize)>,
}

/// 揭示单元的渲染形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    /// 列表项：`depth` 为嵌套层级（0 = 顶层），`ordinal` 为同一兄弟列表内的序号（有序项绘制用）。
    Item { depth: usize, ordinal: usize },
    /// 容器（引用 / 列表项续段）里的子块：`depth` 为所属列表的嵌套层级（0 = 顶层）。
    Block { depth: usize },
}

/// 揭示单元的源区间（相对正文）：行首（含缩进 / 容器前缀）到自身内容行尾，
/// 不含行尾换行，也不含其嵌套子单元。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    pub from: usize,
    pub to: usize,
    pub kind: UnitKind,
}

/// 远端光标在某揭示单元内的位置：行序 + 装饰位移 + 行前文本（绘制时量宽）。
#[derive(Clone, Debug, PartialEq)]
pub struct CursorSpot {
    pub block: usize,
    pub unit: Option<usize>,
    pub line: usize,
    /// 该行文字在单元左缘之后的位移（列表标记列 / 引用竖条留白），不含嵌套缩进。
    pub base_x: f32,
    /// 去掉列表标记与引用前缀后的行前文本。
    pub line_prefix: String,
}

/// 列表项文字相对单元左缘的位移（与 Markdown 渲染的标记列同口径）。
pub const LIST_TEXT_X: f32 = 20.0;
/// 引用块文字相对单元左缘的位移（与 Markdown 渲染的引用留白同口径）。
pub const QUOTE_TEXT_X: f32 = 12.0;

pub use atelyx_plugin_props::{PropsHost, PropsState, Slot};

#[derive(Clone, Debug, Default)]
pub struct NoteModel {
    pub path: Option<String>,
    pub text: String,
    pub can_write: bool,
    pub mode: Mode,
    pub dirty: bool,
    /// 保存状态真源：会话身份 / 磁盘基线 / 在途请求；`save_error` 是它的视图副本。
    pub doc: crate::session::DocState,
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
    /// 正文图片的探测结果（路径 → 原图像素尺寸；`None` = 不能解码）：失败的在只读面回落替代文字。
    pub image_probe: HashMap<String, Option<(u32, u32)>>,
    /// 仓库文件清单（`[[` 候选与内链解析用；宿主清单重建时重取）。
    pub wiki_files: Vec<VaultFileEntry>,
    /// `[[` 候选浮层（`None` = 未在输入链接）。
    pub wiki: Option<WikiSuggest>,
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
        strip_extension(name).to_string()
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
        self.wiki = None;
        // 上一份文档的槽位下标对新正文无意义：草稿与候选态一并丢弃
        atelyx_plugin_props::discard_edit(self);
        self.refresh_blocks();
        self.refresh_props();
    }

    /// 关闭当前笔记：正文、撤销历史与活动块清空，回到只读档。
    pub fn clear(&mut self) {
        self.path = None;
        self.text.clear();
        self.doc.close();
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
        self.wiki = None;
        self.body_start = 0;
        self.props = PropsState::default();
        self.renaming = None;
        self.pending_rename = None;
        self.menu_open = false;
        self.mode = Mode::Read;
    }

    /// 宿主改写正文（协作远端合入 / 属性面板 / 保存合流）。
    /// 正文变化时撤销栈作废（旧快照回放会把远端内容整体改回去）；
    /// 仅 frontmatter 变化（属性写回）时正文不动，记一次可撤销编辑，撤销栈仍与正文对齐。
    pub fn replace_text(&mut self, text: String) {
        if self.text == text {
            return;
        }
        if body_of(&self.text) == body_of(&text) {
            self.undo.push(UndoEntry {
                text: std::mem::replace(&mut self.text, text),
                selection: self.selection,
            });
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.redo.clear();
        } else {
            self.text = text;
            self.undo.clear();
            self.redo.clear();
        }
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

    /// 揭示单元的源区间（绝对偏移）：行首（含缩进 / 容器前缀）到单元自身内容行尾。
    /// 含缩进才能让 Tab / 续项按整行判定嵌套，缩进也就随项一起显示与回写。
    pub fn item_source(&self, block: usize, item: usize) -> Option<(usize, usize)> {
        let info = self.blocks.get(block)?.items.get(item)?;
        Some((self.body_start + info.from, self.body_start + info.to))
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

    /// 揭示单元之间的空行数（渲染时折算间距；与 Markdown 版面口径一致）。
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

    /// 同一列表块内两项之间空行区某行的内容区间（绝对偏移）：`item` 为后一项，首项无前项。
    pub fn item_gap_line_range(&self, block: usize, item: usize, line: usize) -> Option<(usize, usize)> {
        if item == 0 {
            return None;
        }
        let info = self.blocks.get(block)?;
        let (prev, next) = (info.items.get(item - 1)?, info.items.get(item)?);
        let body = self.body();
        let start = body[prev.to..next.from]
            .char_indices()
            .filter(|(_, ch)| *ch == '\n')
            .map(|(offset, _)| prev.to + offset + 1)
            .filter(|at| *at < next.from)
            .nth(line)?;
        let end = body[start..].find('\n').map_or(body.len(), |offset| start + offset);
        Some((self.body_start + start, self.body_start + end))
    }

    /// 光标落在哪个列表项间空行区的哪一行（绝对偏移）：行首 ≤ 光标 ≤ 行尾。
    pub fn item_gap_line_at(&self, offset: usize) -> Option<(usize, usize, usize)> {
        for (block, info) in self.blocks.iter().enumerate() {
            for item in 1..info.items.len() {
                for line in 0..self.item_gap_lines(block, item) {
                    if let Some((from, to)) = self.item_gap_line_range(block, item, line) {
                        if offset >= from && offset <= to {
                            return Some((block, item, line));
                        }
                    }
                }
            }
        }
        None
    }

    /// 远端光标（全文绝对偏移）落在哪个揭示单元的第几行：装饰位移与行前文本供绘制时量宽。
    /// 落在块间空行、单元之间的空行或非法字节位时返回 `None`（不画）。
    pub fn cursor_spot(&self, offset: usize) -> Option<CursorSpot> {
        self.spot_at(offset, true)
    }

    /// 活动编辑框（展示源码原文）的远端光标：行前文本保留缩进与容器前缀，不做渲染位移。
    pub fn source_cursor_spot(&self, offset: usize) -> Option<CursorSpot> {
        self.spot_at(offset, false)
    }

    fn spot_at(&self, offset: usize, rendered: bool) -> Option<CursorSpot> {
        let rel = offset.checked_sub(self.body_start)?;
        let body = self.body();
        // 协作偏移可能来自旧正文：越界或落在字符中间都不猜，直接不画
        if rel > body.len() || !body.is_char_boundary(rel) {
            return None;
        }
        for (index, block) in self.blocks.iter().enumerate() {
            if block.items.is_empty() {
                if rel >= block.from && rel <= block.to {
                    return Some(spot_of(
                        body,
                        index,
                        None,
                        block.from,
                        rel,
                        UnitKind::Block { depth: 0 },
                        rendered,
                    ));
                }
                continue;
            }
            let Some(unit) = block
                .items
                .iter()
                .position(|info| rel >= info.from && rel <= info.to)
            else {
                continue;
            };
            let info = block.items[unit];
            return Some(spot_of(body, index, Some(unit), info.from, rel, info.kind, rendered));
        }
        None
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
        self.blocks = doc.blocks.iter().map(|block| block_info(block, body)).collect();
        // 活动块只指向可整块揭示的块：有揭示单元的块按单元揭示，不保留块级活动态
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
            let map = parsed.data.as_map().map(<[(String, Yaml)]>::to_vec);
            let editable = parsed.ok && map.is_some();
            // 根不是映射的 YAML 无法按属性编辑：面板只显示不可编辑态，避免新建一条覆盖整块
            Some((map.unwrap_or_default(), editable))
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

fn block_info(block: &MarkdownBlock, body: &str) -> BlockInfo {
    let (from, to) = block_range(block);
    let image = match block {
        MarkdownBlock::Paragraph { inline, .. } => image_only(inline).map(image_block),
        _ => None,
    };
    let mut items = Vec::new();
    match block {
        MarkdownBlock::List { items: list, .. } => push_list_units(list, body, 0, &mut items),
        MarkdownBlock::Blockquote { children, .. } => {
            items.extend(children.iter().map(|child| block_unit(child, body, 0)));
        }
        _ => {}
    }
    BlockInfo { from, to, image, items }
}

/// 列表项逐层展开为揭示单元；项自身源止于第一个子块所在行，嵌套项再各自成单元。
fn push_list_units(items: &[MarkdownListItem], body: &str, depth: usize, out: &mut Vec<ItemInfo>) {
    for (ordinal, item) in items.iter().enumerate() {
        let from = line_start(body, item.from);
        let to = item
            .children
            .first()
            .map_or(item.to, |child| own_end(body, block_range(child).0))
            .max(from);
        out.push(ItemInfo { from, to, kind: UnitKind::Item { depth, ordinal } });
        push_item_children(&item.children, body, depth + 1, out);
    }
}

fn push_item_children(children: &[MarkdownBlock], body: &str, depth: usize, out: &mut Vec<ItemInfo>) {
    for child in children {
        match child {
            MarkdownBlock::List { items, .. } => push_list_units(items, body, depth, out),
            MarkdownBlock::Blockquote { children, .. } => {
                out.extend(children.iter().map(|block| block_unit(block, body, depth)));
            }
            _ => out.push(block_unit(child, body, depth)),
        }
    }
}

/// 容器内子块：行首到块所在行尾，源码保留引用 / 缩进前缀。
fn block_unit(block: &MarkdownBlock, body: &str, depth: usize) -> ItemInfo {
    let (from, to) = block_range(block);
    ItemInfo { from: line_start(body, from), to: line_end(body, to), kind: UnitKind::Block { depth } }
}

fn line_start(body: &str, at: usize) -> usize {
    body[..at.min(body.len())].rfind('\n').map_or(0, |line| line + 1)
}

fn line_end(body: &str, at: usize) -> usize {
    let at = at.min(body.len());
    body[at..].find('\n').map_or(body.len(), |offset| at + offset)
}

/// `at` 之前最后一个非空行的行尾：从 `at` 往前逐行跳过空行与仅有空白的行。
fn own_end(body: &str, at: usize) -> usize {
    let mut end = at.min(body.len());
    loop {
        while end > 0 && matches!(body.as_bytes().get(end - 1), Some(b'\n') | Some(b'\r')) {
            end -= 1;
        }
        let start = body[..end].rfind('\n').map_or(0, |line| line + 1);
        if !body[start..end].trim().is_empty() {
            return end;
        }
        if start == 0 {
            return 0;
        }
        end = start;
    }
}

fn image_only(inline: &[InlineSpan]) -> Option<&InlineSpan> {
    match inline {
        [span @ InlineSpan::Image { .. }] => Some(span),
        _ => None,
    }
}

fn image_block(span: &InlineSpan) -> ImageBlock {
    let InlineSpan::Image { src, alt, width, height, .. } = span else { unreachable!() };
    let mut alt = alt.clone();
    let declared_width = width.as_deref().and_then(parse_len);
    let mut declared_height = height.as_deref().and_then(parse_len);
    if declared_width.is_none() && declared_height.is_none() {
        // 格式库只识别 `|W` / `|WxH`，高度单声明（`|x80`）仍留在替代文字里，这里补解析
        if let Some((rest, spec)) = alt.rsplit_once('|') {
            if let Some(height) = height_spec(spec) {
                alt = rest.to_string();
                declared_height = Some(height);
            }
        }
    }
    ImageBlock { src: src.clone(), alt, width: declared_width, height: declared_height }
}

fn height_spec(spec: &str) -> Option<f32> {
    let digits = spec.trim().strip_prefix(['x', 'X'])?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<f32>().ok().filter(|value| *value > 0.0)
}

/// 尺寸声明（`![x|120x80](p.png)` 这类）→ 像素；宽高各自独立，缺省取原图尺寸。
fn parse_len(value: &str) -> Option<f32> {
    let raw = value.trim().trim_end_matches(['%', 'x', 'X']);
    raw.parse::<f32>().ok().filter(|value| *value > 0.0)
}

/// 图片显示尺寸：声明的宽高优先，单边声明按原图比例补另一边，未声明用原图自然尺寸。
#[allow(dead_code)]
pub fn image_display_size(image: &ImageBlock, natural: Option<(u32, u32)>) -> Option<ImageSize> {
    let natural = natural
        .map(|(width, height)| (width as f32, height as f32))
        .filter(|(width, height)| *width > 0.0 && *height > 0.0);
    match (image.width, image.height) {
        (Some(width), Some(height)) => Some(ImageSize { width, height }),
        (Some(width), None) => {
            natural.map(|(natural_w, natural_h)| ImageSize { width, height: width * natural_h / natural_w })
        }
        (None, Some(height)) => {
            natural.map(|(natural_w, natural_h)| ImageSize { width: height * natural_w / natural_h, height })
        }
        (None, None) => natural.map(|(width, height)| ImageSize { width, height }),
    }
}

/// 版面计算：先取显示尺寸，再等比收到可用宽内（`available` ≤ 0 表示不约束）。
pub fn image_layout(image: &ImageBlock, natural: Option<(u32, u32)>, available: f32) -> Option<ImageSize> {
    let size = image_display_size(image, natural)?;
    if available > 0.0 && size.width > available {
        let scale = available / size.width;
        return Some(ImageSize { width: available, height: size.height * scale });
    }
    Some(size)
}

/// 光标前未闭合的 `[[` 查询：返回 `[[` 起点与查询串；含换行 / `]` / `|` 视为已闭合或不是链接。
pub fn wiki_query(value: &str, caret: usize) -> Option<(usize, &str)> {
    let head = value.get(..caret.min(value.len()))?;
    let start = head.rfind("[[")?;
    let query = &head[start + 2..];
    if query.contains('\n') || query.contains(']') || query.contains('|') || query.contains("[[") {
        return None;
    }
    Some((start, query))
}

/// `[[` 候选：仓库内笔记按路径 / 文件名包含查询串（不区分大小写），同目录优先，最多 `limit` 条。
pub fn wiki_candidates(
    files: &[VaultFileEntry],
    query: &str,
    current: Option<&str>,
    limit: usize,
) -> Vec<String> {
    let needle = query.trim().to_lowercase();
    let dir = current.and_then(parent_of);
    let mut hits: Vec<&VaultFileEntry> = files
        .iter()
        .filter(|file| file.openable && is_note(&file.name))
        .filter(|file| {
            needle.is_empty()
                || file.path.to_lowercase().contains(&needle)
                || file.name.to_lowercase().contains(&needle)
        })
        .collect();
    hits.sort_by(|a, b| {
        let a_same = dir.is_some_and(|dir| parent_of(&a.path) == Some(dir));
        let b_same = dir.is_some_and(|dir| parent_of(&b.path) == Some(dir));
        b_same.cmp(&a_same).then_with(|| a.path.cmp(&b.path))
    });
    hits.into_iter().take(limit).map(|file| file.path.clone()).collect()
}

/// 候选路径 → 插入文本：`[[路径|文件名]]`，显示用文件名、目标用完整路径。
pub fn wiki_link(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = strip_extension(name);
    format!("[[{path}|{stem}]]")
}

/// 外链（http / https / mailto）交给系统浏览器，其余按仓库内链处理。
pub fn is_external(target: &str) -> bool {
    let target = target.trim();
    target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:")
}

/// 内链目标 → 仓库相对路径：先按完整路径（含相对当前笔记目录），再按文件名匹配。
pub fn resolve_link(files: &[VaultFileEntry], current: Option<&str>, target: &str) -> Option<String> {
    let target = target.trim().split('#').next().unwrap_or("").trim();
    if target.is_empty() {
        return None;
    }
    let direct = normalize_path(target);
    if let Some(found) = files.iter().find(|file| file.openable && file.path == direct) {
        return Some(found.path.clone());
    }
    if let Some(dir) = current.and_then(parent_of) {
        let joined = normalize_path(&format!("{dir}/{target}"));
        if let Some(found) = files.iter().find(|file| file.openable && file.path == joined) {
            return Some(found.path.clone());
        }
    }
    let name = target.rsplit('/').next().unwrap_or(target).to_lowercase();
    let with_md = if is_note(&name) { name } else { format!("{name}.{EXTENSION}") };
    files
        .iter()
        .find(|file| file.openable && file.name.to_lowercase() == with_md)
        .map(|file| file.path.clone())
}

fn parent_of(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(dir, _)| dir)
}

fn is_note(name: &str) -> bool {
    strip_extension(name).len() != name.len()
}

/// 去掉笔记扩展名（大小写不敏感）；不带该扩展名时原样返回。
fn strip_extension(name: &str) -> &str {
    let suffix = EXTENSION.len() + 1;
    let Some(cut) = name.len().checked_sub(suffix) else { return name };
    if name.is_char_boundary(cut) {
        let tail = &name[cut..];
        if tail.starts_with('.') && tail[1..].eq_ignore_ascii_case(EXTENSION) {
            return &name[..cut];
        }
    }
    name
}

/// 去掉空段与 `.`、按 `..` 回退：内链里的相对写法先归一成仓库相对路径。
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn spot_of(
    body: &str,
    block: usize,
    unit: Option<usize>,
    from: usize,
    rel: usize,
    kind: UnitKind,
    rendered: bool,
) -> CursorSpot {
    let text = &body[from.min(body.len())..rel.min(body.len())];
    let line = text.matches('\n').count();
    let line_start = text.rfind('\n').map_or(0, |index| index + 1);
    let raw = &text[line_start..];
    let (base_x, line_prefix) = if rendered {
        match kind {
            UnitKind::Item { .. } => (LIST_TEXT_X, item_prefix(body, from + line_start, raw)),
            UnitKind::Block { .. } => block_line_prefix(body, from + line_start, raw),
        }
    } else {
        (0.0, raw.to_string())
    };
    CursorSpot { block, unit, line, base_x, line_prefix }
}

/// 列表项的行前文本：首行的标记（`- ` / `1. ` / 任务框）不参与量宽，落在标记内时贴住内容起点；
/// 续行的行首缩进同样不上屏。
fn item_prefix(body: &str, line_abs_start: usize, raw: &str) -> String {
    let line = line_start_line(body, line_abs_start);
    let marker_end = crate::edit::list_marker_end(line).unwrap_or(0);
    if raw.len() <= marker_end {
        return String::new();
    }
    line[marker_end..raw.len()].trim_start_matches([' ', '\t']).to_string()
}

/// 引用容器的行前文本：剥掉逐层 `>` 标记，文字位移按引用竖条的留白算；
/// 引用内再起列表时，列表标记同样不参与量宽，落在标记内贴住内容起点。
fn block_line_prefix(body: &str, line_abs_start: usize, raw: &str) -> (f32, String) {
    let (levels, line_text) = quote_parts(line_start_line(body, line_abs_start));
    let marker_end = crate::edit::list_marker_end(line_text).unwrap_or(0);
    let base = levels as f32 * QUOTE_TEXT_X + if marker_end > 0 { LIST_TEXT_X } else { 0.0 };
    let (_, raw_text) = quote_parts(raw);
    if raw_text.len() <= marker_end {
        return (base, String::new());
    }
    (base, raw_text[marker_end..].trim_start_matches([' ', '\t']).to_string())
}

/// 剥掉行首空白与逐层 `>` 标记。
fn quote_parts(raw: &str) -> (usize, &str) {
    let mut text = raw.trim_start_matches([' ', '\t']);
    let mut levels = 0;
    while let Some(rest) = text.strip_prefix('>') {
        levels += 1;
        text = rest.trim_start_matches([' ', '\t']);
    }
    (levels, text)
}

fn line_start_line(body: &str, at: usize) -> &str {
    let at = at.min(body.len());
    let end = body[at..].find('\n').map_or(body.len(), |offset| at + offset);
    &body[at..end]
}

/// 去掉开头闭合的 frontmatter 块后的正文；无块即全文。
fn body_of(text: &str) -> &str {
    frontmatter::block_end(text).map_or(text, |end| &text[end..])
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

    /// 嵌套列表逐层展开：父子项都是独立揭示单元，父项源区间止于子项行首。
    #[test]
    fn nested_list_items_are_independent_units() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "- 甲\n  - 乙\n    - 丙\n- 丁\n".into(), true);
        let block = &model.blocks[0];
        assert_eq!(block.items.len(), 4, "父子项各自成单元");
        assert!(matches!(block.items[0].kind, UnitKind::Item { depth: 0, ordinal: 0 }));
        assert!(matches!(block.items[1].kind, UnitKind::Item { depth: 1, ordinal: 0 }));
        assert!(matches!(block.items[2].kind, UnitKind::Item { depth: 2, ordinal: 0 }));
        assert!(matches!(block.items[3].kind, UnitKind::Item { depth: 0, ordinal: 1 }));
        let sources: Vec<String> = (0..4)
            .map(|item| {
                let (from, to) = model.item_source(0, item).expect("项在");
                model.text[from..to].to_string()
            })
            .collect();
        assert_eq!(sources, ["- 甲", "  - 乙", "    - 丙", "- 丁"]);
        assert_eq!(model.item_at(model.body_start + 10), Some((0, 1)));
        assert_eq!(model.item_at(model.body_start + 16), Some((0, 2)));
        assert_eq!(model.item_gap_lines(0, 1), 0, "父子之间没有空行");
        let text = model.replace_item(0, 1, "  - 改").expect("项在");
        assert_eq!(text, "- 甲\n  - 改\n    - 丙\n- 丁\n");
    }

    /// 引用内的子块各自成揭示单元：段与列表分别替换，源码保留引用前缀。
    #[test]
    fn blockquote_children_are_independent_units() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "> 甲\n>\n> - 一\n> - 二\n".into(), true);
        let block = &model.blocks[0];
        assert_eq!(block.items.len(), 2, "引用内的段落与列表各一项");
        let (from, to) = model.item_source(0, 0).expect("子块在");
        assert_eq!(&model.text[from..to], "> 甲");
        let (from, to) = model.item_source(0, 1).expect("子块在");
        assert_eq!(&model.text[from..to], "> - 一\n> - 二");
        let at = model.body_start + model.body().find('一').expect("找到列表项");
        assert_eq!(model.item_at(at), Some((0, 1)));
        let text = model.replace_item(0, 1, "> - 改").expect("子块在");
        assert_eq!(text, "> 甲\n>\n> - 改\n");
    }

    /// 远端光标按所在揭示单元的行首换算：返回块 / 项、行序、装饰位移与行前文本。
    #[test]
    fn remote_cursor_maps_to_unit_line_and_prefix() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "甲\n\n- 甲\n  - 乙\n".into(), true);
        let line = model.body().find("乙").expect("找到嵌套项");
        let spot = model.cursor_spot(model.body_start + line).expect("落在嵌套项");
        assert_eq!((spot.block, spot.unit, spot.line), (1, Some(1), 0));
        assert_eq!(spot.line_prefix, "", "列表标记与缩进都不参与绘制");
        assert_eq!(spot.base_x, LIST_TEXT_X, "文字从标记列之后起算");
        // 活动编辑框画的是源码：前缀含缩进与标记，不做渲染位移
        let raw = model.source_cursor_spot(model.body_start + line).expect("源码落点");
        assert_eq!((raw.base_x, raw.line_prefix.as_str()), (0.0, "  - "));

        model.load("a.md".into(), "甲\n乙".into(), true);
        let line = model.body().find('乙').expect("找到第二行");
        let spot = model.cursor_spot(model.body_start + line).expect("落在段落");
        assert_eq!((spot.block, spot.unit, spot.line), (0, None, 1));
        assert_eq!((spot.base_x, spot.line_prefix.as_str()), (0.0, ""));

        model.load("a.md".into(), "> 甲\n> 乙\n".into(), true);
        let line = model.body().find('乙').expect("引用第二行");
        let spot = model.cursor_spot(model.body_start + line).expect("落在引用子块");
        assert_eq!(spot.line_prefix, "", "引用标记不上屏");
        assert_eq!(spot.base_x, QUOTE_TEXT_X);

        // 引用内列表：引用留白之后再让出列表标记列
        model.load("a.md".into(), "> - 甲\n".into(), true);
        let line = model.body().find('甲').expect("引用内列表项");
        let spot = model.cursor_spot(model.body_start + line).expect("落在引用内列表");
        assert_eq!(spot.line_prefix, "", "列表标记不上屏");
        assert_eq!(spot.base_x, QUOTE_TEXT_X + LIST_TEXT_X);
    }

    /// 图片尺寸声明按 `|W` / `|WxH` 记入模型：宽高分开保存，缺省为 None。
    #[test]
    fn image_only_paragraphs_carry_declared_width_and_height() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "![图|120](p.png)\n".into(), true);
        let image = model.blocks[0].image.as_ref().expect("图片块");
        assert_eq!(image.width, Some(120.0));
        assert_eq!(image.height, None);
        model.load("a.md".into(), "![图|120x80](p.png)\n".into(), true);
        let image = model.blocks[0].image.as_ref().expect("图片块");
        assert_eq!((image.width, image.height), (Some(120.0), Some(80.0)));
    }

    /// 高度单声明（`|x80`）：格式库不识别，插件补解析并从替代文字里剥掉。
    #[test]
    fn image_only_paragraphs_accept_height_only_size() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "![图|x80](p.png)\n".into(), true);
        let image = model.blocks[0].image.as_ref().expect("图片块");
        assert_eq!((image.width, image.height), (None, Some(80.0)));
        assert_eq!(image.alt, "图");
    }

    /// 显示尺寸：声明的宽高优先，单边声明按原图比例补另一边，都缺省用原图尺寸。
    #[test]
    fn image_display_size_uses_declared_or_natural_size() {
        let image = ImageBlock { src: "p.png".into(), alt: "图".into(), width: None, height: None };
        let natural = Some((640, 480));
        let size = image_display_size(&image, natural).expect("原图尺寸");
        assert_eq!(size, ImageSize { width: 640.0, height: 480.0 });
        let size = image_display_size(&ImageBlock { width: Some(320.0), ..image.clone() }, natural).expect("按宽补高");
        assert_eq!((size.width, size.height), (320.0, 240.0));
        let size = image_display_size(&ImageBlock { height: Some(120.0), ..image.clone() }, natural).expect("按高补宽");
        assert_eq!((size.width, size.height), (160.0, 120.0));
        assert!(image_display_size(&image, None).is_none(), "无声明也无原图尺寸时算不出");
        let sized = ImageBlock { width: Some(200.0), height: Some(100.0), ..image };
        assert_eq!(
            image_display_size(&sized, None).map(|size| (size.width, size.height)),
            Some((200.0, 100.0)),
            "两边都声明时不需要原图尺寸"
        );
    }

    /// 布局计算把超过可用宽的原图等比收到面板内。
    #[test]
    fn image_layout_clamps_natural_size_to_available_width() {
        let image = ImageBlock { src: "p.png".into(), alt: "图".into(), width: None, height: None };
        let size = image_layout(&image, Some((640, 480)), 320.0).expect("可布局");
        assert_eq!((size.width, size.height), (320.0, 240.0));
        let size = image_layout(&image, Some((640, 480)), 800.0).expect("原样");
        assert_eq!((size.width, size.height), (640.0, 480.0));
    }

    /// 三种写法都要给出可绘制的尺寸：`|W` 补等比高、`|HxW` 用声明值、未声明用原图，超宽都等比收缩。
    #[test]
    fn image_layout_honours_width_only_fixed_and_natural_forms() {
        let natural = Some((640, 480));
        let width_only =
            ImageBlock { src: "p.png".into(), alt: "图".into(), width: Some(120.0), height: None };
        let size = image_layout(&width_only, natural, 320.0).expect("按宽补高");
        assert_eq!((size.width, size.height), (120.0, 90.0), "宽高单边声明不收缩，高按原图比例补");
        let fixed = ImageBlock { src: "p.png".into(), alt: "图".into(), width: Some(400.0), height: Some(300.0) };
        let size = image_layout(&fixed, natural, 320.0).expect("两边都声明");
        assert_eq!((size.width, size.height), (320.0, 240.0), "超宽等比收缩，不拉伸单边");
        let natural_image = ImageBlock { src: "p.png".into(), alt: "图".into(), width: None, height: None };
        let size = image_layout(&natural_image, natural, 320.0).expect("自然尺寸");
        assert_eq!((size.width, size.height), (320.0, 240.0));
        let size = image_layout(&natural_image, natural, 800.0).expect("原样");
        assert_eq!((size.width, size.height), (640.0, 480.0));
    }

    #[test]
    fn wiki_query_tracks_unclosed_double_brackets() {
        assert_eq!(wiki_query("[[方案", 8), Some((0, "方案")));
        assert_eq!(wiki_query("甲 [[方", 9), Some((4, "方")));
        assert_eq!(wiki_query("[[方案]]", 10), None, "已闭合不再出候选");
        assert_eq!(wiki_query("[[方案\n", 9), None, "跨行不算链接");
        assert_eq!(wiki_query("[[方案|", 9), None, "别名已开始");
        assert_eq!(wiki_query("普通文字", 12), None);
    }

    #[test]
    fn wiki_candidates_prefer_same_directory_and_filter_by_query() {
        let files = vec![
            note("方案.md"),
            note("子目录/方案.md"),
            note("子目录/甲.md"),
            note("图片.png"),
        ];
        let hits = wiki_candidates(&files, "方案", Some("子目录/甲.md"), 4);
        assert_eq!(hits, ["子目录/方案.md", "方案.md"], "同目录优先，图片不进候选");
        let hits = wiki_candidates(&files, "", Some("子目录/甲.md"), 2);
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&"子目录/方案.md".to_string()));
    }

    #[test]
    fn wiki_links_resolve_by_path_directory_and_name() {
        let files = vec![note("方案.md"), note("子目录/甲.md"), note("子目录/乙.md")];
        assert_eq!(
            resolve_link(&files, Some("子目录/甲.md"), "乙.md"),
            Some("子目录/乙.md".into()),
            "相对当前笔记目录"
        );
        assert_eq!(resolve_link(&files, Some("甲.md"), "子目录/乙"), Some("子目录/乙.md".into()));
        assert_eq!(resolve_link(&files, Some("子目录/甲.md"), "方案"), Some("方案.md".into()));
        assert_eq!(resolve_link(&files, Some("子目录/甲.md"), "../方案.md"), Some("方案.md".into()));
        assert_eq!(resolve_link(&files, Some("子目录/甲.md"), "不存在"), None);
        assert_eq!(wiki_link("子目录/乙.md"), "[[子目录/乙.md|乙]]");
        assert!(is_external("https://example.com"), "外链走系统浏览器");
        assert!(!is_external("乙.md"));
    }

    fn note(path: &str) -> VaultFileEntry {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        VaultFileEntry { path: path.into(), name, icon: "file-text".into(), openable: true }
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
    fn item_gap_line_range_locates_lines_between_items() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "- 甲\n\n  \n- 乙\n".into(), true);
        assert_eq!(model.item_gap_lines(0, 1), 2, "两项之间两根空行");
        let (from, to) = model.item_gap_line_range(0, 1, 0).expect("项间首行");
        assert_eq!(&model.text[from..to], "", "首行为空");
        let (from, to) = model.item_gap_line_range(0, 1, 1).expect("项间次行");
        assert_eq!(&model.text[from..to], "  ", "空格属于该空行内容");
        assert!(model.item_gap_line_range(0, 0, 0).is_none(), "首项没有前项间空行");
        assert_eq!(model.item_gap_line_at(from), Some((0, 1, 1)), "光标落点归到项间空行");
        assert_eq!(model.item_gap_line_at(to), Some((0, 1, 1)), "行尾仍算该空行");
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
    fn frontmatter_only_replace_keeps_undo_history() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\n标签: 甲\n---\n正文\n".into(), true);
        model.edit_text("---\n标签: 甲\n---\n正文二\n".into());
        assert_eq!(model.undo.len(), 1);
        model.replace_text("---\n标签: 乙\n---\n正文二\n".into());
        assert_eq!(model.undo.len(), 2, "属性写回只追加一次可撤销编辑");
        model.undo();
        assert!(model.text.contains("标签: 甲") && model.text.contains("正文二"), "撤销只回退属性，正文不动");
    }

    #[test]
    fn non_map_frontmatter_is_not_editable() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\n- 甲\n- 乙\n---\n正文\n".into(), true);
        let (pairs, editable) = model.props.data.as_ref().expect("有 frontmatter 即带属性数据");
        assert!(pairs.is_empty(), "序列根没有属性行");
        assert!(!editable, "根不是映射时不开放属性编辑");
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
