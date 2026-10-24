//! 画布卡引用的注入映射：按卡类型把被引用内容变成模型可见的文本块与图片附件。
//! 笔记引用仍走「引用文件」路径块（由模型按需读取），本模块只处理已随引用带来的内容。

use super::refs::{RefHit, RefKind};

/// 一张被引用卡的可用内容形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardSource {
    /// 画布内文本（`bodyMd`）。
    InlineText { title: String, body: String },
    /// 表格全文（`.atb` 渲染出的文本）。
    TableText { title: String, body: String },
    /// 文本附件的正文。
    MediaText { title: String, body: String },
    /// 图片附件：按仓库相对路径进 vision 附件。
    Image { title: String, file: String },
    /// 二进制附件：不可读，只说明形态。
    Binary { title: String },
    /// 引用内容缺失（外部删除 / 读盘失败）。
    Missing { title: String },
}

/// 一张卡的引用身份与内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardContent {
    pub node_id: String,
    pub source: CardSource,
}

/// 命中扫描结果 → 卡片注入块与图片附件路径（按命中顺序；同卡只取一次）。
/// 只消费 `RefKind::Card` 命中；笔记卡引用由「引用文件」路径块承载。
pub fn card_injection(hits: &[RefHit], contents: &[CardContent]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut images: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for hit in hits {
        if hit.kind != RefKind::Card {
            continue;
        }
        let Some(node_id) = hit.node_id.as_deref() else { continue };
        if seen.contains(&node_id) {
            continue;
        }
        seen.push(node_id);
        let Some(content) = contents.iter().find(|c| c.node_id == node_id) else {
            text.push_str(&missing_block("画布卡"));
            continue;
        };
        match &content.source {
            CardSource::InlineText { title, body }
            | CardSource::TableText { title, body }
            | CardSource::MediaText { title, body } => {
                text.push_str(&format!("## 画布卡「{title}」\n{body}\n\n"));
            }
            CardSource::Image { title, file } => {
                if !images.contains(file) {
                    images.push(file.clone());
                }
                text.push_str(&format!("## 画布卡「{title}」（图片，作为附件随消息发送）\n\n"));
            }
            CardSource::Binary { title } => {
                text.push_str(&format!("## 画布卡「{title}」（二进制附件，无法以文本读取）\n\n"));
            }
            CardSource::Missing { title } => {
                text.push_str(&format!("## 画布卡「{title}」（内容缺失）\n\n"));
            }
        }
    }
    (text, images)
}

fn missing_block(label: &str) -> String {
    format!("## {label}「已不在画布上」\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(node_id: &str, start: usize) -> RefHit {
        RefHit {
            path: node_id.into(),
            kind: RefKind::Card,
            node_id: Some(node_id.into()),
            start,
            len: 1,
        }
    }

    #[test]
    fn text_cards_inject_full_body_in_hit_order() {
        let hits = vec![hit("a", 0), hit("b", 3)];
        let contents = vec![
            CardContent {
                node_id: "b".into(),
                source: CardSource::TableText { title: "表".into(), body: "列1\t列2".into() },
            },
            CardContent {
                node_id: "a".into(),
                source: CardSource::InlineText { title: "文本".into(), body: "正文".into() },
            },
        ];
        let (text, images) = card_injection(&hits, &contents);
        assert!(text.find("文本").unwrap() < text.find("表").unwrap());
        assert!(text.contains("## 画布卡「文本」\n正文"));
        assert!(text.contains("## 画布卡「表」\n列1\t列2"));
        assert!(images.is_empty());
    }

    #[test]
    fn images_go_to_attachments_not_text_body() {
        let hits = vec![hit("img", 0)];
        let contents = vec![CardContent {
            node_id: "img".into(),
            source: CardSource::Image { title: "图".into(), file: "素材/a.png".into() },
        }];
        let (text, images) = card_injection(&hits, &contents);
        assert!(!text.contains("素材/a.png"));
        assert_eq!(images, vec!["素材/a.png".to_string()]);
    }

    #[test]
    fn note_path_refs_are_left_to_the_path_block() {
        let hits = vec![RefHit {
            path: "笔记/a.md".into(),
            kind: RefKind::Note,
            node_id: Some("card-1".into()),
            start: 0,
            len: 1,
        }];
        let (text, images) = card_injection(&hits, &[]);
        assert!(text.is_empty() && images.is_empty());
    }

    #[test]
    fn missing_card_and_missing_content_are_visible() {
        let (text, _) = card_injection(&[hit("gone", 0)], &[]);
        assert!(text.contains("已不在画布上"));
        let contents = vec![CardContent {
            node_id: "gone".into(),
            source: CardSource::Missing { title: "旧卡".into() },
        }];
        let (text, _) = card_injection(&[hit("gone", 0)], &contents);
        assert!(text.contains("内容缺失"));
    }
}
