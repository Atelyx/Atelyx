//! 常用形态的预制件：只用协议基元拼树，不进协议。
//!
//! 新界面诉求先在这里找组合——列表项、徽标、提示块、tab 条、状态点、表单行、空态都由既有控件
//! 与节点级表述能力（交互外观、尺寸、排版）拼成；拼不出来的才考虑宿主侧新增能力。

use atelyx_plugin_protocol::view::{
    Align, AxesPadding, BoxSize, ColorSpec, ColorToken, EventKind, Interaction, InteractionState,
    NodeBody, PaddingSpec, SizeSpec, Surface, TextWeight, ViewNode,
};
use crate::view_builders::{icon, NodeId};

/// 语义色档：软底 + 实色成对取用（提示块底 / 徽标底 / 状态点实色）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

impl Tone {
    /// 低饱和底（提示块、徽标底）。
    pub fn soft(self) -> ColorSpec {
        ColorSpec::Token(match self {
            Self::Neutral => ColorToken::Surface,
            Self::Accent => ColorToken::AccentSoft,
            Self::Success => ColorToken::SuccessSoft,
            Self::Warning => ColorToken::WarningSoft,
            Self::Danger => ColorToken::DangerSoft,
        })
    }

    /// 实色（文字、描边、状态点）。
    pub fn solid(self) -> ColorSpec {
        ColorSpec::Token(match self {
            Self::Neutral => ColorToken::TextSecondary,
            Self::Accent => ColorToken::Accent,
            Self::Success => ColorToken::Success,
            Self::Warning => ColorToken::Warning,
            Self::Danger => ColorToken::Danger,
        })
    }
}

fn node(id: &str, body: NodeBody, children: Vec<ViewNode>) -> ViewNode {
    ViewNode {
        id: id.to_string(),
        body,
        interaction: None,
        grow: None,
        children,
        events: Vec::new(),
    }
}

fn text_node(
    id: &str,
    content: impl Into<String>,
    size: f32,
    weight: Option<TextWeight>,
    color: ColorSpec,
    align: Option<Align>,
    wrap: Option<bool>,
) -> ViewNode {
    node(
        id,
        NodeBody::Text {
            content: content.into(),
            size: Some(size),
            weight,
            color: Some(color),
            align,
            family: None,
            wrap,
            max_lines: None,
        },
        Vec::new(),
    )
}

/// 一层外观便捷构造。
fn surface(background: Option<ColorSpec>, border: Option<ColorSpec>, radius: f32) -> Surface {
    Surface { background, border, radius: Some(radius) }
}

/// 分轴内边距便捷构造。
fn axes(horizontal: f32, vertical: f32) -> PaddingSpec {
    PaddingSpec::Axes(AxesPadding { horizontal, vertical })
}

fn fill_width() -> Option<BoxSize> {
    Some(BoxSize { width: Some(SizeSpec::Fill), ..BoxSize::default() })
}

fn shrink_width() -> Option<BoxSize> {
    Some(BoxSize { width: Some(SizeSpec::Shrink), ..BoxSize::default() })
}

/// 列表条目：悬停高亮、选中态描边（整行可点还需调用方声明 `with_events(vec![EventKind::Click])`）。
///
/// 内容按需给子节点。
pub fn list_item<'a>(id: impl Into<NodeId<'a>>, children: Vec<ViewNode>, selected: bool) -> ViewNode {
    let id = id.into();
    let mut item = node(
        id.as_str(),
        NodeBody::Row {
            gap: Some(8.0),
            padding: Some(axes(8.0, 6.0)),
            justify: None,
            cross_align: Some(Align::Center),
            size: fill_width(),
        },
        children,
    );
    item.interaction = Some(Interaction {
        state: Some(if selected { InteractionState::Active } else { InteractionState::Normal }),
        normal: Some(surface(None, None, 6.0)),
        hover: Some(surface(Some(ColorToken::Hover.into()), None, 6.0)),
        active: Some(surface(
            Some(ColorToken::AccentSoft.into()),
            Some(ColorToken::Accent.into()),
            6.0,
        )),
        disabled: None,
    });
    item
}

/// 徽标 / chip：小尺寸圆角底 + 短文本，底色与文字色按语义档配对。
pub fn chip<'a>(id: impl Into<NodeId<'a>>, label: impl Into<String>, tone: Tone) -> ViewNode {
    let id = id.into();
    let label = label.into();
    let mut item = node(
        id.as_str(),
        NodeBody::Row {
            gap: Some(4.0),
            padding: Some(axes(6.0, 2.0)),
            justify: None,
            cross_align: Some(Align::Center),
            size: shrink_width(),
        },
        vec![text_node(
            &format!("{id}-label"),
            label,
            0.8,
            Some(TextWeight::Medium),
            tone.solid(),
            None,
            Some(false),
        )],
    );
    item.interaction = Some(Interaction {
        state: None,
        normal: Some(surface(Some(tone.soft()), None, 10.0)),
        hover: None,
        active: None,
        disabled: None,
    });
    item
}

