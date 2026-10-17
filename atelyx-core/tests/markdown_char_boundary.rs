//! Markdown 偏移体检：解析产出的每个源区间必须落在 UTF-8 字符边界上，
//! 且渲染层按这些区间重解析时不得崩溃。语料由固定种子拼装，覆盖各类块级与行内构造。

use atelyx_core::markdown::{
    parse_inline, parse_markdown, MarkdownBlock, MarkdownTableCell, ParseOptions,
};
use serde_json::Value;
use std::cell::RefCell;
use std::panic;

/// 承载源偏移的字段名（与对拍 harness 同一口径）。
const OFFSET_FIELDS: [&str; 4] = ["from", "to", "contentFrom", "contentTo"];

/// 语料篇数。
const DOCS: usize = 4000;

thread_local! {
    /// panic 文本（钩子在 panic 线程上写，逐线程隔离）。
    static PANIC_TEXT: RefCell<String> = RefCell::new(String::new());
}

/// xorshift64：无外部依赖的确定性语料源。
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// 前置填充：混排 1/2/3/4 字节字符，决定后续记号落在何种字节对齐上。
const FILLERS: &[&str] = &[
    "甲", "中文测试", "a", "€", "😀", "文字文字", " ", "\t", "ab", "，",
];

/// 记号：块级与行内构造各一形态，正文混用多字节字符；行尾空格/制表与 CRLF 均属文档常态。
const TOKENS: &[&str] = &[
    "甲乙丙", "中文测试文本", "hello world", "段落 甲  ",
    "# 一级 甲", "## 二级 乙 ##", "### 甲#乙 尾",
    "- 无序 甲", "+ 列表 www.example.com", "1. 有序 甲", "3) 括号序 乙",
    "- [ ] 未勾 甲", "- [x] 已勾 乙",
    "> 引用 甲", "> [!note] 提示 乙", ">> 嵌套 > 甲", ">  双空格 乙",
    "```rs 甲", "```", "~~~ 乙", "    缩进正文 甲", "`行内码 甲`", "``a`b``",
    "| 列甲 | 列乙 |", "| --- | :-: |", "| www.例.com | [文本](目标) |", "| 甲 | `码 乙` | 尾 |",
    "| 中文 | 甲 |  ", "| 乙 | 丙 |\t", "| 头 | 甲 |\n| --- |\n| 乙 |\t",
    "[链接](http://www.example.com/甲)", "![图](甲.png)", "![尺寸](甲.png|100x200)",
    "[[]]", "[[双链 甲]]", "[^1] 脚注 甲", "中文[^注]乙", "[ref]: http://www.example.com 甲",
    "<http://例.中国/x>", "<b>粗 甲</b>", "</b>游离", "<!-- 注释 甲 -->",
    "&amp;", "&#20013;", "==高亮 甲==", "%%注释 乙%%", "$x_甲^2$", "\\alpha 乙",
    "http://www.example.com/甲?b=1.", "www.甲.com", "https://example.com/(带括号)(甲)",
    "**粗 甲**", "*斜 乙*", "_下_甲_", "~~删 乙~~", "***混***甲", "a*b*c甲",
    "---", "***", "___", "两空格硬换行  ", "尾随制表\t", "  缩进段落 甲",
    "全角。中文，标点（甲）", "　全角空格 乙", "\\甲\\*转义",
    "```md\n甲 乙\n```\n", "$$\n\\alpha 甲\n$$\n",
    "| 头 | 乙 |\n| --- | --- |\n| www.例.com | 尾 |\n",
    "- 甲\n  > 引用内 乙\n", "> - 项 甲\n>   续行 乙\n",
    "> [!warning] 标题\n> 正文 http://www.example.com/甲\n", "---\ntitle: 甲\n---\n",
];

/// 行间隔：含 CRLF、空行与制表，覆盖各行首的不同字节起点。
const SEPS: &[&str] = &[" ", "  ", "\n", "\r\n", "\n\n", "\t", "\n  ", "\r\n\r\n", "\n\n\n"];

fn gen_doc(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(3) {
        out.push_str(FILLERS[rng.below(FILLERS.len())]);
    }
    for _ in 0..8 + rng.below(20) {
        out.push_str(TOKENS[rng.below(TOKENS.len())]);
        if !out.ends_with('\n') {
            out.push_str(SEPS[rng.below(SEPS.len())]);
        }
    }
    out
}

