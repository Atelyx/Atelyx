//! 设置-编辑器（仓库级）：批量重建内部链接（危险操作卡，二段确认 + 写盘队列执行）。

use iced::alignment::Vertical;
use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Length, Padding, Task, border, widget::container::Style};
use atelyx_core::rebuild_links;

use crate::settings_view::group_head;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::semibold;
use crate::{App, Message, RepoKind};

/// 重建链接会话态（设置-编辑器；随容器切换清空）。
#[derive(Debug, Clone, PartialEq)]
pub enum RebuildLinksState {
    Idle,
    /// 二段确认待确认。
    Confirm,
    /// 写盘队列执行中。
    Running,
    /// 完成（展示结果行）。
    Done { scanned: usize, links: usize, written: usize, failed: usize },
}

/// 重建链接消息。
#[derive(Debug, Clone)]
pub enum RebuildLinksMsg {
    /// 进入二段确认。
    Ask,
    /// 取消确认。
    Cancel,
    /// 确认执行。
    Run,
    /// 执行回执（gen 守卫）。
    Done(u64, Result<RebuildReport, String>),
}

/// 重建执行回执数据（队列作业闭包产出，随消息回传）。
#[derive(Debug, Clone)]
pub struct RebuildReport {
    pub scanned: usize,
    pub links: usize,
    pub written: usize,
    pub errors: Vec<(String, String)>,
    pub rewrites: Vec<String>,
}

/// 重建链接消息处理。
pub fn on_msg(app: &mut App, msg: RebuildLinksMsg) -> Task<Message> {
    match msg {
        RebuildLinksMsg::Ask => {
            app.rebuild_links = RebuildLinksState::Confirm;
            Task::none()
        }
        RebuildLinksMsg::Cancel => {
            app.rebuild_links = RebuildLinksState::Idle;
            Task::none()
        }
        RebuildLinksMsg::Run => run(app),
        RebuildLinksMsg::Done(gen, result) => on_done(app, gen, result),
    }
}

/// 执行重建：plan 纯读 + apply 按当前磁盘逐文件重算后写，整体占一个写盘队列执行位
/// （与自动保存等单文件写严格串行，批量改写不与其交错）。
fn run(app: &mut App) -> Task<Message> {
    // 空间模式真源在服务端且无批量端点：入口已禁用，此处兜底不执行
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return Task::none();
    }
    // 根在入队前快照：作业排队期间切仓不改变执行目标（闭包内不重读会话）
    let Ok(root) = app.vault.root() else {
        return Task::none();
    };
    app.rebuild_links = RebuildLinksState::Running;
    let gen = app.epoch();
    let queue = app.vault.writes.clone();
    Task::perform(
        async move {
            let r = queue.bulk(move || {
                let plan = rebuild_links::plan(&root)?;
                let outcome = rebuild_links::apply(&root, &plan)?;
                Ok(RebuildReport {
                    scanned: plan.scanned,
                    links: plan.links,
                    written: outcome.written,
                    errors: outcome.errors,
                    rewrites: plan.rewrites.into_iter().map(|(rel, _)| rel).collect(),
                })
            })
            .await;
            Message::RebuildLinks(RebuildLinksMsg::Done(gen, r))
        },
        |m| m,
    )
}

fn on_done(app: &mut App, gen: u64, result: Result<RebuildReport, String>) -> Task<Message> {
    // 仓库已切换：回执作废（不得把旧仓库的重建结果写进新仓库状态）
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Err(e) => {
            app.rebuild_links = RebuildLinksState::Idle;
            app.notify(NoticeLevel::Error, format!("重建失败：{e}"));
            Task::none()
        }
        Ok(report) => {
            app.rebuild_links = RebuildLinksState::Done {
                scanned: report.scanned,
                links: report.links,
                written: report.written,
                failed: report.errors.len(),
            };
            app.notify(
                NoticeLevel::Success,
                format!("已扫描 {} 个文件，更新 {} 个文件、{} 处链接", report.scanned, report.written, report.links),
            );
            if !report.errors.is_empty() {
                app.notify(NoticeLevel::Error, format!("{} 个文件写入失败", report.errors.len()));
            }
            // 打开中笔记被改写 → 重读正文对齐磁盘；有未保存编辑时不回读
            // （内存输入更新于重建基线，回读会丢用户输入）
            let reload = match app.ui.last_note_file.clone() {
                Some(rel) if report.rewrites.contains(&rel) && !app.dirty => Some(rel),
                _ => None,
            };
            match reload {
                Some(rel) => app.load_note(&rel),
                None => Task::none(),
            }
        }
    }
}

pub fn view(app: &App) -> Element<'static, Message> {
    let mut col = column![].spacing(16).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(group_head("批量操作", "仓库内容", true));
    col = col.push(rebuild_card(app));
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 重建内部链接卡（危险形态：红调边框 + danger 按钮）。
fn rebuild_card(app: &App) -> Element<'static, Message> {
    let space_mode = matches!(app.repo_kind, RepoKind::Space(_));
    let action: Element<'static, Message> = if space_mode {
        // 空间模式（含只读 viewer）：服务端无批量重建端点，统一禁用
        controls::btn("开始重建", &controls::btn_sm(), controls::Variant::Danger, None).into()
    } else {
        match &app.rebuild_links {
            RebuildLinksState::Idle | RebuildLinksState::Done { .. } => controls::btn(
                "开始重建",
                &controls::btn_sm(),
                controls::Variant::Danger,
                Some(Message::RebuildLinks(RebuildLinksMsg::Ask)),
            )
            .into(),
            RebuildLinksState::Confirm => row![
                controls::btn(
                    "确认重建",
                    &controls::btn_sm(),
                    controls::Variant::Danger,
                    Some(Message::RebuildLinks(RebuildLinksMsg::Run)),
                ),
                controls::btn(
                    "取消",
                    &controls::btn_sm(),
                    controls::Variant::Ghost,
                    Some(Message::RebuildLinks(RebuildLinksMsg::Cancel)),
                ),
            ]
            .spacing(8)
            .into(),
            RebuildLinksState::Running => {
                controls::btn("重建中…", &controls::btn_sm(), controls::Variant::Secondary, None).into()
            }
        }
    };
    let mut card = column![
        row![
            column![
                row![icon("triangle-alert", 13.0, pal::DANGER), text("重建内部链接").size(pal::fs_caption()).color(pal::TEXT_PRIMARY).font(semibold())]
                    .spacing(6)
                    .align_y(Vertical::Center),
                text("将批量改写仓库内全部 .md 的链接写法，统一为标准 Markdown 链接。此操作不可撤销，建议先备份重要笔记。")
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            ]
            .spacing(4)
            .width(Length::Fill),
            action,
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    ]
    .spacing(8);
    if space_mode {
        card = card.push(
            text("协作空间不支持批量重建").size(pal::fs_micro()).color(pal::TEXT_MUTED),
        );
    }
    if let RebuildLinksState::Done { scanned, links, written, failed } = &app.rebuild_links {
        let mut line = text(format!("已扫描 {scanned} 个文件，更新 {written} 个文件、{links} 处链接"))
            .size(pal::fs_micro())
            .color(pal::TEXT_SECONDARY);
        if *failed > 0 {
            line = line.color(pal::DANGER);
        }
        card = card.push(line);
    }
    container(card)
        .width(Length::Fill)
        .padding(12.0)
        .style(|_t| Style {
            background: Some(pal::BG_PRIMARY.into()),
            border: border::rounded(pal::RADIUS_MD).color(pal::DANGER).width(1.0),
            ..Style::default()
        })
        .into()
}