/// 提示块：语义软底 + 同色描边的分组盒（信息 / 警告 / 错误）。
pub fn alert<'a>(id: impl Into<NodeId<'a>>, tone: Tone, children: Vec<ViewNode>) -> ViewNode {
    let id = id.into();
    let mut block = node(
        id.as_str(),
        NodeBody::Column {
            gap: Some(4.0),
            padding: Some(PaddingSpec::All(10.0)),
            cross_align: Some(Align::Start),
            align: None,
            size: fill_width(),
        },
        children,
    );
    block.interaction = Some(Interaction {
        state: None,
        normal: Some(surface(Some(tone.soft()), Some(tone.solid()), 6.0)),
        hover: None,
        active: None,
        disabled: None,
    });
    block
}

/// tab 条：一行条目，选中项取激活外观；每个条目回发 `click`（条目 id 为 `<id>-<键>`）。
pub fn tabs<'a>(id: impl Into<NodeId<'a>>, entries: &[(&str, &str)], active: &str) -> ViewNode {
    let id = id.into();
    let items = entries
        .iter()
        .map(|(key, label)| {
            let current = *key == active;
            let text_color = if current { ColorToken::Accent } else { ColorToken::TextSecondary };
            let mut item = node(
                &format!("{id}-{key}"),
                NodeBody::Row {
                    gap: None,
                    padding: Some(axes(10.0, 4.0)),
                    justify: None,
                    cross_align: Some(Align::Center),
                    size: shrink_width(),
                },
                vec![text_node(
                    &format!("{id}-{key}-label"),
                    *label,
                    0.85,
                    Some(TextWeight::Medium),
                    text_color.into(),
                    None,
                    Some(false),
                )],
            );
            item.interaction = Some(Interaction {
                state: Some(if current { InteractionState::Active } else { InteractionState::Normal }),
                normal: Some(surface(None, None, 6.0)),
                hover: Some(surface(Some(ColorToken::Hover.into()), None, 6.0)),
                active: Some(surface(Some(ColorToken::AccentSoft.into()), None, 6.0)),
                disabled: None,
            });
            item.events = vec![EventKind::Click];
            item
        })
        .collect();
    node(
        id.as_str(),
        NodeBody::Row {
            gap: Some(4.0),
            padding: None,
            justify: None,
            cross_align: Some(Align::Center),
            size: fill_width(),
        },
        items,
    )
}

/// 状态点：8×8 圆点，颜色按语义档取实色（配一行文字表达状态）。
pub fn status_dot<'a>(id: impl Into<NodeId<'a>>, tone: Tone) -> ViewNode {
    let id = id.into();
    let mut dot = node(
        id.as_str(),
        NodeBody::Row {
            gap: None,
            padding: None,
            justify: None,
            cross_align: None,
            size: Some(BoxSize {
                width: Some(SizeSpec::Fixed(8.0)),
                height: Some(SizeSpec::Fixed(8.0)),
                ..BoxSize::default()
            }),
        },
        Vec::new(),
    );
    dot.interaction = Some(Interaction {
        state: None,
        normal: Some(surface(Some(tone.solid()), None, 4.0)),
        hover: None,
        active: None,
        disabled: None,
    });
    dot
}

/// 表单行：定宽标签列 + 控件（控件按 `grow` 占满剩余宽度）。
pub fn field_row<'a>(id: impl Into<NodeId<'a>>, label: &str, control: ViewNode) -> ViewNode {
    let id = id.into();
    let mut control = control;
    control.grow = Some(1);
    node(
        id.as_str(),
        NodeBody::Row {
            gap: Some(8.0),
            padding: None,
            justify: None,
            cross_align: Some(Align::Center),
            size: fill_width(),
        },
        vec![
            node(
                &format!("{id}-label"),
                NodeBody::Column {
                    gap: None,
                    padding: None,
                    cross_align: Some(Align::Start),
                    align: None,
                    size: Some(BoxSize {
                        width: Some(SizeSpec::Fixed(96.0)),
                        ..BoxSize::default()
                    }),
                },
                vec![text_node(
                    &format!("{id}-label-text"),
                    label,
                    1.0,
                    None,
                    ColorToken::TextSecondary.into(),
                    None,
                    None,
                )],
            ),
            control,
        ],
    )
}

