//! 对话发送的纯步骤：引用注入块、refs 落盘字段、user 消息、截断点与压缩转写。

use serde_json::{json, Value};

use super::agent::history_to_llm_messages;
use super::refs::{RefEntry, RefHit, RefKind};
use super::stream::LlmMessage;

/// 消息 `refs` 字段的落盘形状（面板只认文件路径，画布卡保留节点身份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefsShape {
    File,
    Card,
}

/// 消息正文 = 引用注入块（路径块 + 卡类型块）+ 用户原始输入。
pub fn message_content(path_block: &str, card_block: &str, display: &str) -> String {
    let mut content = String::with_capacity(path_block.len() + card_block.len() + display.len());
    content.push_str(path_block);
    content.push_str(card_block);
    content.push_str(display);
    content
}

/// 命中引用 → 消息 `refs` 字段：正文里标签被删掉的引用不落。
///
/// [`RefsShape::File`] 一律落 `path`；[`RefsShape::Card`] 补充 `nodeId`，
/// 且画布内文本卡（path 即节点 id）不落 `path`。
pub fn refs_json(hits: &[RefHit], refs: &[RefEntry], shape: RefsShape) -> Vec<Value> {
    refs.iter()
        .filter(|entry| matched(hits, entry, shape))
        .map(|entry| {
            let mut value = json!({
                "kind": "file",
                "label": entry.label,
                "refKind": entry.kind.key(),
            });
            let node_is_path = entry.kind == RefKind::Card
                && entry.node_id.as_deref() == Some(entry.path.as_str());
            if shape == RefsShape::File || !node_is_path {
                value["path"] = json!(entry.path);
            }
            if shape == RefsShape::Card {
                if let Some(id) = &entry.node_id {
                    value["nodeId"] = json!(id);
                }
            }
            value
        })
        .collect()
}

/// 引用条目是否命中正文：卡形状按节点 id 对齐，其余按路径。
fn matched(hits: &[RefHit], entry: &RefEntry, shape: RefsShape) -> bool {
    hits.iter().any(|hit| match (shape, &hit.node_id, &entry.node_id) {
        (RefsShape::Card, Some(hit_id), Some(entry_id)) => hit_id == entry_id,
        _ => hit.path == entry.path,
    })
}

/// 一条 user 消息（发送路径的落盘形状）。
pub fn user_message(
    id: String,
    content: String,
    display: String,
    refs: Vec<Value>,
    attachments: Vec<Value>,
    created_at: i64,
) -> Value {
    json!({
        "id": id,
        "role": "user",
        "content": content,
        "displayContent": display,
        "refs": refs,
        "attachments": attachments,
        "createdAt": created_at,
    })
}

/// 最后一条 user 消息的下标（重生成的截断点 = 下标 + 1）。
pub fn last_user_index(messages: &[Value]) -> Option<usize> {
    messages.iter().rposition(|m| m["role"] == json!("user"))
}

/// 按消息 id 定位回滚截断点：返回保留下来的条数。
///
/// 目标已是末条（或不存在）时返回 `None`，调用方按「无可回滚」早退。
pub fn rollback_keep_count(messages: &[Value], message_id: &str) -> Option<usize> {
    let idx = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(message_id))?;
    (idx + 1 < messages.len()).then_some(idx + 1)
}

/// 压缩锚点：末条消息的 id 与条数（缺 id 的末条无从锚定 = `None`）。
pub fn compaction_anchor(messages: &[Value]) -> Option<(String, usize)> {
    let id = messages.last()?.get("id")?.as_str()?.to_string();
    Some((id, messages.len()))
}

/// 压缩检查点注解（摘要 + 锚点 + 覆盖条数 + 写入时刻）。
pub fn compaction_record(
    summary: String,
    up_to_message_id: &str,
    message_count: usize,
    created_at: i64,
) -> Value {
    json!({
        "summary": summary,
        "upToMessageId": up_to_message_id,
        "messageCount": message_count,
        "createdAt": created_at,
    })
}

