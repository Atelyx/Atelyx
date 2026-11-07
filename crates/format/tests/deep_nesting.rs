//! 深嵌套构造体检：引用 / 列表 / 强调 / YAML 嵌套的递归入口必须有深度上限，
//! 恶意或误粘贴的深嵌套正文（含协作对端写入的）不得打出栈溢出。真实内容远达不到上限，
//! 到顶降级为字面文本 / 平铺段落。

use atelyx_format::frontmatter;
use atelyx_format::markdown::{parse_inline, parse_markdown, ParseOptions};

/// 千级嵌套引用：未设上限时逐层复制全文并递归重解，栈与耗时都按嵌套数放大。
#[test]
fn deep_blockquotes_do_not_overflow_the_stack() {
    let source = "> ".repeat(5_000) + "正文";
    let doc = parse_markdown(&source, &ParseOptions::default());
    assert!(!doc.blocks.is_empty());
}

/// 千级嵌套列表：与引用共用容器递归，同一上限拦住。
#[test]
fn deep_lists_do_not_overflow_the_stack() {
    let mut source = String::new();
    for depth in 0..500 {
        source.push_str(&"  ".repeat(depth));
        source.push_str("- 项\n");
    }
    let doc = parse_markdown(&source, &ParseOptions::default());
    assert!(!doc.blocks.is_empty());
}

/// 千级嵌套强调对：`*_` 交替构造每层都可开可闭的嵌套，未设上限时递归配对打穿栈。
#[test]
fn deep_emphasis_does_not_overflow_the_stack() {
    let source = format!("{}a{}", "*_".repeat(5_000), "_*".repeat(5_000));
    let spans = parse_inline(&source, 0, source.len(), &ParseOptions::default());
    assert!(!spans.is_empty());
}

/// 千级 YAML 流式嵌套：事件遍历递归无上限时打穿栈。200 层越过解析层深度上限（64）
/// 且低于上游 yaml 解析器自身的流式嵌套拒绝线（256 层），到顶层应降级解析而非整份被拒；
/// 序列与映射混用验证跳过层两侧对称计数（只按单侧计数会提前终止、整份被拒）。
#[test]
fn deep_yaml_nesting_does_not_overflow_the_stack() {
    let body = format!("---\na: {}x{}\n---\n正文", "[".repeat(200), "]".repeat(200));
    let parsed = frontmatter::parse(&body);
    assert!(parsed.ok, "纯序列深嵌套应降级解析而非整份被拒");
    // 混合构造须带标量键（`[{` 直嵌会让序列成为映射键，被受限解析器按设计拒绝）；
    // 100 对 = 200 层，越过解析层深度上限 64，且低于上游扫描器的 256 层流式嵌套拒绝线
    let mixed = format!("---\na: {}x{}\n---\n正文", "[{k: ".repeat(100), "}]".repeat(100));
    let parsed = frontmatter::parse(&mixed);
    assert!(parsed.ok, "混用两类容器的深嵌套同样应降级解析而非整份被拒");
    // 超过上游解析器拒绝线的输入被响亮拒绝（ScanError，不崩溃）；只要不栈溢出即可
    let deep = format!("---\na: {}x{}\n---\n正文", "[".repeat(5_000), "]".repeat(5_000));
    let _ = frontmatter::parse(&deep);
}
