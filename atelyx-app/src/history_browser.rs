//! 历史记录弹层：居中模态（版本行 + 展开预览 + 回滚二段确认）。
//! 版本列表真源 = `App.history`（打开弹层按 kind+file 异步加载，gen + 文件身份守卫）；
//! 回滚复用各保存链：笔记/表格走会话回滚，画布为快照直写仓库（无会话链）。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text};
use iced::{Element, Length, Padding, Point, Size, Task, border, widget::container::Style};
use atelyx_core::history::{HistoryAction, HistoryKind, HistoryVersion};

use crate::layout_view::{PopupOpen, PopupState, format_ts, mono};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::semibold;
use crate::{App, Message};

/// 弹层卡定宽。
pub const CARD_WIDTH: f32 = 560.0;
/// 版本行近似高度（估高用；实际随内容自适应）。
const ROW_H: f32 = 32.0;
/// 展开预览区限高（内部滚动）。
const PREVIEW_H: f32 = 200.0;
/// 预览/列表顶部之外的卡头与分隔估高。
const CARD_CHROME_H: f32 = 56.0;
/// 二段确认条估高。
const CONFIRM_H: f32 = 60.0;

/// 历史记录弹层消息。
#[derive(Debug, Clone)]
pub enum HistoryBrowserMsg {
    /// 打开弹层（入口：笔记/表格「···」菜单）。
    Open { window: iced::window::Id, kind: HistoryKind, file: String },
    /// 展开/收起一行预览（互斥展开）。
    Toggle(u64),
    /// 进入回滚二段确认。
    Ask(u64),
    /// 取消二段确认。
    CancelAsk,
    /// 确认回滚（复用各保存链）。
    Restore(u64),
}

/// 打开弹层并按 kind+file 加载版本列表（先清交互态：展开/确认不跨文件延续）。
pub fn open(app: &mut App, window: iced::window::Id, kind: HistoryKind, file: String) -> Task<Message> {
    app.dismiss_popup();
    app.history.clear();
    app.history_loading = true;
    app.history_expanded = None;
    app.history_confirm = None;
    app.popup = Some((
        window,
        PopupOpen {
            point: Point::ORIGIN,
            flip_y: None,
            state: PopupState::HistoryBrowser { kind, file: file.clone() },
            min_width: 0.0,
        },
    ));
    app.load_history_for(kind, &file)
}

/// 弹层消息处理。
pub fn on_msg(app: &mut App, msg: HistoryBrowserMsg) -> Task<Message> {
    match msg {
        HistoryBrowserMsg::Open { window, kind, file } => open(app, window, kind, file),
        HistoryBrowserMsg::Toggle(seq) => {
            app.history_expanded = if app.history_expanded == Some(seq) { None } else { Some(seq) };
            Task::none()
        }
        HistoryBrowserMsg::Ask(seq) => {
            app.history_confirm = Some(seq);
            Task::none()
        }
        HistoryBrowserMsg::CancelAsk => {
            app.history_confirm = None;
            Task::none()
        }
        HistoryBrowserMsg::Restore(seq) => restore(app, seq),
    }
}

/// 居中模态覆盖元素（SCRIM 捕获层 + 居中卡；非本形态弹层 / 非本窗口返回 None）。
pub fn overlay<'a>(app: &'a App, window: iced::window::Id, viewport: Size) -> Option<Element<'a, Message>> {
    let (w, open) = app.popup.as_ref()?;
    if *w != window {
        return None;
    }
    let PopupState::HistoryBrowser { kind, file } = &open.state else {
        return None;
    };
    let capture = mouse_area(space().width(Length::Fill).height(Length::Fill))
        .on_press(Message::Ui(crate::ui::UiMsg::PopupClose { window }));
    let card = browser_card(app, *kind, file, window, viewport);
    // 阴影环与 popup::overlay 同做法：外扩容器露出的半透明黑底环
    let shadow_wrap = container(card)
        .padding(Padding { left: 4.0, top: 2.0, right: 4.0, bottom: 14.0 })
        .style(|_t| Style {
            background: Some(iced::Color::BLACK.scale_alpha(0.35).into()),
            border: border::rounded(pal::RADIUS_MD + 4.0),
            ..Style::default()
        });
    let centered = container(shadow_wrap)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);
    Some(stack![capture, centered].width(Length::Fill).height(Length::Fill).into())
}

