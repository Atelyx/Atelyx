//! 画布弹层：卡片 / 分组 / 多选 / 空白右键与分组改名。

use iced::widget::{container, row, space, text_input};
use iced::{Element, Length, Padding};
use serde_json::Value;

use crate::layout_view::PopupState;
use crate::ui::controls;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message};

use super::common::popup_hint;

pub(super) fn entries<'a>(app: &'a App, state: &PopupState) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::CanvasEdgeMenu { edge_id } => {
            // 连线右键：语义切换 + 反向 + 删除（选中态同步由打开菜单的按压完成）
            let association = app
                .canvas
                .as_ref()
                .and_then(|s| s.data.edges.iter().find(|e| &e.id == edge_id))
                .is_some_and(|e| e.directed == Some(false));
            vec![
                (
                    0,
                    MenuEntry::action_icon(
                        if association { "改为数据流" } else { "改为关联" },
                        if association { "link" } else { "unlink" },
                        canvas_menu(crate::canvas_view::CanvasMenuAction::ToggleEdgeKind {
                            edge: edge_id.clone(),
                        }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "反转方向",
                        "refresh",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::ReverseEdge {
                            edge: edge_id.clone(),
                        }),
                    ),
                ),
                (
                    2,
                    MenuEntry::danger(
                        "删除连线",
                        "bin",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::DeleteEdge {
                            edge: edge_id.clone(),
                        }),
                    ),
                ),
            ]
        }
        PopupState::CanvasCardMenu { node_id } => {
            let node = app
                .canvas
                .as_ref()
                .and_then(|s| s.data.nodes.iter().find(|n| &n.id == node_id));
            let mut entries = vec![(
                0,
                MenuEntry::action_icon("复制", "copy", canvas_menu(crate::canvas_view::CanvasMenuAction::CopySelection)),
            )];
            match node.map(|n| n.node_type.as_str()) {
                Some("conversation") => {
                    entries.push((
                        0,
                        MenuEntry::action_icon(
                            "在 AI 面板打开",
                            "message-square",
                            canvas_menu(crate::canvas_view::CanvasMenuAction::OpenInPanel(node_id.clone())),
                        ),
                    ));
                }
                Some("text") => {
                    if let Some(rel) = node.and_then(|n| n.data.get("file")).and_then(Value::as_str) {
                        entries.push((0, MenuEntry::action_icon("打开笔记", "external-link", Message::OpenFile(rel.to_string()))));
                    } else {
                        entries.push((
                            0,
                            MenuEntry::action_icon(
                                "编辑文本卡",
                                "pencil",
                                canvas_menu(crate::canvas_view::CanvasMenuAction::EditTextCard { node: node_id.clone() }),
                            ),
                        ));
                        entries.push((
                            0,
                            MenuEntry::action_icon(
                                "保存为笔记…",
                                "file-plus",
                                canvas_menu(crate::canvas_view::CanvasMenuAction::SaveAsNote { node: node_id.clone() }),
                            ),
                        ));
                    }
                }
                Some("table") => {
                    if let Some(rel) = node.and_then(|n| n.data.get("file")).and_then(Value::as_str) {
                        entries.push((0, MenuEntry::action_icon("打开表格", "external-link", Message::OpenFile(rel.to_string()))));
                    }
                }
                _ => {}
            }
            // 文本卡色板（按主题分色，便于头脑风暴分组阅读）
            if node.is_some_and(|n| n.node_type == "text") {
                let current = node
                    .and_then(|n| n.data.get("color"))
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                entries.push((0, MenuEntry::Submenu { label: "颜色".into(), icon: Some("palette".into()), disabled: false, pane: 1 }));
                for (i, color) in pal::CANVAS_GROUP_COLORS.iter().enumerate() {
                    let idx = i + 1;
                    entries.push(color_row(
                        idx,
                        current,
                        *color,
                        crate::canvas_view::CanvasMenuAction::TextColor { node: node_id.clone(), index: idx },
                    ));
                }
            }
            entries.push((
                2,
                MenuEntry::danger("删除", "bin", canvas_menu(crate::canvas_view::CanvasMenuAction::DeleteSelection)),
            ));
            entries
        }
        PopupState::CanvasGroupMenu { node_id } => {
            let group = app
                .canvas
                .as_ref()
                .and_then(|s| s.data.nodes.iter().find(|n| &n.id == node_id));
            let collapsed = group
                .and_then(|n| n.data.get("collapsed"))
                .and_then(Value::as_bool)
                == Some(true);
            let current = group
                .and_then(|n| n.data.get("color"))
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let mut entries = vec![
                (
                    0,
                    MenuEntry::Submenu {
                        label: "颜色".into(),
                        icon: Some("palette".into()),
                        disabled: false,
                        pane: 1,
                    },
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "重命名",
                        "pencil",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::GroupRenameStart { node: node_id.clone() }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        if collapsed { "展开" } else { "收起" },
                        if collapsed { "chevrons-up-down" } else { "chevrons-down-up" },
                        canvas_menu(crate::canvas_view::CanvasMenuAction::ToggleCollapse { node: node_id.clone() }),
                    ),
                ),
                (
                    2,
                    // 删组只删容器：成员卡保留（成员判定是空间关系）
                    MenuEntry::danger("删除分组", "bin", canvas_menu(crate::canvas_view::CanvasMenuAction::DeleteSelection)),
                ),
            ];
            // 颜色子面板（pane 1）：色板行 + 当前色角标
            for (i, color) in crate::ui::palette::CANVAS_GROUP_COLORS.iter().enumerate() {
                let idx = i + 1;
                entries.push(color_row(
                    idx,
                    current,
                    *color,
                    crate::canvas_view::CanvasMenuAction::GroupColor { node: node_id.clone(), index: idx },
                ));
            }
            entries
        }
        PopupState::CanvasMultiMenu => vec![
            (
                0,
                MenuEntry::action_icon("复制", "copy", canvas_menu(crate::canvas_view::CanvasMenuAction::CopySelection)),
            ),
            (
                0,
                MenuEntry::action_icon(
                    "创建分组",
                    "layers",
                    canvas_menu(crate::canvas_view::CanvasMenuAction::CreateGroupFromSelection),
                ),
            ),
            (
                2,
                MenuEntry::danger("删除", "bin", canvas_menu(crate::canvas_view::CanvasMenuAction::DeleteSelection)),
            ),
        ],
        PopupState::CanvasBlankMenu { x, y } => {
            let mut entries = vec![
                (
                    0,
                    MenuEntry::action_icon(
                        "新建对话卡",
                        "message-square",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::CreateConversationAt {
                            x: *x,
                            y: *y,
                        }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "新建文本卡",
                        "align-left",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::CreateTextAt {
                            x: *x,
                            y: *y,
                        }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "新建分组",
                        "layers",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::CreateGroupAt { x: *x, y: *y }),
                    ),
                ),
            ];
            // 右键空白不清选中：有选中时提供复制（与卡菜单 / Ctrl+C 同剪贴板口径，可跨画布粘贴）
            if app.canvas.as_ref().is_some_and(|s| !s.view.selection.is_empty()) {
                entries.push((
                    0,
                    MenuEntry::action_icon(
                        "复制",
                        "copy",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::CopySelection),
                    ),
                ));
            }
            entries.push((
                0,
                MenuEntry::action_icon(
                    "粘贴",
                    "clipboard-paste",
                    canvas_menu(crate::canvas_view::CanvasMenuAction::Paste),
                ),
            ));
            entries
        }
        PopupState::CanvasGroupRename { node_id } => {
            let current = app
                .canvas
                .as_ref()
                .and_then(|s| s.data.nodes.iter().find(|n| &n.id == node_id))
                .and_then(|n| n.data.get("label"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let draft = app
                .canvas
                .as_ref()
                .and_then(|s| s.view.group_rename.as_ref())
                .filter(|(id, _)| id == node_id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| current.clone());
            vec![
                (0, MenuEntry::Custom(canvas_rename_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action(
                        "重命名",
                        canvas_menu(crate::canvas_view::CanvasMenuAction::GroupRenameCommit {
                            node: node_id.clone(),
                            name: draft,
                        }),
                    ),
                ),
                (
                    0,
                    MenuEntry::Custom(
                        popup_hint(&format!(
                            "当前：{}",
                            if current.is_empty() { "（未命名）" } else { current.as_str() }
                        )),
                        18.0,
                    ),
                ),
            ]
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::CanvasCardMenu { .. } => 144.0,
        PopupState::CanvasGroupMenu { .. } => 160.0,
        PopupState::CanvasMultiMenu => 144.0,
        PopupState::CanvasEdgeMenu { .. } => 144.0,
        PopupState::CanvasBlankMenu { .. } => 144.0,
        PopupState::CanvasGroupRename { .. } => 200.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 画布右键菜单动作消息。
fn canvas_menu(action: crate::canvas_view::CanvasMenuAction) -> Message {
    Message::Canvas(crate::canvas_view::CanvasMsg::Menu(action))
}

/// 色板行（分组 / 文本卡共用）：色块 + 「色 N」/「色 N（当前）」。
fn color_row(
    index: usize,
    current: usize,
    color: iced::Color,
    action: crate::canvas_view::CanvasMenuAction,
) -> (usize, MenuEntry<'static, Message>) {
    let swatch = container(space().width(12.0).height(12.0)).style(move |_| iced::widget::container::Style {
        background: Some(color.into()),
        border: iced::border::rounded(3.0).color(pal::BORDER),
        ..Default::default()
    });
    let label = if index == current {
        format!("色 {index}（当前）")
    } else {
        format!("色 {index}")
    };
    (
        1,
        MenuEntry::Custom(
            iced::widget::button(
                row![swatch, text(label).size(pal::fs_caption())]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center),
            )
            .padding(Padding::ZERO.horizontal(12.0).vertical(5.0))
            .width(Length::Fill)
            .style(|t, s| controls::row_style(false, pal::RADIUS_SM)(t, s))
            .on_press(canvas_menu(action))
            .into(),
            28.0,
        ),
    )
}

/// 画布分组改名输入（提交由菜单项动作携带草稿；每次渲染重建携带最新草稿）。
fn canvas_rename_input(draft: &str) -> Element<'static, Message> {
    text_input("分组名称", draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(|v| Message::Canvas(crate::canvas_view::CanvasMsg::GroupRenameDraft(v)))
        .style(|_theme, status| controls::form_input_style(status))
        .into()
}