/// 压缩请求的转写正文：按既有检查点折叠后逐条格式化为可读文本。
pub fn compact_transcript(messages: &[Value], compaction: Option<&Value>) -> String {
    let mut transcript = String::new();
    for message in history_to_llm_messages(messages, compaction) {
        match message {
            LlmMessage::User { parts, .. } => {
                transcript.push_str(&format!("用户：{}\n\n", parts.join("\n")));
            }
            LlmMessage::Assistant { content, tool_calls } => {
                let body = match content {
                    Some(c) if !c.is_empty() => c,
                    _ if !tool_calls.is_empty() => format!(
                        "（调用工具 {}）",
                        tool_calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("、")
                    ),
                    _ => String::new(),
                };
                transcript.push_str(&format!("助手：{body}\n\n"));
            }
            LlmMessage::Tool { content, .. } => {
                transcript.push_str(&format!("工具结果：{content}\n\n"));
            }
            LlmMessage::System { .. } => {}
        }
    }
    transcript
}

/// 引用标签文本（`#前缀：标签` + 尾随空格）。
pub fn mention_tag(kind: RefKind, label: &str) -> String {
    format!("#{}：{} ", kind.prefix(), label)
}

/// 把触发段（`#` 起至光标）替换为标签文本，返回新正文与落点光标。
pub fn insert_mention(text: &str, caret: usize, at: usize, tag: &str) -> (String, usize) {
    let caret = caret.min(text.len());
    let to = caret.max(at + 1).min(text.len());
    let mut next = String::with_capacity(text.len() - (to - at) + tag.len());
    next.push_str(&text[..at]);
    next.push_str(tag);
    next.push_str(&text[to..]);
    (next, at + tag.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: RefKind, label: &str, node_id: Option<&str>) -> RefEntry {
        RefEntry {
            path: path.into(),
            label: label.into(),
            kind,
            node_id: node_id.map(str::to_string),
        }
    }

    fn hit(path: &str, kind: RefKind, node_id: Option<&str>) -> RefHit {
        RefHit {
            path: path.into(),
            kind,
            node_id: node_id.map(str::to_string),
            start: 0,
            len: 1,
        }
    }

    fn user(id: &str, content: &str) -> Value {
        json!({ "id": id, "role": "user", "content": content })
    }

    fn assistant(id: &str, content: &str) -> Value {
        json!({ "id": id, "role": "assistant", "content": content })
    }

    /// 注入块按「路径块 → 卡块 → 原文」顺序拼接；两边都没有块时正文即原文。
    #[test]
    fn message_content_keeps_block_order() {
        assert_eq!(message_content("", "", "看这个"), "看这个");
        assert_eq!(
            message_content("[引用文件：\n- a.md\n]\n\n", "", "看这个"),
            "[引用文件：\n- a.md\n]\n\n看这个"
        );
        assert_eq!(message_content("[路径]", "[卡片正文]", "原文"), "[路径][卡片正文]原文");
        // 空上下文：无块无原文
        assert_eq!(message_content("", "", ""), "");
    }

    /// 面板形状只落文件路径；正文里标签被删掉的引用不落字段。
    #[test]
    fn file_refs_json_filters_deleted_tags() {
        let refs = vec![
            entry("a.md", RefKind::Note, "a", None),
            entry("b.md", RefKind::Note, "b", None),
        ];
        let hits = vec![hit("a.md", RefKind::Note, None)];
        let value = refs_json(&hits, &refs, RefsShape::File);
        assert_eq!(value.len(), 1);
        assert_eq!(value[0]["path"], json!("a.md"));
        assert_eq!(value[0]["refKind"], json!("note"));
        assert!(value[0].get("nodeId").is_none());
        // 空引用集 / 无命中
        assert!(refs_json(&hits, &[], RefsShape::File).is_empty());
        assert!(refs_json(&[], &refs, RefsShape::File).is_empty());
    }

    /// 卡形状：按节点 id 对齐并补 nodeId；文本卡（path 即节点 id）不落 path，笔记卡保留 path。
    #[test]
    fn card_refs_json_carries_node_identity() {
        let refs = vec![
            entry("note.md", RefKind::Note, "笔记", Some("n1")),
            entry("n2", RefKind::Card, "文本卡", Some("n2")),
            entry("图.png", RefKind::Card, "媒体卡", Some("n3")),
        ];
        let hits = vec![
            hit("note.md", RefKind::Note, Some("n1")),
            hit("n2", RefKind::Card, Some("n2")),
            hit("图.png", RefKind::Card, Some("n3")),
        ];
        let value = refs_json(&hits, &refs, RefsShape::Card);
        assert_eq!(value.len(), 3);
        assert_eq!(value[0]["path"], json!("note.md"));
        assert_eq!(value[0]["nodeId"], json!("n1"));
        assert!(value[1].get("path").is_none(), "文本卡以节点 id 作身份，不落 path");
        assert_eq!(value[1]["nodeId"], json!("n2"));
        assert_eq!(value[2]["path"], json!("图.png"));
    }

    /// 卡形状按节点 id 判命中：同路径不同节点的引用互不吞并。
    #[test]
    fn card_refs_json_matches_by_node_not_path() {
        let refs = vec![entry("same", RefKind::Card, "旧", Some("n2"))];
        let hits = vec![hit("same", RefKind::Card, Some("n1"))];
        assert!(refs_json(&hits, &refs, RefsShape::Card).is_empty());
        // 缺节点身份时退回路径对齐
        let refs = vec![entry("a.md", RefKind::Note, "a", None)];
        let hits = vec![hit("a.md", RefKind::Note, Some("n1"))];
        assert_eq!(refs_json(&hits, &refs, RefsShape::Card).len(), 1);
    }

    /// user 消息字段齐全（空附件 / 空引用同样落空数组）。
    #[test]
    fn user_message_keeps_wire_fields() {
        let value = user_message("m1".into(), "[块]原文".into(), "原文".into(), vec![], vec![], 7);
        assert_eq!(value["id"], json!("m1"));
        assert_eq!(value["role"], json!("user"));
        assert_eq!(value["content"], json!("[块]原文"));
        assert_eq!(value["displayContent"], json!("原文"));
        assert_eq!(value["refs"], json!([]));
        assert_eq!(value["attachments"], json!([]));
        assert_eq!(value["createdAt"], json!(7));
    }

    /// 重生成截断点取最后一条 user（多轮里非首条）。
    #[test]
    fn last_user_index_picks_latest_user() {
        let messages = vec![user("u1", "甲"), assistant("a1", "乙"), user("u2", "丙"), assistant("a2", "丁")];
        assert_eq!(last_user_index(&messages), Some(2));
        // 空上下文 / 无 user
        assert_eq!(last_user_index(&[]), None);
        assert_eq!(last_user_index(&[assistant("a1", "乙")]), None);
    }

    /// 回滚截断点：目标须后面还有消息，末条不可回滚。
    #[test]
    fn rollback_keep_count_rejects_last_and_missing() {
        let messages = vec![user("u1", "甲"), assistant("a1", "乙"), user("u2", "丙")];
        assert_eq!(rollback_keep_count(&messages, "u1"), Some(1));
        assert_eq!(rollback_keep_count(&messages, "a1"), Some(2));
        assert_eq!(rollback_keep_count(&messages, "u2"), None);
        assert_eq!(rollback_keep_count(&messages, "missing"), None);
        // 空上下文
        assert_eq!(rollback_keep_count(&[], "u1"), None);
    }

    /// 压缩锚点取末条 id 与条数；缺 id 的末条不锚定。
    #[test]
    fn compaction_anchor_uses_last_message_id() {
        let messages = vec![json!({ "id": "m1" }), json!({ "id": "m2" })];
        assert_eq!(compaction_anchor(&messages), Some(("m2".to_string(), 2)));
        assert_eq!(compaction_anchor(&[]), None);
        assert_eq!(compaction_anchor(&[json!({ "role": "user" })]), None);
    }

    /// 压缩注解字段与落盘键一致（锚点 + 覆盖条数 + 时刻）。
    #[test]
    fn compaction_record_matches_disk_keys() {
        let value = compaction_record("摘要".into(), "m2", 2, 9);
        assert_eq!(value["summary"], json!("摘要"));
        assert_eq!(value["upToMessageId"], json!("m2"));
        assert_eq!(value["messageCount"], json!(2));
        assert_eq!(value["createdAt"], json!(9));
    }

    /// 空上下文：没有消息就没有转写（压缩请求不该带空正文）。
    #[test]
    fn compact_transcript_is_empty_without_messages() {
        assert_eq!(compact_transcript(&[], None), "");
        // 只有 system / 未知角色也不产生转写
        assert_eq!(compact_transcript(&[json!({ "role": "system", "content": "规则" })], None), "");
    }

    /// 工具步与纯叙述步都进转写；检查点锚点及其之前折叠为摘要。
    #[test]
    fn compact_transcript_formats_steps_and_folds_checkpoint() {
        let messages = vec![
            user("u1", "早前的问题"),
            assistant("a1", "早前的回答"),
            json!({
                "id": "a2",
                "role": "assistant",
                "content": "",
                "steps": [{ "kind": "tool", "run": { "id": "r1", "name": "read_file", "args": {}, "result": "ok" } }],
            }),
            user("u2", "新的问题"),
        ];
        let transcript = compact_transcript(&messages, None);
        assert!(transcript.contains("助手：早前的回答"), "早前内容进转写");
        assert!(transcript.contains("助手：（调用工具 read_file）"), "只调工具的轮次按工具名占位");
        assert!(transcript.contains("用户：新的问题"));

        let compaction = json!({ "summary": "早前摘要", "upToMessageId": "a2" });
        let folded = compact_transcript(&messages, Some(&compaction));
        assert!(!folded.contains("早前的回答"), "锚点及其之前被摘要块替换");
        assert!(folded.contains("早前摘要"), "摘要块进转写");
        assert!(folded.contains("新的问题"), "锚点之后保留");
    }

    /// 超长上下文：全部消息按序进转写，末条不截断。
    #[test]
    fn compact_transcript_covers_long_history() {
        let mut messages = Vec::new();
        for i in 0..200 {
            if i % 2 == 0 {
                messages.push(user(&format!("u{i}"), &format!("问题{i}")));
            } else {
                messages.push(assistant(&format!("a{i}"), &format!("回答{i}")));
            }
        }
        let transcript = compact_transcript(&messages, None);
        assert_eq!(transcript.matches("用户：").count(), 100);
        assert_eq!(transcript.matches("助手：").count(), 100);
        assert!(transcript.ends_with("助手：回答199\n\n"));
    }

    /// 标签文本与插入点：触发段（# 至光标）被整段替换，光标落在标签之后。
    #[test]
    fn mention_tag_replaces_trigger_segment() {
        let tag = mention_tag(RefKind::Note, "a.md");
        assert_eq!(tag, "#笔记：a.md ");
        // 触发段 = 字节 4..8（「#笔」）；标签整段替换它
        let (text, caret) = insert_mention("看 #笔", 8, 4, &tag);
        assert_eq!(text, format!("看 {tag}"));
        assert_eq!(caret, 4 + tag.len());
        // 光标在触发段之前（异常输入）= 至少吃掉 # 本身，不越界
        let (text, caret) = insert_mention("#", 0, 0, &tag);
        assert_eq!(text, tag);
        assert_eq!(caret, tag.len());
    }
}
