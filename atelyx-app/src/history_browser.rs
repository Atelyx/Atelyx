//! 历史记录弹层：居中模态（版本行 + 展开预览 + 回滚二段确认）。
//! 版本列表真源 = `App.history`（打开弹层按 kind+file 异步加载，gen + 文件身份守卫）；
//! 回滚复用各保存链：笔记/表格走会话回滚，画布为快照直写仓库（无会话链）。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, row, scrollable, space};
use iced::{Element, Length, Padding, Point, Size, Task, border, widget::container::Style};
use atelyx_core::history::{HistoryAction, HistoryKind, HistoryVersion};
use atelyx_core::history_diff::{DiffLine, DiffLineKind, TableDiffItem};

use crate::layout_view::{PopupOpen, PopupState, format_ts};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::mono;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::ui::bold;
use crate::{App, Message};

/// 弹层卡定宽。
pub const CARD_WIDTH: f32 = 560.0;
/// 版本行近似高度（估高用：卡片上下 padding 12 + 单行内容 ≈ 22，实际随内容自适应）。
const ROW_H: f32 = 36.0;
/// 展开预览区限高（内部滚动）。
const PREVIEW_H: f32 = 200.0;
/// 预览/列表顶部之外的卡头与分隔估高。
const CARD_CHROME_H: f32 = 56.0;
/// 二段确认条估高。
const CONFIRM_H: f32 = 60.0;
/// 笔记预览逐行着色的改动行数上限（超出 = 整段重写，着色无信息增益且部件数过多）。
const NOTE_CHANGED_LINES_CAP: usize = 300;

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

/// 展开预览的内容（差异只在展开时算一次，渲染每帧读缓存）。
pub struct HistoryPreview {
    /// 对应版本 seq（与展开态比对，防切换后错用旧预览）。
    pub seq: u64,
    /// 首行统计（表格/画布规模统计或「初始版本快照」；笔记无）。
    pub stat: Option<String>,
    pub body: PreviewBody,
}

/// 预览正文（按文件类型的差异形态）。
pub enum PreviewBody {
    /// 首版无可比对象（表格/画布）。
    Empty,
    Note(Vec<DiffLine>),
    Table(Vec<TableDiffItem>),
    Canvas(Vec<DiffLine>),
}