fn browser_card<'a>(
    app: &'a App,
    kind: HistoryKind,
    file: &str,
    window: iced::window::Id,
    viewport: Size,
) -> Element<'a, Message> {
    let kind_label = match kind {
        HistoryKind::Note => "笔记",
        HistoryKind::Table => "表格",
        HistoryKind::Canvas => "画布",
    };
    let name = file.rsplit('/').next().unwrap_or(file);
    let header = container(
        row![
            icon("history", 14.0, pal::TEXT_SECONDARY),
            text(format!("历史记录 · {kind_label} · {name}"))
                .size(pal::fs_body())
                .color(pal::TEXT_PRIMARY)
                .font(semibold()),
            space().width(Length::Fill),
            controls::icon_button(
                "关闭",
                14.0,
                26.0,
                controls::Variant::Ghost,
                Some(Message::Ui(crate::ui::UiMsg::PopupClose { window })),
                None,
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(10.0));

    let body = if app.history_loading {
        stand_placeholder("加载中…")
    } else if app.history.is_empty() {
        stand_placeholder("暂无历史记录")
    } else {
        let mut list = iced::widget::Column::<Message>::new().padding(6.0).spacing(2);
        // 新→旧：版本列表按追加序（旧→新）存放，倒序展示；预览 diff 相对前一版
        // 空间 viewer 只读：回滚按钮禁用（写请求服务端本会 403，此处界面级降级）
        let viewer = matches!(&app.repo_kind, crate::RepoKind::Space(s) if !s.role.can_write());
        for (i, v) in app.history.iter().enumerate().rev() {
            list = list.push(version_row(v, viewer, window));
            if app.history_expanded == Some(v.seq) {
                list = list.push(preview_block(app, kind, i, v));
            }
        }
        scrollable(container(list).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    // 卡高 = 内容估高钳制在 70vh 内（超出走内滚动）
    let content_h = if app.history_loading || app.history.is_empty() {
        60.0
    } else {
        app.history.len() as f32 * ROW_H
            + app.history_expanded.map(|_| PREVIEW_H + 6.0).unwrap_or(0.0)
    };
    let confirm_h = if app.history_confirm.is_some() { CONFIRM_H } else { 0.0 };
    let card_h = (CARD_CHROME_H + content_h + confirm_h).min(viewport.height * 0.7);

    let mut card = column![header, controls::h_divider_subtle(), body].width(Length::Fill);
    if let Some(seq) = app.history_confirm {
        if let Some(v) = app.history.iter().find(|v| v.seq == seq) {
            card = card.push(confirm_bar(v.seq, v.ts));
        }
    }
    container(card)
        .width(Length::Fixed(CARD_WIDTH))
        .height(Length::Fixed(card_h))
        .style(|_t| Style {
            background: Some(pal::BG_OVERLAY.into()),
            border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
            ..Style::default()
        })
        .into()
}

/// 空态/加载中占位。
fn stand_placeholder(label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(pal::fs_caption()).color(pal::TEXT_MUTED))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

/// 版本行（点击展开预览）：动作 + 作者 + 摘要，右侧时间 + 回滚入口（viewer 禁用并提示）。
fn version_row(v: &HistoryVersion, viewer: bool, window: iced::window::Id) -> Element<'_, Message> {
    let action = match v.action {
        HistoryAction::Edit => "编辑",
        HistoryAction::Restore => "回滚",
    };
    let seq = v.seq;
    let expand = button(
        row![
            text(action)
                .size(pal::fs_micro())
                .color(pal::ACCENT)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
            text(author_line(v)).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
            text(summary_line(v)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            space().width(Length::Fill),
            text(format_ts(v.ts)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(10)
        .align_y(Vertical::Center),
    )
    .padding(Padding::ZERO.horizontal(8.0).vertical(5.0))
    .width(Length::Fill)
    .style(|_t, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(pal::HOVER.into()),
        border: border::rounded(pal::RADIUS_SM),
        ..button::Style::default()
    })
    .on_press(Message::History(HistoryBrowserMsg::Toggle(seq)));
    let rollback = controls::btn(
        "回滚",
        &controls::btn_sm(),
        controls::Variant::AccentSoft,
        (!viewer).then_some(Message::History(HistoryBrowserMsg::Ask(seq))),
    );
    let rollback: Element<'static, Message> = if viewer {
        // 禁用态补悬停提示（tooltip 通道与菜单项同款）
        iced::widget::mouse_area(rollback)
            .on_enter(Message::Ui(crate::ui::UiMsg::HoverTip {
                window,
                label: "当前角色为只读".to_string(),
                box_px: controls::btn_sm().h,
            }))
            .on_exit(Message::Ui(crate::ui::UiMsg::HoverTipEnd {
                window,
                label: "当前角色为只读".to_string(),
            }))
            .into()
    } else {
        rollback.into()
    };
    row![expand, rollback]
        .spacing(6)
        .align_y(Vertical::Center)
        .width(Length::Fill)
        .into()
}

/// 回滚二段确认条。
fn confirm_bar(seq: u64, ts: i64) -> Element<'static, Message> {
    container(
        row![
            text(format!(
                "回滚到 {} 的版本？当前未保存的编辑将被覆盖，此操作会记录一条「回滚」历史。",
                format_ts(ts)
            ))
            .size(pal::fs_caption())
            .color(pal::TEXT_SECONDARY)
            .width(Length::Fill),
            controls::btn(
                "取消",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(Message::History(HistoryBrowserMsg::CancelAsk)),
            ),
            controls::btn(
                "确认回滚",
                &controls::btn_sm(),
                controls::Variant::Danger,
                Some(Message::History(HistoryBrowserMsg::Restore(seq))),
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
    .style(|_t| Style {
        background: Some(pal::DANGER.scale_alpha(0.08).into()),
        border: border::rounded(pal::RADIUS_SM),
        ..Style::default()
    })
    .into()
}

/// 展开预览（限高内滚动）：笔记 = 全文只读快照；表格/画布 = 规模统计 + 相对前一版的差异行。
fn preview_block<'a>(
    app: &'a App,
    kind: HistoryKind,
    index: usize,
    v: &'a HistoryVersion,
) -> Element<'a, Message> {
    // 前一版快照内容（首版无前版 = 空串，diff 解析失败自然返回空差异）
    let prev = index
        .checked_sub(1)
        .and_then(|i| app.history.get(i))
        .map(|p| p.content.as_str())
        .unwrap_or("");
    let mut inner = iced::widget::Column::<Message>::new().spacing(4);
    match kind {
        HistoryKind::Note => {
            inner = inner.push(
                text(v.content.clone())
                    .font(mono())
                    .shaping(iced::advanced::text::Shaping::Advanced)
                    .size(pal::fs_micro())
                    .color(pal::TEXT_PRIMARY)
                    .width(Length::Fill),
            );
        }
        HistoryKind::Table => {
            if let Some(t) = serde_json::from_str::<atelyx_core::vault::TableFile>(&v.content).ok() {
                inner = inner.push(stat_line(format!("「{}」 · {} 字段 · {} 行", t.title, t.fields.len(), t.rows.len())));
            }
            inner = push_diff(inner, prev, &v.content, atelyx_core::history_diff::table_diff);
        }
        HistoryKind::Canvas => {
            let (nodes, edges, messages) = atelyx_core::history_diff::canvas_summary(&v.content);
            inner = inner.push(stat_line(format!("{nodes} 节点 · {edges} 连线 · {messages} 消息")));
            inner = push_diff(inner, prev, &v.content, atelyx_core::history_diff::canvas_diff);
        }
    }
    container(
        scrollable(container(inner).width(Length::Fill).padding(8.0))
            .width(Length::Fill)
            .height(Length::Fixed(PREVIEW_H)),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(8.0).bottom(6.0))
    .style(|_t| Style {
        background: Some(pal::BG_SUNKEN.into()),
        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER_SUBTLE).width(1.0),
        ..Style::default()
    })
    .into()
}

/// 统计行（表格/画布预览首行）。
fn stat_line(label: String) -> Element<'static, Message> {
    text(label)
        .size(pal::fs_micro())
        .color(pal::TEXT_SECONDARY)
        .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() })
        .into()
}

