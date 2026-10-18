//! 日历天详情：点选日期后展开的当日历史版本（点击打开该文件历史弹层）
//! 与手动日程增删改（色点循环换色、标题行内编辑、添加行）。

use iced::widget::{button, column, container, row, space};
use iced::{Element, Length, Padding, border};

use atelyx_core::calendar::CalendarItem;
use atelyx_core::history::{self, HistoryAction};

use crate::calendar_actions::{CalendarMsg, EDIT_INPUT_ID};
use crate::calendar_grid;
use crate::calendar_view::{dot, item_color};
use crate::repohistory_view;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message};

/// 日程色点直径。
const DOT: f32 = 10.0;
/// 日程标题行内输入与添加行输入宽度。
const TITLE_INPUT_W: f32 = 180.0;
/// 当日历史行数上限（超出显示「还有 N 条」）。
const DAY_HISTORY_CAP: usize = 20;

/// 天详情（未选中时给引导文案）。
pub fn day_detail(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let Some(day) = app.calendar.selected else {
        return container(
            text("点选日期查看日程与历史")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding::ZERO.vertical(8.0))
        .into();
    };
    let (_y, m, d) = calendar_grid::civil_from_days(day);
    column![
        section_title(&format!("{m}月{d}日")),
        day_history(app, day, window),
        section_title("日程"),
        schedule_section(app, day, window),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}

/// 分区标题。
fn section_title(label: &str) -> Element<'static, Message> {
    text(label.to_string())
        .size(pal::fs_caption())
        .font(crate::ui::bold())
        .color(pal::TEXT_SECONDARY)
        .into()
}

/// 当日历史版本行（来自仓库历史缓存；上限 DAY_HISTORY_CAP）。
fn day_history(app: &App, day: i64, window: iced::window::Id) -> Element<'static, Message> {
    let entries = match app.repo_history.as_ref() {
        None => {
            return muted("历史加载中…");
        }
        Some(Err(e)) => {
            return muted(&format!("历史加载失败：{e}"));
        }
        Some(Ok(groups)) => calendar_grid::entries_on_day(groups, day),
    };
    if entries.is_empty() {
        return muted("当日无历史版本");
    }
    let mut col = iced::widget::Column::<Message>::new().spacing(1);
    for e in entries.iter().take(DAY_HISTORY_CAP) {
        let action = match e.action {
            HistoryAction::Edit => "编辑",
            HistoryAction::Restore => "回滚",
        };
        let (h, mi) = history::local_time_hm(e.ts);
        let summary = e.summary.clone().unwrap_or_default();
        col = col.push(
            container(
                button(
                    row![
                        icon(repohistory_view::kind_icon(e.kind), 12.0, pal::TEXT_MUTED),
                        text(format!("{h:02}:{mi:02}")).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                        text(action).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                        text(e.author_name.clone()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                        space().width(Length::Fill),
                        text(summary).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                    ]
                    .spacing(8),
                )
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(8.0).vertical(4.0))
                .style(controls::row_style(false, pal::RADIUS_SM))
                .on_press(Message::History(crate::history_browser::HistoryBrowserMsg::Open {
                    window,
                    kind: e.kind,
                    file: e.file.clone(),
                })),
            )
            .width(Length::Fill),
        );
    }
    if entries.len() > DAY_HISTORY_CAP {
        col = col.push(muted(&format!("还有 {} 条", entries.len() - DAY_HISTORY_CAP)));
    }
    col.into()
}

/// 当日日程区：已有日程行（色点循环 / 行内改题 / 删除二段确认）+ 添加行。
fn schedule_section(app: &App, day: i64, window: iced::window::Id) -> Element<'static, Message> {
    let mut items: Vec<CalendarItem> = app
        .calendar
        .items
        .iter()
        .filter(|it| calendar_grid::parse_date(&it.date) == Some(day))
        .cloned()
        .collect();
    items.sort_by_key(|it| it.created_at);
    let mut col = iced::widget::Column::<Message>::new().spacing(4);
    if items.is_empty() {
        col = col.push(muted("当日无日程"));
    }
    for item in &items {
        col = col.push(schedule_row(app, item, window));
    }
    col = col.push(add_row(app));
    col.width(Length::Fill).into()
}

