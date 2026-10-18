//! 仓库历史视图：文件组卡片列表（只读元数据聚合），组头点击打开该文件的历史弹层；
//! 本地扫侧文件元数据，空间走服务端聚合端点。

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, scrollable, space};
use iced::{Element, Length, Padding, Task};

use atelyx_core::history::HistoryKind;
use atelyx_core::space_client::HistoryAggregate;

use crate::space_actions;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message, RepoKind, layout_view};

/// 组内版本行（只读元数据；本地侧文件聚合与服务端聚合的共同投影）。
#[derive(Debug, Clone)]
pub struct RepoVersionRow {
    pub ts: i64,
    pub action: atelyx_core::history::HistoryAction,
    pub author_name: String,
    pub summary: Option<String>,
}

/// 按文件分组的历史条目（版本行新→旧；分组按最近版本时间新→旧）。
#[derive(Debug, Clone)]
pub struct RepoHistoryGroup {
    pub kind: HistoryKind,
    pub file: String,
    pub versions: Vec<RepoVersionRow>,
}

/// 本地侧文件聚合 → 分组（collect_all 版本为旧→新，展示前按时间新→旧重排；空侧文件丢弃）。
pub fn groups_from_local(files: Vec<atelyx_core::history::RepoHistoryFile>) -> Vec<RepoHistoryGroup> {
    let mut groups: Vec<RepoHistoryGroup> = files
        .into_iter()
        .filter(|f| !f.versions.is_empty())
        .map(|f| RepoHistoryGroup {
            kind: f.kind,
            file: f.file,
            versions: f
                .versions
                .into_iter()
                .map(|v| RepoVersionRow {
                    ts: v.ts,
                    action: v.action,
                    author_name: v.author.name,
                    summary: v.summary,
                })
                .collect(),
        })
        .collect();
    for g in &mut groups {
        g.versions.sort_by_key(|v| std::cmp::Reverse(v.ts));
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.versions[0].ts));
    groups
}

/// 服务端聚合版本流 → 分组（防御性收敛：不依赖服务端 entries 排序契约，统一按时间
/// 新→旧重排版本与分组，与本地聚合同序）。
pub fn groups_from_space(agg: HistoryAggregate) -> Vec<RepoHistoryGroup> {
    let mut groups: Vec<RepoHistoryGroup> = Vec::new();
    for e in agg.entries {
        let Some(kind) = parse_kind(&e.kind) else { continue };
        let version = RepoVersionRow {
            ts: e.ts,
            action: parse_action(&e.action),
            author_name: e.author_name,
            summary: e.summary,
        };
        if let Some(g) = groups.iter_mut().find(|g| g.kind == kind && g.file == e.file) {
            g.versions.push(version);
        } else {
            groups.push(RepoHistoryGroup { kind, file: e.file, versions: vec![version] });
        }
    }
    for g in &mut groups {
        g.versions.sort_by_key(|v| std::cmp::Reverse(v.ts));
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.versions[0].ts));
    groups
}

/// 服务端 kind 字段 → HistoryKind（未知值丢弃条目，不臆造归类）。
fn parse_kind(s: &str) -> Option<HistoryKind> {
    match s {
        "note" => Some(HistoryKind::Note),
        "canvas" => Some(HistoryKind::Canvas),
        "table" => Some(HistoryKind::Table),
        _ => None,
    }
}

/// 服务端 action 字段 → HistoryAction（服务端仅写 edit/restore，其余按 edit 展示）。
fn parse_action(s: &str) -> atelyx_core::history::HistoryAction {
    if s == "restore" {
        atelyx_core::history::HistoryAction::Restore
    } else {
        atelyx_core::history::HistoryAction::Edit
    }
}

/// 当前仓库容器是否就绪（本地仓库根可用 / 空间会话在位；仓库历史与日历共用）。
pub(crate) fn container_ready(app: &App) -> bool {
    match &app.repo_kind {
        RepoKind::Local => app.vault.root().is_ok(),
        RepoKind::Space(_) => space_actions::space_key_of(app).is_some(),
    }
}

