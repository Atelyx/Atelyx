//! 「关于」分区：Logo、版本、检查更新、项目主页。

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Padding};

use atelyx_core::markdown::LinkForm;

use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::{icon_handle, semibold};
use crate::{update, App, Message};

/// 字节数 → MB 文案（一位小数）。
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f32 / (1024.0 * 1024.0))
}

pub fn view(app: &App) -> Element<'static, Message> {
    let version = env!("CARGO_PKG_VERSION");
    let home = row![
        icon("globe", 13.0, pal::TEXT_MUTED),
        text("项目主页").size(pal::fs_caption()).color(pal::ACCENT),
        space().width(Length::Fill),
        text("github.com/Atelyx/Atelyx")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)
            .font(crate::layout_view::mono()),
    ]
    .spacing(6);
    // 检查更新行：按钮随流程态换动作（Idle=检查 / Available=下载 / Ready=安装；Checking/Downloading 禁用）
    let (label, action) = match &app.update {
        update::UpdateState::Idle => (
            "检查更新",
            Some(Message::Update(update::UpdateMsg::Check { manual: true })),
        ),
        update::UpdateState::Checking | update::UpdateState::Downloading { .. } => ("检查更新", None),
        update::UpdateState::Available { .. } => {
            ("下载", Some(Message::Update(update::UpdateMsg::Download)))
        }
        update::UpdateState::Ready { .. } => {
            ("立即安装", Some(Message::Update(update::UpdateMsg::Install)))
        }
    };
    let mut update_row = row![
        controls::btn(label, &controls::btn_sm(), controls::Variant::Secondary, action),
    ]
    .spacing(6)
    .align_y(Vertical::Center);
    // 下载中的具体进度在常驻通知条；此处只给流程反馈
    match &app.update {
        update::UpdateState::Checking => {
            update_row = update_row
                .push(text("检查中…").size(pal::fs_caption()).color(pal::TEXT_SECONDARY));
        }
        update::UpdateState::Downloading { received, total, .. } => {
            update_row = update_row.push(
                text(format!("下载中 {}/{} MB", mb(*received), mb(*total)))
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY),
            );
        }
        _ => {}
    }
    let col = column![
        container(
            row![
                // Logo：56px 图标 + 4px 内衬 = 64px ring 盒
                container(
                    iced::widget::image(icon_handle())
                        .width(Length::Fixed(56.0))
                        .height(Length::Fixed(56.0)),
                )
                .padding(4.0)
                .style(|_t| iced::widget::container::Style {
                    background: Some(pal::BG_TERTIARY.into()),
                    border: iced::border::rounded(pal::RADIUS_MD)
                        .color(pal::BORDER_STRONG)
                        .width(1.0),
                    ..iced::widget::container::Style::default()
                }),
                column![
                    text("Atelyx").size(pal::fs_h1()).color(pal::TEXT_PRIMARY).font(semibold()),
                    text(format!("版本 {version}")).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                ]
                .spacing(2),
            ]
            .spacing(12)
            .padding(Padding::ZERO.horizontal(8.0))
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.bottom(16.0)),
        row![
            icon("info", 13.0, pal::TEXT_MUTED),
            text("桌面 AI 工作台：对话、笔记、表格与文件在同一工作台并排使用。")
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .padding(Padding::ZERO.horizontal(8.0)),
        container(
            button(home)
                .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
                .width(Length::Fill)
                .style(controls::row_style(false, pal::RADIUS_SM))
                .on_press(Message::OpenLink(
                    String::from("https://github.com/Atelyx/Atelyx"),
                    LinkForm::External,
                )),
        )
        .width(Length::Fill),
        container(update_row)
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(8.0).top(8.0)),
    ];
    scrollable(container(col.width(Length::Fill).padding(Padding::ZERO.horizontal(12.0).top(4.0).bottom(24.0))))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
