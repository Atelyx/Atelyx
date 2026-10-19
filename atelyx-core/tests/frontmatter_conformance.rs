//! frontmatter 对拍：快照 = 唯一期望源（`fixtures/frontmatter/parse.json`）。
//!
//! 快照里的 data 用带类型标签的 JSON 编码（`t` = 类型，map 为键值对数组保键序），
//! 编码实现与快照生成器共用（`frontmatter::snapshot_json`），避免两边口径漂移。

use atelyx_core::frontmatter::{self, Yaml};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/frontmatter/parse.json")
}

#[test]
fn conformance_matches_snapshot() {
    let raw = fs::read_to_string(fixture_path()).expect(
        "fixtures/frontmatter/parse.json 缺失：对拍快照应随源码入库",
    );
    let snapshot: Value = serde_json::from_str(&raw).unwrap();
    let cases = snapshot["cases"].as_array().expect("快照缺 cases 数组");
    assert!(cases.len() > 20, "语料规模兜底：快照疑似被清空");

    for case in cases {
        let name = case["name"].as_str().unwrap_or_default();
        if let Some(parse) = case.get("parse") {
            let result = frontmatter::parse(case["source"].as_str().unwrap_or_default());
            assert_eq!(frontmatter::snapshot_json(&result.data), parse["data"], "语料 {name}: data 不一致");
            assert_eq!(result.body, parse["body"].as_str().unwrap_or_default(), "语料 {name}: body 不一致");
            assert_eq!(
                result.fm_prefix,
                parse["fmPrefix"].as_str().unwrap_or_default(),
                "语料 {name}: fmPrefix 不一致"
            );
            assert_eq!(result.ok, parse["ok"].as_bool().unwrap_or_default(), "语料 {name}: ok 不一致");
        }
        if let Some(st) = case.get("stringify") {
            let data = frontmatter::yaml_from_snapshot(&st["data"]);
            let out = frontmatter::stringify(&data, st["body"].as_str().unwrap_or_default());
            assert_eq!(out, st["out"].as_str().unwrap_or_default(), "语料 {name}: 序列化输出不一致");
        }
    }
}

/// 空属性（空串 / null）可直接切换为布尔与空列表，供面板「先建空属性再改类型」的流程使用。
#[test]
fn empty_value_converts_to_bool_and_empty_list() {
    use atelyx_core::frontmatter::PropType;
    let empty = Yaml::Str(String::new());
    assert_eq!(
        frontmatter::convert_value(&empty, PropType::Boolean),
        Some(Yaml::Bool(false))
    );
    assert_eq!(
        frontmatter::convert_value(&Yaml::Null, PropType::Boolean),
        Some(Yaml::Bool(false))
    );
    assert_eq!(
        frontmatter::convert_value(&empty, PropType::List),
        Some(Yaml::Seq(Vec::new()))
    );
}

/// 行首空白的多行字符串：字面量块不安全，序列化回落双引号转义；产物必须可解析回原值。
#[test]
fn multiline_with_leading_space_roundtrips_via_double_quotes() {
    let value = "首行\n  缩进行\n\t跳格行";
    let data = frontmatter::Yaml::Map(vec![("text".to_string(), frontmatter::Yaml::Str(value.to_string()))]);
    let out = frontmatter::stringify(&data, "正文");
    assert!(
        out.contains("text: \"首行\\n  缩进行\\n\\t跳格行\""),
        "回落产物应为双引号转义形态，实际：{:?}",
        out
    );
    let parsed = frontmatter::parse(&out);
    assert!(parsed.ok, "回落产物必须可解析：{out}");
    assert_eq!(
        parsed
            .data
            .as_map()
            .and_then(|m| m.first())
            .map(|(_, v)| v.clone()),
        Some(frontmatter::Yaml::Str(value.to_string())),
        "roundtrip 值一致"
    );
}
