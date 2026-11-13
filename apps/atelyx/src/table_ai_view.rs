//! 表格 AI 字段的配置弹层（居中模态）：系统提示词、指令模板、输出、模型与自动开关。

use iced::widget::text_editor::Action;
use iced::widget::{button, column, container, row, space, text, text_editor};
use iced::{alignment, Border, Element, Length, Padding, Size, Theme};

use atelyx_core::table_ai as ai;
use atelyx_core::vault::{self, TableField};

use crate::layout_view::PopupState;
use crate::table_ai::{AiDraft, AiSystemMode};
use crate::table_view as tv;
use crate::ui::{controls, icon::icon, palette as pal, scroll};
use crate::{App, Message, RepoKind};

/// 配置弹层宽度。
pub const MODAL_WIDTH: f32 = 640.0;
/// 模板编辑器高度。
const PROMPT_EDITOR_H: f32 = 150.0;
/// 系统提示词编辑器高度。
const SYSTEM_EDITOR_H: f32 = 88.0;
/// 字段 / 笔记 / 模型候选列表高度。
const PICKER_H: f32 = 112.0;
pub(crate) const PROMPT_EDITOR_ID: &str = "table-ai-prompt-editor";
const SYSTEM_EDITOR_ID: &str = "table-ai-system-editor";

/// 居中模态覆盖元素（遮罩 + 配置卡）；非本形态 / 非本窗口返回 None。
pub fn overlay<'a>(app: &'a App, window: iced::window::Id, viewport: Size) -> Option<Element<'a, Message>> {
    let (popup_window, open) = app.popup.as_ref()?;
    if *popup_window != window {
        return None;
    }
    let PopupState::TableAiField { field_id } = &open.state else {
        return None;
    };
    let session = app.table.as_ref()?;
    let field = session.data.fields.iter().find(|f| f.id == *field_id)?;
    let draft = session.view.ai.draft.as_ref()?;
    let card = config_card(app, session, field, draft, window, viewport);
    Some(crate::ui::popup::modal_overlay(window, card))
}

