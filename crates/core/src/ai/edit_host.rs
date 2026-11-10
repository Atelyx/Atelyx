//! AI 编辑出口：内容写入交宿主执行（壳负责保存链、历史与协作），工具层不直接落盘。

use std::future::Future;
use std::pin::Pin;

use crate::history::HistoryAuthor;

/// AI 身份（本次 run 使用的 Agent）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiIdentity {
    pub agent_id: String,
    pub agent_name: String,
}

/// 一次 AI 编辑的操作者（署名沿用历史口径）与 Agent 身份。
#[derive(Debug, Clone)]
pub struct EditActor {
    pub author: HistoryAuthor,
    pub agent: AiIdentity,
}

/// 文档类型（历史 kind 与保存链按此分派）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Note,
    Table,
    Canvas,
}

/// 笔记编辑操作：统一作用于宿主给出的基线正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteOp {
    /// 整文件覆盖。
    Write(String),
    /// 末尾追加（文件不存在 = 新建）。
    Append(String),
    /// 精确替换（`all = false` 时 `find` 必须唯一命中）。
    Replace { find: String, replace: String, all: bool },
}

/// 表格编辑操作（行 / 字段按稳定 id 定位；一次调用 = 一个补丁）。
#[derive(Debug, Clone, PartialEq)]
pub enum TableOp {
    /// 写单元格（value 形态随字段类型；null = 清空）。
    SetCell { row_id: String, field_id: String, value: serde_json::Value },
    /// 新增行（cells 按字段 id 给初值；缺省 = 空行）。
    AddRow { cells: Vec<(String, serde_json::Value)> },
    /// 删除行（稳定 id）。
    DeleteRows { row_ids: Vec<String> },
}

/// 画布编辑操作（节点 / 连线按稳定 id 定位；一次调用 = 一个补丁）。
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasOp {
    /// 新建文本卡（x / y 缺省 = 自动落位）。
    AddTextCard { text: String, x: Option<f64>, y: Option<f64> },
    /// 新建笔记卡（引用仓库内笔记）。
    AddNoteCard { path: String, x: Option<f64>, y: Option<f64> },
    /// 连线（from / to = 节点 id）。
    Connect { from: String, to: String, kind: Option<String> },
    /// 改写文本卡正文。
    UpdateText { node_id: String, text: String },
    /// 删除节点（连带其连线）。
    DeleteNode { node_id: String },
}

/// 异步编辑结果（不引入 async-trait 依赖）。
pub type EditFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// 宿主编辑出口：壳实现保存链（本地写盘队列 / 空间端点）+ 历史 + 在场。
pub trait AiEditHost: Send + Sync {
    /// 应用一次笔记编辑：`Ok(成功提示文本)` / `Err(可读失败原因)`，失败不得静默。
    fn apply_note(&self, path: &str, op: NoteOp, actor: &EditActor) -> EditFuture<'_>;
    /// 应用一次表格编辑（补丁按稳定 id 合并，打开会话先合入再保存）。
    fn apply_table(&self, path: &str, ops: Vec<TableOp>, actor: &EditActor) -> EditFuture<'_>;
    /// 应用一次画布编辑（补丁按稳定 id 合并，打开会话先合入再保存）。
    fn apply_canvas(&self, path: &str, ops: Vec<CanvasOp>, actor: &EditActor) -> EditFuture<'_>;
}

/// 精确替换：缺省要求唯一命中（多处命中报错、不写入）；`all` = 替换全部出现。
/// 返回替换后的全文与替换处数。
pub fn replace_exact(
    text: &str,
    find: &str,
    replace: &str,
    all: bool,
) -> Result<(String, usize), String> {
    if find.is_empty() {
        return Err("find 不能为空".to_string());
    }
    let count = text.matches(find).count();
    if count == 0 {
        return Err("未找到要替换的原文（find 精确匹配失败）".to_string());
    }
    if count > 1 && !all {
        return Err(format!("find 出现 {count} 次，请提供更精确的原文或设 replace_all=true"));
    }
    Ok((text.replace(find, replace), count))
}

/// 打开且脏时的三方合流决策：以 base 为共同祖先保留两侧改动；
/// 无法安全对齐（重叠改动）返回 Err，调用方不得写盘、不得改动草稿。
pub fn merge_note_edit(base: &str, draft: &str, ai_text: &str) -> Result<String, String> {
    if crate::merge3::merge3_conflict(base, draft, ai_text) {
        return Err("文档有并发改动，AI 修改未落盘".to_string());
    }
    Ok(crate::merge3::merge3(base, draft, ai_text, crate::merge3::ConflictSide::Ours))
}

/// 在基线正文上应用一次编辑操作：返回 (结果全文, 替换处数)。
pub fn apply_note_op(base: &str, op: &NoteOp) -> Result<(String, usize), String> {
    match op {
        NoteOp::Write(text) => Ok((text.clone(), 0)),
        NoteOp::Append(text) => Ok((format!("{base}{text}"), 0)),
        NoteOp::Replace { find, replace, all } => replace_exact(base, find, replace, *all),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_ops_apply_to_base() {
        let base = "# 标题\n\n- 一\n- 一\n";
        assert_eq!(apply_note_op(base, &NoteOp::Write("新".into())).unwrap().0, "新");
        assert_eq!(apply_note_op("a", &NoteOp::Append("b".into())).unwrap().0, "ab");
        let op = NoteOp::Replace { find: "- 一".into(), replace: "- 二".into(), all: false };
        // 多处命中且未开 all：报错（不动内容）
        assert!(apply_note_op(base, &op).unwrap_err().contains("2 次"));
        let op = NoteOp::Replace { find: "- 一".into(), replace: "- 二".into(), all: true };
        let (text, count) = apply_note_op(base, &op).unwrap();
        assert_eq!(text, "# 标题\n\n- 二\n- 二\n");
        assert_eq!(count, 2);
        let op = NoteOp::Replace { find: "无".into(), replace: "x".into(), all: false };
        assert!(apply_note_op(base, &op).is_err());
    }

    #[test]
    fn merge_keeps_both_sides_and_refuses_overlap() {
        let base = "a\nb\nc\n";
        // 用户改首行、AI 改末行：合流保留两侧
        assert_eq!(merge_note_edit(base, "A\nb\nc\n", "a\nb\nC\n").unwrap(), "A\nb\nC\n");
        // 同一行两侧都改：拒绝（调用方不写盘）
        assert!(merge_note_edit(base, "a\nX\nc\n", "a\nY\nc\n").is_err());
        // 用户未改：直接取 AI 结果
        assert_eq!(merge_note_edit(base, base, "a\nb\nC\n").unwrap(), "a\nb\nC\n");
    }
}