/// 打开弹层并按 kind+file 加载版本列表（先清交互态：展开/确认/预览不跨文件延续）。
pub fn open(app: &mut App, window: iced::window::Id, kind: HistoryKind, file: String) -> Task<Message> {
    app.dismiss_popup();
    app.history.clear();
    app.history_loading = true;
    app.history_expanded = None;
    app.history_preview = None;
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

/// 构建某版本的展开预览：差异相对该版本的前一版；首版无前版（表格/画布只示「初始版本快照」）。
/// 供弹层展开与列表刷新共用（刷新后展开项可能已失效 → None，调用方据此收起）。
pub(crate) fn build_preview(app: &App, seq: u64) -> Option<HistoryPreview> {
    let (_, open) = app.popup.as_ref()?;
    let PopupState::HistoryBrowser { kind, .. } = &open.state else {
        return None;
    };
    let kind = *kind;
    let index = app.history.iter().position(|v| v.seq == seq)?;
    let v = &app.history[index];
    let prev = index
        .checked_sub(1)
        .and_then(|i| app.history.get(i))
        .map(|p| p.content.as_str())
        .unwrap_or("");
    if index == 0 {
        // 笔记首版仍可逐行查看（自比对 = 全部未改动行）；表格/画布无前版可比，只报规模
        let body = match kind {
            HistoryKind::Note => PreviewBody::Note(atelyx_core::history_diff::note_diff(&v.content, &v.content)),
            HistoryKind::Table | HistoryKind::Canvas => PreviewBody::Empty,
        };
        let stat = (kind != HistoryKind::Note)
            .then(|| format!("初始版本快照 · {}", scope_line(kind, &v.content)));
        return Some(HistoryPreview { seq, stat, body });
    }
    let body = match kind {
        HistoryKind::Note => PreviewBody::Note(atelyx_core::history_diff::note_diff(prev, &v.content)),
        HistoryKind::Table => PreviewBody::Table(atelyx_core::history_diff::table_diff(prev, &v.content)),
        HistoryKind::Canvas => PreviewBody::Canvas(atelyx_core::history_diff::canvas_diff(prev, &v.content)),
    };
    let stat = match kind {
        HistoryKind::Note => None,
        HistoryKind::Table => serde_json::from_str::<atelyx_core::vault::TableFile>(&v.content)
            .ok()
            .map(|t| format!("「{}」 · {}", t.title, scope_line(kind, &v.content))),
        HistoryKind::Canvas => Some(scope_line(kind, &v.content)),
    };
    Some(HistoryPreview { seq, stat, body })
}

/// 弹层消息处理。
pub fn on_msg(app: &mut App, msg: HistoryBrowserMsg) -> Task<Message> {
    match msg {
        HistoryBrowserMsg::Open { window, kind, file } => open(app, window, kind, file),
        HistoryBrowserMsg::Toggle(seq) => {
            if app.history_expanded == Some(seq) {
                app.history_expanded = None;
                app.history_preview = None;
            } else {
                app.history_expanded = Some(seq);
                app.history_preview = build_preview(app, seq);
            }
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

/// 居中模态覆盖元素（遮罩捕获层 + 居中卡；非本形态弹层 / 非本窗口返回 None）。
pub fn overlay<'a>(app: &'a App, window: iced::window::Id, viewport: Size) -> Option<Element<'a, Message>> {
    let (w, open) = app.popup.as_ref()?;
    if *w != window {
        return None;
    }
    let PopupState::HistoryBrowser { kind, file } = &open.state else {
        return None;
    };
    let card = browser_card(app, *kind, file, window, viewport);
    Some(crate::ui::popup::modal_overlay(window, card))
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
                .font(bold()),
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

    let body = if app.history_loading {
        stand_placeholder("加载中…")
    } else if app.history.is_empty() {
        stand_placeholder("暂无历史记录")
    } else {
        let mut list = iced::widget::Column::<Message>::new().padding(6.0).spacing(6);
        // 新→旧：版本列表按追加序（旧→新）存放，倒序展示；预览差异取展开时算好的缓存
        // 空间 viewer 只读：回滚按钮禁用（写请求服务端本会 403，此处界面级降级）
        let viewer = matches!(&app.repo_kind, crate::RepoKind::Space(s) if !s.role.can_write());
        for v in app.history.iter().rev() {
            list = list.push(version_row(v, viewer, app.history_expanded == Some(v.seq), window));
            if app.history_expanded == Some(v.seq) {
                list = list.push(preview_block(app, v));
            }
        }
        scrollable(container(list).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    // 卡高 = 内容估高钳制在 70vh 内（超出走内滚动）；估高含卡片间距 6 与列表上下 padding 12
    let content_h = if app.history_loading || app.history.is_empty() {
        60.0
    } else {
        let n = app.history.len() as f32;
        n * (ROW_H + 6.0) + 12.0 + app.history_expanded.map(|_| PREVIEW_H + 6.0).unwrap_or(0.0)
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

/// 版本卡片：chevron + 动作 + 作者 + 摘要 + 时间，右侧回滚入口（viewer 禁用并提示）。
fn version_row(v: &HistoryVersion, viewer: bool, expanded: bool, window: iced::window::Id) -> Element<'_, Message> {
    let action = match v.action {
        HistoryAction::Edit => "编辑",
        HistoryAction::Restore => "回滚",
    };
    let seq = v.seq;
    let expand = button(
        row![
            icon(
                if expanded { "chevron-down" } else { "chevron-right" },
                12.0,
                pal::TEXT_MUTED,
            ),
            text(action)
                .size(pal::fs_caption())
                .color(pal::ACCENT)
                .font(crate::ui::bold()),
            text(author_line(v)).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
            text(v.summary.clone().unwrap_or_default()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            space().width(Length::Fill),
            text(format_ts(v.ts)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    )
    .padding(0.0)
    .width(Length::Fill)
    .style(controls::row_style(false, pal::RADIUS_SM))
    .on_press(Message::History(HistoryBrowserMsg::Toggle(seq)));
    // 回滚：金色文字钮（viewer 无 on_press = 禁用态，内容降透明度并补只读提示）
    let fg = if viewer { pal::ACCENT.scale_alpha(0.4) } else { pal::ACCENT };
    let rollback_inner = row![
        icon("rotate-ccw", 12.0, fg),
        text("回滚").size(pal::fs_caption()).color(fg),
    ]
    .spacing(4)
    .align_y(Vertical::Center);
    let rollback: Element<'static, Message> = {
        let widget = button(rollback_inner)
            .padding(Padding::ZERO.horizontal(6.0).vertical(3.0))
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                border: border::rounded(pal::RADIUS_SM),
                ..button::Style::default()
            });
        let widget = if viewer { widget } else { widget.on_press(Message::History(HistoryBrowserMsg::Ask(seq))) };
        if viewer {
            iced::widget::mouse_area(widget)
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
            widget.into()
        }
    };
    container(row![expand, rollback].spacing(6).align_y(Vertical::Center))
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
        .style(|_t| Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_MD),
            ..Style::default()
        })
        .into()
}

/// 回滚二段确认条。
fn confirm_bar(seq: u64, ts: i64) -> Element<'static, Message> {
    container(
        row![
            text(format!("回滚到 {} 的版本？未保存的编辑将被覆盖", format_ts(ts)))
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

/// 展开预览（限高内滚动）：读展开时算好的差异缓存（表格/画布另有规模统计行）。
fn preview_block<'a>(app: &'a App, v: &'a HistoryVersion) -> Element<'a, Message> {
    let mut inner = iced::widget::Column::<Message>::new().spacing(4);
    if let Some(preview) = app.history_preview.as_ref().filter(|p| p.seq == v.seq) {
        if let Some(stat) = &preview.stat {
            inner = inner.push(stat_line(stat.clone()));
        }
        inner = match &preview.body {
            PreviewBody::Empty => inner,
            // 改动行过多（整段重写）时逐行着色既无信息增益又部件爆炸：退回整篇快照
            PreviewBody::Note(lines) if changed_lines(lines) > NOTE_CHANGED_LINES_CAP => {
                inner.push(note_plain_block(&[v.content.as_str()]))
            }
            PreviewBody::Note(lines) => render_note_lines(inner, lines),
            PreviewBody::Table(items) => render_table_items(inner, items),
            PreviewBody::Canvas(lines) => render_diff_lines(inner, lines),
        };
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

/// 改动行条数（非 Same 行；决定是否逐行着色）。
fn changed_lines(lines: &[DiffLine]) -> usize {
    lines.iter().filter(|l| l.kind != DiffLineKind::Same).count()
}

/// 笔记行级预览：未改动行按连续段合并成整块文本（控制部件数），改动行逐行着色——
/// 新增行绿底、被删行红底插在删除位置。
fn render_note_lines<'a>(
    mut inner: iced::widget::Column<'a, Message>,
    lines: &'a [DiffLine],
) -> iced::widget::Column<'a, Message> {
    let mut same: Vec<&str> = Vec::new();
    for l in lines {
        if l.kind == DiffLineKind::Same {
            same.push(l.text.as_str());
            continue;
        }
        if !same.is_empty() {
            inner = inner.push(note_plain_block(&same));
            same.clear();
        }
        inner = inner.push(note_diff_line(l));
    }
    if !same.is_empty() {
        inner = inner.push(note_plain_block(&same));
    }
    inner
}

/// 未改动块的合并渲染（保持多行形态）。
fn note_plain_block<'a>(lines: &[&'a str]) -> Element<'a, Message> {
    text(lines.join("\n"))
        .font(mono())
        .shaping(iced::advanced::text::Shaping::Advanced)
        .size(pal::fs_micro())
        .color(pal::TEXT_PRIMARY)
        .width(Length::Fill)
        .into()
}

/// 改动行（空行补空格保持行高）。
fn note_diff_line<'a>(l: &'a DiffLine) -> Element<'a, Message> {
    let (bg, fg) = match l.kind {
        DiffLineKind::Add => (pal::SUCCESS.scale_alpha(0.14), pal::TEXT_PRIMARY),
        _ => (pal::DANGER.scale_alpha(0.14), pal::TEXT_PRIMARY),
    };
    let content = if l.text.is_empty() { " ".to_string() } else { l.text.clone() };
    container(
        text(content)
            .font(mono())
            .shaping(iced::advanced::text::Shaping::Advanced)
            .size(pal::fs_micro())
            .color(fg)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(4.0))
    .style(move |_t| Style {
        background: Some(bg.into()),
        ..Style::default()
    })
    .into()
}