fn config_card<'a>(
    app: &'a App,
    session: &'a tv::TableSession,
    field: &'a TableField,
    draft: &'a AiDraft,
    window: iced::window::Id,
    viewport: Size,
) -> Element<'a, Message> {
    let close = Message::Ui(crate::ui::UiMsg::PopupClose { window });
    let header = container(
        row![
            icon("bot", 14.0, pal::TEXT_SECONDARY),
            text(format!("AI 字段 · {}", field.name))
                .size(pal::fs_body())
                .color(pal::TEXT_PRIMARY)
                .font(crate::ui::bold()),
            space().width(Length::Fill),
            controls::icon_button("x", 14.0, 26.0, controls::Variant::Ghost, Some(close.clone()), None),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(14.0).vertical(10.0));

    let system_block = column![
        section_label("系统提示词", ""),
        row![
            system_mode_button("无", AiSystemMode::None, draft.system_mode),
            system_mode_button("手写", AiSystemMode::Inline, draft.system_mode),
            system_mode_button("引用笔记", AiSystemMode::Note, draft.system_mode),
        ]
        .spacing(6),
    ]
    .spacing(6);

    let mut body = column![].spacing(14).push(system_block);
    match draft.system_mode {
        AiSystemMode::None => {}
        AiSystemMode::Inline => {
            body = body.push(multiline_editor(
                &draft.system_text,
                SYSTEM_EDITOR_ID,
                SYSTEM_EDITOR_H,
                |action| Message::Table(tv::TableMsg::AiDraftSystemEdited(action)),
            ));
        }
        AiSystemMode::Note => {
            body = body.push(note_picker(app, draft));
        }
    }

    let refs = ai::parse_refs(&draft.prompt.text(), &session.data.fields).unwrap_or_default();
    let ref_names: Vec<String> = refs
        .iter()
        .filter_map(|r| {
            session
                .data
                .fields
                .iter()
                .find(|f| f.id == r.field_id)
                .map(|f| f.name.clone())
        })
        .collect();
    let ref_hint = if ref_names.is_empty() {
        "至少引用一列（点「插入字段」）".to_string()
    } else {
        format!("已引用：{}", ref_names.join("、"))
    };
    let mut prompt_block = column![
        section_label("指令模板", "用 #字段名 引用同行列"),
        multiline_editor(
            &draft.prompt,
            PROMPT_EDITOR_ID,
            PROMPT_EDITOR_H,
            |action| Message::Table(tv::TableMsg::AiDraftPromptEdited(action)),
        ),
        row![
            controls::btn(
                if draft.show_fields { "收起字段" } else { "插入字段" },
                &controls::btn_sm(),
                controls::Variant::Outline,
                Some(Message::Table(tv::TableMsg::AiDraftToggleFields)),
            ),
            text(ref_hint)
                .size(pal::fs_micro())
                .color(if ref_names.is_empty() { pal::WARNING } else { pal::TEXT_MUTED }),
        ]
        .spacing(10)
        .align_y(alignment::Vertical::Center),
    ]
    .spacing(6);
    if draft.show_fields {
        prompt_block = prompt_block.push(field_picker(session, field));
    }
    body = body.push(prompt_block);

    body = body.push(
        column![
            section_label("输出", ""),
            row![
                output_button("短文本", vault::AI_OUTPUT_SHORT, &draft.output),
                output_button("长文本", vault::AI_OUTPUT_LONG, &draft.output),
            ]
            .spacing(6),
        ]
        .spacing(6),
    );

    body = body.push(
        column![
            section_label("模型", ""),
            row![
                text(model_label(app, draft))
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY)
                    .width(Length::Fill),
                controls::btn(
                    if draft.show_models { "收起" } else { "更换" },
                    &controls::btn_sm(),
                    controls::Variant::Outline,
                    Some(Message::Table(tv::TableMsg::AiDraftToggleModels)),
                ),
            ]
            .spacing(10)
            .align_y(alignment::Vertical::Center),
        ]
        .spacing(6),
    );
    if draft.show_models {
        body = body.push(model_picker(app, draft));
    }

    body = body.push(
        column![
            section_label("自动", ""),
            checkbox_row(
                "新增行时生成",
                draft.auto_new,
                Message::Table(tv::TableMsg::AiDraftAutoNew(!draft.auto_new)),
            ),
            checkbox_row(
                "引用列变化时更新",
                draft.auto_source,
                Message::Table(tv::TableMsg::AiDraftAutoSource(!draft.auto_source)),
            ),
        ]
        .spacing(6),
    );

    let read_only = matches!(&app.repo_kind, RepoKind::Space(space) if !space.role.can_write());
    let save = (!read_only).then_some(Message::Table(tv::TableMsg::AiSaveConfig));
    let ro_hint: Element<'static, Message> = text("只读身份：AI 字段配置不可保存")
        .size(pal::fs_micro())
        .color(pal::TEXT_MUTED)
        .into();
    let footer = container(
        row![
            if read_only {
                ro_hint
            } else {
                space().width(Length::Shrink).into()
            },
            space().width(Length::Fill),
            controls::btn("取消", &controls::btn_sm(), controls::Variant::Ghost, Some(close)),
            controls::btn("保存", &controls::btn_sm(), controls::Variant::Primary, save),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(14.0).vertical(10.0));

    let card_h = (viewport.height * 0.76).min(620.0);
    let card = column![
        header,
        controls::h_divider_subtle(),
        scroll::v(container(body).padding(Padding::ZERO.horizontal(14.0).vertical(12.0)))
            .height(Length::Fill),
        controls::h_divider_subtle(),
        footer,
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    container(card)
        .width(Length::Fixed(MODAL_WIDTH.min((viewport.width - 32.0).max(280.0))))
        .height(Length::Fixed(card_h))
        .style(|_t| container::Style {
            background: Some(pal::BG_OVERLAY.into()),
            border: Border { color: pal::BORDER, width: 1.0, radius: pal::RADIUS_MD.into() },
            ..container::Style::default()
        })
        .into()
}

fn section_label(title: &str, hint: &str) -> Element<'static, Message> {
    let mut block = column![text(title.to_string())
        .size(pal::fs_caption())
        .color(pal::TEXT_PRIMARY)
        .font(crate::ui::bold())]
    .spacing(2);
    if !hint.is_empty() {
        block = block.push(text(hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED));
    }
    block.into()
}

