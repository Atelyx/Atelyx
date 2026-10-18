//! 「关于」分区：居中品牌区（Logo / 名称 / 版本 / 定位）+ 信息卡片（检查更新 / 项目主页 / 开源许可）。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{column, container, row, space};
use iced::{Element, Length, Padding};

use atelyx_core::markdown::LinkForm;

use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::ui::{bold, icon_handle};
use crate::{update, App, Message};

/// 开源许可条目（名称 + 许可 + 官方主页）：只列随应用分发的第三方资产。
const LICENSES: [(&str, &str, &str); 1] = [
    ("Lucide 图标形状数据", "ISC", "https://lucide.dev"),
];
/// 许可模态卡宽。
pub(crate) const LICENSE_CARD_WIDTH: f32 = 380.0;

/// 字节数 → MB 文案（一位小数）。
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f32 / (1024.0 * 1024.0))
}

pub fn view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let version = env!("CARGO_PKG_VERSION");
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(brand_block(version));
    // 信息卡片限宽居中，与品牌区同轴（容器居中须包住限宽列，max_width 钳的是自身宽度）
    let mut cards = column![].spacing(8);
    cards = cards.push(info_card(
        "refresh-cw",
        "检查更新",
        update_subline(app, version),
        update_button(app),
    ));
    cards = cards.push(info_card(
        "globe",
        "项目主页",
        row![text("github.com/Atelyx/Atelyx")
            .size(pal::fs_micro())
            .color(pal::TEXT_SECONDARY)
            .font(crate::ui::mono())]
            .into(),
        controls::btn(
            "前往",
            &controls::btn_pill(),
            controls::Variant::Outline,
            Some(Message::OpenLink(
                String::from("https://github.com/Atelyx/Atelyx"),
                LinkForm::External,
            )),
        )
        .into(),
    ));
    cards = cards.push(info_card(
        "file-text",
        "开源许可",
        text("Lucide 图标形状数据（ISC）")
            .size(pal::fs_micro())
            .color(pal::TEXT_SECONDARY)
            .into(),
        controls::btn(
            "查看",
            &controls::btn_pill(),
            controls::Variant::Outline,
            Some(crate::layout_view::license_popup(window)),
        )
        .into(),
    ));
    col = col.push(container(cards.max_width(CARD_AREA_WIDTH)).width(Length::Fill).align_x(Horizontal::Center));
    // 内容定长且必小于最小窗口高度：不滚动，整块在分区里垂直居中
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// 信息卡片区宽度上限（与品牌区同轴的居中列）。
const CARD_AREA_WIDTH: f32 = 550.0;

/// 品牌区：Logo + 强调字重的名称 + 版本胶囊 + 一句话定位，整块居中。
fn brand_block(version: &str) -> Element<'static, Message> {
    container(
        column![
            iced::widget::image(icon_handle())
                .width(Length::Fixed(56.0))
                .height(Length::Fixed(56.0)),
            row![
                text("Atelyx")
                    .size(pal::fs_h1())
                    .color(pal::TEXT_PRIMARY)
                    .font(bold()),
                controls::badge(version, pal::ACCENT_SOFT, pal::ACCENT),
            ]
            .spacing(10)
            .align_y(Vertical::Center),
            // 换行文本默认占满整行宽，须容器居中而非依赖列的 align_x
            container(text("AI时代下的可无限拓展桌面工作台")
                    .size(pal::fs_caption())
                    .color(pal::TEXT_SECONDARY))
                .center_x(Length::Fill),
        ]
        .spacing(10)
        .align_x(Horizontal::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.bottom(16.0))
    .center_x(Length::Fill)
    .into()
}