/// 差异行追加（每行一条；差异为空不追加）。
fn push_diff(
    mut inner: iced::widget::Column<'static, Message>,
    prev: &str,
    cur: &str,
    diff: fn(&str, &str) -> Vec<String>,
) -> iced::widget::Column<'static, Message> {
    for line in diff(prev, cur) {
        inner = inner.push(text(line).size(pal::fs_micro()).color(pal::TEXT_SECONDARY));
    }
    inner
}

/// 作者署名：单作者 name·device 同名只显 name（name 空 = device）；协作署名按名去重顿号连接。
fn author_line(v: &HistoryVersion) -> String {
    let display = |a: &atelyx_core::history::HistoryAuthor| {
        if a.name.is_empty() { a.device.clone() } else { a.name.clone() }
    };
    let mut names = vec![display(&v.author)];
    if let Some(co) = &v.co_authors {
        for c in co {
            let n = display(c);
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names.join("、")
}

/// 摘要展示：summary 缺省时按快照字节数估字数（历史侧主要为中文正文，按 3 字节/字近似）。
fn summary_line(v: &HistoryVersion) -> String {
    v.summary
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} 字", v.content.len() / 3))
}

/// 回滚执行（守卫：列表加载世代 + 打开文件身份；确认后关弹层再走各保存链）。
fn restore(app: &mut App, seq: u64) -> Task<Message> {
    let Some((_, open)) = &app.popup else { return Task::none() };
    let PopupState::HistoryBrowser { kind, file } = &open.state else {
        return Task::none();
    };
    let (kind, file) = (*kind, file.clone());
    let Some(version) = app.history.iter().find(|v| v.seq == seq).cloned() else {
        return Task::none();
    };
    // 世代守卫：容器/仓库切换后版本列表过期，不得据其写盘
    if app.history_gen != app.epoch() {
        app.notify(NoticeLevel::Warning, "历史列表已过期，请重新打开");
        return Task::none();
    }
    match kind {
        HistoryKind::Note => {
            if app.ui.last_note_file.as_deref() != Some(file.as_str()) {
                app.notify(NoticeLevel::Warning, "打开的笔记已切换，请重新打开历史");
                return Task::none();
            }
            app.dismiss_popup();
            // 成功提示挂到保存链落盘回执（「已回滚」只在写盘成功后出现）
            app.history_restore_pending = true;
            app.history_restore(kind, version)
        }
        HistoryKind::Table => {
            if app.ui.last_table_file.as_deref() != Some(file.as_str()) {
                app.notify(NoticeLevel::Warning, "打开的表格已切换，请重新打开历史");
                return Task::none();
            }
            // 会话缺失（表未加载成功）时无回滚目标：不得谎报成功
            if !app.table.as_ref().is_some_and(|s| s.file == file) {
                app.notify(NoticeLevel::Warning, "表格未加载，无法回滚");
                return Task::none();
            }
            app.dismiss_popup();
            // 成功提示挂到保存链落盘回执（「已回滚」只在写盘成功后出现）
            app.history_restore_pending = true;
            app.history_restore(kind, version)
        }
        // 画布无会话链：快照经写盘队列直写仓库，回执通知
        HistoryKind::Canvas => {
            app.dismiss_popup();
            app.canvas_history_restore(file, version)
        }
    }
}