fn system_mode_button(label: &str, mode: AiSystemMode, current: AiSystemMode) -> Element<'static, Message> {
    controls::btn(
        label,
        &controls::btn_sm(),
        if mode == current { controls::Variant::Primary } else { controls::Variant::Outline },
        Some(Message::Table(tv::TableMsg::AiDraftSystemMode(mode))),
    )
    .into()
}

fn output_button(label: &str, output: &str, current: &str) -> Element<'static, Message> {
    controls::btn(
        label,
        &controls::btn_sm(),
        if output == current { controls::Variant::Primary } else { controls::Variant::Outline },
        Some(Message::Table(tv::TableMsg::AiDraftOutput(output.to_string()))),
    )
    .into()
}

fn checkbox_row(label: &str, checked: bool, on_press: Message) -> Element<'static, Message> {
    button(
        row![
            controls::checkbox(
                if checked { controls::CheckState::On } else { controls::CheckState::Off },
                None
            ),
            text(label.to_string()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .padding(Padding::ZERO)
    .style(|_t, status| iced::widget::button::Style {
        background: matches!(status, iced::widget::button::Status::Hovered).then_some(pal::HOVER.into()),
        ..Default::default()
    })
    .on_press(on_press)
    .into()
}

fn multiline_editor<'a>(
    content: &'a text_editor::Content,
    id: &'static str,
    height: f32,
    on_action: impl Fn(Action) -> Message + 'a,
) -> Element<'a, Message> {
    container(
        text_editor(content)
            .id(iced::widget::Id::new(id))
            .on_action(on_action)
            .size(pal::fs_ui())
            .height(Length::Fixed(height))
            .padding(8)
            .style(|_t: &Theme, _s| text_editor::Style {
                background: pal::BG_SUNKEN.into(),
                border: Border { color: pal::BORDER, width: 1.0, radius: pal::RADIUS_SM.into() },
                placeholder: pal::TEXT_MUTED,
                value: pal::TEXT_PRIMARY,
                selection: pal::SELECTION,
            }),
    )
    .width(Length::Fill)
    .into()
}

fn note_picker<'a>(app: &'a App, draft: &'a AiDraft) -> Element<'a, Message> {
    let mut list = column![].spacing(2);
    if app.prompt_notes.is_empty() {
        list = list.push(
            text("暂无提示词标记（文件右键笔记可注册）")
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
        );
    }
    for path in &app.prompt_notes {
        let selected = draft.note == *path;
        let label = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".md").to_string();
        list = list.push(
            button(
                row![
                    controls::checkbox(
                        if selected { controls::CheckState::On } else { controls::CheckState::Off },
                        None
                    ),
                    text(label).size(pal::fs_caption()).color(if selected {
                        pal::TEXT_PRIMARY
                    } else {
                        pal::TEXT_SECONDARY
                    }),
                    space().width(Length::Fill),
                    text(path.chars().take(40).collect::<String>())
                        .size(pal::fs_micro())
                        .color(pal::TEXT_MUTED),
                ]
                .spacing(8)
                .align_y(alignment::Vertical::Center),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0).vertical(5.0))
            .style(|_t, status| iced::widget::button::Style {
                background: matches!(status, iced::widget::button::Status::Hovered)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(Message::Table(tv::TableMsg::AiDraftNote(path.clone()))),
        );
    }
    scroll::v(container(list).padding(4.0))
        .height(Length::Fixed(PICKER_H.min(58.0 + app.prompt_notes.len() as f32 * 26.0)))
        .into()
}