/// 表格差异项：「字段 · 第 N 行：旧 → 新」的单元格行与其余单行，按类型着色。
fn render_table_items<'a>(
    mut inner: iced::widget::Column<'a, Message>,
    items: &'a [TableDiffItem],
) -> iced::widget::Column<'a, Message> {
    for item in items {
        match item {
            TableDiffItem::Line(l) => inner = inner.push(colored_diff_line(l)),
            TableDiffItem::Cell(c) => {
                inner = inner.push(
                    text(format!("{} · 第 {} 行：{} → {}", c.field, c.row, c.from, c.to))
                        .size(pal::fs_micro())
                        .color(pal::TEXT_SECONDARY)
                        .width(Length::Fill),
                )
            }
        }
    }
    inner
}

/// 画布差异行。
fn render_diff_lines<'a>(
    mut inner: iced::widget::Column<'a, Message>,
    lines: &'a [DiffLine],
) -> iced::widget::Column<'a, Message> {
    for l in lines {
        inner = inner.push(colored_diff_line(l));
    }
    inner
}

/// 差异行着色：新增绿、删除红、修改与截断提示次级色。
fn colored_diff_line(l: &DiffLine) -> Element<'_, Message> {
    text(l.text.clone()).size(pal::fs_micro()).color(diff_color(l.kind)).into()
}