/// 空态：水平居中的图标 + 标题 + 说明（高度按内容；需要撑满可视区时给外层容器 `size.height`）。
pub fn empty_state<'a>(id: impl Into<NodeId<'a>>, icon_name: &str, title: &str, hint: &str) -> ViewNode {
    let id = id.into();
    let mut glyph = icon(&format!("{id}-icon"), icon_name);
    glyph.body = NodeBody::Icon {
        name: icon_name.to_string(),
        size: Some(28.0),
        color: Some(ColorToken::TextMuted.into()),
    };
    node(
        id.as_str(),
        NodeBody::Column {
            gap: Some(6.0),
            padding: Some(axes(16.0, 24.0)),
            cross_align: Some(Align::Center),
            align: None,
            size: None,
        },
        vec![
            glyph,
            text_node(
                &format!("{id}-title"),
                title,
                1.0,
                Some(TextWeight::SemiBold),
                ColorToken::TextSecondary.into(),
                Some(Align::Center),
                None,
            ),
            text_node(
                &format!("{id}-hint"),
                hint,
                0.85,
                None,
                ColorToken::TextMuted.into(),
                Some(Align::Center),
                None,
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::view::ButtonVariant;
    use crate::view_builders::button;

    fn sample() -> ViewNode {
        node(
            "root",
            NodeBody::Column {
                gap: Some(6.0),
                padding: None,
                cross_align: Some(Align::Start),
                align: None,
                size: None,
            },
            vec![
                list_item(
                    "item",
                    vec![
                        text_node("item-text", "条目一", 1.0, None, ColorToken::Text.into(), None, None),
                        chip("item-chip", "新", Tone::Accent),
                    ],
                    true,
                ),
                chip("chip", "已就绪", Tone::Success),
                alert("warn", Tone::Warning, vec![text_node(
                    "warn-text",
                    "参数缺失",
                    1.0,
                    None,
                    ColorToken::Text.into(),
                    None,
                    None,
                )]),
                tabs("tabs", &[("a", "常规"), ("b", "高级")], "a"),
                status_dot("dot", Tone::Danger),
                field_row("field", "模型路径", button("field-btn", "选择", ButtonVariant::Ghost)),
                empty_state("empty", "inbox", "暂无记录", "运行一次后这里会显示结果"),
            ],
        )
    }

    #[test]
    fn presets_produce_valid_trees_with_unique_ids() {
        sample().validate().expect("预制件拼出的树应通过校验");
    }

    #[test]
    fn list_item_keeps_selection_under_pointer() {
        let child = || vec![text_node("l", "x", 1.0, None, ColorToken::Text.into(), None, None)];
        let selected = list_item("row", child(), true);
        let spec = selected.interaction.expect("列表项必须带交互外观");
        assert_eq!(spec.state(), InteractionState::Active);
        assert_eq!(
            spec.appearance(false),
            spec.appearance(true),
            "选中项在指针下仍保留选中外观"
        );

        let plain = list_item("row2", child(), false);
        let spec = plain.interaction.expect("列表项必须带交互外观");
        assert_ne!(
            spec.appearance(false),
            spec.appearance(true),
            "未选中项须有悬停反馈"
        );
    }

    #[test]
    fn chip_pairs_soft_background_with_solid_label() {
        let chip_node = chip("c", "新", Tone::Warning);
        let spec = chip_node.interaction.expect("徽标需自带底色");
        assert_eq!(
            spec.appearance(false).and_then(|surface| surface.background),
            Some(Tone::Warning.soft())
        );
        let label = chip_node.children.first().expect("徽标应有标签文本");
        assert!(matches!(
            &label.body,
            NodeBody::Text { color: Some(color), .. } if *color == Tone::Warning.solid()
        ));
    }

    #[test]
    fn tabs_declare_click_and_mark_active_entry() {
        let bar = tabs("t", &[("a", "常规"), ("b", "高级")], "b");
        assert_eq!(bar.children.len(), 2);
        for item in &bar.children {
            assert_eq!(item.events, vec![EventKind::Click]);
        }
        assert_eq!(
            bar.children[1].interaction.as_ref().map(Interaction::state),
            Some(InteractionState::Active)
        );
        assert_eq!(
            bar.children[0].interaction.as_ref().map(Interaction::state),
            Some(InteractionState::Normal)
        );
    }

    #[test]
    fn field_row_gives_control_the_remaining_width() {
        let row = field_row("f", "标签", button("f-btn", "选", ButtonVariant::Ghost));
        assert_eq!(row.children.len(), 2);
        assert_eq!(row.children[1].grow, Some(1));
        assert!(matches!(
            row.children[0].body,
            NodeBody::Column { size: Some(BoxSize { width: Some(SizeSpec::Fixed(96.0)), .. }), .. }
        ));
    }
}