/// 拉取仓库历史聚合（本地 spawn_blocking 扫侧文件元数据 / 空间走服务端聚合端点）。
pub fn load(app: &mut App) -> Task<Message> {
    app.repo_history_loading = true;
    let gen = app.epoch();
    match &app.repo_kind {
        RepoKind::Space(session) => {
            let client = session.client.clone();
            let space_id = session.space_id.clone();
            Task::perform(
                async move {
                    client.history_aggregate(&space_id).await.map(groups_from_space).map_err(|e| e.to_string())
                },
                move |result| Message::RepoHistoryLoaded(gen, result),
            )
        }
        RepoKind::Local => {
            let state = app.vault.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let (root, _, _) = state.session_snapshot()?;
                        atelyx_core::history::collect_all(&root).map(groups_from_local)
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
                },
                move |result| Message::RepoHistoryLoaded(gen, result),
            )
        }
    }
}

/// PersistTick 搭载：面板展示中且缓存失效（None）时发起聚合
/// （日历视图的天详情与活动密度复用同一份缓存，展示中同样触发）。
pub fn tick(app: &mut App) -> Task<Message> {
    if app.repo_history_loading || app.repo_history.is_some() {
        return Task::none();
    }
    let shown = layout_view::view_displayed(app, "repohistory")
        || layout_view::view_displayed(app, "calendar");
    if !container_ready(app) || !shown {
        return Task::none();
    }
    load(app)
}

/// 刷新：失效缓存重取（在途时不重复发起，回执自会落地）。
pub fn refresh(app: &mut App) -> Task<Message> {
    app.repo_history = None;
    if app.repo_history_loading || !container_ready(app) {
        return Task::none();
    }
    load(app)
}

/// 聚合落地（gen 守卫）：成功缓存列表；失败缓存 Err 并提示（仅手动刷新重试，
/// 不回退 None——周期重试失败会刷屏）。
pub fn on_loaded(app: &mut App, gen: u64, result: Result<Vec<RepoHistoryGroup>, String>) -> Task<Message> {
    app.repo_history_loading = false;
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Ok(groups) => app.repo_history = Some(Ok(groups)),
        Err(e) => {
            app.repo_history = Some(Err(e.clone()));
            app.notify(NoticeLevel::Error, format!("历史聚合失败：{e}"));
        }
    }
    Task::none()
}

/// 仓库历史视图：文件组卡片列表 + 组头（图标 + 文件名 + 全路径 + 版本数徽标）。
pub fn repohistory_view(app: &App, window: iced::window::Id) -> Element<'_, Message> {
    if !container_ready(app) {
        return controls::empty_state("history", "未打开仓库", "打开仓库后显示", None);
    }
    let header = container(
        row![
            space().width(Length::Fill),
            controls::icon_button(
                "refresh-cw",
                12.0,
                24.0,
                controls::Variant::Secondary,
                Some(Message::RepoHistoryRefresh),
                Some((window, "刷新".into())),
            ),
        ]
        .spacing(6),
    )
    .padding(Padding::ZERO.horizontal(12.0).vertical(8.0));
    let body: Element<'_, Message> = match &app.repo_history {
        Some(Err(e)) => controls::empty_state(
            "triangle-alert",
            "历史聚合失败",
            e.clone(),
            Some(("重试".into(), Message::RepoHistoryRefresh)),
        ),
        Some(Ok(groups)) if groups.is_empty() => controls::empty_state(
            "history",
            "暂无历史记录",
            "暂无版本记录",
            None,
        ),
        Some(Ok(groups)) => group_list(groups, window),
        // None = 未加载或在途（发起即置 loading，此分支只持续到下个 tick）
        None => {
            text(if app.repo_history_loading { "加载中…" } else { "暂无历史记录" })
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED)
                .into()
        }
    };
    let col = iced::widget::Column::<Message>::new().push(header).push(body);
    scrollable(container(col.width(Length::Fill)).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 文件组卡片列表：组卡片（图标 + 文件名 + 全路径 + 版本数徽标），点击打开该文件的历史弹层。
fn group_list<'a>(groups: &'a [RepoHistoryGroup], window: iced::window::Id) -> Element<'a, Message> {
    let mut col = iced::widget::Column::<Message>::new()
        .padding(Padding::ZERO.horizontal(12.0).vertical(4.0))
        .spacing(6);
    for g in groups {
        col = col.push(group_card(g, window));
    }
    container(col).width(Length::Fill).into()
}