/// 在 catch_unwind 下执行：panic 转为消息文本，测试进程继续跑下一篇。
fn capture<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    PANIC_TEXT.with(|p| p.borrow_mut().clear());
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "未知 panic".to_string());
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        PANIC_TEXT.with(|p| *p.borrow_mut() = format!("{at}: {msg}"));
    }));
    let out = panic::catch_unwind(panic::AssertUnwindSafe(f));
    panic::set_hook(prev);
    out.map_err(|_| PANIC_TEXT.with(|p| p.borrow().clone()))
}

/// 非边界偏移的定位：指出字段及其切进哪个字符；非边界时不能用 `source[..i]` 切片。
fn boundary_report(source: &str, i: usize, field: &str) -> Option<String> {
    if i > source.len() {
        return Some(format!("偏移 {i}（字段 {field}）越出源末尾 {}", source.len()));
    }
    if source.is_char_boundary(i) {
        return None;
    }
    let (at, ch) = source
        .char_indices()
        .find(|(idx, c)| idx + c.len_utf8() > i)
        .unwrap_or((0, '\u{0}'));
    Some(format!(
        "偏移 {i}（字段 {field}）切进 {ch:?}（字节 {at}..{}）内",
        at + ch.len_utf8()
    ))
}

/// 递归校验 JSON 里的每个偏移字段。
fn walk_offsets(v: &Value, source: &str, bad: &mut Option<String>) {
    if bad.is_some() {
        return;
    }
    match v {
        Value::Array(items) => {
            for x in items {
                walk_offsets(x, source, bad);
            }
        }
        Value::Object(map) => {
            for f in OFFSET_FIELDS {
                if let Some(n) = map.get(f).and_then(Value::as_u64) {
                    if let Some(e) = boundary_report(source, n as usize, f) {
                        *bad = Some(e);
                        return;
                    }
                }
            }
            for x in map.values() {
                walk_offsets(x, source, bad);
            }
        }
        _ => {}
    }
}

/// 渲染层会按区间重解析的块内范围（表格单元格、列表项、引用子块、围栏正文）。
fn collect_reparsed(b: &MarkdownBlock, out: &mut Vec<(usize, usize)>) {
    let cell = |c: &MarkdownTableCell| (c.from, c.to);
    match b {
        MarkdownBlock::Table { header, rows, .. } => {
            out.extend(header.iter().map(cell));
            for row in rows {
                out.extend(row.iter().map(cell));
            }
        }
        MarkdownBlock::List { items, .. } => {
            for it in items {
                out.push((it.from, it.to));
                for c in &it.children {
                    collect_reparsed(c, out);
                }
            }
        }
        MarkdownBlock::Blockquote { children, .. } => {
            for c in children {
                collect_reparsed(c, out);
            }
        }
        MarkdownBlock::FencedCode { content_from, content_to, .. } => {
            out.push((*content_from, *content_to));
        }
        _ => {}
    }
}

/// 单文档体检：真实入口 + 按区间重解析 + 逐字段边界校验；返回首个失败原因。
fn check_doc(source: &str) -> Option<String> {
    let opts = ParseOptions::default();
    let doc = match capture(|| parse_markdown(source, &opts)) {
        Ok(doc) => doc,
        Err(msg) => return Some(msg),
    };
    let mut ranges = Vec::new();
    for b in &doc.blocks {
        collect_reparsed(b, &mut ranges);
    }
    for (from, to) in ranges {
        if to < from {
            return Some(format!("区间倒置：from {from} > to {to}"));
        }
        if let Err(msg) = capture(|| parse_inline(source, from, to, &opts)) {
            return Some(format!("区间 [{from},{to}) 重解析 panic: {msg}"));
        }
    }
    let value = serde_json::to_value(&doc.blocks).ok()?;
    let mut bad = None;
    walk_offsets(&value, source, &mut bad);
    bad
}

#[test]
fn parse_offsets_stay_on_char_boundary() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut failures: Vec<String> = Vec::new();
    for n in 0..DOCS {
        let doc = gen_doc(&mut rng);
        if let Some(msg) = check_doc(&doc) {
            failures.push(format!("#{n} {msg}\n文档: {doc:?}"));
            if failures.len() == 3 {
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "语料体检失败 {} 篇：\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
