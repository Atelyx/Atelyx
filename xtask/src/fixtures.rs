//! 对拍快照重生成：语料（输入与选项）保持不动，期望值用当前实现重写。
//! 只在语义变更被确认为预期后执行；重生成后由一致性测试守住后续回归。

use std::collections::HashMap;
use std::path::Path;

use atelyx_core::frontmatter;
use atelyx_core::markdown::{
    self, LinkForm, MentionCandidate, ParseOptions,
};
use serde_json::Value;

const MARKDOWN: &str = "fixtures/markdown/parse.json";
const FRONTMATTER: &str = "fixtures/frontmatter/parse.json";

/// 重写两份快照的期望值，返回改写的文件清单。
pub fn regenerate(root: &Path) -> Result<Vec<&'static str>, String> {
    markdown_cases(root)?;
    frontmatter_cases(root)?;
    Ok(vec![MARKDOWN, FRONTMATTER])
}

fn markdown_cases(root: &Path) -> Result<(), String> {
    let path = root.join(MARKDOWN);
    let mut doc = read_json(&path)?;
    let cases = doc["cases"]
        .as_array_mut()
        .ok_or_else(|| format!("{MARKDOWN} 缺 cases 数组"))?;
    for case in cases {
        let name = case["name"].as_str().unwrap_or_default().to_string();
        let source = case["source"].as_str().unwrap_or_default().to_string();
        let parsed = markdown::parse_markdown(&source, &options_of(case));
        case["expected"] = serde_json::to_value(&parsed)
            .map_err(|e| format!("语料 {name} 序列化失败：{e}"))?;
    }
    write_json(&path, &doc)
}

/// 语料里的 options 与一致性测试同一口径：links 映射把 href 编成形态名，mentions 原样传入。
fn options_of(case: &Value) -> ParseOptions {
    let mentions: Vec<MentionCandidate> =
        serde_json::from_value(case["mentions"].clone()).unwrap_or_default();
    let links: HashMap<String, String> =
        serde_json::from_value(case["links"].clone()).unwrap_or_default();
    ParseOptions {
        resolve_link: Some(Box::new(move |href: &str| match links.get(href).map(String::as_str) {
            Some("external") => LinkForm::External,
            Some("wiki") => LinkForm::Wiki,
            Some("path") => LinkForm::Path,
            Some("create") => LinkForm::Create,
            _ => LinkForm::Plain,
        })),
        mentions,
    }
}

fn frontmatter_cases(root: &Path) -> Result<(), String> {
    let path = root.join(FRONTMATTER);
    let mut doc = read_json(&path)?;
    let cases = doc["cases"]
        .as_array_mut()
        .ok_or_else(|| format!("{FRONTMATTER} 缺 cases 数组"))?;
    for case in cases {
        let source = case["source"].as_str().unwrap_or_default().to_string();
        if case.get("parse").is_some() {
            let parsed = frontmatter::parse(&source);
            case["parse"] = serde_json::json!({
                "data": frontmatter::snapshot_json(&parsed.data),
                "body": parsed.body,
                "fmPrefix": parsed.fm_prefix,
                "ok": parsed.ok,
            });
        }
        if let Some(stringify) = case.get("stringify").cloned() {
            let data = frontmatter::yaml_from_snapshot(&stringify["data"]);
            let body = stringify["body"].as_str().unwrap_or_default();
            case["stringify"] = serde_json::json!({
                "data": stringify["data"],
                "body": stringify["body"],
                "out": frontmatter::stringify(&data, body),
            });
        }
    }
    write_json(&path, &doc)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{} 不是合法 JSON：{e}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("序列化 {} 失败：{e}", path.display()))?;
    std::fs::write(path, format!("{text}\n")).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}