/// 组卡片：kind 图标 + 文件名 + 全路径灰字 + 版本数徽标；点击 = 历史弹层（组内完整版本列表在弹层内浏览）。
fn group_card(g: &RepoHistoryGroup, window: iced::window::Id) -> Element<'static, Message> {
    let name = g.file.rsplit('/').next().unwrap_or(&g.file).to_string();
    let count = format!("{} 版", g.versions.len());
    container(
        button(
            row![
                icon(kind_icon(g.kind), 14.0, pal::TEXT_MUTED),
                column![
                    row![
                        text(name)
                            .size(pal::fs_body())
                            .color(pal::TEXT_PRIMARY)
                            .font(crate::ui::bold()),
                        space().width(Length::Fill),
                        controls::badge(count, pal::BG_SUNKEN, pal::TEXT_SECONDARY),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                    text(g.file.clone()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .padding(0.0)
        .style(controls::row_style(false, pal::RADIUS_MD))
        .on_press(Message::History(crate::history_browser::HistoryBrowserMsg::Open {
            window,
            kind: g.kind,
            file: g.file.clone(),
        })),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(10.0).vertical(8.0))
    .style(|_t| iced::widget::container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: iced::border::rounded(pal::RADIUS_MD),
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// kind → 行首图标（仓库历史行与日历天详情行共用）。
pub(crate) fn kind_icon(kind: HistoryKind) -> &'static str {
    match kind {
        HistoryKind::Note => "file-text",
        HistoryKind::Table => "table",
        HistoryKind::Canvas => "pencil",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::history::{HistoryAction, HistoryAuthor, RepoHistoryFile, VersionMeta};

    fn meta(ts: i64) -> VersionMeta {
        VersionMeta {
            seq: 1,
            ts,
            action: HistoryAction::Edit,
            summary: None,
            author: HistoryAuthor { id: "u".into(), name: "alice".into(), device: "d".into() },
        }
    }

    #[test]
    fn local_groups_sort_versions_and_groups_newest_first() {
        let files = vec![
            RepoHistoryFile {
                kind: HistoryKind::Note,
                file: "a.md".into(),
                versions: vec![meta(100), meta(300), meta(200)],
            },
            RepoHistoryFile {
                kind: HistoryKind::Table,
                file: "b.atb".into(),
                versions: vec![meta(50)],
            },
        ];
        let groups = groups_from_local(files);
        assert_eq!(groups.len(), 2);
        // 分组按最近版本时间倒序：a.md（最新 300）在前
        assert_eq!(groups[0].file, "a.md");
        assert_eq!(
            groups[0].versions.iter().map(|v| v.ts).collect::<Vec<_>>(),
            [300, 200, 100]
        );
        assert_eq!(groups[0].versions[0].author_name, "alice");
        assert_eq!(groups[1].file, "b.atb");
    }

    #[test]
    fn local_groups_drop_empty_side_files() {
        let files = vec![RepoHistoryFile {
            kind: HistoryKind::Note,
            file: "a.md".into(),
            versions: vec![],
        }];
        assert!(groups_from_local(files).is_empty());
    }

    #[test]
    fn space_entries_group_in_feed_order() {
        let agg: HistoryAggregate = serde_json::from_value(serde_json::json!({
            "entries": [
                { "file": "a.md", "kind": "note", "ts": 300, "authorId": "u", "authorName": "bob",
                  "authorDevice": "d", "action": "edit", "summary": "s1" },
                { "file": "b.atb", "kind": "table", "ts": 250, "authorId": "u", "authorName": "bob",
                  "authorDevice": "d", "action": "restore" },
                { "file": "a.md", "kind": "note", "ts": 100, "authorId": "u", "authorName": "bob",
                  "authorDevice": "d", "action": "edit" },
                { "file": "x.md", "kind": "plugin-kind", "ts": 90, "authorId": "u", "authorName": "bob",
                  "authorDevice": "d", "action": "edit" }
            ],
            "timestamps": []
        }))
        .unwrap();
        let groups = groups_from_space(agg);
        // 未知 kind 条目丢弃；同文件版本归同组，保持首次出现序
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].file, "a.md");
        assert_eq!(groups[0].versions.iter().map(|v| v.ts).collect::<Vec<_>>(), [300, 100]);
        assert_eq!(groups[0].versions[0].summary.as_deref(), Some("s1"));
        assert_eq!(groups[1].file, "b.atb");
        assert_eq!(groups[1].versions[0].action, HistoryAction::Restore);
    }
}
