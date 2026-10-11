//! frontmatter 对拍：快照 = 唯一期望源（`fixtures/frontmatter/parse.json`）。
//!
//! data 以 typed 编码进快照（t = 类型标签；map 为键值对数组保键序），Rust 侧按同一编码重建比较。

use atelyx_core::frontmatter::{self, Yaml};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/frontmatter/parse.json")
}

/// Yaml → typed JSON（num 在整数安全范围内落为整数，对齐 JSON.stringify(3) = "3"）。
fn typed_json(y: &Yaml) -> Value {
    match y {
        Yaml::Null => serde_json::json!({ "t": "null" }),
        Yaml::Bool(b) => serde_json::json!({ "t": "bool", "v": b }),
        Yaml::Num(f) => {
            let v = if f.fract() == 0.0 && f.abs() < 1e15 {
                Value::from(*f as i64)
            } else {
                Value::from(*f)
            };
            serde_json::json!({ "t": "num", "v": v })
        }
        Yaml::Date(d) => serde_json::json!({ "t": "date", "v": d }),
        Yaml::Str(s) => serde_json::json!({ "t": "str", "v": s }),
        Yaml::Seq(items) => serde_json::json!({ "t": "seq", "v": items.iter().map(typed_json).collect::<Vec<_>>() }),
        Yaml::Map(pairs) => serde_json::json!({
            "t": "map",
            "v": pairs.iter().map(|(k, v)| serde_json::json!([k, typed_json(v)])).collect::<Vec<_>>(),
        }),
    }
}

/// typed JSON → Yaml（stringify 语料的输入重建）。
fn yaml_from_typed(v: &Value) -> Yaml {
    let t = v["t"].as_str().unwrap_or_default();
    match t {
        "null" => Yaml::Null,
        "bool" => Yaml::Bool(v["v"].as_bool().unwrap_or(false)),
        "num" => Yaml::Num(v["v"].as_f64().unwrap_or_default()),
        "date" => Yaml::Date(v["v"].as_str().unwrap_or_default().into()),
        "str" => Yaml::Str(v["v"].as_str().unwrap_or_default().into()),
        "seq" => Yaml::Seq(v["v"].as_array().map(|a| a.iter().map(yaml_from_typed).collect()).unwrap_or_default()),
        "map" => Yaml::Map(
            v["v"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|pair| {
                            let pair = pair.as_array().unwrap();
                            (pair[0].as_str().unwrap_or_default().into(), yaml_from_typed(&pair[1]))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => Yaml::Null,
    }
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
            assert_eq!(typed_json(&result.data), parse["data"], "语料 {name}: data 不一致");
            assert_eq!(result.body, parse["body"].as_str().unwrap_or_default(), "语料 {name}: body 不一致");
            assert_eq!(
                result.fm_prefix,
                parse["fmPrefix"].as_str().unwrap_or_default(),
                "语料 {name}: fmPrefix 不一致"
            );
            assert_eq!(result.ok, parse["ok"].as_bool().unwrap_or_default(), "语料 {name}: ok 不一致");
        }
        if let Some(st) = case.get("stringify") {
            let data = yaml_from_typed(&st["data"]);
            let out = frontmatter::stringify(&data, st["body"].as_str().unwrap_or_default());
            assert_eq!(out, st["out"].as_str().unwrap_or_default(), "语料 {name}: 序列化输出不一致");
        }
    }
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