/// 当前选择的模型显示名：字段指定优先，否则显示跟随的仓库默认模型。
fn model_label(app: &App, draft: &AiDraft) -> String {
    match (draft.provider_id.as_deref(), draft.model.as_deref()) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
            model_name(app, provider, model)
        }
        _ => match default_model_name(app) {
            Some(name) => format!("跟随默认（{name}）"),
            None => "跟随默认（未设置）".to_string(),
        },
    }
}

/// 模型显示名：昵称优先（空昵称回落 id）；供应商或模型已删仍显示模型 id。
fn model_name(app: &App, provider: &str, model: &str) -> String {
    app.repo_config
        .providers
        .iter()
        .flatten()
        .find(|p| p.id == provider)
        .and_then(|p| p.models.iter().find(|m| m.id == model))
        .map(|m| {
            m.nickname
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| m.id.clone())
        })
        .unwrap_or_else(|| model.to_string())
}

fn default_model_name(app: &App) -> Option<String> {
    match (app.repo_config.model.as_deref(), app.repo_config.model_provider_id.as_deref()) {
        (Some(model), Some(provider)) if !model.is_empty() && !provider.is_empty() => {
            Some(model_name(app, provider, model))
        }
        _ => None,
    }
}

/// 模型清单：跟随默认 + 按供应商分组的模型；定高内滚动。
fn model_picker<'a>(app: &'a App, draft: &AiDraft) -> Element<'a, Message> {
    let follow_selected = draft.provider_id.is_none() || draft.model.is_none();
    let follow_label = format!(
        "跟随默认 · {}",
        default_model_name(app).unwrap_or_else(|| "未设置".to_string())
    );
    let mut list = column![].spacing(2).push(pick_row(
        follow_label,
        follow_selected,
        Message::Table(tv::TableMsg::AiDraftModelPick { provider_id: None, model: None }),
    ));
    let mut any_model = false;
    for provider in app.repo_config.providers.iter().flatten() {
        if provider.models.is_empty() {
            continue;
        }
        any_model = true;
        list = list.push(
            container(
                text(provider.name.clone()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0).vertical(3.0)),
        );
        for model in &provider.models {
            let selected = draft.provider_id.as_deref() == Some(provider.id.as_str())
                && draft.model.as_deref() == Some(model.id.as_str());
            let label = model
                .nickname
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| model.id.clone());
            list = list.push(pick_row(
                label,
                selected,
                Message::Table(tv::TableMsg::AiDraftModelPick {
                    provider_id: Some(provider.id.clone()),
                    model: Some(model.id.clone()),
                }),
            ));
        }
    }
    if !any_model {
        list = list.push(
            text("暂无模型：先在「设置 → AI → 供应商」添加")
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
        );
    }
    scroll::v(container(list).padding(4.0)).height(Length::Fixed(PICKER_H)).into()
}

/// 单行可选项（模型清单用；选中项带勾选框）。
fn pick_row(label: String, selected: bool, on_press: Message) -> Element<'static, Message> {
    button(
        row![
            controls::checkbox(
                if selected { controls::CheckState::On } else { controls::CheckState::Off },
                None
            ),
            text(label)
                .size(pal::fs_caption())
                .color(if selected { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY })
                .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(8.0).vertical(5.0))
    .style(|_t, status| iced::widget::button::Style {
        background: matches!(status, iced::widget::button::Status::Hovered)
            .then_some(pal::HOVER.into()),
        ..Default::default()
    })
    .on_press(on_press)
    .into()
}

fn field_picker<'a>(session: &'a tv::TableSession, current: &TableField) -> Element<'a, Message> {
    let mut list = column![].spacing(2);
    for field in session.data.fields.iter().filter(|f| !ai::is_ai(f) && f.id != current.id) {
        list = list.push(
            button(
                text(format!("#{}", field.name))
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0).vertical(5.0))
            .style(|_t, status| iced::widget::button::Style {
                background: matches!(status, iced::widget::button::Status::Hovered)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(Message::Table(tv::TableMsg::AiDraftInsert(field.id.clone()))),
        );
    }
    scroll::v(container(list).padding(4.0)).height(Length::Fixed(PICKER_H)).into()
}