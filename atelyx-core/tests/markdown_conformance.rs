//! Markdown 内核对拍：TS 内核 dump 的快照（`fixtures/markdown/parse.json`）= 唯一期望源。
//!
//! 快照由 `MD_DUMP=1 pnpm vitest run src/utils/markdownCore/fixture-dump.test.ts` 生成。
//! 偏移空间：TS 快照的 from/to 为 UTF-16 单位，本测试按 source 逐字映射转换为
//! Rust 字节偏移后再比较；Rust 解析器全程使用字节偏移。

use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/markdown/parse.json")
}

/// 快照 JSON 里承载源偏移的字段名。
const OFFSET_FIELDS: [&str; 4] = ["from", "to", "contentFrom", "contentTo"];

/// 构建 UTF-16 单位下标 → 字节偏移映射（含末位哨兵）。
fn utf16_to_byte_map(source: &str) -> Vec<usize> {
    let mut map = Vec::with_capacity(source.chars().count() + 1 + 1);
    let mut byte = 0usize;
    for ch in source.chars() {
        map.push(byte);
        byte += ch.len_utf8();
        if (ch as u32) > 0xFFFF {
            // 代理对：UTF-16 占 2 单位，补一个相同映射
            map.push(byte);
        }
    }
    map.push(byte);
    map
}

/// 递归把 expected 里的偏移字段从 UTF-16 单位转换为字节偏移。
fn convert_offsets(value: &mut Value, map: &[usize]) {
    match value {
        Value::Object(obj) => {
            for (k, v) in obj.iter_mut() {
                if OFFSET_FIELDS.contains(&k.as_str()) {
                    if let Some(u16idx) = v.as_u64() {
                        *v = Value::from(map[u16idx as usize]);
                    }
                } else {
                    convert_offsets(v, map);
                }
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                convert_offsets(v, map);
            }
        }
        _ => {}
    }
}

fn snapshot_cases() -> Vec<(String, Value)> {
    let raw = fs::read_to_string(fixture_path()).expect(
        "fixtures/markdown/parse.json 缺失：先跑 MD_DUMP=1 pnpm vitest run src/utils/markdownCore/fixture-dump.test.ts",
    );
    let root: Value = serde_json::from_str(&raw).unwrap();
    let cases = root["cases"].as_array().expect("快照缺 cases 数组");
    assert!(!cases.is_empty(), "对拍快照为空");
    cases
        .iter()
        .map(|value| {
            let name = value["name"].as_str().expect("快照缺 name").to_string();
            (name, value.clone())
        })
        .collect()
}

/// 找出两棵 JSON 树的第一处差异路径（诊断用，避免整树 dump 淹没信息）。
fn first_diff(a: &Value, b: &Value, path: &mut String) -> Option<String> {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
            for k in keys {
                let push_len = path.len();
                path.push('.');
                path.push_str(k);
                let ma_v = ma.get(k).unwrap_or(&Value::Null);
                let mb_v = mb.get(k).unwrap_or(&Value::Null);
                if let Some(d) = first_diff(ma_v, mb_v, path) {
                    return Some(d);
                }
                path.truncate(push_len);
            }
            None
        }
        (Value::Array(aa), Value::Array(ab)) => {
            let n = aa.len().max(ab.len());
            for i in 0..n {
                let push_len = path.len();
                path.push_str(&format!("[{i}]"));
                let av = aa.get(i).unwrap_or(&Value::Null);
                let bv = ab.get(i).unwrap_or(&Value::Null);
                if let Some(d) = first_diff(av, bv, path) {
                    return Some(d);
                }
                path.truncate(push_len);
            }
            None
        }
        _ => (a != b).then(|| {
            format!(
                "{path}: 期望 {a}，实际 {b}",
                a = truncate(a),
                b = truncate(b)
            )
        }),
    }
}

fn truncate(v: &Value) -> String {
    let s = v.to_string();
    let t: String = s.chars().take(120).collect();
    if t.len() < s.len() { format!("{t}…") } else { s }
}

#[test]
fn parse_conformance() {
    for (name, value) in snapshot_cases() {
        let source = value["source"].as_str().expect("快照缺 source");
        let map = utf16_to_byte_map(source);
        let mut expected = value["expected"].clone();
        convert_offsets(&mut expected, &map);

        let mentions: Vec<atelyx_core::markdown::MentionCandidate> =
            serde_json::from_value(value["mentions"].clone()).unwrap_or_default();
        let links: std::collections::HashMap<String, String> =
            serde_json::from_value(value["links"].clone()).unwrap_or_default();

        let options = atelyx_core::markdown::ParseOptions {
            resolve_link: Some(Box::new(move |href: &str| {
                match links.get(href).map(|s| s.as_str()) {
                    Some("external") => atelyx_core::markdown::LinkForm::External,
                    Some("wiki") => atelyx_core::markdown::LinkForm::Wiki,
                    Some("path") => atelyx_core::markdown::LinkForm::Path,
                    Some("create") => atelyx_core::markdown::LinkForm::Create,
                    Some("plain") | None => atelyx_core::markdown::LinkForm::Plain,
                    Some(other) => panic!("未知链接形态编码: {other}"),
                }
            })),
            mentions,
        };
        let actual = atelyx_core::markdown::parse_markdown(source, &options);
        let actual_value = serde_json::to_value(&actual).unwrap();

        if actual_value != expected {
            let mut path = String::from("$");
            let diff = first_diff(&expected, &actual_value, &mut path)
                .unwrap_or_else(|| "结构差异".into());
            panic!("[{name}] {diff}");
        }
    }
}

#[test]
fn snapshot_contract_roundtrip() {
    // serde 契约自检：快照 → core 类型 → 回 JSON（UTF-16 转换后）与原快照逐值相等。
    for (name, value) in snapshot_cases() {
        let source = value["source"].as_str().expect("快照缺 source");
        let map = utf16_to_byte_map(source);
        let mut expected = value["expected"].clone();
        convert_offsets(&mut expected, &map);
        let doc: atelyx_core::markdown::MarkdownDocument =
            serde_json::from_value(expected.clone())
                .unwrap_or_else(|e| panic!("{name}: 快照与 core 类型不兼容: {e}"));
        let back = serde_json::to_value(&doc).unwrap();
        assert_eq!(back, expected, "{name}: roundtrip 后 JSON 不等");
    }
}

#[test]
fn snapshot_options_contract() {
    // 语料 options（mentions/links 映射）须能被 core 的 ParseOptions 表达。
    for (name, value) in snapshot_cases() {
        if !value["mentions"].is_null() {
            let mentions: Vec<atelyx_core::markdown::MentionCandidate> =
                serde_json::from_value(value["mentions"].clone())
                    .unwrap_or_else(|e| panic!("{name}: mentions 形状不兼容: {e}"));
            assert!(!mentions.is_empty(), "{name}: mentions 不应为空");
        }
        if !value["links"].is_null() {
            assert!(value["links"].as_object().is_some(), "{name}: links 应为映射");
        }
    }
}