fn diff_color(kind: DiffLineKind) -> iced::Color {
    match kind {
        DiffLineKind::Add => pal::SUCCESS,
        DiffLineKind::Remove => pal::DANGER,
        DiffLineKind::Change | DiffLineKind::Same => pal::TEXT_SECONDARY,
    }
}

/// 统计行（表格/画布预览首行）。
fn stat_line(label: String) -> Element<'static, Message> {
    text(label)
        .size(pal::fs_micro())
        .color(pal::TEXT_SECONDARY)
        .font(crate::ui::bold())
        .into()
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

/// 版本规模描述（版本无摘要时的回落）：表格按字段/行数、画布按节点/连线/消息数、笔记按字节估字数
/// （历史侧主要为中文正文，按 3 字节/字近似）。解析失败给中性文案，不谎报空快照。
pub(crate) fn scope_line(kind: HistoryKind, content: &str) -> String {
    match kind {
        HistoryKind::Note => format!("{} 字", content.len() / 3),
        HistoryKind::Table => serde_json::from_str::<atelyx_core::vault::TableFile>(content)
            .map(|t| format!("{} 字段 · {} 行", t.fields.len(), t.rows.len()))
            .unwrap_or_else(|_| "表格快照".to_string()),
        HistoryKind::Canvas => match serde_json::from_str::<atelyx_core::vault::CanvasFile>(content) {
            Ok(c) => {
                let messages = atelyx_core::history_diff::canvas_summary(content).2;
                format!("{} 节点 · {} 连线 · {messages} 消息", c.nodes.len(), c.edges.len())
            }
            Err(_) => "画布快照".to_string(),
        },
    }
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