/// 单条日程行：色点（点击循环换色）+ 标题（点击行内编辑）+ 删除（二段确认行内完成）。
fn schedule_row(app: &App, item: &CalendarItem, window: iced::window::Id) -> Element<'static, Message> {
    let color = item_color(item);
    let title: Element<Message> =
        if app.calendar.edit.as_ref().is_some_and(|(id, _)| id == &item.id) {
            let draft = app
                .calendar
                .edit
                .as_ref()
                .map(|(_, draft)| draft.clone())
                .unwrap_or_default();
            controls::inline_input(
                iced::widget::Id::new(EDIT_INPUT_ID),
                "",
                &draft,
                TITLE_INPUT_W,
                |v| Message::Calendar(CalendarMsg::EditInput(v)),
                Message::Calendar(CalendarMsg::EditCommit),
                Message::Calendar(CalendarMsg::EditPressInside),
            )
        } else {
            button(
                text(item.title.clone())
                    .size(pal::fs_caption())
                    .color(pal::TEXT_PRIMARY),
            )
            .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
            .style(controls::row_style(false, pal::RADIUS_XS))
            .on_press(Message::Calendar(CalendarMsg::EditStart(item.id.clone())))
            .into()
        };
    let tail: Element<'static, Message> =
        if app.calendar.delete_confirm.as_deref() == Some(item.id.as_str()) {
            row![
                controls::btn(
                    "确认删除",
                    &controls::btn_sm(),
                    controls::Variant::Danger,
                    Some(Message::Calendar(CalendarMsg::DeleteConfirm(item.id.clone()))),
                ),
                controls::btn(
                    "取消",
                    &controls::btn_sm(),
                    controls::Variant::Ghost,
                    Some(Message::Calendar(CalendarMsg::DeleteCancel)),
                ),
            ]
            .spacing(4)
            .into()
        } else {
            controls::icon_button(
                "trash-2",
                12.0,
                22.0,
                controls::Variant::Secondary,
                Some(Message::Calendar(CalendarMsg::Delete(item.id.clone()))),
                Some((window, "删除".into())),
            )
        };
    row![
        button(dot(color, DOT))
            .padding(0.0)
            .style(controls::row_style(false, pal::RADIUS_XS))
            .on_press(Message::Calendar(CalendarMsg::CycleColor(item.id.clone()))),
        title,
        space().width(Length::Fill),
        tail,
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}

/// 添加行：标题输入（Enter 提交）+ 7 色选择 + 添加按钮。
fn add_row(app: &App) -> Element<'static, Message> {
    let selected = app.calendar.draft_color % pal::EVENT_COLORS.len();
    let mut colors = iced::widget::Row::<Message>::new().spacing(4);
    for (i, c) in pal::EVENT_COLORS.iter().enumerate() {
        let on = selected == i;
        colors = colors.push(
            button(dot(*c, DOT))
                .padding(1.0)
                .style(move |_t, _s| button::Style {
                    border: if on {
                        border::rounded(pal::RADIUS_XS).width(1.0).color(pal::TEXT_PRIMARY)
                    } else {
                        border::rounded(pal::RADIUS_XS)
                    },
                    ..button::Style::default()
                })
                .on_press(Message::Calendar(CalendarMsg::DraftColor(i))),
        );
    }
    row![
        controls::form_input(
            "添加日程…",
            &app.calendar.draft,
            TITLE_INPUT_W,
            |v| Message::Calendar(CalendarMsg::DraftInput(v)),
            Message::Calendar(CalendarMsg::DraftSubmit),
        ),
        colors,
        space().width(Length::Fill),
        controls::btn(
            "添加",
            &controls::btn_sm(),
            controls::Variant::Secondary,
            Some(Message::Calendar(CalendarMsg::DraftSubmit)),
        ),
    ]
    .spacing(8)
    .width(Length::Fill)
    .into()
}

/// 微字辅助文案（空态 / 状态提示）。
fn muted(label: &str) -> Element<'static, Message> {
    text(label.to_string())
        .size(pal::fs_micro())
        .color(pal::TEXT_MUTED)
        .into()
}