/// 信息卡片行：图标槽 + 标题/说明 + 右侧动作。
fn info_card(
    icon_name: &'static str,
    title: &'static str,
    subline: Element<'static, Message>,
    action: Element<'static, Message>,
) -> Element<'static, Message> {
    container(
        row![
            container(icon(icon_name, 14.0, pal::ACCENT))
                .width(Length::Fixed(30.0))
                .height(Length::Fixed(30.0))
                .center_x(Length::Fixed(30.0))
                .center_y(Length::Fixed(30.0))
                .style(|_t| iced::widget::container::Style {
                    background: Some(pal::BG_SUNKEN.into()),
                    border: iced::border::rounded(pal::RADIUS_SM),
                    ..iced::widget::container::Style::default()
                }),
            column![
                text(title)
                    .size(pal::fs_body())
                    .color(pal::TEXT_PRIMARY)
                    .font(crate::ui::bold()),
                subline,
            ]
            .spacing(2),
            space().width(Length::Fill),
            action,
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
    .style(|_t| iced::widget::container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: iced::border::rounded(pal::RADIUS_MD),
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// 许可模态覆盖层：遮罩捕获层 + 居中卡（非本形态弹层 / 非本窗口返回 None）。
pub fn license_overlay(app: &App, window: iced::window::Id) -> Option<Element<'static, Message>> {
    let (w, open) = app.popup.as_ref()?;
    if *w != window || !matches!(open.state, crate::layout_view::PopupState::LicenseInfo) {
        return None;
    }
    let header = container(
        row![
            icon("file-text", 14.0, pal::TEXT_SECONDARY),
            text("开源许可").size(pal::fs_body()).color(pal::TEXT_PRIMARY).font(bold()),
            space().width(Length::Fill),
            controls::icon_button(
                "x",
                14.0,
                26.0,
                controls::Variant::Ghost,
                Some(Message::Ui(crate::ui::UiMsg::PopupClose { window })),
                Some((window, "关闭".into())),
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(10.0));
    let mut rows = column![].spacing(8).padding(12.0);
    for (name, license, url) in LICENSES {
        rows = rows.push(license_row(name, license, url));
    }
    let card = container(column![header, controls::h_divider_subtle(), rows].width(Length::Fill))
        .width(Length::Fixed(LICENSE_CARD_WIDTH))
        .style(|_t| iced::widget::container::Style {
            background: Some(pal::BG_OVERLAY.into()),
            border: iced::border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
            ..iced::widget::container::Style::default()
        });
    Some(crate::ui::popup::modal_overlay(window, card.into()))
}

/// 许可条目行：名称与许可证类型，右侧前往官方主页。
fn license_row(name: &'static str, license: &'static str, url: &'static str) -> Element<'static, Message> {
    container(
        row![
            column![
                text(name).size(pal::fs_body()).color(pal::TEXT_PRIMARY),
                text(license).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            ]
            .spacing(2),
            space().width(Length::Fill),
            controls::btn(
                "前往",
                &controls::btn_pill(),
                controls::Variant::Outline,
                Some(Message::OpenLink(url.to_string(), LinkForm::External)),
            ),
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(8.0))
    .style(|_t| iced::widget::container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: iced::border::rounded(pal::RADIUS_MD),
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// 检查更新行的说明文案（随流程态变化）。
fn update_subline(app: &App, version: &str) -> Element<'static, Message> {
    let label = match &app.update {
        update::UpdateState::Idle => format!("当前版本 {version}"),
        update::UpdateState::Checking => "检查中…".to_string(),
        update::UpdateState::Available { version: v, .. } => format!("发现新版本 {v}"),
        update::UpdateState::Downloading { received, total, .. } => {
            format!("下载中 {}/{} MB", mb(*received), mb(*total))
        }
        update::UpdateState::Ready { version: v, .. } => format!("{v} 已就绪，重启安装生效"),
    };
    text(label).size(pal::fs_micro()).color(pal::TEXT_SECONDARY).into()
}

/// 检查更新行的动作按钮（Idle=检查 / Available=下载 / Ready=安装；Checking/Downloading 禁用）。
fn update_button(app: &App) -> Element<'static, Message> {
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
    controls::btn(label, &controls::btn_pill(), controls::Variant::Primary, action).into()
}
